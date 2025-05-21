use std::env;
use std::path::PathBuf;

use clap::Parser;
use log::{debug, error, info, trace};
use thiserror::Error;
use tokio::signal;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use bitcoin::BlockHash;
use bitcoin_ipc::db::{self, Database, HeedDb};
use bitcoin_ipc::ipc_lib::{self, IpcLibError, IpcValidate, IpcValidateError};
use bitcoin_ipc::{bitcoin_utils, eth_utils, IpcMessage, BTC_CONFIRMATIONS};
use bitcoincore_rpc::RpcApi;

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

    let btc_rpc = bitcoin_utils::make_rpc_client_from_env();
    let btc_watchonly_rpc = bitcoin_utils::make_watchonly_rpc_client_from_env();

    // Set correct fvm network

    eth_utils::set_fvm_network();

    // Create a cancellation token for the monitor

    let cancel_token = CancellationToken::new();

    let mut monitor = Monitor::new(
        db,
        btc_rpc,
        btc_watchonly_rpc,
        POLL_INTERVAL,
        cancel_token.clone(),
    );

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

/// Side-effects that need to be executed after processing blocks
#[derive(Debug)]
enum SideEffect {
    ImportAddress {
        address: bitcoin::Address,
        label: String,
        timestamp: u32, // block.header.time
    },
}

// TODO use generics for rpc + add a trait for the monitor
struct Monitor<D: Database> {
    db: D,
    rpc: bitcoincore_rpc::Client,
    watchonly_rpc: bitcoincore_rpc::Client,
    check_interval: Duration,
    current_height: u64,
    cancel_token: CancellationToken,
    side_effects: Vec<SideEffect>,
}

impl<D> Monitor<D>
where
    D: Database,
{
    fn new(
        db: D,
        rpc: bitcoincore_rpc::Client,
        watchonly_rpc: bitcoincore_rpc::Client,
        check_interval: Duration,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            db,
            rpc,
            watchonly_rpc,
            check_interval,
            cancel_token,
            current_height: 0,
            side_effects: Vec::new(),
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

        // Process any collected side effects
        self.process_side_effects()?;

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
                    // It could happen, in regtest especially, that there are
                    // multiple blocks mined at once, so we need to process all of them
                    // and not just the most recent one
                    while self.current_height < block_count {
                        let next_height = self.current_height + 1;

                        match self.process_block(next_height).await {
                            Ok(_) => {
                                info!("Processed block {}", next_height);
                                self.current_height = next_height;
                                if let Err(e) =
                                    self.db.set_last_processed_block(self.current_height)
                                {
                                    error!("Failed to update last processed block: {:?}", e);
                                    return Err(e.into());
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

            // Process any collected side effects
            self.process_side_effects()?;

            tokio::time::sleep(self.check_interval).await;
        }
    }

    fn import_watchonly_address(
        &mut self,
        address: bitcoin::Address,
        label: String,
        timestamp: u32,
    ) {
        self.side_effects.push(SideEffect::ImportAddress {
            address,
            label,
            timestamp,
        });
    }

    fn process_side_effects(&mut self) -> Result<(), MonitorError> {
        if self.side_effects.is_empty() {
            trace!("No side effects to process");
            return Ok(());
        }
        debug!("Processing side effects.");

        // Group import address effects
        let import_addresses: Vec<(bitcoin::Address, String, u32)> = self
            .side_effects
            .iter()
            .map(|effect| {
                let SideEffect::ImportAddress {
                    address,
                    label,
                    timestamp,
                } = effect;

                (address.clone(), label.clone(), *timestamp)
            })
            .collect();

        if !import_addresses.is_empty() {
            // Import all addresses in a batch, each with its own timestamp
            bitcoin_ipc::wallet::import_address_batch(&self.watchonly_rpc, &import_addresses)?;

            debug!("Imported {} addresses in batch.", import_addresses.len());
        }

        // Clear processed side effects
        self.side_effects.clear();
        Ok(())
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

    async fn process_block(&mut self, block_height: u64) -> Result<(), MonitorError> {
        debug!("Processing block {}", block_height);
        let block_hash = self.rpc.get_block_hash(block_height)?;
        let block = self.rpc.get_block(&block_hash)?;

        for tx in block.txdata {
            self.process_transaction(&tx, block_height, block_hash, block.header.time)
                .await?;
        }

        Ok(())
    }

    async fn process_transaction(
        &mut self,
        tx: &bitcoin::Transaction,
        block_height: u64,
        block_hash: BlockHash,
        block_time: u32,
    ) -> Result<(), MonitorError> {
        let txid = tx.compute_txid();
        debug!("Processing transaction {}", txid);

        // Process inputs

        for input in &tx.input {
            // TODO check more efficiently if witness has IPC tag
            for witness in input.witness.iter().filter(|w| !w.is_empty()) {
                // Reconstruct the witness data
                let witness_data = match bitcoin_utils::concatenate_op_push_data(witness) {
                    Ok(data) => data,
                    Err(_) => {
                        continue;
                    }
                };

                let ipc_message = IpcMessage::from_witness(witness_data);
                let ipc_message = match ipc_message {
                    Ok(msg) => msg,
                    Err(e) => {
                        trace!("Error deserializing IPC message: {:?}", e);
                        continue;
                    }
                };

                match self
                    .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
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
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(e)?,
            }
        } else if let Ok(fund_msg) = ipc_lib::IpcFundSubnetMsg::from_tx(tx) {
            let ipc_message = IpcMessage::FundSubnet(fund_msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(e)?,
            }
        } else if let Ok(checkpoint_msg) =
            ipc_lib::IpcCheckpointSubnetMsg::from_checkpoint_tx(&self.db, tx)
        {
            let ipc_message = IpcMessage::CheckpointSubnet(checkpoint_msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(e)?,
            }
        } else if let Ok(msg) = ipc_lib::IpcBatchTransferMsg::from_tx(&self.db, tx) {
            let ipc_message = IpcMessage::BatchTransfer(msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(e)?,
            }
        } else if let Ok(msg) = ipc_lib::IpcStakeCollateralMsg::from_tx(&self.db, tx) {
            let ipc_message = IpcMessage::StakeCollateral(msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(e)?,
            }
        } else if let Ok(msg) = ipc_lib::IpcUnstakeCollateralMsg::from_tx(&self.db, tx) {
            let ipc_message = IpcMessage::UnstakeCollateral(msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(e)?,
            }
        }

        Ok(())
    }

    async fn process_ipc_msg(
        &mut self,
        block_height: u64,
        block_hash: BlockHash,
        block_time: u32,
        tx: &bitcoin::Transaction,
        txid: bitcoin::Txid,
        msg: IpcMessage,
    ) -> Result<(), MonitorError> {
        match msg {
            IpcMessage::CreateSubnet(create_subnet_msg) => {
                debug!("Found IPC message: {:?}", create_subnet_msg);
                create_subnet_msg.validate()?;
                // Save to db
                let subnet_genesis_info =
                    create_subnet_msg.save_to_db(&self.db, block_height, txid)?;

                let (whitelist_addr, label) = subnet_genesis_info.whitelist_address_label();

                self.import_watchonly_address(whitelist_addr, label, block_time);

                info!(
                    "Processed CreateSubnet for Subnet ID: {}",
                    subnet_genesis_info.subnet_id
                );

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

                let genesis_info = self
                    .db
                    .get_subnet_genesis_info(join_subnet_msg.subnet_id)
                    .map_err(|e| {
                        error!("Error getting subnet info from Db: {}", e);
                        MonitorError::DbError(e)
                    })?
                    .ok_or(ipc_lib::IpcValidateError::InvalidMsg(format!(
                        "Subnet {} not found.",
                        join_subnet_msg.subnet_id
                    )))?;

                let subnet_state = self
                    .db
                    .get_subnet_state(join_subnet_msg.subnet_id)
                    .map_err(MonitorError::DbError)?;

                join_subnet_msg.validate_for_subnet(&genesis_info, &subnet_state)?;

                join_subnet_msg.validate()?;
                if let Some(subnet) =
                    join_subnet_msg.save_to_db(&self.db, block_height, block_hash, txid)?
                {
                    // Subnet bootstrapped
                    let (committee_addr, label) = subnet.committee_address_label();
                    self.import_watchonly_address(committee_addr, label, block_time);
                }

                info!(
                    "Processed JoinSubnet for Subnet ID: {} Validator XPK: {} Collateral: {}",
                    join_subnet_msg.subnet_id, join_subnet_msg.pubkey, join_subnet_msg.collateral
                );
                Ok(())
            }

            IpcMessage::PrefundSubnet(msg) => {
                debug!("Found IPC message: {:?}", msg);
                msg.validate()?;
                msg.save_to_db(&self.db, block_height, txid)?;
                info!(
                    "Processed PrefundSubnet for Subnet ID: {} Address: {}",
                    msg.subnet_id, msg.address
                );
                Ok(())
            }

            IpcMessage::FundSubnet(msg) => {
                debug!("Found IPC message: {:?}", msg);
                msg.validate()?;
                msg.save_to_db(&self.db, block_height, block_hash, txid)?;
                info!(
                    "Processed FundSubnet for Subnet ID: {} Address: {} Amount: {}",
                    msg.subnet_id, msg.address, msg.amount
                );
                Ok(())
            }

            IpcMessage::CheckpointSubnet(msg) => {
                debug!("Found IPC message: {:#?}", msg);

                msg.validate()?;
                let checkpoint = msg.save_to_db(&self.db, block_height, block_hash, txid)?;

                // Save the checkpoint tx to db
                // we need it available for any batch transfer messages
                {
                    let mut wtxn = self.db.write_txn()?;
                    self.db.save_transaction(&mut wtxn, tx)?;
                    wtxn.commit().map_err(db::DbError::from)?;
                }

                // import new address if the committee changed
                if checkpoint.signed_committee_number != checkpoint.next_committee_number {
                    // get the update subnet state from the database
                    let subnet = self
                        .db
                        .get_subnet_state(msg.subnet_id)
                        .map_err(MonitorError::DbError)?
                        // Should never happen
                        .ok_or(IpcValidateError::InvalidMsg(
                            "Could not fetch subnet id after a checkpoint".to_string(),
                        ))?;

                    info!(
                        "Committee changed for Subnet ID: {}, importing address.",
                        msg.subnet_id
                    );

                    let (new_committee_addr, new_label) = subnet.committee_address_label();
                    self.import_watchonly_address(new_committee_addr, new_label, block_time);
                }

                info!(
                    "Processed CheckpointSubnet for Subnet ID: {} Subnet Height: {} Checkpoint Number: {}",
                    msg.subnet_id, msg.checkpoint_height, checkpoint.checkpoint_number
                );

                Ok(())
            }

            IpcMessage::BatchTransfer(mut msg) => {
                debug!("Found IPC message: {:?}", msg);

                msg.validate()?;

                // Get checkpoint tx and parse as msg

                let checkpoint_tx = self.db.get_transaction(&msg.checkpoint_txid)
                    .map_err(MonitorError::DbError)?
                    .ok_or_else(|| {
                        MonitorError::IpcTxInvalid(format!(
                            "Can't get Checkpoint Txid {} for BatchTransferMsg: not found in database",
                            msg.checkpoint_txid
                        ))
                    })?;

                let checkpoint_msg =
                    ipc_lib::IpcCheckpointSubnetMsg::from_checkpoint_tx(&self.db, &checkpoint_tx)
                        .map_err(|e| {
                        MonitorError::IpcTxInvalid(format!(
                            "BatchTransferMsg has invalid CheckpointMsg as previous tx: {e}"
                        ))
                    })?;

                trace!("BatchTransfer: CheckpointMsg {:?}", checkpoint_msg);

                checkpoint_msg.validate()?;
                msg.validate_for_checkpoint(&checkpoint_tx)?;
                msg.subnet_id = checkpoint_msg.subnet_id;

                // Save to DB

                msg.save_to_db(&self.db, block_height, block_hash, txid)?;

                info!(
                    "Processed BatchTransfer for Subnet ID: {} Checkpoint Txid: {} Number of transfers: {}",
                    msg.subnet_id, msg.checkpoint_txid, msg.transfers.len(),
                );

                Ok(())
            }

            IpcMessage::StakeCollateral(msg) => {
                debug!("Found IPC message: {:?}", msg);
                msg.validate()?;
                msg.save_to_db(&self.db, block_height, block_hash, txid)?;
                info!(
                    "Processed StakeCollateral for Subnet ID: {} Validator XPK: {} Amount: {}",
                    msg.subnet_id, msg.pubkey, msg.amount
                );
                Ok(())
            }

            IpcMessage::UnstakeCollateral(msg) => {
                debug!("Found IPC message: {:?}", msg);
                msg.validate()?;
                msg.save_to_db(&self.db, block_height, block_hash, txid)?;
                info!(
                    "Processed UnstakeCollateral for Subnet ID: {} Validator XPK: {:?} Amount: {}",
                    msg.subnet_id, msg.pubkey, msg.amount
                );
                Ok(())
            }

            IpcMessage::LeaveSubnet(msg) => {
                debug!("Found IPC message: {:?}", msg);
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
    DbError(#[from] db::DbError),

    #[error(transparent)]
    BitcoinRpcError(#[from] bitcoincore_rpc::Error),
}
