use std::{fs, path::Path};
use thiserror::Error;

use crate::{
    db::{BitcoinIpcDatabase, DbError, HeedDb, SubnetState},
    SubnetId,
};
use bitcoin::XOnlyPublicKey;
use heed::RwTxn;
use serde::{Deserialize, Serialize};

//
// Database types and traits
//

// reward_candidates:<snapshot>:<subnet_id>
const REWARD_CANDIDATES_KEY: &str = "reward_candidates:";

// reward_results:<snapshot>
const REWARD_RESULTS_KEY: &str = "reward_results:";

fn reward_candidates_prefix(snapshot: u64) -> String {
    format!("{REWARD_CANDIDATES_KEY}:{snapshot}:")
}

fn reward_candidate_key(snapshot: u64, subnet_id: SubnetId) -> String {
    format!("{REWARD_CANDIDATES_KEY}:{snapshot}:{subnet_id}")
}

fn reward_result_key(snapshot: u64) -> String {
    format!("{REWARD_RESULTS_KEY}:{snapshot}")
}

/// Keeps track of reward-related information for a subnet during a snapshot.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubnetRewardInfo {
    /// The last committee number detected for the subnet, either in previous or in the current snapshot.
    pub most_recent_committee_number: u64,
    /// The collateral amounts rewarded to each validator in the subnet during the snapshot.
    /// Acts as a lazy accumulator, see `RewardTracker`
    pub rewarded_amounts: Option<Vec<(XOnlyPublicKey, bitcoin::Amount)>>,
}

/// The result of a snapshot reward calculation.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SnapshotResult {
    /// The list of rewards for all eligible validators
    pub rewards_list: Vec<(XOnlyPublicKey, bitcoin::Amount)>,
    /// The total collateral rewarded in the snapshot.
    /// Stored for convenience, this should be derivable from `rewards_list`.
    pub total_rewarded_collateral: bitcoin::Amount,
}

pub trait RewardDatabase: BitcoinIpcDatabase {
    /// Returns all subnets that were active throughout the Bitcoin height interval
    /// `[height_from, height_to]` (both sides inclusive).
    /// A subnet is considered active if:
    /// - It was created at or before `height_from`
    /// - It was bootstrapped at or before `height_from`
    /// - It was not killed at or before `height_to`
    fn get_all_active_subnets(
        &self,
        height_from: u64,
        height_to: u64,
    ) -> Result<Vec<SubnetState>, DbError>;

    /// Returns the reward-related information for a subnet that, if it is a candidate
    /// for rewards in snapshot `snapshot`, and None otherwise.
    fn get_reward_info(
        &self,
        snapshot: u64,
        subnet_id: SubnetId,
    ) -> Result<Option<SubnetRewardInfo>, DbError>;

    fn put_reward_info(
        &self,
        txn: &mut RwTxn,
        snapshot: u64,
        subnet_id: SubnetId,
        reward_info: &SubnetRewardInfo,
    ) -> Result<(), DbError>;

    fn delete_reward_info(
        &self,
        txn: &mut RwTxn,
        snapshot: u64,
        subnet_id: SubnetId,
    ) -> Result<(), DbError>;

    fn iter_reward_info(&self, snapshot: u64)
        -> Result<Vec<(SubnetId, SubnetRewardInfo)>, DbError>;

    fn get_reward_result(&self, snapshot: u64) -> Result<Option<SnapshotResult>, DbError>;

    fn put_reward_result(
        &self,
        txn: &mut RwTxn,
        snapshot: u64,
        result: &SnapshotResult,
    ) -> Result<(), DbError>;
}

impl RewardDatabase for HeedDb {
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

    fn get_reward_info(
        &self,
        snapshot: u64,
        subnet_id: SubnetId,
    ) -> Result<Option<SubnetRewardInfo>, DbError> {
        let key = reward_candidate_key(snapshot, subnet_id);
        let txn = self.env.read_txn()?;
        Ok(self.reward_candidates_db.get(&txn, &key)?)
    }

    fn put_reward_info(
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

    fn delete_reward_info(
        &self,
        txn: &mut RwTxn,
        snapshot: u64,
        subnet_id: SubnetId,
    ) -> Result<(), DbError> {
        let key = reward_candidate_key(snapshot, subnet_id);
        self.reward_candidates_db.delete(txn, &key)?;
        Ok(())
    }

    fn iter_reward_info(
        &self,
        snapshot: u64,
    ) -> Result<Vec<(SubnetId, SubnetRewardInfo)>, DbError> {
        use std::str::FromStr;
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

    fn get_reward_result(&self, snapshot: u64) -> Result<Option<SnapshotResult>, DbError> {
        let key = reward_result_key(snapshot);
        let txn = self.env.read_txn()?;
        Ok(self.reward_results_db.get(&txn, &key)?)
    }

    fn put_reward_result(
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
    pub epoch_length: u64,
    pub snapshots_per_epoch: u64,
}

impl RewardConfig {
    pub fn new_from_file(path: impl AsRef<Path>) -> Result<Self, RewardConfigError> {
        let path = path.as_ref();
        let path_str = path.display().to_string();
        let content = fs::read_to_string(path).map_err(|source| RewardConfigError::ReadFailed {
            path: path_str.clone(),
            source,
        })?;
        let cfg = toml::from_str::<RewardConfig>(&content).map_err(|source| {
            RewardConfigError::ParseFailed {
                path: path_str,
                source,
            }
        })?;

        if cfg.snapshots_per_epoch == 0 {
            return Err(RewardConfigError::InvalidConfig(
                "snapshots_per_epoch must be > 0".to_string(),
            ));
        }
        if cfg.epoch_length == 0 {
            return Err(RewardConfigError::InvalidConfig(
                "epoch_length must be > 0".to_string(),
            ));
        }
        if cfg.epoch_length % cfg.snapshots_per_epoch != 0 {
            return Err(RewardConfigError::InvalidConfig(format!(
                "epoch_length ({}) must be divisible by snapshots_per_epoch ({})",
                cfg.epoch_length, cfg.snapshots_per_epoch
            )));
        }

        Ok(cfg)
    }

    pub fn snapshot_length(&self) -> Result<u64, RewardConfigError> {
        if self.snapshots_per_epoch == 0 {
            return Err(RewardConfigError::InternalError(
                "found config with snapshots_per_epoch == 0".to_string(),
            ));
        }
        if self.epoch_length == 0 {
            return Err(RewardConfigError::InternalError(
                "found config with epoch_length == 0".to_string(),
            ));
        }
        if self.epoch_length % self.snapshots_per_epoch != 0 {
            return Err(RewardConfigError::InternalError(format!(
                "found config with epoch_length ({}) not divisible by snapshots_per_epoch ({})",
                self.epoch_length, self.snapshots_per_epoch
            )));
        }
        Ok(self.epoch_length / self.snapshots_per_epoch)
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
        let len = self.snapshot_length()?;
        let snapshot = (h - self.activation_height) / len;
        let start_height = self.activation_height + snapshot * len;
        let end_height = start_height + len - 1;
        Ok(Some((snapshot, start_height, end_height)))
    }
}

#[derive(Error, Debug)]
pub enum RewardConfigError {
    #[error("failed reading {path}: {source}")]
    ReadFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed parsing {path}: {source}")]
    ParseFailed {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    #[error("{0}")]
    InvalidConfig(String),

    #[error("internal error: {0}")]
    InternalError(String),
}

//
// RPC-related types
//

#[derive(Serialize, Deserialize)]
#[cfg(feature = "emission_chain")]
pub struct GetValidatorRewardParams {
    pub bitcoin_height: u64,
}

#[cfg(feature = "emission_chain")]
#[derive(Serialize, Deserialize)]
pub struct GetValidatorRewardResponse {
    pub rewards_list: Vec<(bitcoin::XOnlyPublicKey, bitcoin::Amount)>,
    pub total_rewarded_collateral: bitcoin::Amount,
}

#[cfg(test)]
mod tests {
    use bitcoin::{Amount, XOnlyPublicKey};

    use crate::{
        db::{BitcoinIpcDatabase, SubnetGenesisInfo, SubnetKillState, SubnetValidator},
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
            crate::rewards::RewardDatabase::get_all_active_subnets(&db, 100, 150)
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
            epoch_length: 20,
            snapshots_per_epoch: 4,
        };
        // snapshot_length = 5
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
