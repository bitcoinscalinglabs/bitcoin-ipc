use hex::encode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use tiny_keccak::{Hasher, Keccak};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Account {
    balance: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ValidatorData {
    pub ip: String,
    pub collateral: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IPCSubnetState {
    name: String,
    subnet_pk: String,
    pub file_path: String,
    url: String,
    validators: Vec<ValidatorData>,
    accounts: HashMap<String, Account>,
    child_subnets: Vec<String>,
}

impl IPCSubnetState {
    pub fn new(name: String, url: String, subnet_pk: String) -> Self {
        IPCSubnetState {
            name: name.clone(),
            subnet_pk,
            file_path: format!("{}/{}.json", url, name),
            url,
            validators: Vec::new(),
            accounts: HashMap::new(),
            child_subnets: Vec::new(),
        }
    }

    pub fn get_parent(self) -> Self {
        let mut parent_subnet: Option<Self> = None;

        if self.url.eq("BTC") {
            return self;
        }

        let url = self.url.clone();
        let parent_url_length = url.split('/').count() - 1;
        let parent_name = url
            .split("/")
            .nth(parent_url_length - 1)
            .unwrap_or_default();

        let mut parent_file_name = url
            .split('/')
            .take(parent_url_length)
            .collect::<Vec<&str>>()
            .join("/");

        parent_file_name.push_str("/");
        parent_file_name.push_str(parent_name);
        parent_file_name.push_str(".json");

        println!("Parent file name: {}", parent_file_name);

        if let Ok(mut file) = File::open(parent_file_name) {
            let mut json = String::new();
            file.read_to_string(&mut json)
                .expect("Failed to read parent file");
            parent_subnet = serde_json::from_str(&json).expect("Failed to deserialize parent");
        }
        parent_subnet.clone().expect("Failed to load parent")
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

    pub fn create_account(&mut self, address: &str, initial_balance: u64) {
        if self.accounts.contains_key(address) {
            println!("Account {} already exists", address);
            return;
        }

        self.accounts.insert(
            address.to_string(),
            Account {
                balance: initial_balance,
            },
        );
        self.save_state();
        println!(
            "Account {} created with balance {}",
            address, initial_balance
        );
    }

    pub fn transfer(&mut self, from: &str, to: &str, amount: u64) -> Result<(), String> {
        let from_account = self
            .accounts
            .get_mut(from)
            .ok_or("From account not found")?;
        if from_account.balance < amount {
            return Err("Insufficient balance".to_string());
        }

        from_account.balance -= amount;

        let to_account = self
            .accounts
            .entry(to.to_string())
            .or_insert(Account { balance: 0 });

        to_account.balance += amount;

        self.save_state();
        println!("Transfer successful");
        Ok(())
    }

    pub fn get_checkpoint(&mut self) -> String {
        println!("Checkpointing state...");

        let json = self.save_state();

        let mut keccak = Keccak::v256();
        keccak.update(json.as_bytes());
        let mut hash = [0u8; 32];
        keccak.finalize(&mut hash);
        encode(hash)
    }

    pub fn add_child_subnet(&mut self, child_subnet_name: &str) {
        self.child_subnets.push(child_subnet_name.to_string());
        self.save_state();

        println!("Child subnet {} added", child_subnet_name);
    }

    pub fn add_validator(&mut self, ip: String, collateral: u64) {
        self.validators.push(ValidatorData {
            ip: ip.clone(),
            collateral,
        });
        self.save_state();
        println!("Validator {} added", ip);
    }

    pub fn print_state(&mut self) {
        println!("#################################");
        // print in a more organized manner:
        println!("Subnet: {}", self.name);
        println!("URL: {}", self.url);
        println!("Subnet PK: {}", self.subnet_pk);
        println!("Validators:");
        for validator in &self.validators {
            println!(
                "  IP: {}, collateral: {}",
                validator.ip, validator.collateral
            );
        }

        println!("Accounts:");
        for (address, account) in &self.accounts {
            println!("  {}: {}", address, account.balance);
        }

        println!("Child subnets:");
        for child_subnet in &self.child_subnets {
            println!("  {}", child_subnet);
        }

        println!("Checkpoint: {}", self.get_checkpoint());
        println!();
    }
}
