use crate::{
    bitcoin_utils,
    ipc_lib::{IpcCreateSubnetMsg, IpcFundSubnetMsg},
    multisig::{self, create_subnet_multisig_address, multisig_threshold},
    wallet, SubnetId, NETWORK,
};
use async_trait::async_trait;
use bitcoin::{address::NetworkUnchecked, Address, BlockHash, Txid, XOnlyPublicKey};
use heed::{types::*, Database as HeedDatabase, Env, EnvOpenOptions, RwTxn};
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
    fn multisig_address(&self, subnet_id: &SubnetId) -> Address<NetworkUnchecked>;
    fn threshold(&self) -> u16;
    fn to_committee(&self, subnet_id: &SubnetId) -> SubnetCommittee;
}

impl SubnetValidators for Vec<SubnetValidator> {
    fn multisig_address(&self, subnet_id: &SubnetId) -> Address<NetworkUnchecked> {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let pubkeys = self.iter().map(|v| v.pubkey).collect::<Vec<_>>();
        // TODO remove as 16
        let threshold = multisig_threshold(pubkeys.len() as u16);
        let multisig_address =
            create_subnet_multisig_address(&secp, subnet_id, &pubkeys, threshold.into(), NETWORK)
                .expect("Multisig address should be valid");

        multisig_address.into_unchecked()
    }

    fn threshold(&self) -> u16 {
        // TODO remove as 16
        multisig_threshold(self.len() as u16)
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
    pub threshold: u16,
    /// The current list of validators, with their balances
    pub validators: Vec<SubnetValidator>,
    /// The subnet multisig address
    pub multisig_address: Address<NetworkUnchecked>,
}

impl SubnetCommittee {
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

    pub fn construct_spend_psbt(
        &self,
        rpc: &bitcoincore_rpc::Client,
        subnet_id: &SubnetId,
        to: &Address,
        amount: bitcoin::Amount,
    ) -> Result<bitcoin::Psbt, multisig::MultisigError> {
        let address = self
            .multisig_address
            .clone()
            .require_network(NETWORK)
            .expect("Multisig should be valid for saved subnet genesis info");

        let unspent = wallet::get_unspent_for_address(rpc, &address).expect("temp expect");
        let fee_rate = bitcoin_utils::get_fee_rate(&rpc, None, None);
        let public_keys = self.validators.iter().map(|v| v.pubkey).collect::<Vec<_>>();

        let psbt = multisig::construct_spend_psbt(
            to,
            amount,
            &unspent,
            &address,
            self.validators.len() as u16,
            self.threshold,
            &fee_rate,
            &subnet_id,
            &public_keys,
        )?;

        Ok(psbt)
    }
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

    /// Imports the subnet multisig address to Bitcoin Core
    /// as a watch-only address. Bitcoincore will monitor the UTXOs.
    pub fn import_current_address_to_wallet(
        &self,
        rpc: &bitcoincore_rpc::Client,
    ) -> Result<(), bitcoincore_rpc::Error> {
        let address = self.multisig_address();
        let label = format!("{}-{}", self.id, self.committee_number);
        wallet::import_address(rpc, &address, label)?;
        Ok(())
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

        create_subnet_multisig_address(
            &secp,
            &self.subnet_id,
            &self.create_subnet_msg.whitelist.clone(),
            self.create_subnet_msg.min_validators.into(),
            NETWORK,
        )
        // TODO think about this expect, maybe return a Result
        .expect("Multisig should be valid for saved subnet genesis info")
    }

    /// Imports the whitelist multisig address to Bitcoin Core
    /// as a watch-only address. Bitcoincore will monitor the UTXOs.
    pub fn import_whitelist_address_to_wallet(
        &self,
        rpc: &bitcoincore_rpc::Client,
    ) -> Result<(), bitcoincore_rpc::Error> {
        let address = self.multisig_address();
        // Import the subnet whitelist address to the wallet
        // with a committee number 0
        let label = format!("{}-{}", self.subnet_id, 0);
        wallet::import_address(rpc, &address, label)?;
        Ok(())
    }

    pub fn to_subnet(&self) -> SubnetState {
        SubnetState {
            id: self.subnet_id,
            committee_number: 1,
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
    // TODO use SerdeBincode for this as well
    // There's a conflict of bincode and `serde(tag = "type")` for RootnetMessage
    rootnet_msgs_db: HeedDatabase<Str, SerdeJson<RootnetMessage>>,
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
            debug!("Opening database in read-only mode");
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
            let rootnet_msgs_db = env
                .open_database(&rtxn, Some("rootnet_msgs_db"))?
                .ok_or(DbError::DbNotFound("rootnet_msgs_db".to_string()))?;
            rtxn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
                subnet_genesis_db,
                rootnet_msgs_db,
            })
        } else {
            // In write mode, we can create the databases if they don't exist
            let mut txn = env.write_txn()?;
            let monitor_info = env.create_database(&mut txn, Some("monitor_info"))?;
            let subnet_db = env.create_database(&mut txn, Some("subnet_db"))?;
            let subnet_genesis_db = env.create_database(&mut txn, Some("subnet_genesis_db"))?;
            let rootnet_msgs_db = env.create_database(&mut txn, Some("rootnet_msgs_db"))?;
            txn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
                subnet_genesis_db,
                rootnet_msgs_db,
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

    // Rootnet Messages
    fn get_all_rootnet_msgs(&self, subnet_id: SubnetId) -> Result<Vec<RootnetMessage>, DbError>;
    fn get_rootnet_msgs_by_height(
        &self,
        subnet_id: SubnetId,
        block_height: u64,
    ) -> Result<Vec<RootnetMessage>, DbError>;
    fn get_last_rootnet_msg_nonce(&self, subnet_id: SubnetId) -> Result<u64, DbError>;
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

    fn get_last_rootnet_msg_nonce(&self, subnet_id: SubnetId) -> Result<u64, DbError> {
        let prefix = rootnet_msgs_prefix(subnet_id);
        let txn = self.env.read_txn()?;
        let msgs_iter = self.rootnet_msgs_db.prefix_iter(&txn, &prefix)?;
        let count: u64 = msgs_iter
            .count()
            .try_into()
            .map_err(|_| DbError::TypeConversionError("max roonet messages reached".to_string()))?;
        Ok(count)
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
