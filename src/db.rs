use crate::{
    ipc_lib::{IpcCreateSubnetMsg, IpcFundSubnetMsg},
    multisig::{create_subnet_multisig_address, multisig_threshold, Power, WeightedKey},
    wallet, SubnetId, NETWORK,
};
use async_trait::async_trait;
use bitcoin::{address::NetworkUnchecked, Address, BlockHash, Txid, XOnlyPublicKey};
use heed::{types::*, Database as HeedDatabase, Env, EnvOpenOptions, RoTxn, RwTxn};
use log::{debug, error, trace};
use serde::{Deserialize, Serialize};
use std::{io, path::Path};
use thiserror::Error;

/// Genesis committee configuration number
const GENESIS_COMMITTEE_CONF_NUM: u64 = 0;

const LAST_PROCESSED_BLOCK_KEY: &str = "monitor:last_processed_block";
// subnet_genesis_info:<subnet_id>
const SUBNET_GENESIS_INFO_KEY: &str = "subnet_genesis_info:";
// subnet_state:<subnet_id>
const SUBNET_STATE_KEY: &str = "subnet_state:";
// rootnet_msgs:<subnet_id>:<nonce>
const ROOTNET_MSGS_KEY: &str = "rootnet_msgs:";
// checkpoints:<subnet_id>:<nonce>
const CHECKPOINTS_KEY: &str = "checkpoints:";
// transactions:<txid>
const TRANSACTIONS_KEY: &str = "transactions:";
// committee:<subnet_id>:<committee_number>
const COMMITTEE_KEY: &str = "committee:";
// stake_changes:<subnet_id>:<configuration_number>
const STAKE_CHANGES_KEY: &str = "stake_changes:";

pub type Wtxn<'a> = &'a mut heed::RwTxn<'a>;

fn subnet_state_key(subnet_id: SubnetId) -> String {
    format!("{SUBNET_STATE_KEY}:{}", subnet_id)
}

fn subnet_genesis_info_key(subnet_id: SubnetId) -> String {
    format!("{SUBNET_GENESIS_INFO_KEY}:{}", subnet_id)
}

fn rootnet_msgs_prefix(subnet_id: SubnetId) -> String {
    format!("{ROOTNET_MSGS_KEY}:{}", subnet_id)
}

fn rootnet_msgs_key(subnet_id: SubnetId, nonce: u64) -> String {
    format!("{ROOTNET_MSGS_KEY}:{}:{}", subnet_id, nonce)
}

fn checkpoints_key(subnet_id: SubnetId, number: u64) -> String {
    format!("{CHECKPOINTS_KEY}:{}:{}", subnet_id, number)
}

fn transaction_key(txid: &Txid) -> String {
    format!("{TRANSACTIONS_KEY}:{}", txid)
}

fn committee_key(subnet_id: SubnetId, committee_number: u64) -> String {
    format!("{COMMITTEE_KEY}:{}:{}", subnet_id, committee_number)
}

fn stake_changes_prefix(subnet_id: SubnetId) -> String {
    format!("{STAKE_CHANGES_KEY}:{}", subnet_id)
}

fn stake_change_key(subnet_id: SubnetId, configuration_number: u64) -> String {
    format!("{STAKE_CHANGES_KEY}:{}:{}", subnet_id, configuration_number)
}

#[derive(Serialize, Deserialize)]
struct MonitorInfo {
    pub last_processed_block: u64,
}

/// State of a validator in a subnet
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubnetValidator {
    /// The public key of the validator
    pub pubkey: XOnlyPublicKey,
    /// The ethereum address of the validator pubkey
    pub subnet_address: alloy_primitives::Address,
    /// The power of the validator
    pub power: Power,
    /// The current balance of the validator's stake
    pub collateral: bitcoin::Amount,
    /// Validator backup address
    pub backup_address: Address<NetworkUnchecked>,
    /// The IP address of the validator, as
    /// advertised in the subnet's join message
    pub ip: std::net::SocketAddr,
    /// The transaction ID of the join message
    pub join_txid: bitcoin::Txid,
}

pub(crate) trait SubnetValidators {
    fn total_power(&self) -> Power;
    fn threshold(&self) -> Power;
    fn multisig_address(&self, subnet_id: &SubnetId) -> Address<NetworkUnchecked>;
    fn to_committee(&self, subnet_id: &SubnetId, configuration_number: u64) -> SubnetCommittee;

    fn add_validator(&mut self, validator: &SubnetValidator) -> Result<(), DbError>;
    #[allow(unused)]
    fn remove_validator(
        &mut self,
        validator_xpk: &XOnlyPublicKey,
    ) -> Result<SubnetValidator, DbError>;
}

impl SubnetValidators for Vec<SubnetValidator> {
    fn total_power(&self) -> Power {
        self.iter().map(|v| v.power).sum()
    }

    fn threshold(&self) -> Power {
        multisig_threshold(self.total_power())
    }

    fn multisig_address(&self, subnet_id: &SubnetId) -> Address<NetworkUnchecked> {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let pubkeys = self.iter().map(|v| (v.pubkey, v.power)).collect::<Vec<_>>();
        let multisig_address =
            create_subnet_multisig_address(&secp, subnet_id, &pubkeys, self.threshold(), NETWORK)
                .expect("Multisig address should be valid");

        multisig_address.into_unchecked()
    }

    fn to_committee(&self, subnet_id: &SubnetId, configuration_number: u64) -> SubnetCommittee {
        SubnetCommittee {
            configuration_number,
            threshold: self.threshold(),
            validators: self.to_vec(),
            multisig_address: self.multisig_address(subnet_id),
        }
    }

    fn add_validator(&mut self, validator: &SubnetValidator) -> Result<(), DbError> {
        // Check if the validator already exists
        if self.iter().any(|v| v.pubkey == validator.pubkey) {
            return Err(DbError::InvalidChange(format!(
                "Validator with public key {} already exists",
                validator.pubkey
            )));
        }

        // Add the validator
        self.push(validator.clone());

        Ok(())
    }

    fn remove_validator(
        &mut self,
        validator_xpk: &XOnlyPublicKey,
    ) -> Result<SubnetValidator, DbError> {
        // Find the validator
        let position = self.iter().position(|v| &v.pubkey == validator_xpk);

        // Check if the validator exists
        if let Some(pos) = position {
            // Remove and return the validator
            let validator = self.remove(pos);
            Ok(validator)
        } else {
            Err(DbError::InvalidChange(format!(
                "Validator with public key {} not found",
                validator_xpk
            )))
        }
    }
}

/// The committee of a subnet
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubnetCommittee {
    /// The configuration number id of this committee
    pub configuration_number: u64,
    /// The threshold for the multisig
    pub threshold: Power,
    /// The current list of validators, with their balances
    pub validators: Vec<SubnetValidator>,
    /// The subnet multisig address
    pub multisig_address: Address<NetworkUnchecked>,
}

impl PartialEq for SubnetCommittee {
    fn eq(&self, other: &Self) -> bool {
        // this should suffice as we have the keys and the
        // threshold in the script_pubkey for which we derive the address
        self.multisig_address == other.multisig_address
    }
}

impl Eq for SubnetCommittee {}

impl SubnetCommittee {
    pub fn size(&self) -> u16 {
        self.validators
            .len()
            .try_into()
            .expect("SubnetCommittee size should be < u16::max")
    }

    pub fn total_power(&self) -> Power {
        self.validators.total_power()
    }

    pub fn validator_weighted_keys(&self) -> Vec<WeightedKey> {
        self.validators
            .iter()
            .map(|v| (v.pubkey, v.power))
            .collect()
    }

    pub fn address_checked(&self) -> Address {
        self.multisig_address
            .clone()
            .require_network(NETWORK)
            .expect("Multisig should be valid current network")
    }

    pub fn pubkeys(&self) -> Vec<XOnlyPublicKey> {
        self.validators.iter().map(|v| v.pubkey).collect()
    }

    pub fn get_unspent(
        &self,
        rpc: &bitcoincore_rpc::Client,
    ) -> Result<Vec<bitcoincore_rpc::json::ListUnspentResultEntry>, wallet::WalletError> {
        let address = self
            .multisig_address
            .clone()
            .require_network(NETWORK)
            .expect("Multisig should be valid for saved subnet genesis info");
        wallet::get_unspent_for_address(rpc, &address)
    }

    pub fn is_validator(&self, pubkey: &XOnlyPublicKey) -> bool {
        self.validators.iter().any(|v| &v.pubkey == pubkey)
    }

    pub fn join_new_validator(
        &mut self,
        subnet_id: &SubnetId,
        validator: &SubnetValidator,
    ) -> Result<(), DbError> {
        // Increase configuration number by 2
        // since there is one stake change for the metadata (public key)
        // and one stake change for the deposit
        self.configuration_number += 2;
        self.validators.add_validator(validator)?;
        self.threshold = self.validators.threshold();
        self.multisig_address = self.validators.multisig_address(subnet_id);
        Ok(())
    }

    pub fn modify_validator(
        &mut self,
        subnet_id: &SubnetId,
        validator: &SubnetValidator,
    ) -> Result<(), DbError> {
        // Increase configuration number by 1
        // since there is one stake change for stake/unstake
        self.configuration_number += 1;

        // Find the validator in the committee and update their information
        let validator_position = self
            .validators
            .iter()
            .position(|v| v.pubkey == validator.pubkey)
            .ok_or_else(|| {
                DbError::InvalidChange(format!(
                    "Validator {} not found in committee",
                    validator.pubkey
                ))
            })?;

        // Replace the validator with the updated one
        self.validators[validator_position] = validator.clone();

        // Update the committee's threshold and multisig address
        self.threshold = self.validators.threshold();
        self.multisig_address = self.validators.multisig_address(subnet_id);

        Ok(())
    }

    pub fn remove_validator(
        &mut self,
        subnet_id: &SubnetId,
        pubkey: &XOnlyPublicKey,
    ) -> Result<(), DbError> {
        // Increase configuration number by 1
        // since there is one stake change for stake/unstake
        self.configuration_number += 1;

        // Remove the validator from the committee
        self.validators.remove_validator(pubkey)?;

        // Update the committee's threshold and multisig address
        self.threshold = self.validators.threshold();
        self.multisig_address = self.validators.multisig_address(subnet_id);

        Ok(())
    }
}

/// Subnet checkpoint
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubnetCheckpoint {
    /// The number of the checkpoint (starting from 0)
    pub checkpoint_number: u64,
    /// The block hash of the child subnet at which the checkpoint was cut
    pub checkpoint_hash: bitcoin::hashes::sha256::Hash,
    /// The block height of the checkpoint on the child subnet
    pub checkpoint_height: u64,
    /// The block height of the checkpoint on Bitcoin
    pub block_height: u64,
    /// The txid of the checkpoint on Bitcoin
    pub txid: bitcoin::Txid,
    /// The txid of the batch transfer, if there are any transfers
    pub batch_transfer_txid: Option<bitcoin::Txid>,
    /// The block height of the batch transfer, if there are any transfers
    pub batch_transfer_block_height: Option<u64>,
    /// The number of the committee that signed the checkpoint
    pub signed_committee_number: u64,
    /// The number of the next committee (different if rotation happened)
    pub next_committee_number: u64,
    /// The next committee configuration number
    pub next_configuration_number: u64,
}

/// The current state of a subnet
/// Must only exist if the subnet is bootstrapped
///
/// Note: many more fields will be added here
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubnetState {
    /// Duplicate of the subnet ID, for easy access
    pub id: SubnetId,
    /// The current committee number
    pub committee_number: u64,
    /// The current commitee
    pub committee: SubnetCommittee,
    /// The waiting commitee, Some if it differ
    ///
    /// Waiting committee is the current committee plus
    /// any stake change requests that are pending
    /// Keep in mind that upon a checkpoint, not all
    /// stake changes are guaranteed to be applied
    /// We keep this waiting committee to check for
    /// double-joins and such.
    pub waiting_committee: Option<SubnetCommittee>,
    /// The number of the last checkpoint
    pub last_checkpoint_number: Option<u64>,
}

impl SubnetState {
    /// Returns the total stake of the current committee
    pub fn total_collateral(&self) -> bitcoin::Amount {
        self.committee.validators.iter().map(|v| v.collateral).sum()
    }

    /// Returns the multisig address of the current committee
    pub fn multisig_address(&self) -> Address {
        self.committee
            .multisig_address
            .clone()
            .require_network(NETWORK)
            .expect("Multisig should be valid for saved subnet genesis info")
    }

    pub fn committee_address_label(&self) -> (bitcoin::Address, String) {
        let address = self.multisig_address();
        let label = format!("{}-{}", self.id, self.committee_number);
        (address, label)
    }

    pub fn is_validator(&self, pubkey: &XOnlyPublicKey) -> bool {
        self.committee.is_validator(pubkey)
    }

    pub fn is_waiting_validator(&self, pubkey: &XOnlyPublicKey) -> bool {
        self.waiting_committee
            .as_ref()
            .is_some_and(|nc| nc.is_validator(pubkey))
    }

    pub fn needs_rotation(&self) -> bool {
        self.waiting_committee
            .as_ref()
            .is_some_and(|nc| self.committee != *nc)
    }

    pub fn rotate_to_committee(&mut self, new_committee: SubnetCommittee) {
        trace!(
            "subnet id {} rotating committees. prev={:?} next={:?}",
            self.id,
            self.committee,
            new_committee
        );

        if self.committee == new_committee {
            return;
        }

        if self
            .waiting_committee
            .as_ref()
            .is_some_and(|wc| *wc == new_committee)
        {
            self.waiting_committee = None
        }

        self.committee = new_committee;
        self.committee_number += 1;
    }

    pub fn rotate_to_waiting_committee(&mut self) -> Result<(), DbError> {
        if let Some(next_committee) = self.waiting_committee.take() {
            self.committee = next_committee;
            self.committee_number += 1;
            Ok(())
        } else {
            Err(DbError::InvalidChange(
                "No next committee to rotate to".to_string(),
            ))
        }
    }
}

/// An entry in the subnet genesis balance
///
/// This balance is added to user addresses in genesis, and becomes part of the genesis
/// circulating supply. Users (including validators) can add genesis balance
/// via pre-fund messages.
///
/// One entry is added per pre-fund message.
/// There could be multiple entries for a single subnet address.
#[derive(Serialize, Deserialize, Debug)]
pub struct GenesisBalanceEntry {
    /// The subnet address
    pub subnet_address: alloy_primitives::Address,
    /// The balance
    pub amount: bitcoin::Amount,
    /// The transaction ID of the pre-fund message
    pub prefund_txid: bitcoin::Txid,
    /// The block height of the pre-fund message
    pub block_height: u64,
}

/// Genesis info for a subnet
#[derive(Serialize, Deserialize, Debug)]
pub struct SubnetGenesisInfo {
    /// Duplicate of the subnet ID, for easy access
    pub subnet_id: SubnetId,
    /// The original create subnet msg, which holds
    /// the configuration alongside the validator whitelist
    ///
    /// The pre-boostrap multisig is constructed from the whitelist
    pub create_subnet_msg: IpcCreateSubnetMsg,
    /// The height of the block where the create subnet
    /// message was included
    pub create_msg_block_height: u64,
    /// Marks if the subnet is bootstrapped
    /// The struct should never be modified after bootstrapping
    pub bootstrapped: bool,
    /// The height of the block where the subnet was bootstrapped
    pub genesis_block_height: Option<u64>,
    /// The list of validators that boostrapped the subnet
    pub genesis_validators: Vec<SubnetValidator>,
    /// The initial balance of the subnet at genesis
    /// Filled with pre-fund messages
    ///
    /// use `.genesis_balance()` to get a hashmap
    pub genesis_balance_entries: Vec<GenesisBalanceEntry>,
}

impl SubnetGenesisInfo {
    /// Returns if the subnet has enough validators to bootstrap
    pub fn enough_to_bootstrap(&self) -> bool {
        self.genesis_validators.len() as u16 >= self.create_subnet_msg.min_validators
    }

    pub fn multisig_address(&self) -> Address {
        let secp = bitcoin::secp256k1::Secp256k1::new();

        let whitelist_weighted_keys = self
            .create_subnet_msg
            .whitelist
            .clone()
            .into_iter()
            .map(|xpk| (xpk, 1))
            .collect::<Vec<_>>();

        create_subnet_multisig_address(
            &secp,
            &self.subnet_id,
            &whitelist_weighted_keys,
            self.create_subnet_msg.min_validators.into(),
            NETWORK,
        )
        // TODO think about this expect, maybe return a Result
        .expect("Multisig should be valid for saved subnet genesis info")
    }

    pub fn whitelist_address_label(&self) -> (bitcoin::Address, String) {
        let address = self.multisig_address();
        let label = format!("{}-{}", self.subnet_id, 0);
        (address, label)
    }

    pub fn to_subnet(&self) -> SubnetState {
        SubnetState {
            id: self.subnet_id,
            committee_number: 1,
            last_checkpoint_number: None,
            committee: self
                .genesis_validators
                .to_committee(&self.subnet_id, GENESIS_COMMITTEE_CONF_NUM),
            waiting_committee: None,
        }
    }

    /// Returns the genesis balance for the subnet
    pub fn genesis_balance(
        &self,
    ) -> std::collections::HashMap<alloy_primitives::Address, bitcoin::Amount> {
        let mut balance = std::collections::HashMap::new();
        for entry in &self.genesis_balance_entries {
            balance
                .entry(entry.subnet_address)
                .and_modify(|amount| *amount += entry.amount)
                .or_insert(entry.amount);
        }
        balance
    }

    /// Adds a genesis balance entry to the genesis info
    pub fn add_genesis_balance_entry(
        &mut self,
        subnet_address: alloy_primitives::Address,
        amount: bitcoin::Amount,
        prefund_txid: bitcoin::Txid,
        block_height: u64,
    ) {
        self.genesis_balance_entries.push(GenesisBalanceEntry {
            subnet_address,
            amount,
            prefund_txid,
            block_height,
        });
    }

    /// Removes a genesis balance entry from the genesis info, for a given prefund_txid
    pub fn remove_genesis_balance_entry(&mut self, txid: &bitcoin::Txid) {
        self.genesis_balance_entries
            .retain(|entry| &entry.prefund_txid != txid);
    }
}

/// Message emmited on the Bitcoin chain
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind")]
pub enum RootnetMessage {
    #[serde(rename = "fund")]
    FundSubnet {
        msg: IpcFundSubnetMsg,
        block_height: u64,
        block_hash: BlockHash,
        nonce: u64,
        txid: Txid,
    },
}

impl RootnetMessage {
    pub fn nonce(&self) -> u64 {
        match self {
            RootnetMessage::FundSubnet { nonce, .. } => *nonce,
        }
    }
}

// Staking

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StakingChange {
    Deposit {
        #[serde(with = "bitcoin::amount::serde::as_sat")]
        amount: bitcoin::Amount,
    },
    Withdraw {
        #[serde(with = "bitcoin::amount::serde::as_sat")]
        amount: bitcoin::Amount,
    },
    Join {
        pubkey: bitcoin::secp256k1::PublicKey,
    },
}

/// Stake change event emmited on the Bitcoin chain
///
/// These events are saved in db as seen on the chain
/// However they will be returned to the consumer only
/// after there was a checkpoint on Bitcoin where the
/// stake changes were actually made (like rotating to)
/// another multisig or such)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StakeChangeRequest {
    /// Change request
    pub change: StakingChange,
    /// XOnlyPublicKey of the validator requesting stake change
    pub validator_xpk: XOnlyPublicKey,
    /// The ethereum address of the validator pubkey
    pub validator_subnet_address: alloy_primitives::Address,
    /// Configuration number is practically an incremental "nonce"
    /// of a single change request/event
    pub configuration_number: u64,
    /// State of the committee after the change was applied
    pub committee_after_change: SubnetCommittee,

    /// The block height of the Bitcoin block where the change request was recorded
    pub block_height: u64,
    /// The block hash of the Bitcoin block where the change request was recorded
    pub block_hash: BlockHash,
    /// The block height of the child subnet at which the checkpoint was
    pub checkpoint_block_height: Option<u64>,
    /// The block hash of the child subnet at which the checkpoint was
    pub checkpoint_block_hash: Option<BlockHash>,
    pub txid: Txid,
}

pub struct HeedDb {
    env: Env,
    monitor_info: HeedDatabase<Str, SerdeBincode<MonitorInfo>>,
    subnet_db: HeedDatabase<Str, SerdeBincode<SubnetState>>,
    subnet_genesis_db: HeedDatabase<Str, SerdeBincode<SubnetGenesisInfo>>,
    checkpoints_db: HeedDatabase<Str, SerdeBincode<SubnetCheckpoint>>,
    // TODO use SerdeBincode for this as well
    // There's a conflict of bincode and `serde(tag = "type")` for RootnetMessage
    rootnet_msgs_db: HeedDatabase<Str, SerdeJson<RootnetMessage>>,
    transactions_db: HeedDatabase<Str, SerdeBincode<Vec<u8>>>,
    committee_db: HeedDatabase<Str, SerdeBincode<SubnetCommittee>>,
    stake_changes_db: HeedDatabase<Str, SerdeBincode<StakeChangeRequest>>,
}

impl HeedDb {
    pub async fn new(database_path: &str, read_only: bool) -> Result<Self, DbError> {
        let database_path = Path::new(&database_path);

        if !database_path.exists() {
            if read_only {
                return Err(DbError::DbEnvironmentNotFound(
                    database_path.display().to_string(),
                ));
            }

            debug!(
                "Database directory does not exist, creating: {}",
                database_path.display()
            );

            // Ensure the directory exists
            std::fs::create_dir_all(database_path).map_err(|e| {
                error!("Error creating database directory: {}", e);
                e
            })?;
        }

        let flags = if read_only {
            debug!(
                "Opening database '{}' in read-only mode",
                database_path.display()
            );
            heed::EnvFlags::READ_ONLY
        } else {
            heed::EnvFlags::empty()
        };
        let env = unsafe {
            EnvOpenOptions::new()
                .flags(flags)
                // TODO set max_dbs automatically
                .max_dbs(10)
                .open(database_path)?
        };

        if read_only {
            // In read-only mode, we need to open existing databases
            let rtxn = env.read_txn()?;
            let monitor_info = env
                .open_database(&rtxn, Some("monitor_info"))?
                .ok_or(DbError::DbNotFound("monitor_info".to_string()))?;
            let subnet_db = env
                .open_database(&rtxn, Some("subnet_db"))?
                .ok_or(DbError::DbNotFound("subnet_db".to_string()))?;
            let subnet_genesis_db = env
                .open_database(&rtxn, Some("subnet_genesis_db"))?
                .ok_or(DbError::DbNotFound("subnet_genesis_db".to_string()))?;
            let checkpoints_db = env
                .open_database(&rtxn, Some("checkpoints_db"))?
                .ok_or(DbError::DbNotFound("checkpoints_db".to_string()))?;
            let rootnet_msgs_db = env
                .open_database(&rtxn, Some("rootnet_msgs_db"))?
                .ok_or(DbError::DbNotFound("rootnet_msgs_db".to_string()))?;
            let transactions_db = env
                .open_database(&rtxn, Some("transactions_db"))?
                .ok_or(DbError::DbNotFound("transactions_db".to_string()))?;
            let committee_db = env
                .open_database(&rtxn, Some("committee_db"))?
                .ok_or(DbError::DbNotFound("committee_db".to_string()))?;
            let stake_changes_db = env
                .open_database(&rtxn, Some("stake_changes_db"))?
                .ok_or(DbError::DbNotFound("stake_changes_db".to_string()))?;
            rtxn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
                subnet_genesis_db,
                checkpoints_db,
                rootnet_msgs_db,
                transactions_db,
                committee_db,
                stake_changes_db,
            })
        } else {
            // In write mode, we can create the databases if they don't exist
            let mut txn = env.write_txn()?;
            let monitor_info = env.create_database(&mut txn, Some("monitor_info"))?;
            let subnet_db = env.create_database(&mut txn, Some("subnet_db"))?;
            let subnet_genesis_db = env.create_database(&mut txn, Some("subnet_genesis_db"))?;
            let checkpoints_db = env.create_database(&mut txn, Some("checkpoints_db"))?;
            let rootnet_msgs_db = env.create_database(&mut txn, Some("rootnet_msgs_db"))?;
            let transactions_db = env.create_database(&mut txn, Some("transactions_db"))?;
            let committee_db = env.create_database(&mut txn, Some("committee_db"))?;
            let stake_changes_db = env.create_database(&mut txn, Some("stake_changes_db"))?;
            txn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
                subnet_genesis_db,
                checkpoints_db,
                rootnet_msgs_db,
                transactions_db,
                committee_db,
                stake_changes_db,
            })
        }
    }
}

pub trait Database {
    fn write_txn(&self) -> Result<RwTxn, DbError>;

    // Monitor Info
    fn get_last_processed_block(&self) -> Result<u64, DbError>;
    fn set_last_processed_block(&self, block: u64) -> Result<(), DbError>;

    // Genesis Info
    fn get_subnet_genesis_info(
        &self,
        subnet_id: SubnetId,
    ) -> Result<Option<SubnetGenesisInfo>, DbError>;
    fn save_subnet_genesis_info(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        genesis_info: &SubnetGenesisInfo,
    ) -> Result<(), DbError>;

    // Subnet State
    fn get_subnet_state(&self, subnet_id: SubnetId) -> Result<Option<SubnetState>, DbError>;
    fn save_subnet_state(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        subnet_state: &SubnetState,
    ) -> Result<(), DbError>;
    /// Gets a subnet by its multisig address
    fn get_subnet_by_multisig_address(
        &self,
        multisig_address: &Address<NetworkUnchecked>,
    ) -> Result<Option<SubnetState>, DbError>;

    // Rootnet Messages
    fn get_all_rootnet_msgs(&self, subnet_id: SubnetId) -> Result<Vec<RootnetMessage>, DbError>;
    fn get_rootnet_msgs_by_height(
        &self,
        subnet_id: SubnetId,
        block_height: u64,
    ) -> Result<Vec<RootnetMessage>, DbError>;
    fn get_last_rootnet_msg_nonce_txn(
        &self,
        txn: &RoTxn,
        subnet_id: SubnetId,
    ) -> Result<Option<u64>, DbError>;
    fn get_next_rootnet_msg_nonce(&self, subnet_id: SubnetId) -> Result<u64, DbError>;
    fn get_next_rootnet_msg_nonce_txn(
        &self,
        txn: &RoTxn,
        subnet_id: SubnetId,
    ) -> Result<u64, DbError>;
    fn get_rootnet_msg(
        &self,
        subnet_id: SubnetId,
        nonce: u64,
    ) -> Result<Option<RootnetMessage>, DbError>;
    fn add_rootnet_msg(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        msg: RootnetMessage,
    ) -> Result<(), DbError>;

    // Checkpoints
    fn get_checkpoint(
        &self,
        subnet_id: SubnetId,
        number: u64,
    ) -> Result<Option<SubnetCheckpoint>, DbError>;
    fn save_checkpoint(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        checkpoint: &SubnetCheckpoint,
        number: u64,
    ) -> Result<(), DbError>;

    // Transaction storage
    fn get_transaction(&self, txid: &Txid) -> Result<Option<bitcoin::Transaction>, DbError>;
    fn save_transaction(&self, txn: &mut RwTxn, tx: &bitcoin::Transaction) -> Result<(), DbError>;

    // Committees
    fn get_committee(
        &self,
        subnet_id: SubnetId,
        committee_number: u64,
    ) -> Result<Option<SubnetCommittee>, DbError>;

    fn save_committee(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        committee_number: u64,
        committee: &SubnetCommittee,
    ) -> Result<(), DbError>;

    // Stake changes

    fn get_stake_change(
        &self,
        subnet_id: SubnetId,
        configuration_number: u64,
    ) -> Result<Option<StakeChangeRequest>, DbError>;

    fn get_all_stake_changes(
        &self,
        subnet_id: SubnetId,
    ) -> Result<Vec<StakeChangeRequest>, DbError>;

    fn get_unconfirmed_stake_changes(
        &self,
        subnet_id: SubnetId,
    ) -> Result<Vec<StakeChangeRequest>, DbError>;

    fn get_stake_changes_by_height(
        &self,
        subnet_id: SubnetId,
        block_height: u64,
    ) -> Result<Vec<StakeChangeRequest>, DbError>;

    fn get_last_stake_change_configuration_number(
        &self,
        subnet_id: SubnetId,
    ) -> Result<u64, DbError>;

    fn get_next_stake_change_configuration_number(
        &self,
        subnet_id: SubnetId,
    ) -> Result<u64, DbError>;

    fn add_stake_change(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        stake_change: StakeChangeRequest,
    ) -> Result<(), DbError>;

    fn confirm_stake_changes(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        max_configuration_number: u64,
        confirmed_block_height: u64,
        confirmed_block_hash: BlockHash,
    ) -> Result<(Option<StakeChangeRequest>, Vec<StakeChangeRequest>), DbError>;
}

#[async_trait]
impl Database for HeedDb {
    fn write_txn(&self) -> Result<RwTxn, DbError> {
        self.env.write_txn().map_err(|e| e.into())
    }

    // Monitor Info
    fn get_last_processed_block(&self) -> Result<u64, DbError> {
        let txn = self.env.read_txn()?;
        match self.monitor_info.get(&txn, LAST_PROCESSED_BLOCK_KEY)? {
            Some(MonitorInfo {
                last_processed_block,
            }) => {
                debug!("Last processed block = {}", last_processed_block);
                Ok(last_processed_block)
            }
            None => {
                debug!("No last processed block record, defaulting to 0");
                Ok(0)
            }
        }
    }

    fn set_last_processed_block(&self, block_height: u64) -> Result<(), DbError> {
        trace!("Set last processed block = {}", block_height);
        let mut txn = self.env.write_txn()?;
        self.monitor_info.put(
            &mut txn,
            LAST_PROCESSED_BLOCK_KEY,
            &MonitorInfo {
                last_processed_block: block_height,
            },
        )?;
        txn.commit()?;
        Ok(())
    }

    // Genesis Info

    fn get_subnet_genesis_info(
        &self,
        subnet_id: SubnetId,
    ) -> Result<Option<SubnetGenesisInfo>, DbError> {
        let key = subnet_genesis_info_key(subnet_id);
        let txn = self.env.read_txn()?;
        let subnet = self.subnet_genesis_db.get(&txn, &key)?;
        Ok(subnet)
    }

    fn save_subnet_genesis_info(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        subnet_genesis_info: &SubnetGenesisInfo,
    ) -> Result<(), DbError> {
        let key = subnet_genesis_info_key(subnet_id);
        self.subnet_genesis_db.put(txn, &key, subnet_genesis_info)?;
        Ok(())
    }

    // Subnet State
    fn get_subnet_state(&self, subnet_id: SubnetId) -> Result<Option<SubnetState>, DbError> {
        let key = subnet_state_key(subnet_id);
        let txn = self.env.read_txn()?;
        let subnet = self.subnet_db.get(&txn, &key)?;
        Ok(subnet)
    }

    fn save_subnet_state(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        subnet_state: &SubnetState,
    ) -> Result<(), DbError> {
        let key = subnet_state_key(subnet_id);
        self.subnet_db.put(txn, &key, subnet_state)?;
        Ok(())
    }

    fn get_subnet_by_multisig_address(
        &self,
        multisig_address: &Address<NetworkUnchecked>,
    ) -> Result<Option<SubnetState>, DbError> {
        let txn = self.env.read_txn()?;
        let subnets = self.subnet_db.iter(&txn)?;

        for item in subnets {
            let (_, subnet_state) = item?;

            if &subnet_state.committee.multisig_address == multisig_address {
                return Ok(Some(subnet_state));
            }
        }

        // TDOO reenable below functionality
        // we probably want to know if this is an old multisig address and react
        // appropriately, like ping old committee members to see if they are still
        // active (if recent) or just ignore the msg
        //
        // // If not found in current committees, check old committees
        // let iter = self.committee_db.iter(&txn)?;
        // for item in iter {
        //     let (key_str, committee) = item?;

        //     // Check if this old committee's multisig address matches
        //     if &committee.multisig_address == multisig_address {
        //         // Parse the key to extract subnet ID and committee number
        //         let parts: Vec<&str> = key_str.split(':').collect();
        //         if parts.len() >= 3 {
        //             // Format is "{COMMITTEE_KEY}:{subnet_id}:{committee_number}"
        //             if let Ok(subnet_id) = parts[1].parse::<SubnetId>() {
        //                 // Get the current subnet state for this subnet
        //                 if let Some(subnet_state) = self.get_subnet_state(subnet_id)? {
        //                     trace!(
        //                            "Found multisig address {:?} in old committee (number unknown) for subnet {}",
        //                            *multisig_address,
        //                            subnet_id
        //                        );
        //                     return Ok(Some(subnet_state));
        //                 }
        //             }
        //         }
        //     }
        // }

        Ok(None)
    }

    // Rootnet Messages

    /// Note: Potentially returns a large number of messages,
    /// see `get_rootnet_msgs_by_height` for a more efficient way to get messages
    fn get_all_rootnet_msgs(&self, subnet_id: SubnetId) -> Result<Vec<RootnetMessage>, DbError> {
        let prefix = rootnet_msgs_prefix(subnet_id);
        let txn = self.env.read_txn()?;

        let msgs_iter = self.rootnet_msgs_db.prefix_iter(&txn, &prefix)?;

        let msgs = msgs_iter
            .map(|res| res.map(|(_, msg)| msg))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(msgs)
    }

    fn get_rootnet_msgs_by_height(
        &self,
        subnet_id: SubnetId,
        block_height: u64,
    ) -> Result<Vec<RootnetMessage>, DbError> {
        let prefix = rootnet_msgs_prefix(subnet_id);
        let txn = self.env.read_txn()?;

        let msgs_iter = self.rootnet_msgs_db.prefix_iter(&txn, &prefix)?;

        let msgs = msgs_iter
            .map(|res| res.map(|(_, msg)| msg))
            .filter_map(|res| match res {
                Ok(msg) => {
                    let height = match &msg {
                        RootnetMessage::FundSubnet { block_height, .. } => *block_height,
                    };
                    // Only include messages within the specified height range
                    if height == block_height {
                        Some(Ok(msg))
                    } else {
                        None
                    }
                }
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(msgs)
    }

    fn get_last_rootnet_msg_nonce_txn(
        &self,
        txn: &RoTxn,
        subnet_id: SubnetId,
    ) -> Result<Option<u64>, DbError> {
        let prefix = rootnet_msgs_prefix(subnet_id);
        let msgs_iter = self.rootnet_msgs_db.prefix_iter(txn, &prefix)?;
        let count: u64 = msgs_iter.count().try_into().map_err(|_| {
            DbError::TypeConversionError("max rootnet messages reached".to_string())
        })?;

        if count == 0 {
            return Ok(None);
        }

        Ok(Some(count - 1))
    }

    fn get_next_rootnet_msg_nonce(&self, subnet_id: SubnetId) -> Result<u64, DbError> {
        let txn = self.env.read_txn()?;
        let nonce = self.get_last_rootnet_msg_nonce_txn(&txn, subnet_id)?;
        let nonce = match nonce {
            // If there is a nonce, increment it
            Some(n) => n + 1,
            // Otherwise start from 0
            None => 0,
        };

        Ok(nonce)
    }

    fn get_next_rootnet_msg_nonce_txn(
        &self,
        txn: &RoTxn,
        subnet_id: SubnetId,
    ) -> Result<u64, DbError> {
        let nonce = self.get_last_rootnet_msg_nonce_txn(txn, subnet_id)?;
        let nonce = match nonce {
            // If there is a nonce, increment it
            Some(n) => n + 1,
            // Otherwise start from 0
            None => 0,
        };

        Ok(nonce)
    }

    fn get_rootnet_msg(
        &self,
        subnet_id: SubnetId,
        nonce: u64,
    ) -> Result<Option<RootnetMessage>, DbError> {
        let key = rootnet_msgs_key(subnet_id, nonce);
        let txn = self.env.read_txn()?;
        let msg = self.rootnet_msgs_db.get(&txn, &key)?;
        Ok(msg)
    }

    fn add_rootnet_msg(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        msg: RootnetMessage,
    ) -> Result<(), DbError> {
        let key = rootnet_msgs_key(subnet_id, msg.nonce());
        trace!("Add rootnet msg: {msg:#?}");
        self.rootnet_msgs_db.put(txn, &key, &msg)?;
        Ok(())
    }

    // Checkpoints

    fn get_checkpoint(
        &self,
        subnet_id: SubnetId,
        number: u64,
    ) -> Result<Option<SubnetCheckpoint>, DbError> {
        let key = checkpoints_key(subnet_id, number);
        let txn = self.env.read_txn()?;
        let checkpoint = self.checkpoints_db.get(&txn, &key)?;
        Ok(checkpoint)
    }

    fn save_checkpoint(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        checkpoint: &SubnetCheckpoint,
        number: u64,
    ) -> Result<(), DbError> {
        let key = checkpoints_key(subnet_id, number);
        self.checkpoints_db.put(txn, &key, checkpoint)?;
        Ok(())
    }

    // Transaction storage

    fn get_transaction(&self, txid: &Txid) -> Result<Option<bitcoin::Transaction>, DbError> {
        let txn = self.env.read_txn()?;
        let key = transaction_key(txid);

        match self.transactions_db.get(&txn, &key)? {
            Some(tx_bytes) => {
                let tx = bitcoin::consensus::deserialize(&tx_bytes).map_err(|_| {
                    DbError::TypeConversionError("Failed to deserialize transaction".to_string())
                })?;
                Ok(Some(tx))
            }
            None => Ok(None),
        }
    }

    fn save_transaction(&self, txn: &mut RwTxn, tx: &bitcoin::Transaction) -> Result<(), DbError> {
        let txid = tx.compute_txid();
        let key = transaction_key(&txid);

        let tx_bytes = bitcoin::consensus::serialize(tx);
        self.transactions_db.put(txn, &key, &tx_bytes)?;

        Ok(())
    }

    // Committees

    fn get_committee(
        &self,
        subnet_id: SubnetId,
        committee_number: u64,
    ) -> Result<Option<SubnetCommittee>, DbError> {
        let key = committee_key(subnet_id, committee_number);
        let txn = self.env.read_txn()?;
        let committee = self.committee_db.get(&txn, &key)?;
        Ok(committee)
    }

    fn save_committee(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        committee_number: u64,
        committee: &SubnetCommittee,
    ) -> Result<(), DbError> {
        let key = committee_key(subnet_id, committee_number);
        self.committee_db.put(txn, &key, committee)?;
        Ok(())
    }

    // Stake changes

    fn get_stake_change(
        &self,
        subnet_id: SubnetId,
        configuration_number: u64,
    ) -> Result<Option<StakeChangeRequest>, DbError> {
        let key = stake_change_key(subnet_id, configuration_number);
        let txn = self.env.read_txn()?;
        let change = self.stake_changes_db.get(&txn, &key)?;
        Ok(change)
    }

    fn get_all_stake_changes(
        &self,
        subnet_id: SubnetId,
    ) -> Result<Vec<StakeChangeRequest>, DbError> {
        let prefix = stake_changes_prefix(subnet_id);
        let txn = self.env.read_txn()?;

        let changes_iter = self.stake_changes_db.prefix_iter(&txn, &prefix)?;

        let changes = changes_iter
            .map(|res| res.map(|(_, change)| change))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(changes)
    }

    fn get_unconfirmed_stake_changes(
        &self,
        subnet_id: SubnetId,
    ) -> Result<Vec<StakeChangeRequest>, DbError> {
        let prefix = stake_changes_prefix(subnet_id);
        let txn = self.env.read_txn()?;

        let changes_iter = self.stake_changes_db.prefix_iter(&txn, &prefix)?;

        let changes = changes_iter
            .map(|res| res.map(|(_, change)| change))
            .filter_map(|res| match res {
                Ok(change) => {
                    if change.checkpoint_block_height.is_none() {
                        Some(Ok(change))
                    } else {
                        None
                    }
                }
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(changes)
    }

    /// Returns all stake changes for a given subnet
    /// for a given height
    fn get_stake_changes_by_height(
        &self,
        subnet_id: SubnetId,
        block_height: u64,
    ) -> Result<Vec<StakeChangeRequest>, DbError> {
        let prefix = stake_changes_prefix(subnet_id);
        let txn = self.env.read_txn()?;

        let changes_iter = self.stake_changes_db.prefix_iter(&txn, &prefix)?;

        let changes = changes_iter
            .map(|res| res.map(|(_, change)| change))
            .filter_map(|res| match res {
                Ok(change) => {
                    if change.block_height == block_height {
                        Some(Ok(change))
                    } else {
                        None
                    }
                }
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(changes)
    }

    fn get_last_stake_change_configuration_number(
        &self,
        subnet_id: SubnetId,
    ) -> Result<u64, DbError> {
        let prefix = stake_changes_prefix(subnet_id);
        let txn = self.env.read_txn()?;
        let changes_iter = self.stake_changes_db.prefix_iter(&txn, &prefix)?;
        let count: u64 = changes_iter
            .count()
            .try_into()
            .map_err(|_| DbError::TypeConversionError("max stake changes reached".to_string()))?;

        if count == 0 {
            return Ok(GENESIS_COMMITTEE_CONF_NUM);
        }

        Ok(count)
    }

    fn get_next_stake_change_configuration_number(
        &self,
        subnet_id: SubnetId,
    ) -> Result<u64, DbError> {
        let last_number = self.get_last_stake_change_configuration_number(subnet_id)?;
        Ok(last_number + 1)
    }

    fn add_stake_change(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        stake_change: StakeChangeRequest,
    ) -> Result<(), DbError> {
        let key = stake_change_key(subnet_id, stake_change.configuration_number);
        trace!("Add stake change: {stake_change:#?}");
        self.stake_changes_db.put(txn, &key, &stake_change)?;
        Ok(())
    }

    fn confirm_stake_changes(
        &self,
        txn: &mut RwTxn,
        subnet_id: SubnetId,
        max_configuration_number: u64,
        confirmed_block_height: u64,
        confirmed_block_hash: BlockHash,
    ) -> Result<(Option<StakeChangeRequest>, Vec<StakeChangeRequest>), DbError> {
        // Get all stake changes for the subnet
        let stake_changes = self.get_all_stake_changes(subnet_id)?;

        // Filter unconfirmed stake changes that are up to the specified configuration number
        let mut last_confirmed: Option<StakeChangeRequest> = None;
        let mut confirmed_changes = Vec::new();

        for mut change in stake_changes {
            // Only process changes that:
            // 1. Are not yet confirmed (checkpoint_block_height is None)
            // 2. Have a configuration number <= max_configuration_number
            if change.checkpoint_block_height.is_none()
                && change.configuration_number <= max_configuration_number
            {
                // Set the confirmation details
                change.checkpoint_block_height = Some(confirmed_block_height);
                change.checkpoint_block_hash = Some(confirmed_block_hash);

                // Update in the database
                let key = stake_change_key(subnet_id, change.configuration_number);
                self.stake_changes_db.put(txn, &key, &change)?;

                // Update the last confirmed change if this one has a higher configuration number
                if last_confirmed
                    .as_ref()
                    .is_none_or(|last| change.configuration_number > last.configuration_number)
                {
                    last_confirmed = Some(change.clone());
                }

                // Add to the list of confirmed changes
                confirmed_changes.push(change);
            }
        }

        Ok((last_confirmed, confirmed_changes))
    }
}

#[derive(Error, Debug)]
pub enum DbError {
    /// Database environment not found and cannot be created in read-only mode
    #[error("Database environment {0} not found and cannot be created in read-only mode")]
    DbEnvironmentNotFound(String),

    /// LMDB database not found and cannot be created in read-only mode
    #[error("Database {0} not found and cannot be created in read-only mode")]
    DbNotFound(String),

    #[error("Value not found for key {0}")]
    KeyValueNotFound(String),

    #[error("Key {0} could not be modified: {1}")]
    KeyModificationError(String, String),

    /// Generic error that can be returned by our database
    #[error("{0}")]
    InvalidChange(String),

    #[error(transparent)]
    IoError(#[from] io::Error),

    #[error(transparent)]
    HeedError(#[from] heed::Error),

    #[error("Type conversion error: {0}")]
    TypeConversionError(String),
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::test_utils::*;
    use bitcoin::{hashes::Hash, key::Parity, Amount, BlockHash, Txid, XOnlyPublicKey};
    use std::str::FromStr;

    fn create_test_rootnet_message(
        subnet_id: SubnetId,
        nonce: u64,
        block_height: u64,
    ) -> RootnetMessage {
        let dummy_txid = create_rand_txid();
        let dummy_block_hash = create_rand_blockhash();

        let msg = IpcFundSubnetMsg {
            subnet_id,
            amount: Amount::from_sat(1_000_000),
            address: create_rand_addr(),
        };

        RootnetMessage::FundSubnet {
            msg,
            block_height,
            block_hash: dummy_block_hash,
            nonce,
            txid: dummy_txid,
        }
    }

    #[test]
    #[test_retry::retry(3)]
    fn test_rootnet_message_nonce() {
        let db = create_test_db();
        let subnet_id = generate_subnet_id();
        let block_height = 100;

        let ro_txn = db.env.read_txn().unwrap();

        // Verify no messages exist initially
        assert_eq!(
            db.get_last_rootnet_msg_nonce_txn(&ro_txn, subnet_id)
                .unwrap(),
            None
        );
        drop(ro_txn);
        // Check next nonce is 0
        assert_eq!(db.get_next_rootnet_msg_nonce(subnet_id).unwrap(), 0);

        // Add a message with nonce 0
        let mut txn = db.write_txn().unwrap();
        let msg1 = create_test_rootnet_message(subnet_id, 0, block_height);
        db.add_rootnet_msg(&mut txn, subnet_id, msg1.clone())
            .unwrap();
        txn.commit().unwrap();

        // Check last nonce is now 0
        let ro_txn = db.env.read_txn().unwrap();
        let result = db
            .get_last_rootnet_msg_nonce_txn(&ro_txn, subnet_id)
            .unwrap();
        assert_eq!(result, Some(0));
        drop(ro_txn);

        // Check next nonce is 1
        let next_nonce = db.get_next_rootnet_msg_nonce(subnet_id).unwrap();
        assert_eq!(next_nonce, 1);

        // Add a message with nonce 1
        let mut txn = db.write_txn().unwrap();
        let msg2 = create_test_rootnet_message(subnet_id, 1, block_height);
        db.add_rootnet_msg(&mut txn, subnet_id, msg2.clone())
            .unwrap();
        txn.commit().unwrap();

        // Add a message with nonce 2 at block height 101
        let mut txn = db.write_txn().unwrap();
        let msg2 = create_test_rootnet_message(subnet_id, 2, block_height + 1);
        db.add_rootnet_msg(&mut txn, subnet_id, msg2.clone())
            .unwrap();
        txn.commit().unwrap();

        // Check last nonce is now 2
        let ro_txn = db.env.read_txn().unwrap();
        assert_eq!(
            db.get_last_rootnet_msg_nonce_txn(&ro_txn, subnet_id)
                .unwrap(),
            Some(2)
        );
        drop(ro_txn);

        // Check next nonce is 3
        assert_eq!(db.get_next_rootnet_msg_nonce(subnet_id).unwrap(), 3);

        // Verify we can retrieve messages by nonce
        let retrieved_msg0 = db.get_rootnet_msg(subnet_id, 0).unwrap().unwrap();
        match &retrieved_msg0 {
            RootnetMessage::FundSubnet {
                nonce,
                block_height,
                ..
            } => {
                assert_eq!(*nonce, 0);
                assert_eq!(*block_height, *block_height);
            }
        }

        // Check get_all_rootnet_msgs returns both messages
        assert_eq!(db.get_all_rootnet_msgs(subnet_id).unwrap().len(), 3);

        // Check get_rootnet_msgs_by_height returns only messages at specified height
        assert_eq!(
            db.get_rootnet_msgs_by_height(subnet_id, block_height)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            db.get_rootnet_msgs_by_height(subnet_id, block_height + 1)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    #[test_retry::retry(3)]
    fn test_rootnet_message_nonce_many_messages() {
        let db = create_test_db();
        let subnet_id = generate_subnet_id();
        // Total number of messages
        let total_message_count = 100;

        // Number of messages per block
        let messages_per_block = 10;

        // Calculate number of blocks needed
        let num_blocks = total_message_count / messages_per_block;

        // Add all messages
        let mut nonce = 0;

        for block in 0..num_blocks {
            let block_height = 1000 + block as u64;
            let mut txn = db.write_txn().unwrap();

            // Add messages_per_block messages to this block
            for _ in 0..messages_per_block {
                let msg = create_test_rootnet_message(subnet_id, nonce, block_height);
                db.add_rootnet_msg(&mut txn, subnet_id, msg).unwrap();
                nonce += 1;
            }

            txn.commit().unwrap();

            // check next nonce
            assert_eq!(db.get_next_rootnet_msg_nonce(subnet_id).unwrap(), nonce);
        }

        // Check the last nonce is total_message_count - 1
        let ro_txn = db.env.read_txn().unwrap();
        let last_nonce = db
            .get_last_rootnet_msg_nonce_txn(&ro_txn, subnet_id)
            .unwrap();
        assert_eq!(last_nonce, Some((total_message_count - 1) as u64));
        drop(ro_txn);

        // Check the next nonce is total_message_count
        let next_nonce = db.get_next_rootnet_msg_nonce(subnet_id).unwrap();
        assert_eq!(next_nonce, total_message_count as u64);
        // Check total message count
        let all_msgs = db.get_all_rootnet_msgs(subnet_id).unwrap();
        assert_eq!(all_msgs.len(), total_message_count);

        // Check messages per block for a specific block
        let specific_block = 1005; // should be the 6th block
        let block_msgs = db
            .get_rootnet_msgs_by_height(subnet_id, specific_block)
            .unwrap();
        assert_eq!(block_msgs.len(), messages_per_block);

        // Verify nonces for messages in the specific block
        let expected_first_nonce = messages_per_block * 5; // for block 1005 (0-indexed from 1000)
        let nonces: Vec<u64> = block_msgs.iter().map(|msg| msg.nonce()).collect();

        for (i, &nonce) in nonces.iter().enumerate() {
            assert_eq!(nonce, expected_first_nonce as u64 + i as u64);
        }
    }

    #[test]
    fn test_subnet_committee_join_validator() {
        // Setup: Create a subnet ID and initial committee
        let subnet_id = generate_subnet_id();

        // Create initial validators
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
        let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
        let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
        let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

        let validator1 = SubnetValidator {
            pubkey: pubkey1,
            subnet_address: create_rand_addr(),
            power: 10,
            collateral: Amount::from_sat(1_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8000".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        let validator2 = SubnetValidator {
            pubkey: pubkey2,
            subnet_address: create_rand_addr(),
            power: 15,
            collateral: Amount::from_sat(2_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8001".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        // Create initial committee with two validators
        let mut initial_validators = Vec::new();
        initial_validators.push(validator1);
        initial_validators.push(validator2);

        let mut committee = initial_validators.to_committee(&subnet_id, 0);

        // Verify initial state
        assert_eq!(committee.validators.len(), 2);
        assert_eq!(committee.total_power(), 25); // 10 + 15
        let initial_threshold = committee.threshold;
        let initial_address = committee.multisig_address.clone();

        // Create new validator to add
        let (secret_key3, _) = secp.generate_keypair(&mut rand::thread_rng());
        let pubkey3 = XOnlyPublicKey::from_keypair(&secret_key3.keypair(&secp)).0;

        let validator3 = SubnetValidator {
            pubkey: pubkey3,
            subnet_address: create_rand_addr(),
            power: 20,
            collateral: Amount::from_sat(3_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8002".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        // Add the new validator
        committee
            .join_new_validator(&subnet_id, &validator3)
            .unwrap();

        // Verify the validator was added
        assert_eq!(committee.validators.len(), 3);
        assert!(committee.is_validator(&pubkey3));

        // Verify the power and threshold updated
        assert_eq!(committee.total_power(), 45); // 10 + 15 + 20
        assert!(committee.threshold > initial_threshold);

        // Verify multisig address changed
        assert_ne!(committee.multisig_address, initial_address);

        // Try adding an existing validator (should fail)
        let result = committee.join_new_validator(&subnet_id, &validator3);
        assert!(result.is_err());

        // Verify committee size didn't change after failed addition
        assert_eq!(committee.validators.len(), 3);
    }

    #[test]
    fn test_subnet_state_rotation() {
        // Setup: Create a subnet ID
        let subnet_id = generate_subnet_id();

        // Create validators for the current committee
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
        let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
        let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
        let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

        let validator1 = SubnetValidator {
            pubkey: pubkey1,
            subnet_address: create_rand_addr(),
            power: 10,
            collateral: Amount::from_sat(1_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8000".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        let validator2 = SubnetValidator {
            pubkey: pubkey2,
            subnet_address: create_rand_addr(),
            power: 15,
            collateral: Amount::from_sat(2_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8001".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        // Create validators for the next committee (including a new one)
        let (secret_key3, _) = secp.generate_keypair(&mut rand::thread_rng());
        let pubkey3 = XOnlyPublicKey::from_keypair(&secret_key3.keypair(&secp)).0;

        let validator3 = SubnetValidator {
            pubkey: pubkey3,
            subnet_address: create_rand_addr(),
            power: 20,
            collateral: Amount::from_sat(3_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8002".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        // Create current committee with validators 1 and 2
        let mut current_validators = Vec::new();
        current_validators.push(validator1.clone());
        current_validators.push(validator2.clone());
        let current_committee = current_validators.to_committee(&subnet_id, 1);

        // Create next committee with validators 1 and 3
        let mut next_validators = Vec::new();
        next_validators.push(validator1.clone());
        next_validators.push(validator3.clone());
        let next_committee = next_validators.to_committee(&subnet_id, 2);

        // Create subnet state
        let mut subnet_state = SubnetState {
            id: subnet_id,
            committee_number: 1,
            committee: current_committee.clone(),
            waiting_committee: Some(next_committee.clone()),
            last_checkpoint_number: None,
        };

        // Verify needs_rotation returns true when committees differ
        assert!(subnet_state.needs_rotation());

        // Verify original state
        assert_eq!(subnet_state.committee_number, 1);
        assert!(subnet_state.committee.is_validator(&pubkey1));
        assert!(subnet_state.committee.is_validator(&pubkey2));
        assert!(!subnet_state.committee.is_validator(&pubkey3));

        // Perform rotation
        subnet_state.rotate_to_waiting_committee().unwrap();

        // Verify committee was updated
        assert_eq!(subnet_state.committee_number, 2); // Committee number incremented
        assert!(subnet_state.committee.is_validator(&pubkey1));
        assert!(!subnet_state.committee.is_validator(&pubkey2)); // No longer in committee
        assert!(subnet_state.committee.is_validator(&pubkey3)); // New validator added

        // Verify next_committee was consumed
        assert!(subnet_state.waiting_committee.is_none());

        // Verify needs_rotation now returns false
        assert!(!subnet_state.needs_rotation());

        // Verify error when trying to rotate with no next committee
        let result = subnet_state.rotate_to_waiting_committee();
        assert!(result.is_err());

        // Verify state didn't change after failed rotation
        assert_eq!(subnet_state.committee_number, 2);
        assert!(subnet_state.committee.is_validator(&pubkey1));
        assert!(!subnet_state.committee.is_validator(&pubkey2));
        assert!(subnet_state.committee.is_validator(&pubkey3));
    }

    #[test]
    fn test_different_power_different_multisig() {
        // Setup: Create a subnet ID
        let subnet_id = generate_subnet_id();

        // Create validators
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
        let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
        let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
        let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

        // Create first set of validators with certain powers
        let validator1_set1 = SubnetValidator {
            pubkey: pubkey1,
            subnet_address: create_rand_addr(),
            power: 10, // Power of 10 for first set
            collateral: Amount::from_sat(1_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8000".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        let validator2_set1 = SubnetValidator {
            pubkey: pubkey2,
            subnet_address: create_rand_addr(),
            power: 15, // Power of 15 for first set
            collateral: Amount::from_sat(1_500_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8001".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        // Create second set of validators with the same pubkeys but different powers
        let validator1_set2 = SubnetValidator {
            pubkey: pubkey1,
            subnet_address: create_rand_addr(),
            power: 20, // Power of 20 for second set
            collateral: Amount::from_sat(1_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8000".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        let validator2_set2 = SubnetValidator {
            pubkey: pubkey2,
            subnet_address: create_rand_addr(),
            power: 25, // Power of 25 for second set
            collateral: Amount::from_sat(2_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8001".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        // Create the two committees
        let validators_set1 = vec![validator1_set1, validator2_set1];
        let validators_set2 = vec![validator1_set2, validator2_set2];

        let committee1 = validators_set1.to_committee(&subnet_id, 0);
        let committee2 = validators_set2.to_committee(&subnet_id, 0);

        // Verify both committees have the same validators (by pubkey)
        assert_eq!(committee1.pubkeys(), committee2.pubkeys());

        // Verify the committees have different total powers
        assert_eq!(committee1.total_power(), 25); // 10 + 15
        assert_eq!(committee2.total_power(), 45); // 20 + 25

        // Verify the committees have different thresholds
        assert_ne!(committee1.threshold, committee2.threshold);

        // Verify the multisig addresses are different
        assert_ne!(committee1.multisig_address, committee2.multisig_address);
    }

    #[test]
    fn test_committee_modify_validator() {
        // Setup: Create a subnet ID and initial committee
        let subnet_id = generate_subnet_id();

        // Create secp context for key generation
        let secp = bitcoin::secp256k1::Secp256k1::new();

        // Create two validators
        let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
        let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
        let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
        let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

        // Create the first validator
        let validator1 = SubnetValidator {
            pubkey: pubkey1,
            subnet_address: create_rand_addr(),
            power: 10,
            collateral: Amount::from_sat(1_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8000".parse().unwrap(),
            join_txid: Txid::all_zeros(),
        };

        // Create the second validator
        let validator2 = SubnetValidator {
            pubkey: pubkey2,
            subnet_address: create_rand_addr(),
            power: 15,
            collateral: Amount::from_sat(2_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8001".parse().unwrap(),
            join_txid: Txid::all_zeros(),
        };

        // Create committee with two validators
        let validators = vec![validator1.clone(), validator2.clone()];
        let mut committee = validators.to_committee(&subnet_id, 1);

        // Record the initial state
        let initial_config_number = committee.configuration_number;
        let initial_threshold = committee.threshold;
        let initial_multisig = committee.multisig_address.clone();

        // Create a modified version of validator1 with increased collateral and power
        let mut modified_validator = validator1.clone();
        modified_validator.collateral = Amount::from_sat(3_000_000);
        modified_validator.power = 20;

        // Modify the validator
        let result = committee.modify_validator(&subnet_id, &modified_validator);
        assert!(result.is_ok());

        // Verify the committee state after modification
        assert_eq!(committee.configuration_number, initial_config_number + 1);
        assert_ne!(committee.threshold, initial_threshold);
        assert_ne!(committee.multisig_address, initial_multisig);

        // Check that the validator was updated correctly
        let updated_validator = committee
            .validators
            .iter()
            .find(|v| v.pubkey == pubkey1)
            .unwrap();
        assert_eq!(updated_validator.collateral, Amount::from_sat(3_000_000));
        assert_eq!(updated_validator.power, 20);

        // The second validator should remain unchanged
        let unchanged_validator = committee
            .validators
            .iter()
            .find(|v| v.pubkey == pubkey2)
            .unwrap();
        assert_eq!(unchanged_validator.collateral, Amount::from_sat(2_000_000));
        assert_eq!(unchanged_validator.power, 15);
    }

    #[test]
    fn test_committee_join_new_validator() {
        // Setup: Create a subnet ID and initial committee
        let subnet_id = generate_subnet_id();

        // Create secp context for key generation
        let secp = bitcoin::secp256k1::Secp256k1::new();

        // Create an initial validator
        let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
        let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;

        let validator1 = SubnetValidator {
            pubkey: pubkey1,
            subnet_address: create_rand_addr(),
            power: 10,
            collateral: Amount::from_sat(1_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8000".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        // Create committee with one validator
        let mut committee = vec![validator1].to_committee(&subnet_id, 1);

        // Record the initial state
        let initial_config_number = committee.configuration_number;
        let initial_validator_count = committee.validators.len();
        let initial_power = committee.total_power();

        // Create a new validator to join
        let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
        let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

        let new_validator = SubnetValidator {
            pubkey: pubkey2,
            subnet_address: create_rand_addr(),
            power: 15,
            collateral: Amount::from_sat(2_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8001".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        // Join the new validator
        let result = committee.join_new_validator(&subnet_id, &new_validator);
        assert!(result.is_ok());

        // Verify the committee state after joining
        assert_eq!(committee.configuration_number, initial_config_number + 2); // Increases by 2
        assert_eq!(committee.validators.len(), initial_validator_count + 1);
        assert_eq!(committee.total_power(), initial_power + new_validator.power);

        // Verify the new validator is in the committee
        let joined_validator = committee.validators.iter().find(|v| v.pubkey == pubkey2);
        assert!(joined_validator.is_some());
        assert_eq!(joined_validator.unwrap().power, 15);
    }

    #[test]
    #[test_retry::retry(3)]
    fn test_stake_change_configuration_number() {
        let db = create_test_db();
        let subnet_id = generate_subnet_id();

        // Create validators
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
        let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
        let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
        let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

        let validator1 = SubnetValidator {
            pubkey: pubkey1,
            subnet_address: create_rand_addr(),
            power: 10, // Power of 10 for first set
            collateral: Amount::from_sat(1_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8000".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        let validator2 = SubnetValidator {
            pubkey: pubkey2,
            subnet_address: create_rand_addr(),
            power: 15,
            collateral: Amount::from_sat(2_000_000),
            backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
                .unwrap(),
            ip: "127.0.0.1:8001".parse().unwrap(),
            join_txid: create_rand_txid(),
        };

        // Create initial committee with two validators
        let mut initial_validators = Vec::new();
        initial_validators.push(validator1.clone());

        let mut committee = initial_validators.to_committee(&subnet_id, 0);

        // Initial check - should be GENESIS_COMMITTEE_CONF_NUM + 1
        let next_conf_num = db
            .get_next_stake_change_configuration_number(subnet_id)
            .unwrap();
        assert_eq!(next_conf_num, GENESIS_COMMITTEE_CONF_NUM + 1);

        // Join new validator (should increase configuration by 2)
        committee
            .join_new_validator(&subnet_id, &validator2)
            .unwrap();

        // Create a stake change request for joining
        let join_request = StakeChangeRequest {
            change: StakingChange::Join {
                pubkey: validator2.pubkey.public_key(Parity::Even),
            },
            validator_xpk: validator2.pubkey,
            validator_subnet_address: validator2.subnet_address,
            configuration_number: GENESIS_COMMITTEE_CONF_NUM + 1, // First change is metadata
            committee_after_change: committee.clone(),
            block_height: 100,
            block_hash: BlockHash::all_zeros(),
            checkpoint_block_height: None,
            checkpoint_block_hash: None,
            txid: Txid::all_zeros(),
        };

        // Create a stake change request for initial deposit
        let deposit_request = StakeChangeRequest {
            change: StakingChange::Deposit {
                amount: validator2.collateral,
            },
            validator_xpk: validator2.pubkey,
            validator_subnet_address: validator2.subnet_address,
            configuration_number: GENESIS_COMMITTEE_CONF_NUM + 2, // Second change is deposit
            committee_after_change: committee.clone(),
            block_height: 100,
            block_hash: BlockHash::all_zeros(),
            checkpoint_block_height: None,
            checkpoint_block_hash: None,
            txid: Txid::all_zeros(),
        };

        {
            let mut wtxn = db.write_txn().unwrap();
            // Store the join request
            db.add_stake_change(&mut wtxn, subnet_id, join_request)
                .unwrap();
            // Store the deposit request
            db.add_stake_change(&mut wtxn, subnet_id, deposit_request)
                .unwrap();
            wtxn.commit().unwrap();
        }

        // Check the configuration number after adding validator
        let next_conf_num = db
            .get_next_stake_change_configuration_number(subnet_id)
            .unwrap();
        assert_eq!(next_conf_num, GENESIS_COMMITTEE_CONF_NUM + 3);

        // Modify validator by increasing stake
        let mut updated_validator = validator1.clone();
        updated_validator.power = 20;
        updated_validator.collateral = Amount::from_sat(2_000_000);

        // Modify the validator in the committee
        committee
            .modify_validator(&subnet_id, &updated_validator)
            .unwrap();

        // Create a stake change request for the modification
        let modify_request = StakeChangeRequest {
            change: StakingChange::Deposit {
                amount: Amount::from_sat(1_000_000), // Additional 0.01 BTC
            },
            validator_xpk: validator1.pubkey,
            validator_subnet_address: validator1.subnet_address,
            configuration_number: GENESIS_COMMITTEE_CONF_NUM + 3, // Third change
            committee_after_change: committee.clone(),
            block_height: 101,
            block_hash: BlockHash::all_zeros(),
            checkpoint_block_height: None,
            checkpoint_block_hash: None,
            txid: Txid::all_zeros(),
        };

        {
            let mut wtxn = db.write_txn().unwrap();
            // Store the join request
            db.add_stake_change(&mut wtxn, subnet_id, modify_request)
                .unwrap();
            wtxn.commit().unwrap();
        }

        // Check the configuration number after modifying validator
        let next_conf_num = db
            .get_next_stake_change_configuration_number(subnet_id)
            .unwrap();
        assert_eq!(next_conf_num, GENESIS_COMMITTEE_CONF_NUM + 4);
    }
}
