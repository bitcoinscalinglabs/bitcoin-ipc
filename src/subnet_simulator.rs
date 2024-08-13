use hex::encode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tiny_keccak::{Hasher, Keccak};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Account {
    balance: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubnetState {
    accounts: HashMap<String, Account>,
}

impl SubnetState {
    pub fn new() -> Self {
        SubnetState {
            accounts: HashMap::new(),
        }
    }
}

pub struct SubnetSimulator {
    pub subnet_name: String,
    state: SubnetState,
}

impl SubnetSimulator {
    pub fn new(subnet_name: &str) -> Self {
        println!("Starting simulator for subnet {subnet_name}.");
        SubnetSimulator {
            subnet_name: String::from(subnet_name),
            state: SubnetState::new(),
        }
    }

    pub fn create_account(&mut self, address: &str) {
        if self.state.accounts.contains_key(address) {
            println!("Account {} already exists", address);
            return;
        }

        self.state
            .accounts
            .insert(address.to_string(), Account { balance: 0 });

        println!("Account {}", address);
    }

    pub fn fund_account(&mut self, address: &str, amount: u64) {
        let account = self
            .state
            .accounts
            .get_mut(address)
            .expect("Account not found");

        account.balance += amount;

        println!("Account {} funded", address);
    }

    pub fn transfer(&mut self, from: &str, to: &str, amount: u64) -> Result<(), String> {
        let from_account = self
            .state
            .accounts
            .get_mut(from)
            .ok_or("From account not found")?;
        if from_account.balance < amount {
            return Err("Insufficient balance".to_string());
        }

        from_account.balance -= amount;

        let to_account = self
            .state
            .accounts
            .entry(to.to_string())
            .or_insert(Account { balance: 0 });

        to_account.balance += amount;

        println!("Transfer successful");
        Ok(())
    }

    pub fn get_checkpoint(&mut self) -> String {
        println!("Computing state checkpoint...");

        // Disclaimer: this is not secure. It has not checked whether the serialization method and the HashMap
        // implementations avoid collisions.
        let json = serde_json::to_string(&self.state.accounts).expect("Failed to serialize state");

        let mut keccak = Keccak::v256();
        keccak.update(json.as_bytes());
        let mut hash = [0u8; 32];
        keccak.finalize(&mut hash);
        encode(hash)
    }

    pub fn print_state(&mut self) {
        println!("#################################");
        // print in a more organized manner:
        println!("Accounts:");
        for (address, account) in &self.state.accounts {
            println!("  {}: {}", address, account.balance);
        }

        println!("Checkpoint: {}", self.get_checkpoint());
        println!();
    }
}
