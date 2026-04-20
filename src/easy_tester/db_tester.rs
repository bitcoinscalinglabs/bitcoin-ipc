use std::collections::HashMap;

use bitcoin::{hashes::sha256, Amount, BlockHash};
use log::{error, info};
use rand::RngCore;
use tempfile::TempDir;

use crate::{
    db::{DatabaseCore, DatabaseRewardExtensions, HeedDb, SubnetCheckpoint},
    easy_tester::{
        error::EasyTesterError,
        model::{
            build_create_subnet_msg, create_rand_blockhash, create_rand_txid,
            OutputDb, OutputExpectTarget, SetupSpec, ValidatorSpec,
        },
        tester::Tester,
    },
    eth_utils,
    ipc_lib::{
        IpcBatchTransferMsg, IpcCheckpointSubnetMsg, IpcCreateSubnetMsg, IpcCrossSubnetErcTransfer,
        IpcErcSupplyAdjustment, IpcErcTokenRegistration, IpcJoinSubnetMsg, IpcStakeCollateralMsg,
        IpcUnstakeCollateralMsg, IpcValidate,
    },
    rewards::{RewardConfig, RewardTracker},
    SubnetId,
};

fn create_rand_checkpoint_hash() -> sha256::Hash {
    use bitcoin::hashes::Hash;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    sha256::Hash::from_slice(&bytes).expect("random bytes should make a sha256 hash")
}

pub struct DbTester {
    _temp_dir: TempDir,
    db: HeedDb,
    setup: SetupSpec,
    block_hashes: HashMap<u64, BlockHash>,
    created_subnets: HashMap<String, SubnetId>,
    checkpoint_heights: HashMap<String, u64>,
    // ERC fields
    registered_tokens: HashMap<String, (String, IpcErcTokenRegistration)>,
    pending_registrations: HashMap<String, Vec<IpcErcTokenRegistration>>,
    pending_supply_adjustments: HashMap<String, Vec<IpcErcSupplyAdjustment>>,
    pending_erc_transfers: HashMap<String, Vec<IpcCrossSubnetErcTransfer>>,
    last_rootnet_msgs: Option<LastRootnetMsgs>,
    last_token_balance: Option<alloy_primitives::U256>,
    // Reward fields
    reward_tracker: Option<RewardTracker>,
    last_reward_results: Option<LastRewardResults>,
}

#[derive(Debug)]
struct LastRootnetMsgs {
    _subnet_name: String,
    msgs: Vec<crate::db::RootnetMessage>,
}

#[derive(Debug, Clone)]
struct LastRewardResults {
    snapshot: u64,
    rewards_by_validator: HashMap<String, u64>,
    total_sats: u64,
}

impl DbTester {
    pub async fn new(
        setup: SetupSpec,
        activation_height: Option<u64>,
        snapshot_length: Option<u64>,
    ) -> Result<Self, EasyTesterError> {
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

        let reward_tracker = match (activation_height, snapshot_length) {
            (Some(ah), Some(sl)) => {
                let config = RewardConfig::new(ah, sl)
                    .map_err(|e| EasyTesterError::runtime(format!("invalid reward config: {e}")))?;
                Some(RewardTracker::new_with_config(config))
            }
            _ => None,
        };

        Ok(Self {
            _temp_dir: temp_dir,
            db,
            setup,
            block_hashes: HashMap::new(),
            created_subnets: HashMap::new(),
            checkpoint_heights: HashMap::new(),
            registered_tokens: HashMap::new(),
            pending_registrations: HashMap::new(),
            pending_supply_adjustments: HashMap::new(),
            pending_erc_transfers: HashMap::new(),
            last_rootnet_msgs: None,
            last_token_balance: None,
            reward_tracker,
            last_reward_results: None,
        })
    }

    fn block_hash(&mut self, height: u64) -> BlockHash {
        *self
            .block_hashes
            .entry(height)
            .or_insert_with(create_rand_blockhash)
    }

    fn resolve_subnet_id(&self, subnet_name: &str) -> Result<SubnetId, EasyTesterError> {
        self.created_subnets
            .get(subnet_name)
            .copied()
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "subnet '{subnet_name}' not found in created subnets"
                ))
            })
    }

    fn resolve_validator(&self, validator_name: &str) -> Result<ValidatorSpec, EasyTesterError> {
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

    fn mine_block(&mut self, height: u64) -> Result<(), EasyTesterError> {
        self.block_hash(height);
        self.db
            .set_last_processed_block(height)
            .map_err(|e| EasyTesterError::runtime(format!("failed to mine block {height}: {e}")))?;
        info!("Mined block {}", height);
        Ok(())
    }

    fn create_subnet(&mut self, height: u64, subnet_name: &str) -> Result<(), EasyTesterError> {
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

    fn join_subnet(
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

    fn stake_subnet(
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

    fn unstake_subnet(
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

    fn checkpoint_subnet(
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
        println!(
            "OUTPUT checkpoint subnet='{}' height={} checkpointTx=accepted",
            subnet_name, height
        );
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
            match batch_msg.save_to_db(&self.db, height, block_hash, batch_txid) {
                Ok(_) => println!(
                    "OUTPUT checkpoint subnet='{}' height={} batchTx=accepted",
                    subnet_name, height
                ),
                Err(e) => println!(
                    "OUTPUT checkpoint subnet='{}' height={} batchTx=rejected: {e}",
                    subnet_name, height
                ),
            };
        }
        Ok(checkpoint)
    }

    fn queue_token_registration(
        &mut self,
        _height: u64,
        subnet_name: &str,
        name: &str,
        symbol: &str,
        initial_supply: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.resolve_subnet_id(subnet_name)?;

        let home_token_address = if let Some((prev_subnet, prev_reg)) =
            self.registered_tokens.get(name)
        {
            if prev_subnet != subnet_name {
                return Err(EasyTesterError::runtime(format!(
                    "token '{}' was already registered on subnet '{}', cannot re-register on '{}'",
                    name, prev_subnet, subnet_name
                )));
            }
            info!(
                    "Duplicate registration for token '{}' on subnet '{}' (allowed, will be ignored by L2 contract)",
                    name, subnet_name
                );
            prev_reg.home_token_address
        } else {
            alloy_primitives::Address::from_slice(&rand::random::<[u8; 20]>())
        };

        let etr = IpcErcTokenRegistration {
            home_token_address,
            name: name.to_string(),
            symbol: symbol.to_string(),
            decimals: 18,
            initial_supply,
        };

        self.registered_tokens
            .insert(name.to_string(), (subnet_name.to_string(), etr.clone()));

        self.pending_registrations
            .entry(subnet_name.to_string())
            .or_default()
            .push(etr);

        info!(
            "Queued token registration on subnet '{}': {} ({}), initial_supply={}",
            subnet_name, name, symbol, initial_supply
        );
        Ok(())
    }

    fn queue_erc_transfer(
        &mut self,
        _height: u64,
        src_subnet: &str,
        dst_subnet: &str,
        token_name: &str,
        amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.resolve_subnet_id(src_subnet)?;
        let destination_subnet_id = self.resolve_subnet_id(dst_subnet)?;

        let (reg_subnet, reg) = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "token '{}' not registered (use register_token first)",
                token_name
            ))
        })?;

        let home_subnet_id = self.resolve_subnet_id(reg_subnet)?;

        let etx = IpcCrossSubnetErcTransfer {
            home_subnet_id,
            home_token_address: reg.home_token_address,
            amount,
            destination_subnet_id,
            recipient: alloy_primitives::Address::from_slice(&rand::random::<[u8; 20]>()),
        };

        self.pending_erc_transfers
            .entry(src_subnet.to_string())
            .or_default()
            .push(etx);

        info!(
            "Queued ERC transfer from subnet '{}' to subnet '{}', token='{}', amount={}",
            src_subnet, dst_subnet, token_name, amount
        );
        Ok(())
    }

    fn queue_mint(
        &mut self,
        _height: u64,
        subnet_name: &str,
        token_name: &str,
        amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.resolve_subnet_id(subnet_name)?;
        let (_, reg) = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!("token '{}' not registered", token_name))
        })?;

        let delta = alloy_primitives::I256::try_from(amount)
            .map_err(|e| EasyTesterError::runtime(format!("mint amount too large for I256: {e}")))?;

        let ems = IpcErcSupplyAdjustment {
            home_token_address: reg.home_token_address,
            delta,
        };
        self.pending_supply_adjustments
            .entry(subnet_name.to_string())
            .or_default()
            .push(ems);

        info!("Queued mint for token '{}' on subnet '{}', amount={}", token_name, subnet_name, amount);
        Ok(())
    }

    fn queue_burn(
        &mut self,
        _height: u64,
        subnet_name: &str,
        token_name: &str,
        amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.resolve_subnet_id(subnet_name)?;
        let (_, reg) = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!("token '{}' not registered", token_name))
        })?;

        let pos = alloy_primitives::I256::try_from(amount)
            .map_err(|e| EasyTesterError::runtime(format!("burn amount too large for I256: {e}")))?;
        let delta = pos.checked_neg()
            .ok_or_else(|| EasyTesterError::runtime("burn amount overflow (I256::MIN)".to_string()))?;

        let ems = IpcErcSupplyAdjustment {
            home_token_address: reg.home_token_address,
            delta,
        };
        self.pending_supply_adjustments
            .entry(subnet_name.to_string())
            .or_default()
            .push(ems);

        info!("Queued burn for token '{}' on subnet '{}', amount={}", token_name, subnet_name, amount);
        Ok(())
    }

    fn msg_field_value(msg: &crate::db::RootnetMessage, field: &str) -> Result<String, String> {
        match field {
            "kind" => Ok(match msg {
                crate::db::RootnetMessage::FundSubnet { .. } => "fund".to_string(),
                crate::db::RootnetMessage::ErcTransfer { .. } => "erc_transfer".to_string(),
                crate::db::RootnetMessage::ErcRegistration { .. } => "erc_registration".to_string(),
            }),
            "tokenName" => match msg {
                crate::db::RootnetMessage::ErcRegistration { registration, .. } => {
                    Ok(registration.name.clone())
                }
                _ => {
                    Err("field 'tokenName' only available on erc_registration messages".to_string())
                }
            },
            "tokenSymbol" => match msg {
                crate::db::RootnetMessage::ErcRegistration { registration, .. } => {
                    Ok(registration.symbol.clone())
                }
                _ => Err(
                    "field 'tokenSymbol' only available on erc_registration messages".to_string(),
                ),
            },
            "tokenDecimals" => match msg {
                crate::db::RootnetMessage::ErcRegistration { registration, .. } => {
                    Ok(registration.decimals.to_string())
                }
                _ => Err(
                    "field 'tokenDecimals' only available on erc_registration messages".to_string(),
                ),
            },
            "token" => match msg {
                crate::db::RootnetMessage::ErcTransfer { msg: etx, .. } => {
                    Ok(format!("{}", etx.home_token_address))
                }
                crate::db::RootnetMessage::ErcRegistration { registration, .. } => {
                    Ok(format!("{}", registration.home_token_address))
                }
                _ => Err("field 'token' only available on ERC messages".to_string()),
            },
            "amount" => match msg {
                crate::db::RootnetMessage::ErcTransfer { msg: etx, .. } => {
                    Ok(etx.amount.to_string())
                }
                crate::db::RootnetMessage::FundSubnet { msg, .. } => {
                    Ok(msg.amount.to_sat().to_string())
                }
                _ => Err("field 'amount' not available on erc_registration messages".to_string()),
            },
            _ => Err(format!("unknown field '{}'", field)),
        }
    }
}

fn fmt_sats_with_underscores(sats: u64) -> String {
    let s = sats.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i != 0 && i % 3 == 0 {
            out.push('_');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn validator_ordinal(name: &str) -> Option<u64> {
    name.strip_prefix("validator")?.parse::<u64>().ok()
}

impl Tester for DbTester {
    fn exec_mine_block(&mut self, height: u64) -> Result<(), EasyTesterError> {
        self.mine_block(height)?;

        if let Some(rt) = &mut self.reward_tracker {
            match rt.update_after_block(&self.db, height) {
                Ok(_) => info!("Updated reward bookkeeping after block {}", height),
                Err(e) => {
                    error!(
                        "Error updating reward bookkeeping after block {}: {:?}",
                        height, e
                    );
                    return Err(EasyTesterError::runtime(format!(
                        "reward bookkeeping after block {height} failed: {e}"
                    )));
                }
            }
        }

        Ok(())
    }

    fn exec_create_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        self.create_subnet(height, subnet_name)
    }

    fn exec_join_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        collateral_sats: u64,
    ) -> Result<(), EasyTesterError> {
        self.join_subnet(height, subnet_name, validator_name, collateral_sats)
    }

    fn exec_stake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        self.stake_subnet(height, subnet_name, validator_name, amount_sats)
    }

    fn exec_unstake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        self.unstake_subnet(height, subnet_name, validator_name, amount_sats)
    }

    fn exec_deposit(
        &mut self,
        _height: u64,
        subnet_name: &str,
        address_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        // The db tester has no real Bitcoin UTXOs, so deposit is a no-op.
        // We still resolve the subnet to catch typos early.
        self.resolve_subnet_id(subnet_name)?;
        info!(
            "Deposit (no-op for db tester): {} sats to '{}' on subnet '{}'",
            amount_sats, address_name, subnet_name
        );
        Ok(())
    }

    fn exec_checkpoint_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        let token_registrations = self
            .pending_registrations
            .remove(subnet_name)
            .unwrap_or_default();
        let supply_adjustments = self
            .pending_supply_adjustments
            .remove(subnet_name)
            .unwrap_or_default();
        let erc_transfers = self
            .pending_erc_transfers
            .remove(subnet_name)
            .unwrap_or_default();

        let subnet_id = self.resolve_subnet_id(subnet_name)?;
        let checkpoint = self.checkpoint_subnet(
            height,
            subnet_name,
            token_registrations,
            supply_adjustments,
            erc_transfers,
        )?;

        if let Some(rt) = &mut self.reward_tracker {
            rt.update_after_checkpoint(&self.db, height, subnet_id, &checkpoint)
                .map_err(|e| {
                    EasyTesterError::runtime(format!(
                        "reward bookkeeping after checkpoint failed: {e}"
                    ))
                })?;
        }

        Ok(())
    }

    fn exec_register_token(
        &mut self,
        height: u64,
        subnet_name: &str,
        name: &str,
        symbol: &str,
        initial_supply: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.queue_token_registration(height, subnet_name, name, symbol, initial_supply)
    }

    fn exec_mint_token(
        &mut self,
        height: u64,
        subnet_name: &str,
        token_name: &str,
        amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.queue_mint(height, subnet_name, token_name, amount)
    }

    fn exec_burn_token(
        &mut self,
        height: u64,
        subnet_name: &str,
        token_name: &str,
        amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.queue_burn(height, subnet_name, token_name, amount)
    }

    fn exec_erc_transfer(
        &mut self,
        height: u64,
        src_subnet: &str,
        dst_subnet: &str,
        token_name: &str,
        amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        self.queue_erc_transfer(height, src_subnet, dst_subnet, token_name, amount)
    }

    fn exec_output_read(
        &mut self,
        _height: u64,
        db: OutputDb,
        args: &[String],
    ) -> Result<(), EasyTesterError> {
        // Reset last-read state
        self.last_rootnet_msgs = None;
        self.last_token_balance = None;
        self.last_reward_results = None;

        match db {
            OutputDb::RootnetMsgs => {
                let subnet_name = &args[0];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                let msgs = self
                    .db
                    .get_all_rootnet_msgs(subnet_id)
                    .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?;

                println!(
                    "OUTPUT read rootnet_msgs subnet='{}': {} messages",
                    subnet_name,
                    msgs.len()
                );
                for (i, msg) in msgs.iter().enumerate() {
                    let kind = match msg {
                        crate::db::RootnetMessage::FundSubnet { .. } => "fund",
                        crate::db::RootnetMessage::ErcTransfer { .. } => "erc_transfer",
                        crate::db::RootnetMessage::ErcRegistration { .. } => "erc_registration",
                    };
                    println!("  [{}] kind={}, nonce={}", i, kind, msg.nonce());
                }

                self.last_rootnet_msgs = Some(LastRootnetMsgs {
                    _subnet_name: subnet_name.to_string(),
                    msgs,
                });
            }

            OutputDb::TokenBalance => {
                let subnet_name = &args[0];
                let token_name = &args[1];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;

                let (home_subnet_name, reg) = self
                    .registered_tokens
                    .get(token_name.as_str())
                    .ok_or_else(|| {
                        EasyTesterError::runtime(format!("token '{}' not registered", token_name))
                    })?;
                let home_subnet_id = self.resolve_subnet_id(home_subnet_name)?;

                let balance = self
                    .db
                    .get_token_balance(home_subnet_id, reg.home_token_address, subnet_id)
                    .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?;

                println!(
                    "OUTPUT read token_balance subnet='{}' token='{}': {}",
                    subnet_name, token_name, balance
                );
                self.last_token_balance = Some(balance);
            }

            OutputDb::RewardResults => {
                let rt = self.reward_tracker.as_ref().ok_or_else(|| {
                    EasyTesterError::runtime(
                        "reward tracking not configured (add activation_height and snapshot_length to config file)",
                    )
                })?;
                let _ = rt; // borrow check — actual read goes through DatabaseRewardExtensions

                let snapshot = args[0]
                    .parse::<u64>()
                    .map_err(|e| EasyTesterError::runtime(format!("invalid snapshot: {e}")))?;

                let res = DatabaseRewardExtensions::get_snapshot_result(&self.db, snapshot)
                    .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?;

                match res {
                    None => {
                        println!("OUTPUT read RewardResults snapshot={} => None", snapshot);
                    }
                    Some(r) => {
                        let mut pk_to_name: HashMap<bitcoin::XOnlyPublicKey, String> =
                            HashMap::new();
                        for (name, v) in &self.setup.validators {
                            pk_to_name.insert(v.pubkey, name.clone());
                        }

                        let total_sats = r.total_rewarded_collateral.to_sat();
                        println!("OUTPUT read RewardResults snapshot={}", snapshot);
                        println!("rewards_list:");

                        let mut rewards_by_validator: HashMap<String, u64> = HashMap::new();
                        let mut rewards_unknown: HashMap<String, u64> = HashMap::new();
                        for (pk, amt) in &r.rewards_list {
                            let sats = amt.to_sat();
                            if let Some(name) = pk_to_name.get(pk) {
                                *rewards_by_validator.entry(name.clone()).or_insert(0) += sats;
                            } else {
                                *rewards_unknown.entry(pk.to_string()).or_insert(0) += sats;
                            }
                        }

                        let mut rows: Vec<(bool, u64, String, u64)> = Vec::new();
                        for (name, sats) in &rewards_by_validator {
                            rows.push((
                                true,
                                validator_ordinal(name).unwrap_or(u64::MAX),
                                name.clone(),
                                *sats,
                            ));
                        }
                        for (label, sats) in &rewards_unknown {
                            rows.push((false, u64::MAX, label.clone(), *sats));
                        }
                        rows.sort_by(|a, b| match (a.0, b.0) {
                            (true, false) => std::cmp::Ordering::Less,
                            (false, true) => std::cmp::Ordering::Greater,
                            _ => (a.1, &a.2).cmp(&(b.1, &b.2)),
                        });
                        for (_known, _ord, label, sats) in &rows {
                            println!("  {} -> {} SAT", label, fmt_sats_with_underscores(*sats));
                        }
                        println!(
                            "total_rewarded_collateral -> {} SAT",
                            fmt_sats_with_underscores(total_sats)
                        );

                        self.last_reward_results = Some(LastRewardResults {
                            snapshot,
                            rewards_by_validator,
                            total_sats,
                        });
                    }
                }
            }

            OutputDb::Subnet => {
                let subnet_name = &args[0];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                println!(
                    "OUTPUT read Subnet {:?} => {:?}",
                    subnet_name,
                    self.db
                        .get_subnet_state(subnet_id)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
                );
            }

            OutputDb::SubnetGenesis => {
                let subnet_name = &args[0];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                println!(
                    "OUTPUT read SubnetGenesis {:?} => {:?}",
                    subnet_name,
                    self.db
                        .get_subnet_genesis_info(subnet_id)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
                );
            }

            OutputDb::Committee => {
                let subnet_name = &args[0];
                let committee_number = args[1].parse::<u64>().map_err(|e| {
                    EasyTesterError::runtime(format!("invalid committee_number: {e}"))
                })?;
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                println!(
                    "OUTPUT read Committee {:?} {} => {:?}",
                    subnet_name,
                    committee_number,
                    self.db
                        .get_committee(subnet_id, committee_number)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
                );
            }

            OutputDb::StakeChanges => {
                let subnet_name = &args[0];
                let configuration_number = args[1].parse::<u64>().map_err(|e| {
                    EasyTesterError::runtime(format!("invalid configuration_number: {e}"))
                })?;
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                println!(
                    "OUTPUT read StakeChanges {:?} {} => {:?}",
                    subnet_name,
                    configuration_number,
                    self.db
                        .get_stake_change(subnet_id, configuration_number)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
                );
            }

            OutputDb::KillRequests => {
                let subnet_name = &args[0];
                let current_block_height = args[1].parse::<u64>().map_err(|e| {
                    EasyTesterError::runtime(format!("invalid current_block_height: {e}"))
                })?;
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                println!(
                    "OUTPUT read KillRequests {:?} {} => {:?}",
                    subnet_name,
                    current_block_height,
                    self.db
                        .get_valid_kill_requests(subnet_id, current_block_height)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
                );
            }

            OutputDb::RewardCandidates => {
                let snapshot = args[0]
                    .parse::<u64>()
                    .map_err(|e| EasyTesterError::runtime(format!("invalid snapshot: {e}")))?;
                let subnet_name = &args[1];
                let subnet_id = self.resolve_subnet_id(subnet_name)?;
                println!(
                    "OUTPUT read RewardCandidates {} {:?} => {:?}",
                    snapshot,
                    subnet_name,
                    DatabaseRewardExtensions::get_reward_candidate_info(
                        &self.db, snapshot, subnet_id
                    )
                    .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
                );
            }
            OutputDb::TokenMetadata => {
                return Err(EasyTesterError::runtime(
                    "read token_metadata is not supported by DbTester",
                ));
            }
        }

        Ok(())
    }

    fn exec_output_expect(
        &mut self,
        _height: u64,
        target: OutputExpectTarget,
        expected_value: &str,
    ) -> Result<String, EasyTesterError> {
        // token_balance expect
        if let Some(balance) = self.last_token_balance {
            let parts: Vec<&str> = target.path.split('.').collect();
            match parts.as_slice() {
                ["balance"] => {
                    use crate::easy_tester::model::parse_u256_allow_underscores;
                    let expected = parse_u256_allow_underscores(expected_value)
                        .map_err(|e| EasyTesterError::runtime(format!("balance must be numeric: {e}")))?;
                    if balance != expected {
                        return Err(EasyTesterError::runtime(format!(
                            "EXPECT failed (line {}): result.balance expected {}, got {}",
                            target.line_no, expected, balance
                        )));
                    }
                    return Ok(format!("result.balance == {}", expected));
                }
                _ => {
                    return Err(EasyTesterError::runtime(format!(
                        "after 'read token_balance', only 'result.balance' is supported, got 'result.{}'",
                        target.path
                    )));
                }
            }
        }

        // reward_results expect
        if let Some(last) = self.last_reward_results.as_ref() {
            let expected_sats: u64 = expected_value.parse::<u64>().map_err(|e| {
                EasyTesterError::runtime(format!(
                    "expect rhs must be numeric for reward_results: {e}"
                ))
            })?;
            let parts: Vec<&str> = target.path.split('.').collect();
            match parts.as_slice() {
                ["rewards_list", key] | ["reward_list", key] => {
                    let got = last.rewards_by_validator.get(*key).copied().unwrap_or(0);
                    if got != expected_sats {
                        return Err(EasyTesterError::runtime(format!(
                            "EXPECT failed (line {}, snapshot {}): result.rewards_list.{} expected {} sats, got {} sats",
                            target.line_no, last.snapshot, key, expected_sats, got
                        )));
                    }
                    return Ok(format!(
                        "result.rewards_list.{} == {} SAT",
                        key,
                        fmt_sats_with_underscores(expected_sats)
                    ));
                }
                ["total_rewarded_collateral"] => {
                    let got = last.total_sats;
                    if got != expected_sats {
                        return Err(EasyTesterError::runtime(format!(
                            "EXPECT failed (line {}, snapshot {}): result.total_rewarded_collateral expected {} sats, got {} sats",
                            target.line_no, last.snapshot, expected_sats, got
                        )));
                    }
                    return Ok(format!(
                        "result.total_rewarded_collateral == {} SAT",
                        fmt_sats_with_underscores(expected_sats)
                    ));
                }
                _ => {
                    return Err(EasyTesterError::runtime(format!(
                        "unsupported expect path 'result.{}' after 'read reward_results'",
                        target.path
                    )));
                }
            }
        }

        // rootnet_msgs expect
        let last = self.last_rootnet_msgs.as_ref().ok_or_else(|| {
            EasyTesterError::runtime("expect used but no previous 'read' command")
        })?;

        let parts: Vec<&str> = target.path.split('.').collect();
        Ok(match parts.as_slice() {
            ["count"] => {
                let expected: u64 = expected_value
                    .parse::<u64>()
                    .map_err(|e| EasyTesterError::runtime(format!("count must be numeric: {e}")))?;
                let got = last.msgs.len() as u64;
                if got != expected {
                    return Err(EasyTesterError::runtime(format!(
                        "EXPECT failed (line {}): result.count expected {}, got {}",
                        target.line_no, expected, got
                    )));
                }
                format!("result.count == {}", expected)
            }
            [index_str, field] => {
                let index: usize = index_str.parse().map_err(|e| {
                    EasyTesterError::runtime(format!("invalid index '{}': {}", index_str, e))
                })?;
                let msg = last.msgs.get(index).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "result[{}] out of range (have {} messages)",
                        index,
                        last.msgs.len()
                    ))
                })?;
                let got =
                    Self::msg_field_value(msg, field).map_err(|e| EasyTesterError::runtime(e))?;

                let values_match = match (got.parse::<u64>(), expected_value.parse::<u64>()) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => got == expected_value,
                };
                if !values_match {
                    return Err(EasyTesterError::runtime(format!(
                        "EXPECT failed (line {}): result.{}.{} expected '{}', got '{}'",
                        target.line_no, index, field, expected_value, got
                    )));
                }
                format!("result.{}.{} == {}", index, field, got)
            }
            _ => {
                return Err(EasyTesterError::runtime(format!(
                    "unsupported expect path 'result.{}'",
                    target.path
                )));
            }
        })
    }
}
