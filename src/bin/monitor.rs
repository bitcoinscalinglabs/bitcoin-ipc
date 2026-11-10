use std::any::Any;
use std::env;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use futures::FutureExt;
use log::{debug, error, info, trace, warn};
use thiserror::Error;
use tokio::signal;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use bitcoin::{script::Instruction, Block, BlockHash};
use bitcoin_ipc::db::{self, DatabaseCore, HeedDb};
use bitcoin_ipc::ipc_lib::{self, IpcLibError, IpcTag, IpcValidate, IpcValidateError};
use bitcoin_ipc::{bitcoin_utils, eth_utils, IpcMessage, BTC_CONFIRMATIONS};
use bitcoincore_rpc::RpcApi;

use bitcoin_ipc::db::DatabaseRewardExtensions;
use bitcoin_ipc::rewards::RewardTracker;

trait Database: DatabaseCore + DatabaseRewardExtensions {}
impl<T: DatabaseCore + DatabaseRewardExtensions> Database for T {}

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Default blocks per batched fetch during cold sync; override with
/// `MONITOR_SYNC_BATCH_SIZE`. Higher cuts round-trips but raises per-batch
/// memory (≈ N × block size).
const DEFAULT_SYNC_BATCH_SIZE: u64 = 100;

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

    // Cold-sync batch size (number of blocks per batched RPC round-trip)
    let sync_batch_size = std::env::var("MONITOR_SYNC_BATCH_SIZE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_SYNC_BATCH_SIZE);

    let mut monitor = Monitor::new(
        db,
        btc_rpc,
        btc_watchonly_rpc,
        sync_batch_size,
        poll_interval,
        cancel_token.clone(),
    );

    #[cfg(feature = "emission_chain")]
    monitor.set_reward_tracker();

    // Spawn monitor task. A transient bitcoind RPC failure (dropped/refused connection,
    // bitcoind restarting) is retried with backoff instead of killing the process:
    // sync() reloads the last processed block from the DB on re-entry, so this resumes
    // rather than restarting. Non-RPC errors (DB, config) still propagate and exit.
    let retry_cancel = cancel_token.clone();
    let monitor_handle = tokio::spawn(async move {
        let max_backoff = Duration::from_secs(30);
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = Instant::now();
            match monitor.sync_and_listen().await {
                // sync_and_listen() expected to run until cancelled by the user
                // or crashed due to unhandled error.
                Ok(()) => return Ok(()),
                Err(e) if is_retryable(&e) => {
                    if retry_cancel.is_cancelled() {
                        return Ok(());
                    }
                    // Reset backoff after a healthy stretch so an isolated blip
                    // recovers fast while a sustained outage backs off.
                    if started.elapsed() > Duration::from_secs(60) {
                        backoff = Duration::from_secs(1);
                    }
                    warn!(
                        "A retriable error occured, retrying in {:?}. Error: {:?}. Ran {:?}.",
                        backoff,
                        e,
                        started.elapsed()
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                }
                Err(e) => return Err(e),
            }
        }
    });

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

/// Transient errors worth retrying — the bitcoind RPC class (dropped/refused
/// connection, bitcoind restarting, wallet mid-load). DB, config, and IPC errors
/// are real and are not retried.
fn is_retryable(e: &MonitorError) -> bool {
    matches!(e, MonitorError::BitcoinRpcError(_))
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
    sync_batch_size: u64,
    check_interval: Duration,
    current_height: u64,
    cancel_token: CancellationToken,
    side_effects: Vec<SideEffect>,
    reward_tracker: Option<RewardTracker>,
}

impl<D> Monitor<D>
where
    D: Database,
{
    fn new(
        db: D,
        rpc: bitcoincore_rpc::Client,
        watchonly_rpc: bitcoincore_rpc::Client,
        sync_batch_size: u64,
        check_interval: Duration,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            db,
            rpc,
            watchonly_rpc,
            sync_batch_size,
            check_interval,
            cancel_token,
            current_height: 0,
            side_effects: Vec::new(),
            reward_tracker: None,
        }
    }

    #[cfg(feature = "emission_chain")]
    /// Sets the reward tracker. Must be called to enable reward bookkeeping.
    pub fn set_reward_tracker(&mut self) {
        self.reward_tracker = Some(RewardTracker::new());
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

            // Catch up in batched windows. current_height advances per
            // block so a crash or cancellation mid-window resumes cleanly.
            while self.current_height < latest_block_height {
                if self.cancel_token.is_cancelled() {
                    debug!("Cancellation requested, stopping sync");
                    return Ok(false);
                }

                let window_start = self.current_height + 1;
                let window_end =
                    (self.current_height + self.sync_batch_size).min(latest_block_height);

                let blocks = bitcoin_utils::fetch_blocks_batched(
                    self.rpc.get_jsonrpc_client(),
                    window_start,
                    window_end,
                )?;

                for (offset, (block_hash, block)) in blocks.into_iter().enumerate() {
                    let height = window_start + offset as u64;
                    if let Err(e) = self.process_block_data(height, block_hash, block).await {
                        error!("Error processing block {}: {:?}.", height, e);
                        return Err(e);
                    }
                    debug!("Processed block {}", height);
                    self.current_height = height;
                    if let Err(e) = self.db.set_last_processed_block(self.current_height) {
                        error!("Failed to update last processed block: {:?}", e);
                    }

                    // Reward bookkeeping hooks (must run after processing all tx in each block).
                    if let Some(ref mut reward_tracker) = self.reward_tracker {
                        match reward_tracker.update_after_block(&self.db, height) {
                            Ok(_) => {}
                            Err(e) => error!(
                                "Error updating reward bookkeeping after block {}: {:?}",
                                height, e
                            ),
                        }
                    }
                }

                info!(
                    "Synced blocks {}..={} (tip {})",
                    window_start, window_end, latest_block_height
                );
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

                        // Reward bookkeeping hooks (must run after processing all tx in each block).

                        if let Some(ref mut reward_tracker) = self.reward_tracker {
                            match reward_tracker.update_after_block(&self.db, next_height) {
                                Ok(_) => {}
                                Err(e) => error!(
                                    "Error updating reward bookkeeping after block {}: {:?}",
                                    next_height, e
                                ),
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
            // Drop addresses whose descriptor is already registered in the watch-only wallet.
            //
            // `importdescriptors` adds the descriptor immediately (the address becomes `ismine`
            // and carries its label) and only then rescans historical blocks for its UTXOs in the
            // background. That rescan is slow and its RPC call routinely times out or returns
            // "wallet is rescanning" (handled below). Without this filter we would re-issue
            // `importdescriptors` on every poll cycle until we observe an `Ok` — but each call
            // contends the wallet lock and starves the running rescan, so the rescan never
            // finishes, the `Ok` never comes, and we loop forever (fleet-wide, one bitcoind).
            // Recognizing "already registered" lets us stop after the first attempt and let the
            // background rescan complete undisturbed.
            let pending: Vec<(bitcoin::Address, String, u32)> = import_addresses
                .into_iter()
                .filter(|(address, _, _)| {
                    // On a query error, assume not-imported and let the import below run — it is
                    // idempotent and its errors are swallowed, so this never gets stuck.
                    !bitcoin_ipc::wallet::is_address_imported(&self.watchonly_rpc, address)
                        .unwrap_or(false)
                })
                .collect();

            if pending.is_empty() {
                // Every queued address is registered (its background rescan finishes on its own).
                self.side_effects.clear();
                return Ok(());
            }

            // Import the not-yet-registered addresses in a batch, each with its own timestamp.
            match bitcoin_ipc::wallet::import_address_batch(&self.watchonly_rpc, &pending) {
                Ok(_) => {
                    debug!("Imported {} addresses in batch.", pending.len());
                    // Clear processed side effects
                    self.side_effects.clear();
                    return Ok(());
                }
                Err(e) => {
                    // import_address_batch() may:
                    // (1) time out (because importdescriptors may take minutes or hours:
                    // https://bitcoincore.org/en/doc/27.0.0/rpc/wallet/importdescriptors/)
                    // (2) Return error "Wallet is currently rescanning", if the import of
                    // another address is still in progress.
                    // In both cases the descriptor is (or will be) registered, so we swallow the
                    // error, keep the side effects, and re-check next cycle — where the filter
                    // above sees it registered and clears it, WITHOUT re-issuing importdescriptors.

                    // Case (1)
                    if matches!(
                        &e,
                        bitcoincore_rpc::Error::JsonRpc(
                            bitcoincore_rpc::jsonrpc::Error::Transport(_)
                        )
                    ) {
                        warn!("Watch-only import RPC transport error (timeout or connection); descriptor added, rescan continues in background.");
                        return Ok(());
                    }
                    // Case (2)
                    if matches!(&e, bitcoincore_rpc::Error::JsonRpc(_))
                        && e.to_string().to_lowercase().contains("rescanning")
                    {
                        warn!("Watch-only wallet is rescanning; descriptor added, will confirm next cycle.");
                        return Ok(());
                    }
                    return Err(e.into());
                }
            }
        }

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
        self.process_block_data(block_height, block_hash, block)
            .await
    }

    /// Processes an already-fetched block. Shared by `process_block` (single
    /// fetch) and the batched cold-sync path.
    async fn process_block_data(
        &mut self,
        block_height: u64,
        block_hash: BlockHash,
        block: Block,
    ) -> Result<(), MonitorError> {
        for tx in block.txdata {
            let processing =
                self.process_transaction(&tx, block_height, block_hash, block.header.time);
            // A panic on a transaction's content skips the transaction. A database write
            // transaction still open at the panic is dropped, which aborts it.
            match AssertUnwindSafe(processing).catch_unwind().await {
                Ok(result) => result?,
                Err(panic) => warn!(
                    "Skipping transaction {}: panic while processing it: {}",
                    tx.compute_txid(),
                    panic_message(panic.as_ref())
                ),
            }
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

                let tag = IpcTag::from_payload(&witness_data);
                let ipc_message = IpcMessage::from_witness(witness_data);
                let ipc_message = match ipc_message {
                    Ok(msg) => msg,
                    Err(e) => {
                        match tag {
                            // The other IPC tags are handled from the outputs below
                            Some(tag @ (IpcTag::CreateSubnet | IpcTag::JoinSubnet)) => warn!(
                                "Skipping IPC message in transaction {txid}: cannot parse {}: {e}",
                                tag.as_str()
                            ),
                            _ => trace!("Error deserializing IPC message: {:?}", e),
                        }
                        continue;
                    }
                };

                match self
                    .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                    .await
                {
                    Ok(_) => {}
                    Err(e) => self.handle_ipc_msg_error(txid, e)?,
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
                Err(e) => self.handle_ipc_msg_error(txid, e)?,
            }
        } else if let Ok(fund_msg) = ipc_lib::IpcFundSubnetMsg::from_tx(tx) {
            let ipc_message = IpcMessage::FundSubnet(fund_msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(txid, e)?,
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
                Err(e) => self.handle_ipc_msg_error(txid, e)?,
            }
        } else if let Ok(msg) = ipc_lib::IpcBatchTransferMsg::from_tx(&self.db, tx) {
            let ipc_message = IpcMessage::BatchTransfer(msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(txid, e)?,
            }
        } else if let Ok(msg) = ipc_lib::IpcStakeCollateralMsg::from_tx(&self.db, tx) {
            let ipc_message = IpcMessage::StakeCollateral(msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(txid, e)?,
            }
        } else if let Ok(msg) = ipc_lib::IpcUnstakeCollateralMsg::from_tx(&self.db, tx) {
            let ipc_message = IpcMessage::UnstakeCollateral(msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(txid, e)?,
            }
        } else if let Ok(msg) = ipc_lib::IpcKillSubnetMsg::from_tx(&self.db, tx) {
            let ipc_message = IpcMessage::KillSubnet(msg);

            match self
                .process_ipc_msg(block_height, block_hash, block_time, tx, txid, ipc_message)
                .await
            {
                Ok(_) => {}
                Err(e) => self.handle_ipc_msg_error(txid, e)?,
            }
        } else if let Some(e) = self.output_parse_error(tx) {
            warn!("Skipping IPC message in transaction {txid}: {e}");
        }

        Ok(())
    }

    /// The parse error of the IPC message whose tag is in the first push of the first output,
    /// or else in the first witness item of the first input; `None` without such a tag.
    /// The tag is read with non-minimal pushes allowed, so that the parsers' rejection of
    /// a non-minimal push is reported too.
    fn output_parse_error(&self, tx: &bitcoin::Transaction) -> Option<IpcLibError> {
        let output_tag = tx.output.first().and_then(|output| {
            output
                .script_pubkey
                .instructions()
                .find_map(|ins| match ins {
                    Ok(Instruction::PushBytes(data)) => Some(IpcTag::from_payload(data.as_bytes())),
                    _ => None,
                })
                .flatten()
        });
        let witness_tag = || {
            let witness = tx.input.first()?.witness.nth(0)?;
            IpcTag::from_payload(&bitcoin_utils::concatenate_op_push_data(witness).ok()?)
        };
        match output_tag.or_else(witness_tag)? {
            IpcTag::PrefundSubnet => ipc_lib::IpcPrefundSubnetMsg::from_tx(tx).err(),
            IpcTag::FundSubnet => ipc_lib::IpcFundSubnetMsg::from_tx(tx).err(),
            IpcTag::CheckpointSubnet => {
                ipc_lib::IpcCheckpointSubnetMsg::from_checkpoint_tx(&self.db, tx).err()
            }
            IpcTag::BatchTransfer => ipc_lib::IpcBatchTransferMsg::from_tx(&self.db, tx).err(),
            IpcTag::StakeCollateral => ipc_lib::IpcStakeCollateralMsg::from_tx(&self.db, tx).err(),
            IpcTag::UnstakeCollateral => {
                ipc_lib::IpcUnstakeCollateralMsg::from_tx(&self.db, tx).err()
            }
            IpcTag::KillSubnet => ipc_lib::IpcKillSubnetMsg::from_tx(&self.db, tx).err(),
            // Parsed from the witness in `process_transaction`
            IpcTag::CreateSubnet | IpcTag::JoinSubnet => None,
        }
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

                // Reward bookkeeping hooks after a checkpoint.

                if let Some(ref mut reward_tracker) = self.reward_tracker {
                    match reward_tracker.update_after_checkpoint(
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
                }

                Ok(())
            }

            IpcMessage::BatchTransfer(mut msg) => {
                debug!("Found IPC message: {:#?}", msg);

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
                    "Processed BatchTransfer for Subnet ID: {} Checkpoint Txid: {}",
                    msg.subnet_id, msg.checkpoint_txid,
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
    fn handle_ipc_msg_error(
        &self,
        txid: bitcoin::Txid,
        error: MonitorError,
    ) -> Result<(), MonitorError> {
        match error {
            // Ignorable errors: caused by the message's content
            MonitorError::IpcMsgInvalid(_)
            | MonitorError::IpcTxInvalid(_)
            | MonitorError::IpcMsgProcessingError(
                IpcLibError::IpcValidateError(_)
                | IpcLibError::MsgParseError(..)
                | IpcLibError::MultisigError(_)
                | IpcLibError::DbError(db::DbError::InvalidChange(_)),
            ) => {
                warn!("Skipping IPC message in transaction {txid}: {error}");
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

fn panic_message(panic: &(dyn Any + Send)) -> &str {
    if let Some(message) = panic.downcast_ref::<&str>() {
        message
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message
    } else {
        "unknown panic payload"
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
    /// Fatal, except for the variants caused by the message's content
    /// (see `handle_ipc_msg_error`)
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

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::hashes::{sha256, Hash};
    use bitcoin::script::PushBytesBuf;
    use bitcoin::secp256k1::{Keypair, Secp256k1, SecretKey};
    use bitcoin::{
        absolute, transaction, Address, Amount, CompactTarget, OutPoint, ScriptBuf, Transaction,
        TxIn, TxMerkleNode, TxOut, Txid, Witness, XOnlyPublicKey,
    };
    use bitcoin_ipc::ipc_lib::{IpcCreateSubnetMsg, IpcJoinSubnetMsg, SubnetId};
    use tempfile::TempDir;

    async fn test_monitor() -> (Monitor<HeedDb>, TempDir) {
        eth_utils::set_fvm_network();
        let dir = tempfile::tempdir().unwrap();
        let db = HeedDb::new(dir.path().to_str().unwrap(), false)
            .await
            .unwrap();
        // Processing a Bitcoin block makes no RPC call, so the clients never connect
        let rpc = || {
            bitcoincore_rpc::Client::new("http://127.0.0.1:1", bitcoincore_rpc::Auth::None).unwrap()
        };
        let monitor = Monitor::new(
            db,
            rpc(),
            rpc(),
            1,
            Duration::from_secs(1),
            CancellationToken::new(),
        );
        (monitor, dir)
    }

    /// LMDB writes its data file only when a write transaction commits
    fn db_data_file(dir: &TempDir) -> Vec<u8> {
        std::fs::read(dir.path().join("data.mdb")).unwrap()
    }

    fn bitcoin_block(txdata: Vec<Transaction>) -> Block {
        Block {
            header: bitcoin::block::Header {
                version: bitcoin::block::Version::ONE,
                prev_blockhash: BlockHash::all_zeros(),
                merkle_root: TxMerkleNode::all_zeros(),
                time: 0,
                bits: CompactTarget::from_consensus(0),
                nonce: 0,
            },
            txdata,
        }
    }

    /// Transaction carrying `data` in its input witness, as a reveal transaction does.
    /// The spent outpoint is derived from `data`, so different data gives different txids.
    fn reveal_tx(data: &[u8], output: Vec<TxOut>) -> Transaction {
        let previous_output = OutPoint {
            txid: Txid::from_byte_array(sha256::Hash::hash(data).to_byte_array()),
            vout: 0,
        };
        let script = bitcoin_utils::make_push_data_script(data);
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output,
                witness: Witness::from_slice(&[script.to_bytes()]),
                ..Default::default()
            }],
            output,
        }
    }

    /// Transaction carrying `data` in an OP_RETURN output, followed by a payment output
    fn op_return_tx(data: &[u8]) -> Transaction {
        let op_return = TxOut {
            value: Amount::ZERO,
            script_pubkey: bitcoin_utils::make_op_return_script(
                PushBytesBuf::try_from(data.to_vec()).unwrap(),
            ),
        };
        let mut tx = reveal_tx(data, vec![op_return, payment(10_000)]);
        tx.input[0].witness.clear();
        tx
    }

    fn payment(sat: u64) -> TxOut {
        TxOut {
            value: Amount::from_sat(sat),
            script_pubkey: ScriptBuf::new(),
        }
    }

    /// `tag` followed by `len` bytes
    fn tagged(tag: &str, len: usize) -> Vec<u8> {
        [tag.as_bytes(), &vec![1; len]].concat()
    }

    fn pubkey(seed: u8) -> XOnlyPublicKey {
        let secret = SecretKey::from_slice(&[seed; 32]).unwrap();
        Keypair::from_secret_key(&Secp256k1::new(), &secret)
            .x_only_public_key()
            .0
    }

    fn create_subnet_msg(
        min_validator_stake: Amount,
        whitelist: Vec<XOnlyPublicKey>,
    ) -> IpcCreateSubnetMsg {
        IpcCreateSubnetMsg {
            min_validator_stake,
            min_validators: whitelist.len() as u16,
            bottomup_check_period: 10,
            active_validators_limit: 20,
            min_cross_msg_fee: Amount::from_sat(50),
            whitelist,
        }
    }

    /// Processes the `malformed` transactions in a Bitcoin block and checks that none of them
    /// writes to the database; then processes them again, followed by a valid create-subnet
    /// transaction in the same Bitcoin block, and checks that the valid one is handled.
    async fn assert_skipped(
        monitor: &mut Monitor<HeedDb>,
        dir: &TempDir,
        malformed: Vec<Transaction>,
    ) {
        let before = db_data_file(dir);
        monitor
            .process_block_data(2, BlockHash::all_zeros(), bitcoin_block(malformed.clone()))
            .await
            .unwrap();
        assert!(
            db_data_file(dir) == before,
            "a skipped transaction wrote to the database"
        );

        let msg = create_subnet_msg(Amount::from_sat(1_000), vec![pubkey(9)]);
        let valid = reveal_tx(&msg.to_bytes().unwrap(), vec![]);
        let subnet_id = SubnetId::from_txid(&valid.compute_txid());
        let mut txdata = malformed;
        txdata.push(valid);
        monitor
            .process_block_data(3, BlockHash::all_zeros(), bitcoin_block(txdata))
            .await
            .unwrap();
        assert!(monitor
            .db
            .get_subnet_genesis_info(subnet_id)
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn test_skips_malformed_create_subnet() {
        let (mut monitor, dir) = test_monitor().await;
        let msg = create_subnet_msg(Amount::from_sat(1_000), vec![pubkey(1), pubkey(2)]);
        let bytes = msg.to_bytes().unwrap();

        // A create-subnet message with a `power_unit_exponent` field after the stake and
        // epoch, timeout, deposit and kill fields after the whitelist
        let with_extra_fields = postcard::to_stdvec(&(
            1_000u64,
            4u8,
            2u16,
            10u64,
            20u16,
            50u64,
            vec![pubkey(1), pubkey(2)],
            100u64,
            10u32,
            100u32,
            1_000u64,
            10u64,
            10u64,
        ))
        .unwrap();

        let mut unknown_tag = bytes.clone();
        unknown_tag[..6].copy_from_slice(b"IPCXXX");

        let mut invalid = msg.clone();
        invalid.min_validators = 0;

        assert_skipped(
            &mut monitor,
            &dir,
            vec![
                reveal_tx(&bytes[..bytes.len() / 2], vec![]),
                reveal_tx(&[b"IPCCRT".as_slice(), &with_extra_fields].concat(), vec![]),
                reveal_tx(&[bytes.as_slice(), &[0]].concat(), vec![]),
                reveal_tx(&unknown_tag, vec![]),
                reveal_tx(&invalid.to_bytes().unwrap(), vec![]),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn test_skips_malformed_join_subnet() {
        let (mut monitor, dir) = test_monitor().await;
        // A minimum stake of 1 sat admits validators with power 0
        let msg = create_subnet_msg(Amount::from_sat(1), vec![pubkey(1)]);
        let create = reveal_tx(&msg.to_bytes().unwrap(), vec![]);
        let subnet_id = SubnetId::from_txid(&create.compute_txid());
        monitor
            .process_block_data(1, BlockHash::all_zeros(), bitcoin_block(vec![create]))
            .await
            .unwrap();

        let join = |subnet_id: SubnetId| {
            IpcJoinSubnetMsg {
                subnet_id,
                collateral: Amount::ZERO,
                ip: "127.0.0.1:26656".parse().unwrap(),
                backup_address: Address::p2tr(
                    &Secp256k1::new(),
                    pubkey(1),
                    None,
                    bitcoin_ipc::NETWORK,
                )
                .into_unchecked(),
                pubkey: pubkey(1),
            }
            .to_bytes()
            .unwrap()
        };
        let bytes = join(subnet_id);

        assert_skipped(
            &mut monitor,
            &dir,
            vec![
                reveal_tx(&bytes[..bytes.len() / 2], vec![payment(20_000)]),
                reveal_tx(
                    &join(SubnetId::from_txid(&Txid::all_zeros())),
                    vec![payment(20_000)],
                ),
                // Bootstraps the subnet with total power 0, which has no multisig: a panic
                reveal_tx(&bytes, vec![payment(5_000)]),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn test_skips_malformed_prefund_subnet() {
        let (mut monitor, dir) = test_monitor().await;
        assert_skipped(
            &mut monitor,
            &dir,
            vec![
                op_return_tx(&tagged("IPCPFD", 10)),
                op_return_tx(&tagged("IPCPFD", 40)),
                op_return_tx(&tagged("IPCZZZ", 40)),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn test_skips_malformed_fund_subnet() {
        let (mut monitor, dir) = test_monitor().await;
        assert_skipped(
            &mut monitor,
            &dir,
            vec![
                op_return_tx(&tagged("IPCFND", 10)),
                op_return_tx(&tagged("IPCFND", 40)),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn test_skips_malformed_checkpoint() {
        let (mut monitor, dir) = test_monitor().await;
        assert_skipped(
            &mut monitor,
            &dir,
            vec![
                op_return_tx(&tagged("IPCCPT", 10)),
                op_return_tx(&tagged("IPCCPT", 72)),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn test_skips_malformed_batch_transfer() {
        let (mut monitor, dir) = test_monitor().await;
        assert_skipped(
            &mut monitor,
            &dir,
            vec![
                reveal_tx(&tagged("IPCTFR", 0), vec![payment(10_000)]),
                reveal_tx(&tagged("IPCTFR", 40), vec![payment(10_000)]),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn test_skips_malformed_stake_collateral() {
        let (mut monitor, dir) = test_monitor().await;
        assert_skipped(
            &mut monitor,
            &dir,
            vec![
                op_return_tx(&tagged("IPCSTK", 10)),
                op_return_tx(&tagged("IPCSTK", 32)),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn test_skips_malformed_unstake_collateral() {
        let (mut monitor, dir) = test_monitor().await;
        assert_skipped(
            &mut monitor,
            &dir,
            vec![
                op_return_tx(&tagged("IPCUST", 10)),
                op_return_tx(&tagged("IPCUST", 72)),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn test_skips_malformed_kill_subnet() {
        let (mut monitor, dir) = test_monitor().await;
        assert_skipped(
            &mut monitor,
            &dir,
            vec![
                op_return_tx(&tagged("IPCKIL", 10)),
                op_return_tx(&tagged("IPCKIL", 64)),
            ],
        )
        .await;
    }

    /// Warnings logged by the tests of this module; the first call installs the logger
    fn warnings() -> &'static std::sync::Mutex<Vec<String>> {
        struct Capture;
        impl log::Log for Capture {
            fn enabled(&self, metadata: &log::Metadata) -> bool {
                metadata.level() <= log::Level::Warn
            }
            fn log(&self, record: &log::Record) {
                if self.enabled(record.metadata()) {
                    if let Ok(mut warnings) = warnings().lock() {
                        warnings.push(record.args().to_string());
                    }
                }
            }
            fn flush(&self) {}
        }
        static WARNINGS: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> =
            std::sync::OnceLock::new();
        WARNINGS.get_or_init(|| {
            log::set_logger(&Capture).unwrap();
            log::set_max_level(log::LevelFilter::Warn);
            std::sync::Mutex::new(Vec::new())
        })
    }

    #[tokio::test]
    async fn test_warns_on_unparsable_ipc_payload() {
        warnings();
        let (mut monitor, _dir) = test_monitor().await;
        let mut non_minimal_push = op_return_tx(b"IPCPFD");
        // OP_RETURN OP_PUSHDATA1 0x06 "IPCPFD"
        non_minimal_push.output[0].script_pubkey =
            bitcoin::ScriptBuf::from_bytes([&[0x6a, 0x4c, 0x06][..], b"IPCPFD"].concat());
        let tagged = vec![
            reveal_tx(&[b"IPCCRT".as_slice(), &[0xff; 3]].concat(), vec![]),
            op_return_tx(&[b"IPCPFD".as_slice(), &[0xff; 3]].concat()),
            reveal_tx(&[b"IPCTFR".as_slice(), &[0xff; 3]].concat(), vec![]),
            non_minimal_push,
        ];
        let ordinary = op_return_tx(b"not an IPC message");
        let mut txdata = tagged.clone();
        txdata.push(ordinary.clone());

        monitor
            .process_block_data(2, BlockHash::all_zeros(), bitcoin_block(txdata))
            .await
            .unwrap();

        let warnings = warnings().lock().unwrap().clone();
        let warned = |tx: &Transaction| {
            let txid = tx.compute_txid().to_string();
            warnings.iter().any(|w| w.contains(&txid))
        };
        assert!(tagged.iter().all(warned));
        assert!(!warned(&ordinary));
    }
}
