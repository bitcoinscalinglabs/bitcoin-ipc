use crate::bitcoin_utils;

use bitcoin::key::{TapTweak, TweakedKeypair};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::{TapSighashType, Transaction, TxOut};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::{collections::HashMap, fs::File};

use bitcoin::secp256k1::{Message, Secp256k1};

use thiserror::Error;

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

impl Default for SubnetState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SubnetSimulator {
    pub subnet_id: String,
    state: SubnetState,
    keypair: bitcoin::secp256k1::Keypair,
}

impl SubnetSimulator {
    pub fn new(subnet_id: &str) -> Result<Self, SubnetSimulatorError> {
        println!("Starting simulator for subnet {subnet_id}.");

        if let Ok(mut file) = File::open(format!("{}/keypair.yaml", subnet_id)) {
            let mut json = String::new();
            file.read_to_string(&mut json)?;

            if let Ok(keypair) = serde_json::from_str(&json) {
                return Ok(SubnetSimulator {
                    subnet_id: String::from(subnet_id),
                    state: SubnetState::new(),
                    keypair,
                });
            }
        }

        Ok(SubnetSimulator {
            subnet_id: String::from(subnet_id),
            state: SubnetState::new(),
            keypair: bitcoin_utils::generate_keypair(subnet_id.to_string())?,
        })
    }

    pub fn create_account(&mut self, address: &String) {
        if self.state.accounts.contains_key(address) {
            println!("Account {} already exists", address);
            return;
        }

        self.state
            .accounts
            .insert(address.to_string(), Account { balance: 0 });

        println!("Account {}", address);
    }

    pub fn fund_account(&mut self, address: &String, amount: u64) {
        let account = self
            .state
            .accounts
            .get_mut(address)
            .expect("Account not found");

        account.balance += amount;

        println!("Account {} funded", address);
    }

    pub fn transfer(&mut self, from: &String, to: &String, amount: u64) -> Result<(), String> {
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

    pub fn get_checkpoint(&mut self) -> [u8; 32] {
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
        let signatures: Vec<Vec<u8>> = tx
            .input
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let secp = Secp256k1::new();
                let mut sighash_cache = SighashCache::new(&tx);

                let sighash = sighash_cache
                    .taproot_key_spend_signature_hash(
                        i,
                        &Prevouts::All(&prevouts),
                        TapSighashType::Default,
                    )
                    .expect("failed to construct sighash");

                // Sign the sighash using the secp256k1 library
                let tweaked_keypair: TweakedKeypair = self.keypair.tap_tweak(&secp, None);
                let msg = Message::from_digest_slice(&sighash[..]).expect("32 bytes");

                let signature = secp.sign_schnorr(&msg, &tweaked_keypair.to_inner());

                bitcoin::taproot::Signature {
                    signature,
                    sighash_type: TapSighashType::Default,
                }
                .to_vec()
            })
            .collect();

        for (i, input) in tx.input.iter_mut().enumerate() {
            input.witness.push(signatures[i].clone());
            println!("Signed input {}", i);
        }

        tx
    }

    pub fn get_public_key(&self) -> bitcoin::secp256k1::PublicKey {
        self.keypair.public_key()
    }

    pub fn get_keypair(&self) -> bitcoin::secp256k1::Keypair {
        self.keypair
    }

    pub fn print_state(&mut self) {
        println!("#################################");
        // print in a more organized manner:
        println!("Subnet: {}", self.subnet_id);
        println!("Subnet PK: {}", self.get_public_key());
        let subnet_address =
            bitcoin_utils::get_address_from_private_key(self.keypair.secret_key(), crate::NETWORK);
        println!("Subnet Address: {}", subnet_address);
        println!("Accounts:");
        for (address, account) in &self.state.accounts {
            println!("  {}: {}", address, account.balance);
        }

        let checkpoint = self.get_checkpoint();
        let str_cp = hex::encode(checkpoint);

        println!("Checkpoint: {}", str_cp);
        println!();
    }
}

#[derive(Error, Debug)]
pub enum SubnetSimulatorError {
    #[error("account not found")]
    BitcoinUtilsError(#[from] crate::bitcoin_utils::BitcoinUtilsError),

    #[error("Error reading address")]
    ErrorReadingAddress,

    #[error("error when reading the keypair file")]
    IoError(#[from] std::io::Error),
}
