use bitcoin_ipc::bitcoin_utils::{concatenate_op_push_data, make_rpc_client_from_env};
use bitcoin_ipc::db::{self, Database, Db};
use bitcoin_ipc::ipc_lib::{self, IpcValidate};
use bitcoin_ipc::{IpcMessage, BTC_CONFIRMATIONS};
use bitcoincore_rpc::RpcApi;
use dotenv::dotenv;
use log::{debug, error, info};
use thiserror::Error;
use tokio::signal;
use tokio::sync::oneshot;
use tokio::time::Duration;

// TODO make configurable
const POLL_INTERVAL: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() {
    // Load .env file

    dotenv().ok();

    // Initialize the logger, configurable by the RUST_LOG env

    env_logger::init();

    // Initialize the database
    let db = Db::new(&std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Failed to initialize database");

    // Init the bitcoincore_rpc client

    let btc_rpc = make_rpc_client_from_env();

    // TODO make configurable
    let mut monitor = Monitor::new(db, btc_rpc, POLL_INTERVAL);

    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        // Sync
        if let Err(e) = monitor.sync().await {
            error!("Error syncing: {:?}", e);
            // Signal termination
            tx.send(Err(e)).expect("Could not signal termination.");
        }
        // Listen for new block
        monitor.listen().await;
    });

    // Wait for a termination signal (e.g., Ctrl+C) or the spawned task to complete
    tokio::select! {
        _ = signal::ctrl_c() => {
            println!(); // print new line after ^C
            info!("Received Ctrl+C");
        }
        result = rx => {
            match result {
                Ok(Ok(())) => info!("Monitor task completed"),
                Ok(Err(e)) => error!("Monitor task failed: {:?}", e),
                Err(_) => error!("Monitor task channel closed unexpectedly"),
            }
        }
    }

    info!("Shutting down");
}

// TODO use generics for deps + add a trait for the monitor
struct Monitor {
    db: Db,
    rpc: bitcoincore_rpc::Client,
    check_interval: Duration,
    current_height: u64,
}

impl Monitor {
    fn new(db: Db, rpc: bitcoincore_rpc::Client, check_interval: Duration) -> Self {
        Self {
            db,
            rpc,
            check_interval,
            current_height: 0,
        }
    }

    async fn sync(&mut self) -> Result<(), bitcoincore_rpc::Error> {
        info!("Syncing...");

        // Get the last processed block from the database
        // TODO handle errors
        self.current_height = self.db.get_last_processed_block().await.unwrap_or(0);

        loop {
            // Get the latest block height
            let latest_block_height = self.get_latest_confirmed_height()?;

            // Process blocks from current_height to latest_block_height
            while self.current_height < latest_block_height {
                let next_height = self.current_height + 1;
                match self.process_block(next_height).await {
                    Ok(_) => {
                        info!("Processed block {}", next_height);
                        self.current_height = next_height;
                        if let Err(e) = self.db.set_last_processed_block(self.current_height).await
                        {
                            error!("Failed to update last processed block: {:?}", e);
                        }
                    }
                    Err(e) => {
                        error!(
                            "Error processing block {}: {:?}. Retrying...",
                            next_height, e
                        );
                        // Retry logic can be added here if needed
                    }
                }
            }

            // Refetch the latest block height
            let latest_block_height = self.get_latest_confirmed_height()?;

            // Check if we are up-to-date
            if self.current_height == latest_block_height {
                info!("Sync completed");
                break;
            }
        }

        Ok(())
    }

    async fn listen(&mut self) {
        info!("Listening for new blocks");
        loop {
            match self.get_latest_confirmed_height() {
                Ok(block_count) => {
                    if block_count > self.current_height {
                        match self.process_block(block_count).await {
                            Ok(_) => {
                                info!("Processed block {}", block_count);
                                self.current_height = block_count;
                                if let Err(e) =
                                    self.db.set_last_processed_block(self.current_height).await
                                {
                                    error!("Failed to update last processed block: {:?}", e);
                                }
                            }
                            Err(e) => {
                                error!(
                                    "Error processing block {}: {:?}. Retrying...",
                                    block_count, e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Error fetching block count: {:?}", e);
                }
            }
            tokio::time::sleep(self.check_interval).await;
        }
    }

    fn get_latest_confirmed_height(&self) -> Result<u64, bitcoincore_rpc::Error> {
        let latest = self.rpc.get_block_count()?;

        // Since BTC_CONFIRMATIONS is 0 in regtest and sigtest
        // Clippy will complain about absurd comparisons
        #[allow(clippy::absurd_extreme_comparisons)]
        if latest < BTC_CONFIRMATIONS {
            return Ok(0);
        }

        Ok(latest - BTC_CONFIRMATIONS)
    }

    async fn process_block(&self, block_height: u64) -> Result<(), MonitorError> {
        info!("Processing block {}", block_height);
        let block_hash = self.rpc.get_block_hash(block_height)?;
        let block = self.rpc.get_block(&block_hash)?;

        for tx in block.txdata {
            self.process_transaction(&tx, block_height).await?;
        }

        Ok(())
    }

    async fn process_transaction(
        &self,
        tx: &bitcoin::Transaction,
        block_height: u64,
    ) -> Result<(), MonitorError> {
        let txid = tx.compute_txid();
        debug!("Processing transaction {}", txid);

        // Process inputs

        for input in &tx.input {
            // TODO check more efficiently if witness has IPC tag
            for witness in input.witness.iter().filter(|w| !w.is_empty()) {
                // Reconstruct the witness data
                let concatenated_data = match concatenate_op_push_data(witness) {
                    Ok(data) => data,
                    Err(_) => {
                        continue;
                    }
                };

                let witness_str = find_valid_utf8(&concatenated_data);
                let ipc_message = IpcMessage::deserialize(witness_str);

                match ipc_message {
                    Ok(msg) => {
                        self.process_ipc_msg(block_height, &txid, msg).await?;
                    }
                    Err(_) => {
                        continue;
                    }
                }
            }
        }

        Ok(())
    }

    async fn process_ipc_msg(
        &self,
        block_height: u64,
        txid: &bitcoin::Txid,
        msg: IpcMessage,
    ) -> Result<(), MonitorError> {
        match msg {
            IpcMessage::CreateSubnet(create_subnet_params) => {
                if let Err(e) = create_subnet_params.validate() {
                    error!(
                        "create_subnet msg invalid msg={:?} error={:?}",
                        create_subnet_params, e
                    );
                    return Ok(());
                }

                let subnet_id = ipc_lib::subnet_id_from_txid(txid);

                debug!(
                    "block={} subnet_id={} msg={:?}",
                    block_height, subnet_id, create_subnet_params
                );

                // TODO handle errors better
                if let Err(e) = self
                    .db
                    .save_subnet_create_msg(block_height, &subnet_id, &create_subnet_params)
                    .await
                {
                    error!("Failed to save subnet to DB: {:?}", e);
                }

                Ok(())
            }
        }
    }
}

fn find_valid_utf8(data: &[u8]) -> &str {
    let mut start = 0;
    while start < data.len() {
        match std::str::from_utf8(&data[start..]) {
            Ok(valid_str) => return valid_str,
            Err(_) => start += 1,
        }
    }
    ""
}

#[derive(Error, Debug)]
pub enum MonitorError {
    #[error(transparent)]
    DbError(#[from] db::DbError),

    #[error(transparent)]
    BitcoinRpcError(#[from] bitcoincore_rpc::Error),
}
