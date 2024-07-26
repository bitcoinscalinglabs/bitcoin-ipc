use bitcoin::address::NetworkChecked;
use bitcoin::Address;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::str::FromStr;

use crate::bitcoin_utils;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ValidatorData {
    pub name: String,
    pub ip: String,
    pub pk: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IPCState {
    name: String,
    subnet_pk: String,
    pub file_path: String,
    url: String,
    required_number_of_validators: u64,
    required_collateral: u64,
    validators: Vec<ValidatorData>,
}

impl IPCState {
    pub fn new(
        name: String,
        url: String,
        subnet_pk: String,
        required_number_of_validators: u64,
        required_collateral: u64,
    ) -> Self {
        IPCState {
            name: name.clone(),
            subnet_pk,
            file_path: format!("{}/{}.json", url, name),
            url,
            required_number_of_validators,
            required_collateral,
            validators: Vec::new(),
        }
    }

    pub fn load_state(file_path: String) -> Result<Self, Box<dyn std::error::Error>> {
        let mut subnet_state: Option<Self> = None;
        if let Ok(mut file) = File::open(file_path) {
            let mut json = String::new();
            file.read_to_string(&mut json)
                .expect("Failed to read state file");
            subnet_state = serde_json::from_str(&json).expect("Failed to deserialize state");
        }
        Ok(subnet_state.clone().ok_or_else(|| "Failed to load state")?)
    }

    pub fn save_state(&self) -> String {
        let json = serde_json::to_string(&self).expect("Failed to serialize state");

        let path = std::path::Path::new(&self.file_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("Failed to create directories");
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.file_path)
            .expect("Failed to open state file");

        file.write_all(json.as_bytes())
            .expect("Failed to write state to file");

        return json;
    }

    pub fn add_validator(&mut self, ip: String, name: String, pk: String) {
        self.validators.push(ValidatorData {
            ip: ip.clone(),
            name: name.clone(),
            pk: pk.clone(),
        });
        self.save_state();
        println!("Validator {} {} {} added", ip, name, pk);
    }

    pub fn has_required_validators(&self) -> bool {
        self.validators.len() as u64 >= self.required_number_of_validators
    }

    pub fn get_required_collateral(&self) -> u64 {
        self.required_collateral
    }

    pub fn get_url(&self) -> String {
        self.url.clone()
    }

    pub fn get_name(&self) -> String {
        self.name.clone()
    }

    pub fn get_subnet_address(&self) -> Address<NetworkChecked> {
        let pubkey =
            bitcoin::secp256k1::PublicKey::from_str(&self.subnet_pk).expect("Invalid public key");
        bitcoin_utils::get_address_from_public_key(pubkey, crate::NETWORK)
    }

    pub fn load_all() -> Result<Vec<Self>, Box<dyn std::error::Error>> {
        let mut ipc_states = Vec::new();
        let btc_dir = Path::new("BTC");

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
        println!("Subnet: {}", self.name);
        println!("URL: {}", self.url);
        println!("Subnet PK: {}", self.subnet_pk.clone());
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
