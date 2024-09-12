use crate::bitcoin_utils;

use bitcoin::key::{TapTweak, TweakedKeypair};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::{TapSighashType, Transaction, TxOut};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::File};

use bitcoin::secp256k1::{Message, Secp256k1};
use bitcoin::XOnlyPublicKey;
use std::io::{Read, Write};
use std::path::Path;

use thiserror::Error;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Account {
    balance: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubnetState {
    accounts: BTreeMap<String, Account>,
}

impl SubnetState {
    pub fn new() -> Self {
        SubnetState {
            accounts: BTreeMap::new(),
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

        let state_file_path = &format!("{}/subnet_state.json", subnet_id);

        if !Path::new(state_file_path).exists() {
            let json = serde_json::to_string(&SubnetState::new())?;

            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(state_file_path)?;

            file.write_all(json.as_bytes())?;
        }

        if let Ok(mut file) = File::open(format!("{}/keypair.yaml", subnet_id)) {
            let mut json = String::new();
            file.read_to_string(&mut json)?;

            let state = match SubnetSimulator::load_state(subnet_id) {
                Ok(st) => st,
                Err(_) => SubnetState::new(),
            };

            if let Ok(keypair) = serde_json::from_str(&json) {
                return Ok(SubnetSimulator {
                    subnet_id: String::from(subnet_id),
                    state,
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

    pub fn create_account(&mut self, address: &String) -> Result<(), SubnetStateError> {
        self.state = SubnetSimulator::load_state(&self.subnet_id)?;

        if self.state.accounts.contains_key(address) {
            return Err(SubnetStateError::AccountAlreadyExists);
        }

        self.state
            .accounts
            .insert(address.to_string(), Account { balance: 0 });

        self.save_state()?;

        println!("Account {} created", address);

        Ok(())
    }

    pub fn fund_account(&mut self, address: &String, amount: u64) -> Result<(), SubnetStateError> {
        self.state = SubnetSimulator::load_state(&self.subnet_id)?;

        if !self.state.accounts.contains_key(address) {
            match self.create_account(address) {
                Ok(_) => {}
                Err(_) => {
                    return Err(SubnetStateError::CannotCreateAccount);
                }
            }
        }

        let account = match self.state.accounts.get_mut(address) {
            Some(a) => a,
            None => {
                return Err(SubnetStateError::AccountNotFound);
            }
        };

        account.balance += amount;

        self.save_state()?;

        println!("Account {} funded", address);

        Ok(())
    }

    pub fn transfer(
        &mut self,
        from: &String,
        to: &String,
        amount: u64,
    ) -> Result<(), SubnetStateError> {
        self.state = SubnetSimulator::load_state(&self.subnet_id)?;

        let from_account = match self.state.accounts.get_mut(from) {
            Some(a) => a,
            None => {
                return Err(SubnetStateError::AccountNotFound);
            }
        };

        if from_account.balance < amount {
            return Err(SubnetStateError::InsufficientFunds);
        }

        from_account.balance -= amount;

        // TODO: update logic when address is from another subnet.
        let to_account = self
            .state
            .accounts
            .entry(to.to_string())
            .or_insert(Account { balance: 0 });

        to_account.balance += amount;

        self.save_state()?;

        println!("Transfer successful");
        Ok(())
    }

    pub fn get_checkpoint(&mut self) -> Result<[u8; 32], SubnetStateError> {
        println!("Computing state checkpoint...");
        self.state = SubnetSimulator::load_state(&self.subnet_id)?;

        // Disclaimer: this is not secure. It has not checked whether the serialization method and the BTreeMap
        // implementations avoid collisions.
        let json = serde_json::to_string(&self.state.accounts).expect("Failed to serialize state");

        Ok(bitcoin_utils::hash(json))
    }

    pub fn load_state(subnet_id: &str) -> Result<SubnetState, SubnetStateError> {
        let mut file = File::open(format!("{}/subnet_state.json", subnet_id))?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let subnet_state = serde_json::from_str(&content)?;
        Ok(subnet_state)
    }

    pub fn save_state(&self) -> Result<String, SubnetStateError> {
        let json = serde_json::to_string(&self.state)?;

        let path = std::path::Path::new(&self.subnet_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(format!("{}/subnet_state.json", self.subnet_id))?;

        file.write_all(json.as_bytes())?;

        Ok(json)
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
        let subnet_address = bitcoin_utils::get_address_from_x_only_public_key(
            XOnlyPublicKey::from(self.get_public_key()),
            crate::NETWORK,
        );
        println!("Subnet Address: {}", subnet_address);
        println!("Accounts:");
        for (address, account) in &self.state.accounts {
            println!("  {}: {}", address, account.balance);
        }

        let checkpoint = match self.get_checkpoint() {
            Ok(cp) => cp,
            Err(_) => {
                println!("Failed to get checkpoint");
                return;
            }
        };
        let str_cp = hex::encode(checkpoint);

        println!("Checkpoint: {}", str_cp);
        println!();
    }
}

#[derive(Error, Debug)]
pub enum SubnetSimulatorError {
    #[error("account not found")]
    BitcoinUtilsError(#[from] crate::bitcoin_utils::BitcoinUtilsError),

    #[error("error when reading the keypair file")]
    IoError(#[from] std::io::Error),

    #[error("error when reading the file")]
    JsonError(#[from] serde_json::Error),

    #[error("error while funding account")]
    CannotFundAccount,
}

#[derive(Error, Debug)]
pub enum SubnetStateError {
    #[error("invalid subnet PK")]
    InvalidSubnetPK,

    #[error("cannot open or read file")]
    IoError(#[from] std::io::Error),

    #[error("cannot open or read file")]
    JsonError(#[from] serde_json::Error),

    #[error("account not found")]
    AccountNotFound,

    #[error("insufficient funds")]
    InsufficientFunds,

    #[error("account already exists")]
    AccountAlreadyExists,

    #[error("cannot create account")]
    CannotCreateAccount,
}
