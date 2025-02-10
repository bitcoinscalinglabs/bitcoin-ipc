use std::env;
use std::path::PathBuf;

use bitcoin_ipc::bitcoin_utils::{concatenate_op_push_data, make_rpc_client_from_env};
use bitcoin_ipc::db::{self, Database, HeedDb};
use bitcoin_ipc::ipc_lib::{self, IpcLibError, IpcValidate};
use bitcoin_ipc::{eth_utils, IpcMessage, BTC_CONFIRMATIONS};
use bitcoincore_rpc::RpcApi;
use clap::Parser;
use log::{debug, error, info, trace};
use thiserror::Error;
use tokio::signal;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

// TODO make configurable
const POLL_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to env file
    #[arg(long, default_value = ".env")]
    env: String,
}

#[tokio::main]
async fn main() {
    // Parse command line arguments

    let args = Args::parse();

    // Load .env file

    let env_path = if args.env.starts_with('/') {
        PathBuf::from(&args.env)
    } else {
        env::current_dir().map(|a| a.join(&args.env)).unwrap()
    };

    dotenv::from_path(env_path.as_path())
        .unwrap_or_else(|_| panic!("Failed to load env file: {}", args.env));

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

    // Set correct fvm network

    eth_utils::set_fvm_network();

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
        self.current_height = self
            .db
            .get_last_processed_block()
            .map_err(MonitorError::CannotGetMonitorInfo)?;

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
                        if let Err(e) = self.db.set_last_processed_block(self.current_height) {
                            error!("Failed to update last processed block: {:?}", e);
                        }
                    }
                    Err(e) => {
                        error!("Error processing block {}: {:?}.", next_height, e);
                        // Retry logic can be added here if needed
                        return Err(e);
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
                                    self.db.set_last_processed_block(self.current_height)
                                {
                                    error!("Failed to update last processed block: {:?}", e);
                                }
                            }
                            Err(e) => {
                                error!("Error processing block {}: {:?}.", block_count, e);
                                // Retry logic can be added here if needed
                                return Err(e);
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

                let ipc_message = match ipc_message {
                    Ok(msg) => msg,
                    Err(e) => {
                        trace!("Error deserializing IPC message: {:?}", e);
                        continue;
                    }
                };

                match self
                    .process_ipc_msg(block_height, tx, txid, ipc_message)
                    .await
                {
                    Ok(_) => {}
                    Err(e) => self.handle_ipc_msg_error(e)?,
                }
            }
        }

        // Process outputs

        // TODO make a general way to find messages in outputs or from entire tx
        if let Ok(prefund_msg) = ipc_lib::IpcPrefundSubnetMsg::from_tx(tx) {
            let ipc_message = IpcMessage::PrefundSubnet(prefund_msg);

            match self
                .process_ipc_msg(block_height, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(e)?,
            }
        }

        Ok(())
    }

    async fn process_ipc_msg(
        &self,
        block_height: u64,
        tx: &bitcoin::Transaction,
        txid: bitcoin::Txid,
        msg: IpcMessage,
    ) -> Result<(), MonitorError> {
        match msg {
            IpcMessage::CreateSubnet(create_subnet_msg) => {
                debug!("Found IPC message: {:?}", create_subnet_msg);
                create_subnet_msg.validate()?;
                let subnet_id = create_subnet_msg.save_to_db(&self.db, block_height, txid)?;
                info!("Processed CreateSubnet for Subnet ID: {}", subnet_id);
                Ok(())
            }

            IpcMessage::JoinSubnet(mut join_subnet_msg) => {
                join_subnet_msg.collateral = match tx.output.first() {
                    Some(output) => output.value,
                    None => {
                        debug!("Found IPC message: {:?}", join_subnet_msg);
                        return Err(MonitorError::IpcTxInvalid(
                            "Transaction output must be non zero".to_string(),
                        ));
                    }
                };
                debug!("Found IPC message: {:?}", join_subnet_msg);

                join_subnet_msg.validate()?;
                join_subnet_msg.save_to_db(&self.db, block_height, txid)?;
                info!(
                    "Processed JoinSubnet for Subnet ID: {}",
                    join_subnet_msg.subnet_id
                );
                Ok(())
            }

            IpcMessage::PrefundSubnet(msg) => {
                debug!("Found IPC message: {:?}", msg);
                msg.validate()?;
                // msg.save_to_db(&self.db, block_height, txid)?;
                info!("Processed PrefundSubnet for Subnet ID: {}", msg.subnet_id);
                Ok(())
            }
        }
    }

    /// Helper function to handle IPC message processing errors
    fn handle_ipc_msg_error(&self, error: MonitorError) -> Result<(), MonitorError> {
        match error {
            // Ignorable errors
            MonitorError::IpcMsgInvalid(e) => {
                error!("Invalid IPC message: {:?}", e);
                Ok(())
            }
            MonitorError::IpcTxInvalid(e) => {
                error!("Invalid IPC message transaction: {:?}", e);
                Ok(())
            }
            MonitorError::IpcMsgProcessingError(IpcLibError::IpcValidateError(e)) => {
                error!("Invalid IPC message: {:?}", e);
                Ok(())
            }
            // Propagate all other errors
            e => {
                error!("Fatal error processing IPC message: {:?}", e);
                Err(e)
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
    #[error("Current block height ahead of tip. Latest: {0}. Current: {1}")]
    BlockHeightAheadOfTip(u64, u64),

    #[error("IPC message error: {0}")]
    IpcTxInvalid(String),

    /// Returned when an IPC message is invalid
    /// The message is ignored
    #[error(transparent)]
    IpcMsgInvalid(#[from] ipc_lib::IpcValidateError),

    /// Returned when there was an error processing
    /// an IPC message
    ///
    /// All variants of this error are fatal, except for `IpcValidateError`
    #[error(transparent)]
    IpcMsgProcessingError(#[from] IpcLibError),

    #[error("Cannot get last processed block: {0}")]
    CannotGetMonitorInfo(db::DbError),

    #[error(transparent)]
    BitcoinRpcError(#[from] bitcoincore_rpc::Error),
}
