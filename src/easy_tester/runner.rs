use std::collections::HashMap;

use bitcoin::{Amount, BlockHash, Txid};
use log::info;
use tempfile::TempDir;

use crate::{
    db::{BitcoinIpcDatabase, HeedDb},
    easy_tester::{
        error::EasyTesterError,
        model::{
            build_create_subnet_msg, create_rand_blockhash, create_rand_txid, ParsedTestFile,
            ScenarioCommand,
        },
    },
    eth_utils,
    ipc_lib::{IpcCreateSubnetMsg, IpcJoinSubnetMsg, IpcValidate},
    SubnetId,
};

pub struct DbTester {
    _temp_dir: TempDir,
    db: HeedDb,
    setup: ParsedTestFile,
    current_block: Option<u64>,
    block_hashes: HashMap<u64, BlockHash>,
    created_subnets: HashMap<String, SubnetId>,
}

impl DbTester {
    pub async fn new(parsed: ParsedTestFile) -> Result<Self, EasyTesterError> {
        eth_utils::set_fvm_network();

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
            setup: parsed,
            current_block: None,
            block_hashes: HashMap::new(),
            created_subnets: HashMap::new(),
        })
    }

    pub fn db(&self) -> &HeedDb {
        &self.db
    }

    pub fn block_hash(&mut self, height: u64) -> BlockHash {
        *self
            .block_hashes
            .entry(height)
            .or_insert_with(create_rand_blockhash)
    }

    pub fn run(&mut self) -> Result<(), EasyTesterError> {
        for cmd in self.setup.scenario.clone() {
            self.exec(cmd)?;
        }
        Ok(())
    }

    fn require_block(&self) -> Result<u64, EasyTesterError> {
        self.current_block.ok_or_else(|| {
            EasyTesterError::runtime("scenario error: must set 'block <height>' before actions")
        })
    }

    fn exec(&mut self, cmd: ScenarioCommand) -> Result<(), EasyTesterError> {
        match cmd {
            ScenarioCommand::Block { height } => {
                self.current_block = Some(height);
                self.block_hash(height);
                info!("Set current block height to {}", height);
                Ok(())
            }
            ScenarioCommand::Create { subnet_name } => {
                let height = self.require_block()?;
                let spec = self
                    .setup
                    .setup
                    .subnets
                    .get(&subnet_name)
                    .ok_or_else(|| {
                        EasyTesterError::runtime(format!(
                            "internal error: subnet '{subnet_name}' missing from parsed setup"
                        ))
                    })?
                    .clone();

                if self.created_subnets.contains_key(&subnet_name) {
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
                    .insert(subnet_name.clone(), genesis_info.subnet_id);

                info!(
                    "Created subnet '{}' with subnet_id={}",
                    subnet_name, genesis_info.subnet_id
                );
                Ok(())
            }
            ScenarioCommand::Join {
                subnet_name,
                validator_name,
                collateral_sats,
            } => {
                let height = self.require_block()?;
                let block_hash = self.block_hash(height);

                let subnet_id = *self.created_subnets.get(&subnet_name).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "internal error: subnet '{subnet_name}' not found in created subnets"
                    ))
                })?;

                let validator = self
                    .setup
                    .setup
                    .validators
                    .get(&validator_name)
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
        }
    }
}

