use std::collections::HashMap;

use bitcoin::{hashes::sha256, Amount, BlockHash};
use log::info;
use rand::RngCore;
use tempfile::TempDir;

use crate::{
    db::{DatabaseCore, HeedDb, SubnetCheckpoint},
    easy_tester::{
        error::EasyTesterError,
        model::{
            build_create_subnet_msg, create_rand_blockhash, create_rand_txid, SetupSpec,
            ValidatorSpec,
        },
    },
    eth_utils,
    ipc_lib::{
        IpcBatchTransferMsg, IpcCheckpointSubnetMsg, IpcCreateSubnetMsg, IpcCrossSubnetErcTransfer,
        IpcErcSupplyAdjustment, IpcErcTokenRegistration, IpcJoinSubnetMsg, IpcStakeCollateralMsg,
        IpcUnstakeCollateralMsg, IpcValidate,
    },
    SubnetId,
};

fn create_rand_checkpoint_hash() -> sha256::Hash {
    use bitcoin::hashes::Hash;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    sha256::Hash::from_slice(&bytes).expect("random bytes should make a sha256 hash")
}

/// Shared state and operations for all easy_tester backends.
/// Holds the temp DB, setup spec, and common bookkeeping (block hashes,
/// created subnets, checkpoint heights).
pub struct BaseTester {
    pub _temp_dir: TempDir,
    pub db: HeedDb,
    pub setup: SetupSpec,
    pub block_hashes: HashMap<u64, BlockHash>,
    pub created_subnets: HashMap<String, SubnetId>,
    pub checkpoint_heights: HashMap<String, u64>,
}

impl BaseTester {
    pub async fn new(setup: SetupSpec) -> Result<Self, EasyTesterError> {
        eth_utils::set_fvm_network();

        let temp_dir = tempfile::tempdir()
            .map_err(|e| EasyTesterError::runtime(format!("failed to create temp dir: {e}")))?;
        let db_path = temp_dir
            .path()
            .to_str()
            .ok_or_else(|| EasyTesterError::runtime("temp dir path is not valid utf-8"))?;

        let db = HeedDb::new(db_path, false)
            .await
            .map_err(|e| EasyTesterError::runtime(format!("failed to create DB: {e}")))?;

        Ok(Self {
            _temp_dir: temp_dir,
            db,
            setup,
            block_hashes: HashMap::new(),
            created_subnets: HashMap::new(),
            checkpoint_heights: HashMap::new(),
        })
    }

    pub fn block_hash(&mut self, height: u64) -> BlockHash {
        *self
            .block_hashes
            .entry(height)
            .or_insert_with(create_rand_blockhash)
    }

    pub fn resolve_subnet_id(&self, subnet_name: &str) -> Result<SubnetId, EasyTesterError> {
        self.created_subnets
            .get(subnet_name)
            .copied()
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "subnet '{subnet_name}' not found in created subnets"
                ))
            })
    }

    pub fn resolve_validator(
        &self,
        validator_name: &str,
    ) -> Result<ValidatorSpec, EasyTesterError> {
        self.setup
            .validators
            .get(validator_name)
            .cloned()
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "validator '{validator_name}' missing from parsed setup"
                ))
            })
    }

    pub fn mine_block(&mut self, height: u64) -> Result<(), EasyTesterError> {
        self.block_hash(height);
        self.db
            .set_last_processed_block(height)
            .map_err(|e| EasyTesterError::runtime(format!("failed to mine block {height}: {e}")))?;
        info!("Mined block {}", height);
        Ok(())
    }

    pub fn create_subnet(&mut self, height: u64, subnet_name: &str) -> Result<(), EasyTesterError> {
        let spec = self
            .setup
            .subnets
            .get(subnet_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "subnet '{subnet_name}' missing from parsed setup"
                ))
            })?
            .clone();

        if self.created_subnets.contains_key(subnet_name) {
            return Err(EasyTesterError::runtime(format!(
                "subnet '{subnet_name}' already created"
            )));
        }

        let create_msg: IpcCreateSubnetMsg = build_create_subnet_msg(&spec);
        create_msg
            .validate()
            .map_err(|e| EasyTesterError::runtime(format!("create msg invalid: {e}")))?;

        let txid = create_rand_txid();
        let genesis_info = create_msg
            .save_to_db(&self.db, height, txid)
            .map_err(|e| EasyTesterError::runtime(format!("create failed: {e}")))?;

        self.created_subnets
            .insert(subnet_name.to_string(), genesis_info.subnet_id);

        info!(
            "Created subnet '{}' with subnet_id={}",
            subnet_name, genesis_info.subnet_id
        );
        Ok(())
    }

    pub fn join_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        collateral_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let block_hash = self.block_hash(height);
        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let validator = self.resolve_validator(validator_name)?;
        let collateral = Amount::from_sat(collateral_sats);

        let join_msg = IpcJoinSubnetMsg {
            subnet_id,
            collateral,
            ip: validator.default_ip,
            backup_address: validator.default_backup_address.clone(),
            pubkey: validator.pubkey,
        };

        join_msg
            .validate()
            .map_err(|e| EasyTesterError::runtime(format!("join msg invalid: {e}")))?;

        let genesis_info = self
            .db
            .get_subnet_genesis_info(subnet_id)
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "subnet genesis info missing for {subnet_id} (did you run create?)"
                ))
            })?;

        let subnet_state = self
            .db
            .get_subnet_state(subnet_id)
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?;

        join_msg
            .validate_for_subnet(&genesis_info, &subnet_state)
            .map_err(|e| EasyTesterError::runtime(format!("join rejected: {e}")))?;

        let txid = create_rand_txid();
        join_msg
            .save_to_db(&self.db, height, block_hash, txid)
            .map_err(|e| EasyTesterError::runtime(format!("join failed: {e}")))?;

        info!(
            "Join: validator '{}' joined subnet '{}' with {} sats",
            validator_name, subnet_name, collateral_sats
        );
        Ok(())
    }

    pub fn stake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let block_hash = self.block_hash(height);
        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let validator = self.resolve_validator(validator_name)?;

        let amount = Amount::from_sat(amount_sats);
        let msg = IpcStakeCollateralMsg {
            subnet_id,
            amount,
            pubkey: validator.pubkey,
        };

        msg.validate()
            .map_err(|e| EasyTesterError::runtime(format!("stake msg invalid: {e}")))?;

        let subnet_state = self
            .db
            .get_subnet_state(subnet_id)
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "subnet state missing for {subnet_id} (did you run create/bootstrap?)"
                ))
            })?;

        msg.validate_for_subnet(&subnet_state)
            .map_err(|e| EasyTesterError::runtime(format!("stake rejected: {e}")))?;

        let txid = create_rand_txid();
        msg.save_to_db(&self.db, height, block_hash, txid)
            .map_err(|e| EasyTesterError::runtime(format!("stake failed: {e}")))?;

        info!(
            "Stake: validator '{}' staked {} sats to subnet '{}'",
            validator_name, amount_sats, subnet_name
        );
        Ok(())
    }

    pub fn unstake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let block_hash = self.block_hash(height);
        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let validator = self.resolve_validator(validator_name)?;

        let amount = Amount::from_sat(amount_sats);
        let msg = IpcUnstakeCollateralMsg {
            subnet_id,
            amount,
            pubkey: Some(validator.pubkey),
        };

        msg.validate()
            .map_err(|e| EasyTesterError::runtime(format!("unstake msg invalid: {e}")))?;

        let genesis_info = self
            .db
            .get_subnet_genesis_info(subnet_id)
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "subnet genesis info missing for {subnet_id} (did you run create?)"
                ))
            })?;

        let subnet_state = self
            .db
            .get_subnet_state(subnet_id)
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "subnet state missing for {subnet_id} (did you run create/bootstrap?)"
                ))
            })?;

        msg.validate_for_subnet(&genesis_info, &subnet_state)
            .map_err(|e| EasyTesterError::runtime(format!("unstake rejected: {e}")))?;

        let txid = create_rand_txid();
        msg.save_to_db(&self.db, height, block_hash, txid)
            .map_err(|e| EasyTesterError::runtime(format!("unstake failed: {e}")))?;

        info!(
            "Unstake: validator '{}' unstaked {} sats from subnet '{}'",
            validator_name, amount_sats, subnet_name
        );
        Ok(())
    }

    /// Creates a checkpoint for the given subnet. Returns the checkpoint record
    /// so callers can hook in additional logic (e.g. reward bookkeeping).
    ///
    /// `token_registrations` and `erc_transfers` are included in the checkpoint
    /// message if provided (non-empty).
    pub fn checkpoint_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        token_registrations: Vec<IpcErcTokenRegistration>,
        supply_adjustments: Vec<IpcErcSupplyAdjustment>,
        erc_transfers: Vec<IpcCrossSubnetErcTransfer>,
    ) -> Result<SubnetCheckpoint, EasyTesterError> {
        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let block_hash = self.block_hash(height);

        let subnet_state = self
            .db
            .get_subnet_state(subnet_id)
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
            .ok_or_else(|| {
                EasyTesterError::runtime(format!("subnet state missing for {subnet_id}"))
            })?;

        let current_cfg = subnet_state.committee.configuration_number;
        let latest_cfg = self
            .db
            .get_last_stake_change_configuration_number(subnet_id)
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?;
        let next_cfg = if latest_cfg > current_cfg {
            latest_cfg
        } else {
            0
        };

        let checkpoint_height = self
            .checkpoint_heights
            .entry(subnet_name.to_string())
            .and_modify(|h| *h += 1)
            .or_insert(1);

        let msg = IpcCheckpointSubnetMsg {
            subnet_id,
            checkpoint_hash: create_rand_checkpoint_hash(),
            checkpoint_height: *checkpoint_height,
            next_committee_configuration_number: next_cfg,
            withdrawals: vec![],
            transfers: vec![],
            token_registrations,
            token_supply_adjustments: supply_adjustments,
            token_transfers: erc_transfers,
            unstakes: vec![],
            change_address: None,
            is_kill_checkpoint: false,
        };

        msg.validate()
            .map_err(|e| EasyTesterError::runtime(format!("checkpoint msg invalid: {e}")))?;

        let has_batch_data = !msg.token_registrations.is_empty()
            || !msg.token_supply_adjustments.is_empty()
            || !msg.token_transfers.is_empty();

        let token_registrations = msg.token_registrations.clone();
        let supply_adjustments = msg.token_supply_adjustments.clone();
        let erc_transfers = msg.token_transfers.clone();

        let txid = create_rand_txid();
        let checkpoint = msg
            .save_to_db(&self.db, height, block_hash, txid)
            .map_err(|e| EasyTesterError::runtime(format!("checkpoint failed: {e}")))?;

        // Simulate the batch transfer reveal step — this is what the monitor does
        // when it sees the reveal tx. It saves ETR and ETX as rootnet messages.
        if has_batch_data {
            let batch_msg = IpcBatchTransferMsg {
                subnet_id,
                checkpoint_txid: txid,
                checkpoint_vout: 0,
                transfers: vec![],
                token_registrations,
                token_supply_adjustments: supply_adjustments,
                token_transfers: erc_transfers,
            };
            let batch_txid = create_rand_txid();
            batch_msg
                .save_to_db(&self.db, height, block_hash, batch_txid)
                .map_err(|e| {
                    EasyTesterError::runtime(format!("batch transfer save failed: {e}"))
                })?;
            info!(
                "Batch transfer reveal simulated for subnet '{}'",
                subnet_name
            );
        }

        info!(
            "Checkpoint: subnet '{}' committed at height {} (next_cfg={})",
            subnet_name, height, next_cfg
        );
        Ok(checkpoint)
    }
}
