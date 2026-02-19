use std::env;
use std::path::PathBuf;

use clap::Parser;
use log::{debug, error, info, trace};
use thiserror::Error;
use tokio::signal;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use bitcoin::BlockHash;
use bitcoin_ipc::db::{self, BitcoinIpcDatabase, HeedDb};
use bitcoin_ipc::ipc_lib::{self, IpcLibError, IpcValidate, IpcValidateError};
use bitcoin_ipc::{bitcoin_utils, eth_utils, IpcMessage, BTC_CONFIRMATIONS};
use bitcoincore_rpc::RpcApi;

#[cfg(feature = "emission_chain")]
use bitcoin_ipc::rewards::{RewardConfig, RewardDatabase, SubnetRewardInfo};

#[cfg(feature = "emission_chain")]
trait MonitorDatabase: BitcoinIpcDatabase + RewardDatabase {}
#[cfg(feature = "emission_chain")]
impl<T: BitcoinIpcDatabase + RewardDatabase> MonitorDatabase for T {}

#[cfg(not(feature = "emission_chain"))]
trait MonitorDatabase: BitcoinIpcDatabase {}
#[cfg(not(feature = "emission_chain"))]
impl<T: BitcoinIpcDatabase> MonitorDatabase for T {}

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

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

    // Set poll interval
    let poll_interval = std::env::var("MONITOR_POLL_INTERVAL")
        .map(|s| Duration::from_secs(s.parse::<u64>().unwrap_or(DEFAULT_POLL_INTERVAL.as_secs())))
        .unwrap_or(DEFAULT_POLL_INTERVAL);

    let mut monitor = Monitor::new(
        db,
        btc_rpc,
        btc_watchonly_rpc,
        poll_interval,
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

#[cfg(feature = "emission_chain")]
struct RewardTracker {
    config: RewardConfig,
}

// TODO use generics for rpc + add a trait for the monitor
struct Monitor<D: MonitorDatabase> {
    db: D,
    rpc: bitcoincore_rpc::Client,
    watchonly_rpc: bitcoincore_rpc::Client,
    check_interval: Duration,
    current_height: u64,
    cancel_token: CancellationToken,
    side_effects: Vec<SideEffect>,
    #[cfg(feature = "emission_chain")]
    reward_calculator: RewardTracker,
}

impl<D> Monitor<D>
where
    D: MonitorDatabase,
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
            #[cfg(feature = "emission_chain")]
            reward_calculator: RewardTracker::new(),
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

                // Reward bookkeeping hooks (must run after processing all tx in each block).
                #[cfg(feature = "emission_chain")]
                match self
                    .reward_calculator
                    .update_after_block(&self.db, next_height)
                {
                    Ok(_) => info!("Updated reward bookkeeping after block {}", next_height),
                    Err(e) => error!(
                        "Error updating reward bookkeeping after block {}: {:?}",
                        next_height, e
                    ),
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
        } else if let Ok(msg) = ipc_lib::IpcKillSubnetMsg::from_tx(&self.db, tx) {
            let ipc_message = IpcMessage::KillSubnet(msg);

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

                // Reward bookkeeping on checkpoint.
                #[cfg(feature = "emission_chain")]
                match self.reward_calculator.update_after_checkpoint(
                    &self.db,
                    block_height,
                    msg.subnet_id,
                    &checkpoint,
                ) {
                    Ok(_) => info!("Updated reward bookkeeping after checkpoint"),
                    Err(e) => error!(
                        "Error updating reward bookkeeping after checkpoint: {:?}",
                        e
                    ),
                }

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

            IpcMessage::KillSubnet(msg) => {
                debug!("Found IPC message: {:?}", msg);
                msg.validate()?;
                msg.save_to_db(&self.db, block_height, block_hash, txid)?;
                info!(
                    "Processed KillSubnet for Subnet ID: {} Validator XPK: {:?}",
                    msg.subnet_id, msg.pubkey,
                );
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

/// Reward logic, meant to be run on the emission chain.
/// Function update_after_block() must be called after the monitor finishes processing a Bitcoin block.
/// Function update_after_checkpoint() must be called after the monitor finishes processing a subnet checkpoint.
/// For blocks with a checkpoint, update_after_checkpoint() must be called first and then update_after_block().
#[cfg(feature = "emission_chain")]
impl RewardTracker {
    fn new() -> Self {
        let config = {
            let reward_config_path = std::env::var("REWARD_CONFIG_PATH")
                .unwrap_or_else(|_| "reward_config.toml".to_string());
            RewardConfig::new_from_file(&reward_config_path).unwrap_or_else(|e| {
                panic!(
                    "Failed to load REWARD_CONFIG_PATH='{}': {}",
                    reward_config_path, e
                )
            })
        };
        Self { config }
    }

    // After processing a block, we need to do the following:
    // 1. If we are at the start of a snapshot, store all subnets that are candidates for rewards in this snapshot.
    // 2. If we are at the end of a snapshot, calculate the rewards for all subnets that are candidates for rewards in this snapshot.
    fn update_after_block(
        &mut self,
        db: &dyn MonitorDatabase,
        block_height: u64,
    ) -> Result<(), MonitorError> {
        let Some((snapshot, start_height, end_height)) = self
            .config
            .snapshot_boundaries_from_height(block_height)
            .map_err(|e| MonitorError::ConfigError(e.to_string()))?
        else {
            trace!("Validator rewards not yet activated at block height {block_height}.");
            return Ok(());
        };

        // 1.
        // On the first block of a snapshot, store all subnets that are candidates for rewards in this snapshot.
        // The candidate subnets are those returned by get_all_active_subnets().
        // Their validators will get rewarded at the end of the snapshot if they are still in the subnet's committee.
        if block_height == start_height {
            info!("Start of snapshot {snapshot} at block height {start_height}.");
            let active_subnets = db.get_all_active_subnets(start_height, end_height)?;

            let mut wtxn = db.write_txn()?;
            for subnet in active_subnets {
                db.put_reward_info(
                    &mut wtxn,
                    snapshot,
                    subnet.id,
                    &SubnetRewardInfo {
                        most_recent_committee_number: subnet.committee_number,
                        rewarded_amounts: None,
                    },
                )?;
            }
            wtxn.commit().map_err(db::DbError::from)?;
        }

        // 2.
        // At the end of a snapshot, calculate the rewards for all subnets that are still in the candidates database.
        //
        // The per-subnet rewards are computed in two places:
        // A) If the subnet had a committee rotation during the snapshot, the rewards have already been computed
        // and are already stored in the SubnetRewardInfo.rewarded_amounts entry of that subnet (see update_after_checkpoint()).
        // B) If the subnet did not have a committee rotation during the snapshot, then SubnetRewardInfo.rewarded_amounts is None.
        // We compute the rewards now by getting the committee snapshot at the most recent committee number and then iterating
        // over the validators in the committee.
        //
        // The total rewards are computed by summing the per-subnet rewards for each validator.
        if block_height == end_height {
            use bitcoin_ipc::rewards::SnapshotResult;

            info!("End of snapshot {snapshot} at block height {end_height}.");
            let candidates = db.iter_reward_info(snapshot)?;

            let mut rewards_total: std::collections::HashMap<
                bitcoin::XOnlyPublicKey,
                bitcoin::Amount,
            > = std::collections::HashMap::new();

            for (subnet_id, info) in candidates {
                let rewards_in_subnet =
                    self.get_or_init_reward_amounts(db, subnet_id, &info, block_height)?;

                for (pk, amt) in rewards_in_subnet {
                    rewards_total
                        .entry(pk)
                        .and_modify(|a| *a += amt)
                        .or_insert(amt);
                }
            }

            let rewards_list = rewards_total.into_iter().collect::<Vec<_>>();
            let total_rewarded_collateral = rewards_list.iter().map(|(_, a)| *a).sum();

            let mut wtxn = db.write_txn()?;
            db.put_reward_result(
                &mut wtxn,
                snapshot,
                &SnapshotResult {
                    rewards_list,
                    total_rewarded_collateral,
                },
            )?;
            wtxn.commit().map_err(db::DbError::from)?;
        }

        Ok(())
    }

    /// At the beginning of a snapshot (see update_after_block()), we created a candidate for each active subnet.
    /// Now, during the snapshot, we react to checkpoint events:
    /// 1. Kill checkpoint: delete the candidate (subnet is not eligible for rewards).
    /// 2. Rotation checkpoint: lazily materialize and update a min-collateral accumulator.
    ///
    /// The SubnetRewardInfo.rewarded_amounts entry for each subnet acts as an accumulator,
    /// keeping track of validators and their minimum collateral seen across all rotations during the snapshot.
    ///
    /// If a subnet has no rotations during the snapshot, we avoid storing the accumulator and later derive
    /// rewards directly from the start committee snapshot at finalization time (see update_after_block()).
    fn update_after_checkpoint(
        &mut self,
        db: &dyn MonitorDatabase,
        block_height: u64,
        subnet_id: bitcoin_ipc::SubnetId,
        checkpoint: &db::SubnetCheckpoint,
    ) -> Result<(), MonitorError> {
        let Some((current_snapshot, start_height, _end_height)) = self
            .config
            .snapshot_boundaries_from_height(block_height)
            .map_err(|e| MonitorError::ConfigError(e.to_string()))?
        else {
            trace!("Validator rewards not yet activated at block height {block_height}.");
            return Ok(());
        };

        // Changes made on the first block of a snapshot are already accounted for in update_after_block().
        if block_height == start_height {
            return Ok(());
        }

        // 1. Kill checkpoint: remove subnet from candidates (and do nothing else).
        if checkpoint.is_kill_checkpoint {
            let mut wtxn = db.write_txn()?;
            db.delete_reward_info(&mut wtxn, current_snapshot, subnet_id)
                .map_err(MonitorError::DbError)?;
            wtxn.commit().map_err(db::DbError::from)?;
            return Ok(());
        }

        // Nothing else to do if no committee rotation happened.
        if checkpoint.signed_committee_number == checkpoint.next_committee_number {
            return Ok(());
        }

        // Only update if subnet is still a candidate for this snapshot.
        let Some(mut cand_info) = db
            .get_reward_info(current_snapshot, subnet_id)
            .map_err(MonitorError::DbError)?
        else {
            return Ok(());
        };

        // The following should never happen
        if checkpoint.next_committee_number == cand_info.most_recent_committee_number {
            trace!("Received checkpoint with next_committee_number equal to the most_recent_committee_number in reward candidate database for subnet {subnet_id}");
            return Ok(());
        }

        // 2. Rotation checkpoint: read SubnetRewardInfo.rewarded_amounts, get the new committee,
        // update SubnetRewardInfo.rewarded_amounts based on the new committee.
        let mut rewards_in_subnet =
            self.get_or_init_reward_amounts(db, subnet_id, &cand_info, block_height)?;

        let new_committee = db
            .get_committee(subnet_id, checkpoint.next_committee_number)?
            .ok_or_else(|| {
                MonitorError::IpcTxInvalid(format!(
                    "could not get committee {} for subnet {} at block height {}",
                    checkpoint.next_committee_number, subnet_id, block_height
                ))
            })?;

        let new_committee_collaterals: std::collections::HashMap<
            bitcoin::XOnlyPublicKey,
            bitcoin::Amount,
        > = new_committee
            .validators
            .into_iter()
            .filter(|v| v.collateral > bitcoin::Amount::ZERO)
            .map(|v| (v.pubkey, v.collateral))
            .collect();

        // Go through the original committee and their rewards, and keep only those validators that are still in the new committee.
        // Keep the minimum collateral across the old and the new committee.
        rewards_in_subnet = rewards_in_subnet
            .into_iter()
            .filter_map(|(pk, rewarded_collateral)| {
                let new_collateral = new_committee_collaterals.get(&pk)?;
                Some((pk, rewarded_collateral.min(*new_collateral)))
            })
            .collect();

        cand_info.most_recent_committee_number = checkpoint.next_committee_number;
        cand_info.rewarded_amounts = Some(rewards_in_subnet);

        let mut wtxn = db.write_txn()?;
        db.put_reward_info(&mut wtxn, current_snapshot, subnet_id, &cand_info)
            .map_err(MonitorError::DbError)?;
        wtxn.commit().map_err(db::DbError::from)?;

        Ok(())
    }

    /// If we have already cached some SubnetRewardInfo.rewarded_amounts for a subnet then return it,
    /// otherwise read the rewarded amounts from the committee snapshot at SubnetRewardInfo
    fn get_or_init_reward_amounts(
        &mut self,
        db: &dyn MonitorDatabase,
        subnet_id: bitcoin_ipc::SubnetId,
        info: &SubnetRewardInfo,
        block_height: u64,
    ) -> Result<Vec<(bitcoin::XOnlyPublicKey, bitcoin::Amount)>, MonitorError> {
        let reward_amounts: Vec<(bitcoin::XOnlyPublicKey, bitcoin::Amount)> =
            if let Some(v) = &info.rewarded_amounts {
                v.clone()
            } else {
                let committee = db
                    .get_committee(subnet_id, info.most_recent_committee_number)?
                    .ok_or_else(|| {
                        MonitorError::IpcTxInvalid(format!(
                            "could not get committee {} for subnet {} at block height {}",
                            info.most_recent_committee_number, subnet_id, block_height
                        ))
                    })?;
                committee
                    .validators
                    .into_iter()
                    .filter(|v| v.collateral > bitcoin::Amount::ZERO)
                    .map(|v| (v.pubkey, v.collateral))
                    .collect()
            };
        Ok(reward_amounts)
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

    #[error("Invalid configuration: {0}")]
    ConfigError(String),
}
