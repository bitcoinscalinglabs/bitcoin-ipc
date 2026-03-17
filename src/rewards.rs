use crate::{
    db::{
        reward_candidate_key, reward_candidates_prefix, reward_result_key, DatabaseCore,
        DatabaseRewardExtensions, DbError, HeedDb, SnapshotResult, SubnetRewardInfo, SubnetState,
    },
    SubnetId,
};
use heed::RwTxn;
use log::{info, trace};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

impl DatabaseRewardExtensions for HeedDb {
    fn get_all_active_subnets(
        &self,
        height_from: u64,
        height_to: u64,
    ) -> Result<Vec<SubnetState>, DbError> {
        let all = self.get_all_subnets()?;
        let mut active = Vec::new();

        for subnet_state in all {
            use crate::db::SubnetKillState;

            let genesis = match self.get_subnet_genesis_info(subnet_state.id)? {
                Some(g) => g,
                None => continue,
            };

            if genesis.create_msg_block_height > height_from {
                continue;
            }

            let Some(genesis_height) = genesis.genesis_block_height else {
                continue;
            };

            if genesis_height > height_from {
                continue;
            }

            if let SubnetKillState::Killed { parent_height } = subnet_state.killed {
                if parent_height <= height_to {
                    continue;
                }
            }

            active.push(subnet_state);
        }

        Ok(active)
    }

    fn get_reward_candidate_info(
        &self,
        snapshot: u64,
        subnet_id: SubnetId,
    ) -> Result<Option<SubnetRewardInfo>, DbError> {
        let key = reward_candidate_key(snapshot, subnet_id);
        let txn = self.env.read_txn()?;
        Ok(self.reward_candidates_db.get(&txn, &key)?)
    }

    fn put_reward_candidate_info(
        &self,
        txn: &mut RwTxn,
        snapshot: u64,
        subnet_id: SubnetId,
        candidate: &SubnetRewardInfo,
    ) -> Result<(), DbError> {
        let key = reward_candidate_key(snapshot, subnet_id);
        self.reward_candidates_db.put(txn, &key, candidate)?;
        Ok(())
    }

    fn delete_reward_candidate_info(
        &self,
        txn: &mut RwTxn,
        snapshot: u64,
        subnet_id: SubnetId,
    ) -> Result<(), DbError> {
        let key = reward_candidate_key(snapshot, subnet_id);
        self.reward_candidates_db.delete(txn, &key)?;
        Ok(())
    }

    fn iter_reward_candidate_info(
        &self,
        snapshot: u64,
    ) -> Result<Vec<(SubnetId, SubnetRewardInfo)>, DbError> {
        let txn = self.env.read_txn()?;
        let prefix = reward_candidates_prefix(snapshot);
        let mut out = Vec::new();
        for res in self.reward_candidates_db.prefix_iter(&txn, &prefix)? {
            let (k, v) = res?;
            // key format: "reward_candidates:<snapshot>:<subnet_id>"
            let subnet_id_str = k.rsplit(':').next().ok_or_else(|| {
                DbError::InvalidChange("invalid reward candidate key".to_string())
            })?;
            let subnet_id = SubnetId::from_str(subnet_id_str).map_err(|_| {
                DbError::InvalidChange(format!(
                    "invalid subnet_id in reward candidate key: {}",
                    subnet_id_str
                ))
            })?;
            out.push((subnet_id, v));
        }
        Ok(out)
    }

    fn get_snapshot_result(&self, snapshot: u64) -> Result<Option<SnapshotResult>, DbError> {
        let key = reward_result_key(snapshot);
        let txn = self.env.read_txn()?;
        Ok(self.reward_results_db.get(&txn, &key)?)
    }

    fn put_snapshot_result(
        &self,
        txn: &mut RwTxn,
        snapshot: u64,
        result: &SnapshotResult,
    ) -> Result<(), DbError> {
        let key = reward_result_key(snapshot);
        self.reward_results_db.put(txn, &key, result)?;
        Ok(())
    }
}

//
// Reward configuration
//

#[derive(Debug, Clone, Deserialize)]
pub struct RewardConfig {
    pub activation_height: u64,
    pub snapshot_length: u64,
}

impl RewardConfig {
    pub fn new_from_env() -> Result<Self, RewardConfigError> {
        let activation_height = std::env::var("ACTIVATION_HEIGHT")
            .expect("ACTIVATION_HEIGHT env var not defined but required when emission chain features are enabled")
            .parse::<u64>()
            .expect("ACTIVATION_HEIGHT must be a valid u64");
        let snapshot_length = std::env::var("SNAPSHOT_LENGTH")
            .expect("SNAPSHOT_LENGTH env var not defined but required when emission chain features are enabled")
            .parse::<u64>()
            .expect("SNAPSHOT_LENGTH must be a valid u64");

        Self::new(activation_height, snapshot_length)
    }

    pub fn new(activation_height: u64, snapshot_length: u64) -> Result<Self, RewardConfigError> {
        let cfg = RewardConfig {
            activation_height,
            snapshot_length,
        };

        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), RewardConfigError> {
        if self.snapshot_length == 0 {
            return Err(RewardConfigError::InvalidConfig(
                "snapshot_length must be > 0".to_string(),
            ));
        }
        Ok(())
    }

    /// Returns (snapshot_number, start_height, end_height) for a Bitcoin height `h`,
    /// or `None` if `h < activation_height`.
    pub fn snapshot_boundaries_from_height(
        &self,
        h: u64,
    ) -> Result<Option<(u64, u64, u64)>, RewardConfigError> {
        if h < self.activation_height {
            return Ok(None);
        }
        let len = self.snapshot_length;
        let snapshot = (h - self.activation_height) / len;
        let start_height = self.activation_height + snapshot * len;
        let end_height = start_height + len - 1;
        Ok(Some((snapshot, start_height, end_height)))
    }

    /// Returns (start_height, end_height) for the given snapshot number.
    pub fn snapshot_boundaries(&self, snapshot: u64) -> Result<(u64, u64), RewardConfigError> {
        let len = self.snapshot_length;
        let start_height = self.activation_height + snapshot * len;
        let end_height = start_height + len - 1;
        Ok((start_height, end_height))
    }
}

#[derive(Error, Debug)]
pub enum RewardConfigError {
    #[error("{0}")]
    InvalidConfig(String),

    #[error("internal error: {0}")]
    InternalError(String),
}

//
// RPC-related types
//

#[derive(Serialize, Deserialize)]
pub struct GetRewardedCollateralsParams {
    pub snapshot: u64,
}

#[derive(Serialize, Deserialize)]
pub struct GetRewardedCollateralsResponse {
    pub collaterals: Vec<(alloy_primitives::Address, bitcoin::Amount)>,
    pub total_rewarded_collateral: bitcoin::Amount,
}

//
// Reward tracker logic (shared by monitor + testers)
//

#[derive(Error, Debug)]
pub enum RewardTrackerError {
    #[error(transparent)]
    Db(#[from] crate::db::DbError),

    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("IPC message error: {0}")]
    IpcTxInvalid(String),
}

/// Reward logic, meant to be run on the emission chain.
/// Function `update_after_block()` must be called after the caller finishes processing a Bitcoin block.
/// Function `update_after_checkpoint()` must be called after the caller finishes processing a subnet checkpoint.
/// For blocks with a checkpoint, `update_after_checkpoint()` must be called first and then `update_after_block()`.
pub struct RewardTracker {
    config: RewardConfig,
}

impl RewardTracker {
    pub fn new() -> Self {
        let config = RewardConfig::new_from_env()
            .unwrap_or_else(|e| panic!("Failed to create reward config: {}", e));
        Self { config }
    }

    pub fn new_with_config(config: RewardConfig) -> Self {
        Self { config }
    }

    // After processing a block, we need to do the following:
    // 1. If we are at the start of a snapshot, store all subnets that are candidates for rewards in this snapshot.
    // 2. If we are at the end of a snapshot, calculate the rewards for all subnets that are candidates for rewards in this snapshot.
    pub fn update_after_block(
        &mut self,
        db: &dyn crate::db::DatabaseRewardExtensions,
        block_height: u64,
    ) -> Result<(), RewardTrackerError> {
        let Some((snapshot, start_height, end_height)) = self
            .config
            .snapshot_boundaries_from_height(block_height)
            .map_err(|e| RewardTrackerError::Config(e.to_string()))?
        else {
            trace!("Validator rewards not yet activated at block height {block_height}.");
            return Ok(());
        };

        // 1. Start of snapshot: store candidate subnets.
        if block_height == start_height {
            info!("Start of snapshot {snapshot} at block height {start_height}.");
            let active_subnets = db.get_all_active_subnets(start_height, end_height)?;

            let mut wtxn = db.write_txn()?;
            for subnet in active_subnets {
                db.put_reward_candidate_info(
                    &mut wtxn,
                    snapshot,
                    subnet.id,
                    &crate::db::SubnetRewardInfo {
                        most_recent_committee_number: subnet.committee_number,
                        rewarded_amounts: None,
                    },
                )?;
            }
            wtxn.commit().map_err(crate::db::DbError::from)?;
        }

        // 2. End of snapshot: finalize rewards.
        if block_height == end_height {
            info!("End of snapshot {snapshot} at block height {end_height}.");
            let candidates = db.iter_reward_candidate_info(snapshot)?;

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
            db.put_snapshot_result(
                &mut wtxn,
                snapshot,
                &crate::db::SnapshotResult {
                    rewards_list,
                    total_rewarded_collateral,
                },
            )?;
            wtxn.commit().map_err(crate::db::DbError::from)?;
        }
        info!("Updated reward bookkeeping after block {block_height}.");
        Ok(())
    }

    /// React to a checkpoint during a snapshot:
    /// 1. Kill checkpoint: delete the candidate (subnet is not eligible for rewards).
    /// 2. Rotation checkpoint: lazily materialize and update a min-collateral accumulator.
    pub fn update_after_checkpoint(
        &mut self,
        db: &dyn crate::db::DatabaseRewardExtensions,
        block_height: u64,
        subnet_id: SubnetId,
        checkpoint: &crate::db::SubnetCheckpoint,
    ) -> Result<(), RewardTrackerError> {
        let Some((current_snapshot, start_height, _end_height)) = self
            .config
            .snapshot_boundaries_from_height(block_height)
            .map_err(|e| RewardTrackerError::Config(e.to_string()))?
        else {
            trace!("Validator rewards not yet activated at block height {block_height}.");
            return Ok(());
        };

        // Changes made on the first block of a snapshot are already accounted for in update_after_block().
        if block_height == start_height {
            return Ok(());
        }

        // 1. Kill checkpoint: remove subnet from candidates.
        if checkpoint.is_kill_checkpoint {
            let mut wtxn = db.write_txn()?;
            db.delete_reward_candidate_info(&mut wtxn, current_snapshot, subnet_id)?;
            wtxn.commit().map_err(crate::db::DbError::from)?;
            return Ok(());
        }

        // Nothing else to do if no committee rotation happened.
        if checkpoint.signed_committee_number == checkpoint.next_committee_number {
            return Ok(());
        }

        // Only update if subnet is still a candidate for this snapshot.
        let Some(mut cand_info) = db.get_reward_candidate_info(current_snapshot, subnet_id)? else {
            return Ok(());
        };

        // The following should never happen
        if checkpoint.next_committee_number == cand_info.most_recent_committee_number {
            trace!("Received checkpoint with next_committee_number equal to the most_recent_committee_number in reward candidate database for subnet {subnet_id}");
            return Ok(());
        }

        // 2. Rotation checkpoint: update the min-collateral accumulator.
        let mut rewards_in_subnet =
            self.get_or_init_reward_amounts(db, subnet_id, &cand_info, block_height)?;

        let new_committee = db
            .get_committee(subnet_id, checkpoint.next_committee_number)?
            .ok_or_else(|| {
                RewardTrackerError::IpcTxInvalid(format!(
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
        db.put_reward_candidate_info(&mut wtxn, current_snapshot, subnet_id, &cand_info)?;
        wtxn.commit().map_err(crate::db::DbError::from)?;

        Ok(())
    }

    fn get_or_init_reward_amounts(
        &mut self,
        db: &dyn crate::db::DatabaseRewardExtensions,
        subnet_id: SubnetId,
        info: &crate::db::SubnetRewardInfo,
        block_height: u64,
    ) -> Result<Vec<(bitcoin::XOnlyPublicKey, bitcoin::Amount)>, RewardTrackerError> {
        let reward_amounts: Vec<(bitcoin::XOnlyPublicKey, bitcoin::Amount)> =
            if let Some(v) = &info.rewarded_amounts {
                v.clone()
            } else {
                let committee = db
                    .get_committee(subnet_id, info.most_recent_committee_number)?
                    .ok_or_else(|| {
                        RewardTrackerError::IpcTxInvalid(format!(
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

#[cfg(test)]
mod tests {
    use bitcoin::{Amount, XOnlyPublicKey};

    use crate::{
        db::{
            DatabaseCore, DatabaseRewardExtensions, SubnetGenesisInfo, SubnetKillState,
            SubnetValidator,
        },
        rewards::RewardConfig,
        test_utils::create_test_db,
        IpcCreateSubnetMsg, SubnetId,
    };

    #[test]
    fn test_get_all_active_subnets() {
        let db = create_test_db();

        fn create_genesis_info(
            subnet_id: SubnetId,
            create_msg_block_height: u64,
            bootstrapped: bool,
            genesis_block_height: Option<u64>,
            whitelist: Vec<XOnlyPublicKey>,
            genesis_validators: Vec<SubnetValidator>,
        ) -> SubnetGenesisInfo {
            let create_subnet_msg = IpcCreateSubnetMsg {
                min_validator_stake: Amount::from_sat(100_000_000),
                min_validators: 4,
                bottomup_check_period: 1,
                active_validators_limit: 10,
                min_cross_msg_fee: Amount::from_sat(1),
                whitelist,
            };

            SubnetGenesisInfo {
                subnet_id,
                create_subnet_msg,
                create_msg_block_height,
                bootstrapped,
                genesis_block_height,
                genesis_validators,
                genesis_balance_entries: Vec::new(),
            }
        }

        // 1) Not bootstrapped, create_height=50. No SubnetState stored.
        let subnet1_id = crate::test_utils::generate_subnet_id();
        let subnet1_whitelist = crate::test_utils::generate_xonly_pubkeys(4);
        let subnet1_genesis =
            create_genesis_info(subnet1_id, 50, false, None, subnet1_whitelist, Vec::new());

        // 2) Bootstrapped, create_height=50, genesis_height=110. Store SubnetState.
        let subnet2_state = crate::test_utils::generate_subnet(4);
        let subnet2_id = subnet2_state.id;
        let subnet2_whitelist = subnet2_state.committee.pubkeys();
        let subnet2_genesis = create_genesis_info(
            subnet2_id,
            50,
            true,
            Some(110),
            subnet2_whitelist,
            subnet2_state.committee.validators.clone(),
        );

        // 3) Bootstrapped, create_height=50, genesis_height=90, killed=NotKilled.
        let mut subnet3_state = crate::test_utils::generate_subnet(4);
        let subnet3_id = subnet3_state.id;
        let subnet3_whitelist = subnet3_state.committee.pubkeys();
        subnet3_state.killed = SubnetKillState::NotKilled;
        let subnet3_genesis = create_genesis_info(
            subnet3_id,
            50,
            true,
            Some(90),
            subnet3_whitelist,
            subnet3_state.committee.validators.clone(),
        );

        // 4) Same as (3) but killed=ToBeKilled.
        let mut subnet4_state = crate::test_utils::generate_subnet(4);
        let subnet4_id = subnet4_state.id;
        let subnet4_whitelist = subnet4_state.committee.pubkeys();
        subnet4_state.killed = SubnetKillState::ToBeKilled;
        let subnet4_genesis = create_genesis_info(
            subnet4_id,
            50,
            true,
            Some(90),
            subnet4_whitelist,
            subnet4_state.committee.validators.clone(),
        );

        // 5) Same as (3) but killed=Killed{parent_height=160}.
        let mut subnet5_state = crate::test_utils::generate_subnet(4);
        let subnet5_id = subnet5_state.id;
        let subnet5_whitelist = subnet5_state.committee.pubkeys();
        subnet5_state.killed = SubnetKillState::Killed { parent_height: 160 };
        let subnet5_genesis = create_genesis_info(
            subnet5_id,
            50,
            true,
            Some(90),
            subnet5_whitelist,
            subnet5_state.committee.validators.clone(),
        );

        // 6) Same as (3) but killed=Killed{parent_height=140}.
        let mut subnet6_state = crate::test_utils::generate_subnet(4);
        let subnet6_id = subnet6_state.id;
        let subnet6_whitelist = subnet6_state.committee.pubkeys();
        subnet6_state.killed = SubnetKillState::Killed { parent_height: 140 };
        let subnet6_genesis = create_genesis_info(
            subnet6_id,
            50,
            true,
            Some(90),
            subnet6_whitelist,
            subnet6_state.committee.validators.clone(),
        );

        // Write everything to DB in one transaction.
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_genesis_info(&mut wtxn, subnet1_id, &subnet1_genesis)
            .unwrap();
        db.save_subnet_genesis_info(&mut wtxn, subnet2_id, &subnet2_genesis)
            .unwrap();
        db.save_subnet_genesis_info(&mut wtxn, subnet3_id, &subnet3_genesis)
            .unwrap();
        db.save_subnet_genesis_info(&mut wtxn, subnet4_id, &subnet4_genesis)
            .unwrap();
        db.save_subnet_genesis_info(&mut wtxn, subnet5_id, &subnet5_genesis)
            .unwrap();
        db.save_subnet_genesis_info(&mut wtxn, subnet6_id, &subnet6_genesis)
            .unwrap();

        db.save_subnet_state(&mut wtxn, subnet2_id, &subnet2_state)
            .unwrap();
        db.save_subnet_state(&mut wtxn, subnet3_id, &subnet3_state)
            .unwrap();
        db.save_subnet_state(&mut wtxn, subnet4_id, &subnet4_state)
            .unwrap();
        db.save_subnet_state(&mut wtxn, subnet5_id, &subnet5_state)
            .unwrap();
        db.save_subnet_state(&mut wtxn, subnet6_id, &subnet6_state)
            .unwrap();
        wtxn.commit().unwrap();

        // Query active subnets between 100 and 150. Expected: subnets 3, 4, 5.
        let mut active_ids: Vec<_> =
            DatabaseRewardExtensions::get_all_active_subnets(&db, 100, 150)
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect();
        active_ids.sort();

        let mut expected = vec![subnet3_id, subnet4_id, subnet5_id];
        expected.sort();

        assert_eq!(active_ids, expected);
    }

    #[test]
    fn test_snapshot_boundaries() {
        let cfg = RewardConfig {
            activation_height: 100,
            snapshot_length: 5,
        };
        // snapshot 0: [100, 104]
        // snapshot 1: [105, 109]
        // snapshot 2: [110, 114]
        assert_eq!(cfg.snapshot_boundaries_from_height(99).unwrap(), None);
        assert_eq!(
            cfg.snapshot_boundaries_from_height(100).unwrap(),
            Some((0, 100, 104))
        );
        assert_eq!(
            cfg.snapshot_boundaries_from_height(104).unwrap(),
            Some((0, 100, 104))
        );
        assert_eq!(
            cfg.snapshot_boundaries_from_height(105).unwrap(),
            Some((1, 105, 109))
        );
    }
}
