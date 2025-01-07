use crate::ipc_lib::{self, SubnetId};
use async_trait::async_trait;
use bitcoin::{address::NetworkUnchecked, Address};
use heed::{types::*, Database as HeedDatabase, Env, EnvOpenOptions};
use log::{debug, error, trace};
use serde::{Deserialize, Serialize};
use std::{io, path::Path};
use thiserror::Error;

const LAST_PROCESSED_BLOCK_KEY: &str = "monitor:last_processed_block";
const SUBNET_INFO_PREFIX: &str = "subnet_info:";

// Temporary struct until the DB structure is better defined
#[derive(Serialize, Deserialize, Debug)]
pub struct Subnet {
    pub genesis_block_height: u64,
    pub subnet_id: SubnetId,
    pub multisig_address: Address<NetworkUnchecked>,
    pub create_subnet_msg: ipc_lib::IpcCreateSubnetMsg,
}

#[derive(Serialize, Deserialize)]
struct MonitorInfo {
    pub last_processed_block: u64,
}

pub struct HeedDb {
    env: Env,
    monitor_info: HeedDatabase<Str, SerdeBincode<MonitorInfo>>,
    subnet_db: HeedDatabase<Str, SerdeBincode<Subnet>>,
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
            rtxn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
            })
        } else {
            // In write mode, we can create the databases if they don't exist
            let mut txn = env.write_txn()?;
            let monitor_info = env.create_database(&mut txn, Some("monitor_info"))?;
            let subnet_db = env.create_database(&mut txn, Some("subnet_db"))?;
            txn.commit()?;

            Ok(Self {
                env,
                monitor_info,
                subnet_db,
            })
        }
    }
}

#[async_trait]
pub trait Database {
    async fn get_last_processed_block(&self) -> Result<u64, DbError>;
    async fn set_last_processed_block(&self, block: u64) -> Result<(), DbError>;
    async fn save_subnet_create_msg(
        &self,
        subnet_id: SubnetId,
        block_height: u64,
        multisig_address: bitcoin::Address<NetworkUnchecked>,
        create_subnet_msg: ipc_lib::IpcCreateSubnetMsg,
    ) -> Result<(), DbError>;
    async fn get_subnet_info(&self, subnet_id: SubnetId) -> Result<Option<Subnet>, DbError>;
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

    async fn save_subnet_create_msg(
        &self,
        subnet_id: SubnetId,
        genesis_block_height: u64,
        multisig_address: bitcoin::Address<NetworkUnchecked>,
        create_subnet_msg: ipc_lib::IpcCreateSubnetMsg,
    ) -> Result<(), DbError> {
        // TODO check network

        let subnet = Subnet {
            genesis_block_height,
            subnet_id,
            multisig_address,
            create_subnet_msg,
        };
        let key = format!("{SUBNET_INFO_PREFIX}:{}", subnet_id);
        debug!("key={} subnet={:#?}", key, &subnet);
        let mut txn = self.env.write_txn()?;
        self.subnet_db.put(&mut txn, &key, &subnet)?;
        txn.commit()?;
        Ok(())
    }

    async fn get_subnet_info(&self, subnet_id: SubnetId) -> Result<Option<Subnet>, DbError> {
        let key = format!("{SUBNET_INFO_PREFIX}:{}", subnet_id);
        let txn = self.env.read_txn()?;
        let subnet = self.subnet_db.get(&txn, &key)?;
        Ok(subnet)
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
