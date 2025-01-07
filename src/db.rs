use crate::ipc_lib::{self, SubnetId};
use async_trait::async_trait;
use bitcoin::{address::NetworkUnchecked, Address, XOnlyPublicKey};
use heed::{types::*, Database as HeedDatabase, Env, EnvOpenOptions};
use log::{debug, error, trace};
use serde::{Deserialize, Serialize};
use std::{io, path::Path};
use thiserror::Error;

const LAST_PROCESSED_BLOCK_KEY: &str = "monitor:last_processed_block";
#[allow(dead_code)]
const SUBNET_INFO_PREFIX: &str = "subnet_info:";
const SUBNET_GENESIS_INFO_PREFIX: &str = "subnet_genesis_info:";

/// State of a validator in a subnet
#[derive(Serialize, Deserialize, Debug)]
pub struct SubnetValidator {
    /// The public key of the validator
    pub pubkey: XOnlyPublicKey,
    /// The current balance of the validator's stake
    pub balance: bitcoin::Amount,
    /// The IP address of the validator, as
    /// advertised in the subnet's join/pre-fund message
    pub ip: std::net::SocketAddr,
    /// The transaction ID of the join/pre-fund message
    pub join_txid: bitcoin::Txid,
}

/// The current state of a subnet
/// Must only exist if the subnet is bootstrapped
///
/// Note: many more fields will be added here
#[derive(Serialize, Deserialize, Debug)]
pub struct SubnetState {
    /// Duplicate of the subnet ID, for easy access
    pub subnet_id: SubnetId,
    /// The current list of validators, with their balances
    pub validators: Vec<SubnetValidator>,
    /// The subnet multisig address
    pub multisig_address: Address<NetworkUnchecked>,
}

/// Genesis info for a subnet
#[derive(Serialize, Deserialize, Debug)]
pub struct SubnetGenesisInfo {
    /// The original create subnet msg, which holds
    /// the configuration alongside the validatorsg whitelist
    ///
    /// The pre-boostrap multisig is constructed from the whitelist
    pub create_subnet_msg: ipc_lib::IpcCreateSubnetMsg,
    /// Marks if the subnet is bootstrapped
    /// The struct should never be modified after bootstrapping
    pub bootstrapped: bool,
    /// The height of the block where the create subnet
    /// message was included
    pub genesis_block_height: u64,
    /// The height of the block where the subnet was bootstrapped
    pub boostrap_block_height: Option<u64>,
    /// The list of validators that boostrapped the subnet
    /// (by pre-funding the subnet)
    pub genesis_validators: Vec<SubnetValidator>,
}

impl SubnetGenesisInfo {
    pub fn multisig_address(&self) -> Address<NetworkUnchecked> {
        self.create_subnet_msg
            .multisig_address_from_whitelist()
            .expect("Multisig should be valid for saved subnet genesis info")
            .into_unchecked()
    }
}

#[derive(Serialize, Deserialize)]
struct MonitorInfo {
    pub last_processed_block: u64,
}

pub struct HeedDb {
    env: Env,
    monitor_info: HeedDatabase<Str, SerdeBincode<MonitorInfo>>,
    #[allow(dead_code)]
    subnet_db: HeedDatabase<Str, SerdeBincode<SubnetState>>,
    subnet_genesis_db: HeedDatabase<Str, SerdeBincode<SubnetGenesisInfo>>,
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
            rtxn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
                subnet_genesis_db,
            })
        } else {
            // In write mode, we can create the databases if they don't exist
            let mut txn = env.write_txn()?;
            let monitor_info = env.create_database(&mut txn, Some("monitor_info"))?;
            let subnet_db = env.create_database(&mut txn, Some("subnet_db"))?;
            let subnet_genesis_db = env.create_database(&mut txn, Some("subnet_genesis_db"))?;
            txn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
                subnet_genesis_db,
            })
        }
    }
}

// TODO maybe split into multiple traits
#[async_trait]
pub trait Database {
    // Monitor Info
    async fn get_last_processed_block(&self) -> Result<u64, DbError>;
    async fn set_last_processed_block(&self, block: u64) -> Result<(), DbError>;

    // Genesis Info
    async fn get_subnet_genesis_info(
        &self,
        subnet_id: SubnetId,
    ) -> Result<Option<SubnetGenesisInfo>, DbError>;
    async fn save_subnet_create_msg(
        &self,
        subnet_id: SubnetId,
        block_height: u64,
        create_subnet_msg: ipc_lib::IpcCreateSubnetMsg,
    ) -> Result<(), DbError>;

    // TODO Subnet State
}

#[async_trait]
impl Database for HeedDb {
    async fn get_last_processed_block(&self) -> Result<u64, DbError> {
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

    async fn set_last_processed_block(&self, block_height: u64) -> Result<(), DbError> {
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

    async fn get_subnet_genesis_info(
        &self,
        subnet_id: SubnetId,
    ) -> Result<Option<SubnetGenesisInfo>, DbError> {
        let key = format!("{SUBNET_GENESIS_INFO_PREFIX}:{}", subnet_id);
        let txn = self.env.read_txn()?;
        let subnet = self.subnet_genesis_db.get(&txn, &key)?;
        Ok(subnet)
    }

    async fn save_subnet_create_msg(
        &self,
        subnet_id: SubnetId,
        genesis_block_height: u64,
        create_subnet_msg: ipc_lib::IpcCreateSubnetMsg,
    ) -> Result<(), DbError> {
        // TODO check network

        let subnet = SubnetGenesisInfo {
            create_subnet_msg,
            bootstrapped: false,
            genesis_block_height,
            boostrap_block_height: None,
            genesis_validators: Vec::with_capacity(0),
        };
        let key = format!("{SUBNET_GENESIS_INFO_PREFIX}:{}", subnet_id);
        debug!("key={} subnet={:#?}", key, &subnet);
        let mut txn = self.env.write_txn()?;
        self.subnet_genesis_db.put(&mut txn, &key, &subnet)?;
        txn.commit()?;
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

    #[error(transparent)]
    IoError(#[from] io::Error),

    #[error(transparent)]
    HeedError(#[from] heed::Error),

    #[error("Type conversion error: {0}")]
    TypeConversionError(String),
}
