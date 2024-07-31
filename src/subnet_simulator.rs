use crate::bitcoin_utils;
use bitcoin::sighash::SighashCache;
use bitcoin::{EcdsaSighashType, Transaction, TxOut};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::{collections::HashMap, fs::File};

use bitcoin::secp256k1::{Message, Secp256k1};

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
    keypair: bitcoin::secp256k1::Keypair,
}

impl SubnetSimulator {
    pub fn new(subnet_name: &str) -> Self {
        println!("Starting simulator for subnet {subnet_name}.");

        if let Ok(mut file) = File::open(format!("{}/{}/keypair.yaml", crate::L1_NAME, subnet_name))
        {
            let mut json = String::new();
            file.read_to_string(&mut json)
                .expect("Failed to read state file");
            if let Ok(keypair) = serde_json::from_str(&json) {
                return SubnetSimulator {
                    subnet_name: String::from(subnet_name),
                    state: SubnetState::new(),
                    keypair,
                };
            }
        }

        return SubnetSimulator {
            subnet_name: String::from(subnet_name),
            state: SubnetState::new(),
            keypair: bitcoin_utils::generate_keypair(subnet_name.to_string()),
        };
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

        bitcoin_utils::hash(json)
    }

    /// This function signs a transaction with the keypair of the subnet a.k.a. subnetPK
    /// # Arguments
    ///
    /// * `tx` - The transaction to sign
    /// * `prevouts` - The txouts referenced by the inputs of the transaction
    ///
    /// # Returns
    ///
    /// * A signed transaction
    pub fn sign_transaction(&self, mut tx: Transaction, prevouts: Vec<TxOut>) -> Transaction {
        let secp = Secp256k1::new();
        let mut sighash_cache = SighashCache::new(&tx);

        let signatures: Vec<Vec<u8>> = tx
            .input
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let prevout = &prevouts[i];

                let sighash = sighash_cache.p2wpkh_signature_hash(
                    i,
                    &prevout.script_pubkey,
                    prevout.value,
                    EcdsaSighashType::All,
                );

                match sighash {
                    Ok(sighash) => println!("Sighash: {:?}", sighash),
                    Err(e) => {
                        println!("Failed to compute sighash: {}", e);
                        return vec![];
                    }
                }

                let message = Message::from_digest_slice(&sighash.unwrap()[..]).unwrap();
                let sig = secp.sign_ecdsa(&message, &self.keypair.secret_key());
                let mut sig_vec = sig.serialize_der().to_vec();
                sig_vec.push(EcdsaSighashType::All as u8);

                sig_vec
            })
            .collect();

        for (i, input) in tx.input.iter_mut().enumerate() {
            input.witness.push(signatures[i].clone());
            input.witness.push(self.keypair.public_key().serialize());
            println!("Signed input {}", i);
        }

        tx
    }

    pub fn get_public_key(&self) -> bitcoin::secp256k1::PublicKey {
        self.keypair.public_key()
    }

    pub fn print_state(&mut self) {
        println!("#################################");
        // print in a more organized manner:
        println!("Subnet: {}", self.subnet_name);
        println!("Subnet PK: {}", self.get_public_key());
        println!("Accounts:");
        for (address, account) in &self.state.accounts {
            println!("  {}: {}", address, account.balance);
        }

        println!("Checkpoint: {}", self.get_checkpoint());
        println!();
    }
}
