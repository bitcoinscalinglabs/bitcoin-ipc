use crate::ipc_lib::{self, SubnetId};
use async_trait::async_trait;
use bitcoin::{address::NetworkUnchecked, Address};
use heed::{types::*, Database as HeedDatabase, Env, EnvOpenOptions};
use log::{debug, error, trace};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

const LAST_PROCESSED_BLOCK_KEY: &str = "monitor:last_processed_block";

// Temporary struct until the DB structure is better defined
#[derive(Serialize, Deserialize, Debug)]
pub struct Subnet {
    genesis_block_height: u64,
    subnet_id: SubnetId,
    multisig_address: Address<NetworkUnchecked>,
    create_subnet_msg: ipc_lib::IpcCreateSubnetMsg,
}

#[derive(Serialize, Deserialize)]
struct MonitorInfo {
    last_processed_block: u64,
}

pub struct Db {
    env: Env,
    monitor_info: HeedDatabase<Str, SerdeBincode<MonitorInfo>>,
    subnet_db: HeedDatabase<Str, SerdeBincode<Subnet>>,
}

impl Db {
    pub async fn new(database_path: &str) -> Result<Self, DbError> {
        let database_path = Path::new(&database_path);

        if !database_path.exists() {
            debug!(
                "Database directory does not exist, creating: {}",
                database_path.display()
            );

            // Ensure the directory exists
            std::fs::create_dir_all(database_path).map_err(|e| {
                error!("Error creating database directory: {}", e);
                DbError::HeedError(heed::Error::Io(e))
            })?;
        }

        let env = unsafe { EnvOpenOptions::new().max_dbs(10).open(database_path)? };
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
    async fn get_subnet_create_msg(&self, subnet_id: &str) -> Result<Option<Subnet>, DbError>;
}

#[async_trait]
impl Database for Db {
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
        let subnet = Subnet {
            genesis_block_height,
            subnet_id,
            multisig_address,
            create_subnet_msg,
        };
        let key = format!("create_msg:{}", subnet_id);
        let mut txn = self.env.write_txn()?;
        self.subnet_db.put(&mut txn, &key, &subnet)?;
        txn.commit()?;
        Ok(())
    }

    async fn get_subnet_create_msg(&self, subnet_id: &str) -> Result<Option<Subnet>, DbError> {
        let key = format!("create_msg:{}", subnet_id);
        let txn = self.env.read_txn()?;
        let subnet = self.subnet_db.get(&txn, &key)?;
        Ok(subnet)
    }
}

#[derive(Error, Debug)]
pub enum DbError {
    #[error(transparent)]
    HeedError(#[from] heed::Error),

    #[error("Type conversion error: {0}")]
    TypeConversionError(String),
}
