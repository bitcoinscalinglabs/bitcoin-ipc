use std::collections::HashMap;

use log::{error, info};

use crate::{
    db::{DatabaseCore, DatabaseRewardExtensions},
    easy_tester::{
        base::BaseTester,
        error::EasyTesterError,
        model::{parse_u64_allow_underscores, OutputDb, OutputExpectTarget, SetupSpec},
        tester::Tester,
    },
    rewards::{RewardConfig, RewardTracker},
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

pub struct RewardTester {
    base: BaseTester,
    reward_tracker: RewardTracker,
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
        let config = RewardConfig::new(activation_height, snapshot_length)
            .map_err(|e| EasyTesterError::runtime(format!("invalid reward config: {e}")))?;
        let reward_tracker = RewardTracker::new_with_config(config);

        let base = BaseTester::new(setup).await?;

        Ok(Self {
            base,
            reward_tracker,
            last_reward_results: None,
        })
    }
}

impl Tester for RewardTester {
    fn exec_mine_block(&mut self, height: u64) -> Result<(), EasyTesterError> {
        self.base.mine_block(height)?;

        // Mimic monitor reward bookkeeping hook after finishing the block.
        match self
            .reward_tracker
            .update_after_block(&self.base.db, height)
        {
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

        Ok(())
    }

    fn exec_create_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        self.base.create_subnet(height, subnet_name)
    }

    fn exec_join_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        collateral_sats: u64,
    ) -> Result<(), EasyTesterError> {
        self.base
            .join_subnet(height, subnet_name, validator_name, collateral_sats)
    }

    fn exec_stake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        self.base
            .stake_subnet(height, subnet_name, validator_name, amount_sats)
    }

    fn exec_unstake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        self.base
            .unstake_subnet(height, subnet_name, validator_name, amount_sats)
    }

    fn exec_checkpoint_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        let subnet_id = self.base.resolve_subnet_id(subnet_name)?;
        let checkpoint = self
            .base
            .checkpoint_subnet(height, subnet_name, vec![], vec![])?;

        self.reward_tracker
            .update_after_checkpoint(&self.base.db, height, subnet_id, &checkpoint)
            .map_err(|e| {
                EasyTesterError::runtime(format!(
                    "reward bookkeeping after checkpoint failed: {e}"
                ))
            })?;

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

            let res = DatabaseRewardExtensions::get_snapshot_result(&self.base.db, snapshot)
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
                    for (name, v) in &self.base.setup.validators {
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
                let subnet_id = self.base.resolve_subnet_id(subnet_name)?;
                format!(
                    "{:?}",
                    self.base.db.get_subnet_state(subnet_id).map_err(|e| {
                        EasyTesterError::runtime(format!("db read failed: {e}"))
                    })?
                )
            }

            OutputDb::SubnetGenesis => {
                let subnet_name = &args[0];
                let subnet_id = self.base.resolve_subnet_id(subnet_name)?;
                format!(
                    "{:?}",
                    self.base
                        .db
                        .get_subnet_genesis_info(subnet_id)
                        .map_err(|e| {
                            EasyTesterError::runtime(format!("db read failed: {e}"))
                        })?
                )
            }
            OutputDb::Committee => {
                let subnet_name = &args[0];
                let committee_number = parse_u64_allow_underscores(&args[1]).map_err(|e| {
                    EasyTesterError::runtime(format!("invalid committee_number: {e}"))
                })?;
                let subnet_id = self.base.resolve_subnet_id(subnet_name)?;
                format!(
                    "{:?}",
                    self.base
                        .db
                        .get_committee(subnet_id, committee_number)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?,
                )
            }
            OutputDb::StakeChanges => {
                let subnet_name = &args[0];
                let configuration_number = parse_u64_allow_underscores(&args[1]).map_err(|e| {
                    EasyTesterError::runtime(format!("invalid configuration_number: {e}"))
                })?;
                let subnet_id = self.base.resolve_subnet_id(subnet_name)?;
                format!(
                    "{:?}",
                    self.base
                        .db
                        .get_stake_change(subnet_id, configuration_number)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?,
                )
            }
            OutputDb::KillRequests => {
                let subnet_name = &args[0];
                let current_block_height = parse_u64_allow_underscores(&args[1]).map_err(|e| {
                    EasyTesterError::runtime(format!("invalid current_block_height: {e}"))
                })?;
                let subnet_id = self.base.resolve_subnet_id(subnet_name)?;
                format!(
                    "{:?}",
                    self.base
                        .db
                        .get_valid_kill_requests(subnet_id, current_block_height)
                        .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?,
                )
            }
            OutputDb::RewardCandidates => {
                let snapshot = parse_u64_allow_underscores(&args[0])
                    .map_err(|e| EasyTesterError::runtime(format!("invalid snapshot: {e}")))?;
                let subnet_name = &args[1];
                let subnet_id = self.base.resolve_subnet_id(subnet_name)?;
                format!(
                    "{:?}",
                    DatabaseRewardExtensions::get_reward_candidate_info(
                        &self.base.db,
                        snapshot,
                        subnet_id
                    )
                    .map_err(|e| { EasyTesterError::runtime(format!("db read failed: {e}")) })?
                )
            }
            OutputDb::RewardResults => {
                unreachable!("RewardResults is handled above for pretty printing")
            }
            OutputDb::RootnetMsgs => {
                return Err(EasyTesterError::runtime(
                    "RewardTester does not support reading rootnet_msgs",
                ));
            }
        };

        println!("OUTPUT read {:?} {:?} => {}", db, args, out);
        Ok(())
    }

    fn exec_output_expect(
        &mut self,
        _height: u64,
        target: OutputExpectTarget,
        expected_value: &str,
    ) -> Result<(), EasyTesterError> {
        let expected_sats: u64 = parse_u64_allow_underscores(expected_value)
            .map_err(|e| EasyTesterError::runtime(format!("expect rhs must be numeric for RewardTester: {e}")))?;

        let Some(last) = self.last_reward_results.as_ref() else {
            return Err(EasyTesterError::runtime(
                "expect used but there is no previous 'read reward_results' result",
            ));
        };

        let parts: Vec<&str> = target.path.split('.').collect();
        match parts.as_slice() {
            ["rewards_list", key] | ["reward_list", key] => {
                let got = last.rewards_by_validator.get(*key).copied().unwrap_or(0);
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
            ["total_rewarded_collateral"] => {
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
            _ => {
                return Err(EasyTesterError::runtime(format!(
                    "unsupported expect path 'result.{}' for RewardTester",
                    target.path
                )));
            }
        }

        Ok(())
    }
}
