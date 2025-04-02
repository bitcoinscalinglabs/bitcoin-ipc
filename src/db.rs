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

trait SubnetValidators {
    fn total_power(&self) -> Power;
    fn threshold(&self) -> Power;
    fn multisig_address(&self, subnet_id: &SubnetId) -> Address<NetworkUnchecked>;
    fn to_committee(&self, subnet_id: &SubnetId) -> SubnetCommittee;
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

    fn to_committee(&self, subnet_id: &SubnetId) -> SubnetCommittee {
        SubnetCommittee {
            threshold: self.threshold(),
            validators: self.to_vec(),
            multisig_address: self.multisig_address(subnet_id),
        }
    }
}

/// The committee of a subnet
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubnetCommittee {
    /// The threshold for the multisig
    pub threshold: Power,
    /// The current list of validators, with their balances
    pub validators: Vec<SubnetValidator>,
    /// The subnet multisig address
    pub multisig_address: Address<NetworkUnchecked>,
}

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
}

/// Subnet checkpoint
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubnetCheckpoint {
    /// The number of the checkpoint (starting from 0)
    pub checkpoint_number: u64,
    /// The block hash of the child subnet at which the checkpoint was cut
    pub checkpoint_hash: bitcoin::hashes::sha256::Hash,
    /// The block height of the checkpoint on Bitcoin
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
    /// The number of the last checkpoint
    pub last_checkpoint_number: Option<u64>,
}

impl SubnetState {
    /// Returns the total stake of the current committee
    pub fn stake(&self) -> bitcoin::Amount {
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
            committee: self.genesis_validators.to_committee(&self.subnet_id),
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
            rtxn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
                subnet_genesis_db,
                checkpoints_db,
                rootnet_msgs_db,
                transactions_db,
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
            txn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
                subnet_genesis_db,
                checkpoints_db,
                rootnet_msgs_db,
                transactions_db,
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

    #[error(transparent)]
    IoError(#[from] io::Error),

    #[error(transparent)]
    HeedError(#[from] heed::Error),

    #[error("Type conversion error: {0}")]
    TypeConversionError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{hashes::Hash, Amount, BlockHash, Txid};
    use tempfile::tempdir;

    fn create_test_db() -> HeedDb {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().to_str().unwrap();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(HeedDb::new(db_path, false))
            .unwrap()
    }

    fn create_rand_subnet_id() -> SubnetId {
        SubnetId::from_txid(&Txid::from_slice(&rand::random::<[u8; 32]>()).unwrap())
    }
    fn create_rand_txid() -> Txid {
        Txid::from_slice(&rand::random::<[u8; 32]>()).unwrap()
    }
    fn create_rand_blockhash() -> BlockHash {
        BlockHash::from_slice(&rand::random::<[u8; 32]>()).unwrap()
    }
    fn create_rand_addr() -> alloy_primitives::Address {
        alloy_primitives::Address::from_slice(&rand::random::<[u8; 20]>())
    }

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
    fn test_rootnet_message_nonce() {
        let db = create_test_db();
        let subnet_id = create_rand_subnet_id();
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
    fn test_rootnet_message_nonce_many_messages() {
        let db = create_test_db();
        let subnet_id = create_rand_subnet_id();
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
}
