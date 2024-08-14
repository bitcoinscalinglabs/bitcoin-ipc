use crate::bitcoin_utils;

use bitcoin::key::{TapTweak, TweakedKeypair};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::{TapSighashType, Transaction, TxOut};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use bitcoin::secp256k1::{Message, Secp256k1};

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

pub struct SubnetSimulator {
    pub subnet_name: String,
    state: SubnetState,
    keypair: bitcoin::secp256k1::Keypair,
}

impl SubnetSimulator {
    pub fn new(subnet_name: &str) -> Result<Self, SubnetSimulatorError> {
        println!("Starting simulator for subnet {subnet_name}.");

        let state_file_path = &format!("{}/{}/subnet_state.json", crate::L1_NAME, subnet_name);

        if !Path::new(state_file_path).exists() {
            let json = serde_json::to_string(&SubnetState::new())?;

            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(state_file_path)?;

            file.write_all(json.as_bytes())?;
        }

        if let Ok(mut file) = File::open(format!("{}/{}/keypair.json", crate::L1_NAME, subnet_name))
        {
            let mut json = String::new();
            file.read_to_string(&mut json)?;

            let keypair = match serde_json::from_str(&json) {
                Ok(kp) => kp,
                Err(_) => bitcoin_utils::generate_keypair(subnet_name.to_string())?,
            };
            let state = match SubnetSimulator::load_state(subnet_name) {
                Ok(st) => st,
                Err(_) => SubnetState::new(),
            };

            return Ok(SubnetSimulator {
                subnet_name: String::from(subnet_name),
                state,
                keypair,
            });
        }

        return Ok(SubnetSimulator {
            subnet_name: String::from(subnet_name),
            state: SubnetState::new(),
            keypair: bitcoin_utils::generate_keypair(subnet_name.to_string())?,
        });
    }
    pub fn create_account(&mut self, address: &String) -> Result<(), SubnetStateError> {
        self.state = SubnetSimulator::load_state(&self.subnet_name)?;

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
        self.state = SubnetSimulator::load_state(&self.subnet_name)?;

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
        self.state = SubnetSimulator::load_state(&self.subnet_name)?;

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
        self.state = SubnetSimulator::load_state(&self.subnet_name)?;

        // Disclaimer: this is not secure. It has not checked whether the serialization method and the BTreeMap
        // implementations avoid collisions.
        let json = serde_json::to_string(&self.state.accounts).expect("Failed to serialize state");

        Ok(bitcoin_utils::hash(json))
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

    pub fn load_state(subnet_name: &str) -> Result<SubnetState, SubnetStateError> {
        let mut file = File::open(format!(
            "{}/{}/subnet_state.json",
            crate::L1_NAME,
            subnet_name
        ))?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let subnet_state = serde_json::from_str(&content)?;
        Ok(subnet_state)
    }

    pub fn save_state(&self) -> Result<String, SubnetStateError> {
        let json = serde_json::to_string(&self.state)?;

        let path = std::path::Path::new(&self.subnet_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(format!(
                "{}/{}/subnet_state.json",
                crate::L1_NAME,
                self.subnet_name
            ))?;

        file.write_all(json.as_bytes())?;

        Ok(json)
    }

    pub fn print_state(&mut self) {
        println!("#################################");
        self.state = match SubnetSimulator::load_state(&self.subnet_name) {
            Ok(st) => st,
            Err(_) => {
                println!("Failed to load state");
                return;
            }
        };
        // print in a more organized manner:
        println!("Subnet: {}", self.subnet_name);
        println!("Subnet PK: {}", self.get_public_key());
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
