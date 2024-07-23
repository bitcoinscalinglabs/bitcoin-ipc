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

    pub fn create_account(&mut self, address: &str) {
        if self.accounts.contains_key(address) {
            println!("Account {} already exists", address);
            return;
        }

        self.accounts
            .insert(address.to_string(), Account { balance: 0 });

        println!("Account {}", address);
    }

    pub fn fund_account(&mut self, address: &str, amount: u64) {
        let account = self.accounts.get_mut(address).expect("Account not found");

        account.balance += amount;

        println!("Account {} funded", address);
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

        println!("Transfer successful");
        Ok(())
    }

    pub fn get_checkpoint(&mut self) -> String {
        println!("Checkpointing state...");

        let json = serde_json::to_string(&self.accounts).expect("Failed to serialize state");

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
        for (address, account) in &self.accounts {
            println!("  {}: {}", address, account.balance);
        }

        println!("Checkpoint: {}", self.get_checkpoint());
        println!();
    }
}
