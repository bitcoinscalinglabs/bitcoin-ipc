use std::collections::HashMap;

use bitcoin::{hashes::sha256, Amount, BlockHash, Txid};
use log::{error, info};
use rand::RngCore;
use tempfile::TempDir;

use crate::{
    db::{DatabaseCore, DatabaseRewardExtensions, HeedDb},
    easy_tester::{
        error::EasyTesterError,
        model::{
            build_create_subnet_msg, create_rand_blockhash, create_rand_txid,
            parse_u64_allow_underscores, OutputDb, OutputExpectTarget, SetupSpec,
        },
        tester::Tester,
    },
    eth_utils,
    ipc_lib::{
        IpcCheckpointSubnetMsg, IpcCreateSubnetMsg, IpcJoinSubnetMsg, IpcStakeCollateralMsg,
        IpcUnstakeCollateralMsg, IpcValidate,
    },
    rewards::{RewardConfig, RewardTracker},
    SubnetId,
};

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

fn create_rand_checkpoint_hash() -> sha256::Hash {
    use bitcoin::hashes::Hash;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    sha256::Hash::from_slice(&bytes).expect("random bytes should make a sha256 hash")
}

pub struct RewardTester {
    _temp_dir: TempDir,
    db: HeedDb,
    setup: SetupSpec,
    reward_tracker: RewardTracker,
    block_hashes: HashMap<u64, BlockHash>,
    created_subnets: HashMap<String, SubnetId>,
    checkpoint_heights: HashMap<String, u64>,
    last_reward_results: Option<LastRewardResults>,
}

#[derive(Debug, Clone)]
struct LastRewardResults {
    snapshot: u64,
    rewards_by_validator: HashMap<String, u64>,
    total_sats: u64,
}

impl RewardTester {
    pub async fn new(
        setup: SetupSpec,
        activation_height: u64,
        snapshot_length: u64,
    ) -> Result<Self, EasyTesterError> {
        eth_utils::set_fvm_network();

        let config = RewardConfig::new(activation_height, snapshot_length)
            .map_err(|e| EasyTesterError::runtime(format!("invalid reward config: {e}")))?;
        let reward_tracker = RewardTracker::new_with_config(config);

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
            reward_tracker,
            block_hashes: HashMap::new(),
            created_subnets: HashMap::new(),
            checkpoint_heights: HashMap::new(),
            last_reward_results: None,
        })
    }

    fn block_hash(&mut self, height: u64) -> BlockHash {
        *self
            .block_hashes
            .entry(height)
            .or_insert_with(create_rand_blockhash)
    }
}

impl Tester for RewardTester {
    fn exec_mine_block(&mut self, height: u64) -> Result<(), EasyTesterError> {
        self.block_hash(height);
        self.db
            .set_last_processed_block(height)
            .map_err(|e| EasyTesterError::runtime(format!("failed to mine block {height}: {e}")))?;

        // Mimic monitor reward bookkeeping hook after finishing the block.
        match self.reward_tracker.update_after_block(&self.db, height) {
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

        info!("Mined block {}", height);
        Ok(())
    }

    fn exec_create_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        let spec = self
            .setup
            .subnets
            .get(subnet_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "internal error: subnet '{subnet_name}' missing from parsed setup"
                ))
            })?
            .clone();

        if self.created_subnets.contains_key(subnet_name) {
            return Err(EasyTesterError::runtime(format!(
                "internal error: subnet '{subnet_name}' already created"
            )));
        }

        let create_msg: IpcCreateSubnetMsg = build_create_subnet_msg(&spec);
        create_msg
            .validate()
            .map_err(|e| EasyTesterError::runtime(format!("create msg invalid: {e}")))?;

        let txid: Txid = create_rand_txid();
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

    fn exec_join_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        collateral_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let block_hash = self.block_hash(height);

        let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "internal error: subnet '{subnet_name}' not found in created subnets"
            ))
        })?;

        let validator = self
            .setup
            .validators
            .get(validator_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "internal error: validator '{validator_name}' missing from parsed setup"
                ))
            })?
            .clone();
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
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?;

        let Some(genesis_info) = genesis_info else {
            return Err(EasyTesterError::runtime(format!(
                "scenario error: subnet genesis info missing for {subnet_id} (did you run create?)"
            )));
        };

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

    fn exec_stake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let block_hash = self.block_hash(height);
        let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "internal error: subnet '{subnet_name}' not found in created subnets"
            ))
        })?;

        let validator = self
            .setup
            .validators
            .get(validator_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "internal error: validator '{validator_name}' missing from parsed setup"
                ))
            })?
            .clone();

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
                    "scenario error: subnet state missing for {subnet_id} (did you run create/bootstrap?)"
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

    fn exec_unstake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let block_hash = self.block_hash(height);
        let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "internal error: subnet '{subnet_name}' not found in created subnets"
            ))
        })?;

        let validator = self
            .setup
            .validators
            .get(validator_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "internal error: validator '{validator_name}' missing from parsed setup"
                ))
            })?
            .clone();

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
                    "scenario error: subnet genesis info missing for {subnet_id} (did you run create?)"
                ))
            })?;

        let subnet_state = self
            .db
            .get_subnet_state(subnet_id)
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "scenario error: subnet state missing for {subnet_id} (did you run create/bootstrap?)"
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

    fn exec_checkpoint_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "internal error: subnet '{subnet_name}' not found in created subnets"
            ))
        })?;

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
            unstakes: vec![],
            change_address: None,
            is_kill_checkpoint: false,
        };

        msg.validate()
            .map_err(|e| EasyTesterError::runtime(format!("checkpoint msg invalid: {e}")))?;

        let txid = create_rand_txid();
        let checkpoint = msg
            .save_to_db(&self.db, height, block_hash, txid)
            .map_err(|e| EasyTesterError::runtime(format!("checkpoint failed: {e}")))?;

        self.reward_tracker
            .update_after_checkpoint(&self.db, height, subnet_id, &checkpoint)
            .map_err(|e| {
                EasyTesterError::runtime(format!("reward bookkeeping after checkpoint failed: {e}"))
            })?;

        info!(
            "Checkpoint: subnet '{}' committed at height {} (next_cfg={})",
            subnet_name, height, next_cfg
        );
        Ok(())
    }

    fn exec_output_read(
        &mut self,
        _height: u64,
        db: OutputDb,
        args: &[String],
    ) -> Result<(), EasyTesterError> {
        if db == OutputDb::RewardResults {
            let snapshot = parse_u64_allow_underscores(&args[0])
                .map_err(|e| EasyTesterError::runtime(format!("invalid snapshot: {e}")))?;

            let res = DatabaseRewardExtensions::get_snapshot_result(&self.db, snapshot)
                .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?;

            match res {
                None => {
                    self.last_reward_results = None;
                    println!("OUTPUT read RewardResults snapshot={} => None", snapshot);
                }
                Some(r) => {
                    // Map pubkeys back to validator names (if any).
                    let mut pk_to_name: std::collections::HashMap<bitcoin::XOnlyPublicKey, String> =
                        std::collections::HashMap::new();
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

                    // Prepare sortable display rows (aggregated).
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

                    rows.sort_by(|a, b| {
                        // known validators first, then by ordinal/name; unknown last by pubkey string
                        match (a.0, b.0) {
                            (true, false) => std::cmp::Ordering::Less,
                            (false, true) => std::cmp::Ordering::Greater,
                            _ => (a.1, &a.2).cmp(&(b.1, &b.2)),
                        }
                    });

                    for (_known, _ord, label, sats) in rows {
                        println!("  {} -> {} SAT", label, fmt_sats_with_underscores(sats));
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

            return Ok(());
        }

        // For now, `expect` only applies to reward_results, so reset any stored value when
        // reading other DBs.
        self.last_reward_results = None;

        let out = match db {
            OutputDb::Subnet => {
                let subnet_name = &args[0];
                let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "internal error: subnet '{subnet_name}' not found in created subnets"
                    ))
                })?;
                format!(
                    "{:?}",
                    self.db.get_subnet_state(subnet_id).map_err(|e| {
                        EasyTesterError::runtime(format!("db read failed: {e}"))
                    })?
                )
            }

            OutputDb::SubnetGenesis => {
                let subnet_name = &args[0];
                let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "internal error: subnet '{subnet_name}' not found in created subnets"
                    ))
                })?;
                format!(
                    "{:?}",
                    self.db.get_subnet_genesis_info(subnet_id).map_err(|e| {
                        EasyTesterError::runtime(format!("db read failed: {e}"))
                    })?
                )
            }
            OutputDb::Committee => {
                let subnet_name = &args[0];
                let committee_number = parse_u64_allow_underscores(&args[1]).map_err(|e| {
                    EasyTesterError::runtime(format!("invalid committee_number: {e}"))
                })?;
                let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "internal error: subnet '{subnet_name}' not found in created subnets"
                    ))
                })?;
                format!(
                    "{:?}",
                    self.db
                        .get_committee(subnet_id, committee_number)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?,
                )
            }
            OutputDb::StakeChanges => {
                let subnet_name = &args[0];
                let configuration_number = parse_u64_allow_underscores(&args[1]).map_err(|e| {
                    EasyTesterError::runtime(format!("invalid configuration_number: {e}"))
                })?;
                let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "internal error: subnet '{subnet_name}' not found in created subnets"
                    ))
                })?;
                format!(
                    "{:?}",
                    self.db
                        .get_stake_change(subnet_id, configuration_number)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?,
                )
            }
            OutputDb::KillRequests => {
                let subnet_name = &args[0];
                let current_block_height = parse_u64_allow_underscores(&args[1]).map_err(|e| {
                    EasyTesterError::runtime(format!("invalid current_block_height: {e}"))
                })?;
                let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "internal error: subnet '{subnet_name}' not found in created subnets"
                    ))
                })?;
                format!(
                    "{:?}",
                    self.db
                        .get_valid_kill_requests(subnet_id, current_block_height)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?,
                )
            }
            OutputDb::RewardCandidates => {
                let snapshot = parse_u64_allow_underscores(&args[0])
                    .map_err(|e| EasyTesterError::runtime(format!("invalid snapshot: {e}")))?;
                let subnet_name = &args[1];
                let subnet_id = *self.created_subnets.get(subnet_name).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "internal error: subnet '{subnet_name}' not found in created subnets"
                    ))
                })?;
                format!(
                    "{:?}",
                    DatabaseRewardExtensions::get_reward_candidate_info(
                        &self.db, snapshot, subnet_id
                    )
                    .map_err(|e| { EasyTesterError::runtime(format!("db read failed: {e}")) })?
                )
            }
            OutputDb::RewardResults => {
                unreachable!("RewardResults is handled above for pretty printing")
            }
        };

        println!("OUTPUT read {:?} {:?} => {}", db, args, out);
        Ok(())
    }

    fn exec_output_expect(
        &mut self,
        _height: u64,
        target: OutputExpectTarget,
        expected_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let Some(last) = self.last_reward_results.as_ref() else {
            return Err(EasyTesterError::runtime(
                "expect used but there is no previous 'read reward_results' result",
            ));
        };

        match target {
            OutputExpectTarget::RewardResultsRewardsList { key } => {
                // `key` is expected to be a validator name (parse-time enforced).
                let got = last.rewards_by_validator.get(&key).copied().unwrap_or(0);
                if got != expected_sats {
                    return Err(EasyTesterError::runtime(format!(
                        "EXPECT failed (snapshot {}): result.rewards_list.{} expected {} sats, got {} sats",
                        last.snapshot, key, expected_sats, got
                    )));
                }
                println!(
                    "OUTPUT expect result.rewards_list.{} == {} SAT (ok)",
                    key,
                    fmt_sats_with_underscores(expected_sats)
                );
            }
            OutputExpectTarget::RewardResultsTotalRewardedCollateral => {
                let got = last.total_sats;
                if got != expected_sats {
                    return Err(EasyTesterError::runtime(format!(
                        "EXPECT failed (snapshot {}): result.total_rewarded_collateral expected {} sats, got {} sats",
                        last.snapshot, expected_sats, got
                    )));
                }
                println!(
                    "OUTPUT expect result.total_rewarded_collateral == {} SAT (ok)",
                    fmt_sats_with_underscores(expected_sats)
                );
            }
        }

        Ok(())
    }
}
