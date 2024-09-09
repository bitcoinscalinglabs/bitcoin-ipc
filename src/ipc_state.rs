use thiserror::Error;

use bitcoin::address::NetworkChecked;
use bitcoin::{secp256k1::PublicKey, Address};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::str::FromStr;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ValidatorData {
    name: String,
    ip: String,
    pk: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IPCState {
    subnet_id: String,
    subnet_address: String,
    subnet_pk: PublicKey,
    required_number_of_validators: u64,
    required_collateral: u64,
    validators: Vec<ValidatorData>,
}

impl IPCState {
    pub fn new(
        parent_id: String,
        subnet_address: String,
        subnet_pk: PublicKey,
        required_number_of_validators: u64,
        required_collateral: u64,
    ) -> Self {
        IPCState {
            subnet_id: format!("{}/{}", parent_id, subnet_address),
            subnet_address,
            subnet_pk,
            required_number_of_validators,
            required_collateral,
            validators: Vec::new(),
        }
    }

    pub fn load_state(filepath: String) -> Result<Self, IpcStateError> {
        let mut file = File::open(filepath)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let subnet_state = serde_json::from_str(&content)?;
        Ok(subnet_state)
    }

    pub fn save_state(&self) -> Result<String, IpcStateError> {
        let json = serde_json::to_string(&self)?;

        let file_path = format!("{}/{}.json", self.subnet_id, self.subnet_address);

        let path = std::path::Path::new(&file_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&file_path)?;

        file.write_all(json.as_bytes())?;

        Ok(json)
    }

    pub fn add_validator(
        &mut self,
        ip: String,
        name: String,
        pk: String,
    ) -> Result<(), IpcStateError> {
        self.validators.push(ValidatorData {
            ip: ip.clone(),
            name: name.clone(),
            pk: pk.clone(),
        });
        self.save_state()?;
        println!("Validator {} {} {} added", ip, name, pk);
        Ok(())
    }

    pub fn has_required_validators(&self) -> bool {
        self.validators.len() as u64 >= self.required_number_of_validators
    }

    pub fn get_required_collateral(&self) -> u64 {
        self.required_collateral
    }

    pub fn get_subnet_id(&self) -> String {
        self.subnet_id.clone()
    }

    pub fn get_subnet_address_str(&self) -> String {
        self.subnet_address.clone()
    }

    pub fn get_file_path(&self) -> String {
        format!("{}/{}.json", self.subnet_id, self.subnet_address)
    }

    pub fn get_subnet_pk(&self) -> PublicKey {
        self.subnet_pk
    }

    pub fn get_subnet_address(&self) -> Result<Address<NetworkChecked>, IpcStateError> {
        match Address::from_str(&self.subnet_address) {
            Ok(address) => Ok(address.assume_checked()),
            Err(_) => Err(IpcStateError::InvalidSubnetPK),
        }
    }

    pub fn load_all() -> Result<Vec<Self>, IpcStateError> {
        let mut ipc_states = Vec::new();
        let btc_dir = Path::new(crate::L1_NAME);

        if btc_dir.is_dir() {
            for entry in fs::read_dir(btc_dir)? {
                let entry = entry?;
                let path = entry.path();

                if path.is_dir() {
                    for sub_entry in fs::read_dir(path)? {
                        let sub_entry = sub_entry?;
                        let sub_path = sub_entry.path();

                        if sub_path.extension().and_then(|s| s.to_str()) == Some("json") {
                            let file = File::open(&sub_path)?;
                            let ipc_state: IPCState = serde_json::from_reader(file)?;
                            ipc_states.push(ipc_state);
                        }
                    }
                }
            }
        }

        Ok(ipc_states)
    }

    pub fn print_state(&mut self) {
        println!("#################################");
        // print in a more organized manner:
        println!("Subnet: {}", self.subnet_id);
        println!("File path: {}", self.get_file_path());
        println!("Subnet Address: {}", self.subnet_address.clone());
        println!(
            "Required number of validators: {}",
            self.required_number_of_validators
        );
        println!("Required collateral: {}", self.required_collateral);
        println!("Validators:");
        for validator in &self.validators {
            println!(
                "  IP: {}, name: {}, pk: {}",
                validator.ip, validator.name, validator.pk
            );
        }
    }
}

#[derive(Error, Debug)]
pub enum IpcStateError {
    #[error("invalid subnet PK")]
    InvalidSubnetPK,

    #[error("cannot open or read file")]
    IoError(#[from] std::io::Error),

    #[error("cannot open or read file")]
    JsonError(#[from] serde_json::Error),
}
