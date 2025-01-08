use bitcoin_ipc::bitcoin_utils::{concatenate_op_push_data, make_rpc_client_from_env};
use bitcoin_ipc::db::{self, Database, HeedDb};
use bitcoin_ipc::ipc_lib::{self, IpcValidate};
use bitcoin_ipc::{IpcMessage, BTC_CONFIRMATIONS};
use bitcoincore_rpc::RpcApi;
use dotenv::dotenv;
use log::{debug, error, info};
use thiserror::Error;
use tokio::signal;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

// TODO make configurable
const POLL_INTERVAL: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() {
    // Load .env file

    dotenv().ok();

    // Initialize the logger, configurable by the RUST_LOG env

    env_logger::init();

    // Initialize the database
    let db = HeedDb::new(
        &std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
        false,
    )
    .await
    .expect("Failed to initialize database");

    // Init the bitcoincore_rpc client

    let btc_rpc = make_rpc_client_from_env();

    // Create a cancellation token for the monitor

    let cancel_token = CancellationToken::new();

    let mut monitor = Monitor::new(db, btc_rpc, POLL_INTERVAL, cancel_token.clone());

    // Spawn monitor task

    let monitor_handle = tokio::spawn(async move { monitor.sync_and_listen().await });

    // Listen for ctrl+c

    tokio::spawn(async move {
        match signal::ctrl_c().await {
            Ok(()) => {
                println!(); // print new line after ^C
                info!("Received Ctrl+C, initiating shutdown...");

                // Send cancellation signal to the monitor
                cancel_token.cancel();
            }
            Err(err) => {
                error!("Error listening for Ctrl+C: {}", err);
            }
        }
    });

    match monitor_handle.await {
        Ok(Ok(())) => info!("Monitor completed"),
        Ok(Err(e)) => error!("Monitor failed: {:?}", e),
        Err(e) => error!("Monitor panicked: {:?}", e),
    }

    info!("Shutting down");
}

// TODO use generics for rpc + add a trait for the monitor
struct Monitor<D: Database> {
    db: D,
    rpc: bitcoincore_rpc::Client,
    check_interval: Duration,
    current_height: u64,
    cancel_token: CancellationToken,
}

impl<D> Monitor<D>
where
    D: Database,
{
    fn new(
        db: D,
        rpc: bitcoincore_rpc::Client,
        check_interval: Duration,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            db,
            rpc,
            check_interval,
            cancel_token,
            current_height: 0,
        }
    }

    /// Syncs with the Bitcoin network
    /// Returns `Ok(true)` if it finished syncing
    async fn sync(&mut self) -> Result<bool, MonitorError> {
        info!("Syncing...");

        // Get the last processed block from the database
        self.current_height = self.db.get_last_processed_block().await?;

        loop {
            if self.cancel_token.is_cancelled() {
                debug!("Cancellation requested, stopping sync");
                return Ok(false);
            }

            // Get the latest block height
            let latest_block_height = self.get_latest_confirmed_height()?;

            debug!("Latest confirmed block height: {}", latest_block_height);

            if self.current_height > latest_block_height {
                error!("Current block height is greater than the latest block height. Aborting.");
                return Err(MonitorError::BlockHeightAheadOfTip(
                    latest_block_height,
                    self.current_height,
                ));
            }

            // Process blocks from current_height to latest_block_height
            while self.current_height < latest_block_height {
                if self.cancel_token.is_cancelled() {
                    debug!("Cancellation requested, stopping sync");
                    return Ok(false);
                }

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

        Ok(true)
    }

    async fn listen(&mut self) -> Result<(), MonitorError> {
        info!("Listening for new blocks");

        loop {
            if self.cancel_token.is_cancelled() {
                debug!("Cancellation requested, stopping listener");
                return Ok(());
            }

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

    async fn sync_and_listen(&mut self) -> Result<(), MonitorError> {
        let syncing_completed = match self.sync().await {
            Err(e) => {
                error!("Error syncing: {}", e);
                return Err(e);
            }
            Ok(r) => r,
        };

        // If sync is not completed, return early
        // Syncing could be interrupted by a cancellation request
        // or by an error
        if !syncing_completed {
            return Ok(());
        };

        if let Err(e) = self.listen().await {
            error!("Error listening: {}", e);
            return Err(e);
        }
        Ok(())
    }

    fn get_latest_confirmed_height(&self) -> Result<u64, MonitorError> {
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

                let subnet_id = ipc_lib::SubnetId::from_txid(txid);

                let multisig_addr = create_subnet_params
                    .multisig_address_from_whitelist()
                    .map_err(|e| MonitorError::IpcMsgError(e.to_string()))?;

                debug!("multisig_address: {}", multisig_addr);

                debug!(
                    "block={} subnet_id={} msg={:?}",
                    block_height, subnet_id, create_subnet_params
                );

                // TODO handle errors better
                if let Err(e) = self
                    .db
                    .save_subnet_create_msg(subnet_id, block_height, create_subnet_params.clone())
                    .await
                {
                    error!("Failed to save subnet to DB: {:?}", e);
                }

                Ok(())
            }

            IpcMessage::PrefundSubnet(_) => todo!(),
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
    #[error("Current block height ahead of tip. Latest: {0}. Current: {1}")]
    BlockHeightAheadOfTip(u64, u64),

    // TODO better errors
    #[error("Error processing IPC message: {0}")]
    IpcMsgError(String),

    #[error(transparent)]
    DbError(#[from] db::DbError),

    #[error(transparent)]
    BitcoinRpcError(#[from] bitcoincore_rpc::Error),
}
