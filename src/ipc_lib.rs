use bitcoin::address::NetworkUnchecked;
use bitcoin::hashes::Hash;
use bitcoin::Amount;
use bitcoin::Transaction;
use bitcoin::Txid;
use bitcoin::XOnlyPublicKey;
use ipc_serde::IpcSerialize;
use log::trace;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

use crate::bitcoin_utils::{self, submit_to_mempool};
use crate::db;
use crate::eth_utils::eth_addr_from_x_only_pubkey;
use crate::eth_utils::ETH_ADDR_LEN;
use crate::multisig::create_subnet_multisig_address;
use crate::wallet;
use crate::NETWORK;

// Temporary prelude module to re-export the necessary types
pub mod prelude {
    pub use super::{
        IpcCreateSubnetMsg, IpcJoinSubnetMsg, IpcMessage, IpcSerialize, IpcTag, SubnetId,
        IPC_CHECKPOINT_TAG, IPC_CREATE_SUBNET_TAG, IPC_DELETE_SUBNET_TAG, IPC_FUND_SUBNET_TAG,
        IPC_JOIN_SUBNET_TAG, IPC_PREFUND_SUBNET_TAG, IPC_TAG_DELIMITER, IPC_TRANSFER_TAG,
        IPC_WITHDRAW_TAG,
    };
}

pub type FvmAddress = fvm_shared::address::Address;

// Tag

pub const IPC_TAG_DELIMITER: &str = "#";
pub const IPC_CREATE_SUBNET_TAG: &str = "IPC:CREATE";
pub const IPC_PREFUND_SUBNET_TAG: &str = "IPC:PREFUND";
pub const IPC_JOIN_SUBNET_TAG: &str = "IPC:JOIN";
pub const IPC_FUND_SUBNET_TAG: &str = "IPC:FUND";
pub const IPC_CHECKPOINT_TAG: &str = "IPC:CHECKPOINT";
pub const IPC_TRANSFER_TAG: &str = "IPC:TRANSFER";
pub const IPC_WITHDRAW_TAG: &str = "IPC:WITHDRAW";
pub const IPC_DELETE_SUBNET_TAG: &str = "IPC:DELETE";

// Define the IPC tags enum
#[derive(Debug, PartialEq)]
pub enum IpcTag {
    CreateSubnet,
    JoinSubnet,
    PrefundSubnet,
    FundSubnet,
}

impl IpcTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CreateSubnet => IPC_CREATE_SUBNET_TAG,
            Self::JoinSubnet => IPC_JOIN_SUBNET_TAG,
            Self::PrefundSubnet => IPC_PREFUND_SUBNET_TAG,
            Self::FundSubnet => IPC_FUND_SUBNET_TAG,
        }
    }
}

impl std::str::FromStr for IpcTag {
    type Err = IpcSerializeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            IPC_CREATE_SUBNET_TAG => Ok(Self::CreateSubnet),
            IPC_JOIN_SUBNET_TAG => Ok(Self::JoinSubnet),
            IPC_PREFUND_SUBNET_TAG => Ok(Self::PrefundSubnet),
            IPC_FUND_SUBNET_TAG => Ok(Self::FundSubnet),
            _ => Err(IpcSerializeError::UnknownTag(s.to_string())),
        }
    }
}

// IPCSerialize trait

#[derive(Debug, Error)]
pub enum IpcSerializeError {
    #[error("Missing field: {0}")]
    MissingField(String),
    #[error("Error parsing field {0}: {1}")]
    ParseFieldError(String, String),
    #[error("Unknown IPC tag: {0}")]
    UnknownTag(String),
    #[error("Deserialization error: {0}")]
    DeserializationError(String),
}

pub trait IpcSerialize {
    fn ipc_serialize(&self) -> String;
    fn ipc_deserialize(s: &str) -> Result<Self, IpcSerializeError>
    where
        Self: Sized;
}

// IPCValidate trait

#[derive(Debug, Error)]
pub enum IpcValidateError {
    #[error("Invalid message: {0}")]
    InvalidMsg(String),

    #[error("Invalid field {0}: {1}")]
    InvalidField(&'static str, String),
}

pub trait IpcValidate {
    fn validate(&self) -> Result<(), IpcValidateError>;
}

// IPC Messages

#[derive(Serialize, Deserialize, IpcSerialize, Debug, Clone)]
#[tag(IPC_CREATE_SUBNET_TAG)]
pub struct IpcCreateSubnetMsg {
    /// The minimum number of collateral required for validators in Satoshis
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub min_validator_stake: Amount,
    /// Minimum number of validators required to bootstrap the subnet
    pub min_validators: u16,
    /// The bottom up checkpoint period in number of blocks
    pub bottomup_check_period: u64,
    /// The max number of active validators in subnet
    pub active_validators_limit: u16,
    /// Minimum fee for cross-net messages in subnet (in Satoshis)
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub min_cross_msg_fee: Amount,
    /// The addresses of whitelisted validators
    pub whitelist: Vec<XOnlyPublicKey>,
}

impl IpcValidate for IpcCreateSubnetMsg {
    fn validate(&self) -> Result<(), IpcValidateError> {
        if self.min_validators == 0 {
            return Err(IpcValidateError::InvalidField(
                "min_validators",
                "The minimum number of validators must be greater than 0".to_string(),
            ));
        }

        if self.whitelist.len() < self.min_validators as usize {
            return Err(IpcValidateError::InvalidField(
                "whitelist",
                "Number of whitelisted validators is less than the minimum required validators"
                    .to_string(),
            ));
        }

        if self.bottomup_check_period == 0 {
            return Err(IpcValidateError::InvalidField(
                "bottomup_check_period",
                "Must be greater than 0".to_string(),
            ));
        }

        if self.active_validators_limit < self.min_validators {
            return Err(IpcValidateError::InvalidField(
                "active_validators_limit",
                "Must be greater than or equal to min_validators".to_string(),
            ));
        }

        if self.min_cross_msg_fee == Amount::ZERO {
            return Err(IpcValidateError::InvalidField(
                "min_cross_msg_fee",
                "Must be greater than 0".to_string(),
            ));
        }

        Ok(())
    }
}

impl IpcCreateSubnetMsg {
    /// Submits the create subnet message to the Bitcoin network
    /// Using commit-reveal scheme
    /// Returns the subnet id — derived from the commit txid
    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
    ) -> Result<SubnetId, IpcLibError> {
        let subnet_data = self.ipc_serialize();

        info!(
            "Submitting create subnet msg to bitcoin. Data={}",
            subnet_data
        );

        // The address to return any non-dust values
        let return_address = wallet::get_new_address(rpc)?;

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (commit_tx, reveal_tx) = bitcoin_utils::create_commit_reveal_txs(
            rpc,
            &secp,
            &return_address,
            subnet_data.as_bytes(),
            None,
        )?;

        let commit_txid = commit_tx.compute_txid();
        let reveal_txid = reveal_tx.compute_txid();
        debug!(
            "Create subnet commit_txid={} reveal_txid={}",
            commit_txid, reveal_txid
        );

        match submit_to_mempool(rpc, vec![commit_tx.clone(), reveal_tx.clone()]) {
            Ok(_) => {
                let subnet_id = SubnetId::from_txid(&reveal_txid);
                info!("Submitted create subnet msg for subnet_id={}", subnet_id);
                Ok(subnet_id)
            }
            Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
        }
    }

    /// Saves the create subnet message to the database
    /// by creating a new subnet genesis info
    ///
    /// Returns the subnet id and the multisig address
    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        txid: bitcoin::Txid,
    ) -> Result<db::SubnetGenesisInfo, IpcLibError> {
        let subnet_id = SubnetId::from_txid(&txid);

        let genesis_info = db::SubnetGenesisInfo {
            subnet_id,
            create_subnet_msg: self.clone(),
            bootstrapped: false,
            create_msg_block_height: block_height,
            genesis_block_height: None,
            genesis_validators: Vec::with_capacity(0),
            genesis_balance_entries: Vec::with_capacity(0),
        };

        trace!("Saving {self:?} to DB, genesis_info={genesis_info:?}");
        let mut wtxn = db.write_txn()?;
        db.save_subnet_genesis_info(&mut wtxn, subnet_id, &genesis_info)?;
        wtxn.commit()?;

        Ok(genesis_info)
    }

    /// Creates a multisig address from the whitelisted public keys
    /// and the subnet id
    pub fn multisig_address_from_whitelist(
        &self,
        subnet_id: &SubnetId,
    ) -> Result<bitcoin::Address, IpcLibError> {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let multisig_address = create_subnet_multisig_address(
            &secp,
            subnet_id,
            &self.whitelist.clone(),
            self.min_validators.into(),
            NETWORK,
        )?;

        Ok(multisig_address)
    }
}

#[derive(Serialize, Deserialize, IpcSerialize, Debug, Clone)]
#[tag(IPC_JOIN_SUBNET_TAG)]
pub struct IpcJoinSubnetMsg {
    /// The subnet id of the subnet to join
    pub subnet_id: SubnetId,
    /// The amount to collateral to lock in the subnet
    #[ipc_serde(skip)]
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub collateral: bitcoin::Amount,
    /// The IP address of the validator, as
    /// advertised in the subnet's join message
    pub ip: std::net::SocketAddr,
    /// The bitcoin address of the validator
    /// to receive back the collateral in case of
    /// subnet termination.
    pub backup_address: bitcoin::Address<NetworkUnchecked>,
    /// The pubkey of the validator
    pub pubkey: bitcoin::XOnlyPublicKey,
}

impl IpcJoinSubnetMsg {
    /// Validates the join subnet message, for the given genesis info
    pub fn validate_for_genesis_info(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
    ) -> Result<(), IpcValidateError> {
        // Check if the subnet is already bootstrapped
        if genesis_info.bootstrapped {
            // TODO handle when subnet already bootstrapped
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {} is already bootstrapped.",
                self.subnet_id
            )));
        }

        // Check if the collateral is at least the minimum validator stake
        if self.collateral < genesis_info.create_subnet_msg.min_validator_stake {
            return Err(IpcValidateError::InvalidField(
                "collateral",
                format!(
                    "Collateral must be at least {}, supplied {}",
                    genesis_info.create_subnet_msg.min_validator_stake, self.collateral,
                ),
            ));
        }

        // Check if the validator's public key is whitelisted
        if !genesis_info
            .create_subnet_msg
            .whitelist
            .contains(&self.pubkey)
        {
            return Err(IpcValidateError::InvalidField(
                "pubkey",
                "Validator public key not whitelisted".to_string(),
            ));
        }

        // Check if the validator with this public key is already registered
        if genesis_info
            .genesis_validators
            .iter()
            .any(|v| v.pubkey == self.pubkey)
        {
            return Err(IpcValidateError::InvalidField(
                "pubkey",
                "Validator with this public key already registered in subnet".to_string(),
            ));
        }

        Ok(())
    }

    /// Submits the join subnet message to the Bitcoin network
    /// Using commit-reveal scheme
    /// And sends the collateral to the subnet multisig address
    ///
    /// Returns the join txid — the txid of the reveal transaction
    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
        multisig_address: &bitcoin::Address,
    ) -> Result<Txid, IpcLibError> {
        let subnet_data = self.ipc_serialize();

        info!(
            "Submitting join subnet msg to bitcoin. Multisig address = {}. Data={}",
            multisig_address, subnet_data
        );

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (commit_tx, reveal_tx) = bitcoin_utils::create_commit_reveal_txs(
            rpc,
            &secp,
            multisig_address,
            subnet_data.as_bytes(),
            Some(self.collateral),
        )?;

        let commit_txid = commit_tx.compute_txid();
        let reveal_txid = reveal_tx.compute_txid();
        debug!(
            "Join subnet commit_txid={} reveal_txid={}",
            commit_txid, reveal_txid
        );

        match submit_to_mempool(rpc, vec![commit_tx.clone(), reveal_tx.clone()]) {
            Ok(_) => {
                info!(
                    "Submitted join subnet msg for subnet_id={} join_txid={}",
                    self.subnet_id, reveal_txid,
                );
                Ok(reveal_txid)
            }
            Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
        }
    }

    /// Modifies the database to account for the join subnet message
    /// Returning SubnetState if the subnet is bootstrapped
    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        txid: Txid,
    ) -> Result<Option<db::SubnetState>, IpcLibError> {
        let mut genesis_info =
            db.get_subnet_genesis_info(self.subnet_id)?
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "subnet id={} does not exist",
                    self.subnet_id
                )))?;

        self.validate_for_genesis_info(&genesis_info)?;

        let subnet_address = eth_addr_from_x_only_pubkey(self.pubkey);

        let new_validator = db::SubnetValidator {
            pubkey: self.pubkey,
            subnet_address,
            collateral: self.collateral,
            backup_address: self.backup_address.clone(),
            ip: self.ip,
            join_txid: txid,
        };
        trace!("Processing {self:?}, adding new validator {new_validator:?}");
        genesis_info.genesis_validators.push(new_validator);

        // Write to DB
        let mut wtxn = db.write_txn()?;

        //
        // Check if the subnet is bootstrapped
        //
        if genesis_info.enough_to_bootstrap() {
            trace!("Subnet {} bootstrapped", self.subnet_id);
            genesis_info.bootstrapped = true;
            genesis_info.genesis_block_height = Some(block_height);

            // Save the newly create subnet state
            let subnet_state = genesis_info.to_subnet();
            db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet_state)?;
            db.save_subnet_genesis_info(&mut wtxn, self.subnet_id, &genesis_info)?;
            wtxn.commit()?;
            Ok(Some(subnet_state))
        } else {
            db.save_subnet_genesis_info(&mut wtxn, self.subnet_id, &genesis_info)?;
            wtxn.commit()?;
            Ok(None)
        }
    }
}

impl IpcValidate for IpcJoinSubnetMsg {
    fn validate(&self) -> Result<(), IpcValidateError> {
        // Check subnet_id is not all zeros
        if self.subnet_id == SubnetId::default() {
            return Err(IpcValidateError::InvalidField(
                "subnet_id",
                "Subnet ID cannot be all zeros".to_string(),
            ));
        }

        // Check collateral is not zero
        if self.collateral == Amount::ZERO {
            return Err(IpcValidateError::InvalidField(
                "collateral",
                "Collateral amount must be greater than zero".to_string(),
            ));
        }

        // Check backup address
        if !self.backup_address.is_valid_for_network(NETWORK) {
            return Err(IpcValidateError::InvalidField(
                "backup_address",
                format!("Bitcoin address must be for {}", NETWORK),
            ));
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcPrefundSubnetMsg {
    /// The subnet id of the subnet to prefund
    /// This is derived from 2nd output
    /// that is sent to the subnet multisig address
    pub subnet_id: SubnetId,
    /// The amount to deposit in the subnet
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub amount: bitcoin::Amount,
    /// The address to prefund in the subnet
    pub address: alloy_primitives::Address,
}

impl IpcValidate for IpcPrefundSubnetMsg {
    fn validate(&self) -> Result<(), IpcValidateError> {
        if self.amount == bitcoin::Amount::MIN {
            return Err(IpcValidateError::InvalidField(
                "value",
                "Value must be greater than 0".to_string(),
            ));
        }
        Ok(())
    }
}

impl IpcPrefundSubnetMsg {
    // Locktime for the pre-release script
    const RELEASE_LOCKTIME: u32 = 6;
    // The length of the subnet tag - helper
    const PREFUND_TAG_LEN: usize = IPC_PREFUND_SUBNET_TAG.len();
    // The total length of the op_return data - helper
    const DATA_LEN: usize = Self::PREFUND_TAG_LEN + Txid::LEN + ETH_ADDR_LEN;

    /// Validates the join subnet message, for the given genesis info
    pub fn validate_for_genesis_info(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
    ) -> Result<(), IpcValidateError> {
        // Check if the subnet is already bootstrapped
        if genesis_info.bootstrapped {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {} is already bootstrapped.",
                self.subnet_id
            )));
        }

        Ok(())
    }

    /// Reconstructs an IpcPrefundSubnetMsg from a bitcoin::Transaction.
    ///
    /// Given that:
    ///   • The first output is an OP_RETURN containing our custom pushdata,
    ///     whose layout is:
    ///         [prefund tag | 32-byte txid | 20-byte alloy address]
    ///   • The second output is the funding output with nonzero value.
    ///
    /// Returns an error if any expected data is missing or malformed.
    pub fn from_tx(tx: &Transaction) -> Result<Self, IpcLibError> {
        use bitcoin::blockdata::script::Instruction;

        // Helper closure for error creation
        let err = |msg: String| IpcLibError::MsgParseError(IPC_PREFUND_SUBNET_TAG, msg);

        // Verify we have both required outputs
        if tx.output.len() < 2 {
            return Err(err("Transaction must have at least 2 outputs".into()));
        }
        // Get OP_RETURN data from first output
        let op_return_data = tx.output[0]
            .script_pubkey
            .instructions_minimal()
            .find_map(|ins| match ins {
                Ok(Instruction::PushBytes(data)) => Some(data.as_bytes()),
                _ => None,
            })
            .ok_or_else(|| err("First output must be OP_RETURN with pushdata".into()))?;

        // Check total length matches our expected format
        if op_return_data.len() != Self::DATA_LEN {
            return Err(err(format!(
                "OP_RETURN data length mismatch: got {}, expected {}",
                op_return_data.len(),
                Self::DATA_LEN
            )));
        }

        // Split data into its components
        let (tag, rest) = op_return_data.split_at(Self::PREFUND_TAG_LEN);
        let (txid_bytes, addr_bytes) = rest.split_at(Txid::LEN);

        // Verify tag
        if tag != IPC_PREFUND_SUBNET_TAG.as_bytes() {
            return Err(err(format!(
                "Invalid tag: got '{}', expected '{}'",
                String::from_utf8_lossy(tag),
                IPC_PREFUND_SUBNET_TAG
            )));
        }

        // Convert txid bytes to Txid
        let txid =
            Txid::from_slice(txid_bytes).map_err(|e| err(format!("Invalid txid bytes: {}", e)))?;
        let subnet_id = SubnetId::from_txid(&txid);

        // Convert address bytes to alloy Address
        let address = alloy_primitives::Address::from_slice(addr_bytes);

        // Get value from second output
        let amount = tx.output[1].value;

        Ok(Self {
            subnet_id,
            amount,
            address,
        })
    }

    pub fn to_tx(
        &self,
        multisig_address: &bitcoin::Address,
        release_address: &bitcoin::Address,
    ) -> Result<Transaction, IpcLibError> {
        let secp = bitcoin::secp256k1::Secp256k1::new();

        //
        // Create the first output: op_return with
        // ipc tag, subnet_id (txid) and user's subnet address to fund
        //

        let prefund_tag: [u8; Self::PREFUND_TAG_LEN] = IPC_PREFUND_SUBNET_TAG
            .as_bytes()
            .try_into()
            .expect("IPC_PREFUND_SUBNET_TAG has incorrect length");
        let subnet_id_txid: [u8; Txid::LEN] = self.subnet_id.txid().as_raw_hash().to_byte_array();
        let subnet_addr: [u8; ETH_ADDR_LEN] = self.address.into_array();

        // Construct op_return data
        let mut op_return_data = [0u8; Self::PREFUND_TAG_LEN + Txid::LEN + ETH_ADDR_LEN];

        op_return_data[0..Self::PREFUND_TAG_LEN].copy_from_slice(&prefund_tag);
        op_return_data[Self::PREFUND_TAG_LEN..(Self::PREFUND_TAG_LEN + Txid::LEN)]
            .copy_from_slice(&subnet_id_txid);
        op_return_data[(Self::PREFUND_TAG_LEN + Txid::LEN)..].copy_from_slice(&subnet_addr);

        // Make op_return script and txout
        let op_return_script = bitcoin_utils::make_op_return_script(op_return_data);
        let data_tx_out = bitcoin::TxOut {
            value: bitcoin::Amount::ZERO,
            script_pubkey: op_return_script,
        };

        //
        // Create second output: pre-fund + pre-release script
        //

        let prefund_script = bitcoin_utils::create_send_with_timelock_release_tx_script(
            &secp,
            multisig_address,
            release_address,
            Self::RELEASE_LOCKTIME,
        )?;
        let prefund_tx_out = bitcoin::TxOut {
            value: self.amount,
            script_pubkey: prefund_script,
        };

        // Construct transaction

        let tx_outs = vec![data_tx_out, prefund_tx_out];
        let prefund_tx = bitcoin_utils::create_tx_from_txouts(tx_outs);
        debug!("Prefund TX: {prefund_tx:?}");

        Ok(prefund_tx)
    }

    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
        multisig_address: &bitcoin::Address,
    ) -> Result<Txid, IpcLibError> {
        info!(
            "Submitting pre-fund subnet msg to bitcoin. Multisig address = {}. Amount={}",
            multisig_address, self.amount
        );

        let release_address = wallet::get_new_address(rpc)?;

        // Construct, fund and sign the prefund transaction

        let prefund_tx = self.to_tx(multisig_address, &release_address)?;

        let prefund_tx = crate::wallet::fund_tx(rpc, prefund_tx, None)?;
        trace!("Prefund funded TX: {prefund_tx:?}");
        let prefund_tx = crate::wallet::sign_tx(rpc, prefund_tx)?;
        trace!("Prefund signed TX: {prefund_tx:?}");

        // Submit the prefund transaction to the mempool

        let prefund_txid = prefund_tx.compute_txid();
        match submit_to_mempool(rpc, vec![prefund_tx]) {
            Ok(_) => {
                info!(
                    "Submitted prefund subnet msg for subnet_id={} prefund_txid={}",
                    self.subnet_id, prefund_txid,
                );
                Ok(prefund_txid)
            }
            Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
        }
    }

    /// Modifies the database to account for the join subnet message
    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let mut genesis_info =
            db.get_subnet_genesis_info(self.subnet_id)?
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "subnet id={} does not exist",
                    self.subnet_id
                )))?;

        self.validate_for_genesis_info(&genesis_info)?;

        trace!("Processing {self:?}, adding new genesis_balance entry");

        genesis_info.add_genesis_balance_entry(self.address, self.amount, txid, block_height);

        let mut wtxn = db.write_txn()?;
        db.save_subnet_genesis_info(&mut wtxn, self.subnet_id, &genesis_info)?;
        wtxn.commit()?;

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcFundSubnetMsg {
    /// The subnet id of the subnet to prefund
    /// This is derived from 2nd output
    /// that is sent to the subnet multisig address
    pub subnet_id: SubnetId,
    /// The amount to deposit in the subnet
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub amount: bitcoin::Amount,
    /// The address to prefund in the subnet
    pub address: alloy_primitives::Address,
}

impl IpcValidate for IpcFundSubnetMsg {
    fn validate(&self) -> Result<(), IpcValidateError> {
        if self.amount == bitcoin::Amount::MIN {
            return Err(IpcValidateError::InvalidField(
                "value",
                "Value must be greater than 0".to_string(),
            ));
        }
        Ok(())
    }
}

impl IpcFundSubnetMsg {
    // The length of the subnet tag - helper
    const FUND_TAG_LEN: usize = IPC_FUND_SUBNET_TAG.len();
    // The total length of the op_return data - helper
    const DATA_LEN: usize = Self::FUND_TAG_LEN + Txid::LEN + ETH_ADDR_LEN;

    /// Validates the fund msg for the given subnet
    pub fn validate_for_subnet(
        &self,
        _subnet_state: &db::SubnetState,
    ) -> Result<(), IpcValidateError> {
        // For now no need to validate anything, the subnet must exist

        Ok(())
    }

    // Create a rootnet message from the fund subnet message
    pub fn to_rootnet_message(
        &self,
        nonce: u64,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> db::RootnetMessage {
        db::RootnetMessage::FundSubnet {
            msg: self.clone(),
            nonce,
            block_height,
            block_hash,
            txid,
        }
    }

    /// Modifies the database to account for the join subnet message
    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let subnet_state =
            db.get_subnet_state(self.subnet_id)?
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "subnet id={} does not exist",
                    self.subnet_id
                )))?;

        self.validate_for_subnet(&subnet_state)?;

        trace!("Processing {self:?}, adding new rootnet message");

        // Get next nonce

        let nonce = db.get_next_rootnet_msg_nonce(self.subnet_id)?;
        let mut wtxn = db.write_txn()?;
        // Construct rootnet message
        let rootnet_msg = self.to_rootnet_message(nonce, block_height, block_hash, txid);
        debug!("New rootnet message: {rootnet_msg:?}");
        // save message
        db.add_rootnet_msg(&mut wtxn, self.subnet_id, rootnet_msg)?;
        wtxn.commit()?;

        Ok(())
    }

    /// Reconstructs an IpcFundSubnetMsg from a bitcoin::Transaction.
    ///
    /// Given that:
    ///   • The first output is an OP_RETURN containing our custom pushdata,
    ///     whose layout is:
    ///         [fund tag | 32-byte txid | 20-byte alloy address]
    ///   • The second output is the funding output with nonzero value.
    ///
    /// Returns an error if any expected data is missing or malformed.
    pub fn from_tx(tx: &Transaction) -> Result<Self, IpcLibError> {
        use bitcoin::blockdata::script::Instruction;

        // Helper closure for error creation
        let err = |msg: String| IpcLibError::MsgParseError(IPC_FUND_SUBNET_TAG, msg);

        // Verify we have both required outputs
        if tx.output.len() < 2 {
            return Err(err("Transaction must have at least 2 outputs".into()));
        }
        // Get OP_RETURN data from first output
        let op_return_data = tx.output[0]
            .script_pubkey
            .instructions_minimal()
            .find_map(|ins| match ins {
                Ok(Instruction::PushBytes(data)) => Some(data.as_bytes()),
                _ => None,
            })
            .ok_or_else(|| err("First output must be OP_RETURN with pushdata".into()))?;

        // Check total length matches our expected format
        if op_return_data.len() != Self::DATA_LEN {
            return Err(err(format!(
                "OP_RETURN data length mismatch: got {}, expected {}",
                op_return_data.len(),
                Self::DATA_LEN
            )));
        }

        // Split data into its components
        let (tag, rest) = op_return_data.split_at(Self::FUND_TAG_LEN);
        let (txid_bytes, addr_bytes) = rest.split_at(Txid::LEN);

        // Verify tag
        if tag != IPC_FUND_SUBNET_TAG.as_bytes() {
            return Err(err(format!(
                "Invalid tag: got '{}', expected '{}'",
                String::from_utf8_lossy(tag),
                IPC_FUND_SUBNET_TAG
            )));
        }

        // Convert txid bytes to Txid
        let txid =
            Txid::from_slice(txid_bytes).map_err(|e| err(format!("Invalid txid bytes: {}", e)))?;
        let subnet_id = SubnetId::from_txid(&txid);

        // Convert address bytes to alloy Address
        let address = alloy_primitives::Address::from_slice(addr_bytes);

        // Get value from second output
        let amount = tx.output[1].value;

        Ok(Self {
            subnet_id,
            amount,
            address,
        })
    }

    pub fn to_tx(&self, multisig_address: &bitcoin::Address) -> Result<Transaction, IpcLibError> {
        //
        // Create the first output: op_return with
        // ipc tag, subnet_id (txid) and user's subnet address to fund
        //
        let fund_tag: [u8; Self::FUND_TAG_LEN] = IPC_FUND_SUBNET_TAG
            .as_bytes()
            .try_into()
            .expect("IPC_FUND_SUBNET_TAG has incorrect length");
        let subnet_id_txid: [u8; Txid::LEN] = self.subnet_id.txid().as_raw_hash().to_byte_array();
        let subnet_addr: [u8; ETH_ADDR_LEN] = self.address.into_array();

        // Construct op_return data
        let mut op_return_data = [0u8; Self::FUND_TAG_LEN + Txid::LEN + ETH_ADDR_LEN];

        op_return_data[0..Self::FUND_TAG_LEN].copy_from_slice(&fund_tag);
        op_return_data[Self::FUND_TAG_LEN..(Self::FUND_TAG_LEN + Txid::LEN)]
            .copy_from_slice(&subnet_id_txid);
        op_return_data[(Self::FUND_TAG_LEN + Txid::LEN)..].copy_from_slice(&subnet_addr);

        // Make op_return script and txout
        let op_return_script = bitcoin_utils::make_op_return_script(op_return_data);
        let data_tx_out = bitcoin::TxOut {
            value: bitcoin::Amount::ZERO,
            script_pubkey: op_return_script,
        };

        //
        // Create second output: pre-fund + pre-release script
        //

        let fund_tx_out = bitcoin::TxOut {
            value: self.amount,
            script_pubkey: multisig_address.script_pubkey(),
        };

        // Construct transaction

        let tx_outs = vec![data_tx_out, fund_tx_out];
        let fund_tx = bitcoin_utils::create_tx_from_txouts(tx_outs);
        debug!("Fund TX: {fund_tx:?}");

        Ok(fund_tx)
    }

    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
        multisig_address: &bitcoin::Address,
    ) -> Result<Txid, IpcLibError> {
        info!(
            "Submitting fund subnet msg to bitcoin. Multisig address = {}. Amount={}",
            multisig_address, self.amount
        );

        // Construct, fund and sign the prefund transaction

        let fund_tx = self.to_tx(multisig_address)?;

        let fund_tx = crate::wallet::fund_tx(rpc, fund_tx, None)?;
        trace!("Fund msg funded TX: {fund_tx:?}");
        let fund_tx = crate::wallet::sign_tx(rpc, fund_tx)?;
        trace!("Fund msg signed TX: {fund_tx:?}");

        // Submit the prefund transaction to the mempool

        let fund_txid = fund_tx.compute_txid();
        match submit_to_mempool(rpc, vec![fund_tx]) {
            Ok(_) => {
                info!(
                    "Submitted fund subnet msg for subnet_id={} fund_txid={}",
                    self.subnet_id, fund_txid,
                );
                Ok(fund_txid)
            }
            Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
        }
    }
}

//
// IPC Messages
//

// Define the IPCMessage enum
#[derive(Debug)]
pub enum IpcMessage {
    CreateSubnet(IpcCreateSubnetMsg),
    JoinSubnet(IpcJoinSubnetMsg),
    PrefundSubnet(IpcPrefundSubnetMsg),
    FundSubnet(IpcFundSubnetMsg),
}

impl IpcMessage {
    pub fn deserialize(s: &str) -> Result<Self, IpcSerializeError> {
        let tag = s
            .split(IPC_TAG_DELIMITER)
            .next()
            .ok_or_else(|| IpcSerializeError::DeserializationError("Missing tag".to_string()))?;

        // Temporary clippy warning because there is only one value
        #[allow(clippy::manual_map)]
        match IpcTag::from_str(tag)? {
            IpcTag::CreateSubnet => Ok(IpcMessage::CreateSubnet(
                IpcCreateSubnetMsg::ipc_deserialize(s)?,
            )),
            IpcTag::JoinSubnet => Ok(IpcMessage::JoinSubnet(IpcJoinSubnetMsg::ipc_deserialize(
                s,
            )?)),
            //
            // The bellow messages aren't using serialization in witness
            //
            IpcTag::PrefundSubnet => Err(IpcSerializeError::DeserializationError(
                "Invalid tag".to_string(),
            )),
            IpcTag::FundSubnet => Err(IpcSerializeError::DeserializationError(
                "Invalid tag".to_string(),
            )),
        }
    }
}

//
// Subnet ID
//

pub const L1_DELEGATED_NAMESPACE: u64 = 20;

/// Create Subnet IPC message is sent as a commit-reveal transaction pair.
/// Subnet ID is derived from the transaction ID of the reveal transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubnetId(FvmAddress);

#[derive(Debug, Error)]
pub enum SubnetIdError {
    #[error("Invalid Subnet Id format. Expected '{0}/<addr>', got '{1}'")]
    InvalidFormat(&'static str, String),
    #[error("Invalid delegated address: {0}")]
    InvalidFvmAddress(#[from] fvm_shared::address::Error),
}

impl SubnetId {
    /// Creates a new SubnetId from a transaction ID
    pub fn from_txid(txid: &Txid) -> Self {
        let addr = FvmAddress::new_delegated(L1_DELEGATED_NAMESPACE, txid.as_ref())
            .expect("txid is longer than 32 bytes, unreachable");
        Self(addr)
    }

    pub fn addr(&self) -> &FvmAddress {
        &self.0
    }

    // Getting the txid from the delegated address
    // It's impossible to create other types of addresses or with other data
    // So it's safe to panic to handle errors
    pub fn txid(&self) -> Txid {
        let payload = self.0.payload();
        let sub_address = match payload {
            fvm_shared::address::Payload::Delegated(del_addr) => del_addr.subaddress(),
            _ => panic!("SubnetId doesn't have a delegated address"),
        };

        Txid::from_slice(sub_address).expect("SubnetId subaddress is an invalid txid")
    }
}

impl Default for SubnetId {
    fn default() -> Self {
        Self::from_txid(&Txid::all_zeros())
    }
}

impl FromStr for SubnetId {
    type Err = SubnetIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // check if it starts with /
        if !s.starts_with('/') {
            return Err(SubnetIdError::InvalidFormat(crate::L1_NAME, s.to_string()));
        }
        let s = &s[1..];

        let parts: Vec<&str> = s.split('/').collect();
        if parts.len() != 2 || parts[0] != &crate::L1_NAME[1..] {
            return Err(SubnetIdError::InvalidFormat(crate::L1_NAME, s.to_string()));
        }

        let addr = FvmAddress::from_str(parts[1])?;
        Ok(SubnetId(addr))
    }
}

impl std::fmt::Display for SubnetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", crate::L1_NAME, self.0)
    }
}

impl serde::Serialize for SubnetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialize as string in the format "L1_NAME/txid"
        self.to_string().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for SubnetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        SubnetId::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Error, Debug)]
pub enum IpcLibError {
    #[error("Error parsing {0}: {1}")]
    MsgParseError(&'static str, String),

    #[error(transparent)]
    IpcValidateError(#[from] IpcValidateError),

    #[error(transparent)]
    DbError(#[from] crate::db::DbError),

    #[error(transparent)]
    HeedError(#[from] heed::Error),

    #[error(transparent)]
    BitcoinUtilsError(#[from] crate::bitcoin_utils::BitcoinUtilsError),

    #[error(transparent)]
    WalletError(#[from] crate::wallet::WalletError),

    #[error(transparent)]
    MultisigError(#[from] crate::multisig::MultisigError),
}

#[cfg(test)]
mod tests {
    use crate::L1_NAME;

    use super::*;

    #[test]
    fn test_ipc_tag_as_str() {
        assert_eq!(IpcTag::CreateSubnet.as_str(), IPC_CREATE_SUBNET_TAG);
    }

    #[test]
    fn test_ipc_tag_from_str() {
        assert_eq!(
            IpcTag::from_str(IPC_CREATE_SUBNET_TAG).unwrap(),
            IpcTag::CreateSubnet
        );
        assert!(IpcTag::from_str("INVALID_TAG").is_err());
    }

    //
    // Create subnet
    //

    #[test]
    fn test_ipc_create_subnet_msg_serialize() {
        let validator1 = XOnlyPublicKey::from_str(
            "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
        )
        .unwrap();
        let validator2 = XOnlyPublicKey::from_str(
            "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df",
        )
        .unwrap();

        let params = IpcCreateSubnetMsg {
            min_validator_stake: Amount::from_sat(1000),
            min_validators: 2,
            bottomup_check_period: 10,
            active_validators_limit: 20,
            min_cross_msg_fee: Amount::from_sat(50),
            whitelist: vec![validator1, validator2],
        };

        let serialized = params.ipc_serialize();
        println!("{}", serialized);

        assert!(serialized.starts_with(IPC_CREATE_SUBNET_TAG));
        assert!(serialized.contains(&format!("{}min_validator_stake=1000", IPC_TAG_DELIMITER)));
        assert!(serialized.contains(&format!("{}min_validators=2", IPC_TAG_DELIMITER)));
        assert!(serialized.contains(&format!("{}bottomup_check_period=10", IPC_TAG_DELIMITER)));
        assert!(serialized.contains(&format!("{}active_validators_limit=20", IPC_TAG_DELIMITER)));
        assert!(serialized.contains(&format!("{}min_cross_msg_fee=50", IPC_TAG_DELIMITER)));
        assert!(serialized.contains(&format!(
            "{}whitelist={},{}",
            IPC_TAG_DELIMITER, validator1, validator2
        )));
    }

    #[test]
    fn test_ipc_create_subnet_msg_deserialize() {
        let validator1 = XOnlyPublicKey::from_str(
            "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
        )
        .unwrap();
        let validator2 = XOnlyPublicKey::from_str(
            "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df",
        )
        .unwrap();

        let serialized = format!(
            "{}{}min_validator_stake=1000{}min_validators=2{}bottomup_check_period=10{}active_validators_limit=20{}min_cross_msg_fee=50{}whitelist={},{}",
            IPC_CREATE_SUBNET_TAG,
            IPC_TAG_DELIMITER,
            IPC_TAG_DELIMITER,
            IPC_TAG_DELIMITER,
            IPC_TAG_DELIMITER,
            IPC_TAG_DELIMITER,
            IPC_TAG_DELIMITER,
            validator1,
            validator2,
        );

        println!("{}", serialized);

        let params = IpcCreateSubnetMsg::ipc_deserialize(&serialized).unwrap();
        assert_eq!(params.min_validator_stake, Amount::from_sat(1000));
        assert_eq!(params.min_validators, 2);
        assert_eq!(params.bottomup_check_period, 10);
        assert_eq!(params.active_validators_limit, 20);
        assert_eq!(params.min_cross_msg_fee, Amount::from_sat(50));
        assert_eq!(params.whitelist, vec![validator1, validator2]);
    }

    //
    // Join subnet
    //

    #[test]
    fn test_ipc_join_subnet_msg_validate() {
        let pubkey = XOnlyPublicKey::from_str(
            "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
        )
        .unwrap();

        let valid_msg = IpcJoinSubnetMsg {
            subnet_id: SubnetId::from_txid(
                &Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                    .unwrap(),
            ),
            collateral: Amount::from_sat(1000),
            ip: "127.0.0.1:8080".parse().unwrap(),
            backup_address: bitcoin::Address::from_str(
                "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
            )
            .unwrap(),
            pubkey,
        };
        assert!(valid_msg.validate().is_ok());

        // Test invalid subnet_id
        let mut invalid_msg = valid_msg.clone();
        invalid_msg.subnet_id = SubnetId::default();
        assert!(matches!(
            invalid_msg.validate(),
            Err(IpcValidateError::InvalidField("subnet_id", _))
        ));

        // Test zero collateral
        let mut invalid_msg = valid_msg.clone();
        invalid_msg.collateral = Amount::ZERO;
        assert!(matches!(
            invalid_msg.validate(),
            Err(IpcValidateError::InvalidField("collateral", _))
        ));

        // Test wrong network address
        let mut invalid_msg = valid_msg.clone();
        invalid_msg.backup_address =
        	// TODO make this work with any network type
            bitcoin::Address::from_str("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx").unwrap();
        assert!(matches!(
            invalid_msg.validate(),
            Err(IpcValidateError::InvalidField("backup_address", _))
        ));
    }

    #[test]
    fn test_ipc_join_subnet_msg_serialize_deserialize() {
        let pubkey = XOnlyPublicKey::from_str(
            "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
        )
        .unwrap();

        let create_txid =
            &Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();

        let msg = IpcJoinSubnetMsg {
            subnet_id: SubnetId::from_txid(create_txid),
            collateral: Amount::from_sat(1000),
            ip: "127.0.0.1:8080".parse().unwrap(),
            backup_address: bitcoin::Address::from_str(
                "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
            )
            .unwrap(),
            pubkey,
        };

        let serialized = msg.ipc_serialize();

        println!("{}", serialized);

        // Check serialization
        assert!(serialized.starts_with(IPC_JOIN_SUBNET_TAG));
        // collateral should not be in serialized, it's included in the output
        assert!(!serialized.contains("collateral"));

        assert!(serialized.contains(&format!("{}ip=127.0.0.1:8080", IPC_TAG_DELIMITER)));
        assert!(serialized.contains(&format!(
            "{}backup_address=bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
            IPC_TAG_DELIMITER
        )));
        assert!(serialized.contains(&format!(
            "{}pubkey=18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
            IPC_TAG_DELIMITER
        )));
        assert!(serialized.contains(&format!(
            "{}subnet_id={}/t420fhor637l2pmjle6whfq7go5upmf74qg6drcffcmr2t64kusy6lzfagfyi6m",
            IPC_TAG_DELIMITER, L1_NAME
        )));

        // Test deserialization
        let deserialized = IpcJoinSubnetMsg::ipc_deserialize(&serialized).unwrap();

        // Skipped fields should be default values
        assert_eq!(deserialized.subnet_id, SubnetId::from_txid(create_txid));
        assert_eq!(deserialized.collateral, Amount::from_sat(0));
        // Other fields should match
        assert_eq!(deserialized.ip, msg.ip);
        assert_eq!(deserialized.backup_address, msg.backup_address);
        assert_eq!(deserialized.pubkey, msg.pubkey);

        // It should be invalid because it's missing collateral and subnet_id
        assert!(deserialized.validate().is_err());
    }

    //
    // SubnetId
    //

    #[test]
    fn test_subnet_id_creation() {
        let txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();
        let subnet_id = SubnetId::from_txid(&txid);
        let addr = subnet_id.addr();

        println!("{subnet_id} {addr}");

        assert_eq!(subnet_id.txid(), txid);
    }

    #[test]
    fn test_subnet_id_from_str() {
        let subnet_id_str = format!(
            "{}/t420fhor637l2pmjle6whfq7go5upmf74qg6drcffcmr2t64kusy6lzfagfyi6m",
            crate::L1_NAME
        );
        let subnet_id = SubnetId::from_str(&subnet_id_str).unwrap();

        assert_eq!(
            subnet_id.txid().to_string(),
            "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b"
        );
    }

    #[test]
    fn test_subnet_id_display() {
        let txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();
        let subnet_id = SubnetId::from_txid(&txid);

        assert_eq!(
            subnet_id.to_string(),
            format!(
                "{}/t420fhor637l2pmjle6whfq7go5upmf74qg6drcffcmr2t64kusy6lzfagfyi6m",
                crate::L1_NAME
            )
        );
    }

    #[test]
    fn test_invalid_subnet_id() {
        // Test invalid txid
        let result = SubnetId::from_str(&format!("{}/invalid-txid", crate::L1_NAME));
        assert!(matches!(result, Err(SubnetIdError::InvalidFvmAddress(_))));

        // Test missing prefix
        let result =
            SubnetId::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b");
        assert!(matches!(result, Err(SubnetIdError::InvalidFormat(_, _))));

        // Test missing prefix
        let result =
            SubnetId::from_str("t420fhor637l2pmjle6whfq7go5upmf74qg6drcffcmr2t64kusy6lzfagfyi6m");
        assert!(matches!(result, Err(SubnetIdError::InvalidFormat(_, _))));

        // Test wrong prefix
        let result = SubnetId::from_str(
            "wrongchain/t420fhor637l2pmjle6whfq7go5upmf74qg6drcffcmr2t64kusy6lzfagfyi6m",
        );
        assert!(matches!(result, Err(SubnetIdError::InvalidFormat(_, _))));
    }

    #[test]
    fn test_subnet_id_serde() {
        let txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();
        let subnet_id = SubnetId::from_txid(&txid);

        // Test JSON serialization
        let serialized = serde_json::to_string(&subnet_id).unwrap();
        let expected = format!(
            "\"{}/t420fhor637l2pmjle6whfq7go5upmf74qg6drcffcmr2t64kusy6lzfagfyi6m\"",
            crate::L1_NAME
        );
        assert_eq!(serialized, expected);

        // Test JSON deserialization
        let deserialized: SubnetId = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, subnet_id);
    }
}

#[cfg(test)]
mod prefund_msg_tests {
    use super::*;

    fn create_test_msg() -> IpcPrefundSubnetMsg {
        let txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();
        let subnet_id = SubnetId::from_txid(&txid);
        let eth_addr =
            alloy_primitives::Address::from_str("742d35Cc6634C0532925a3b844Bc454e4438f44e")
                .unwrap();

        IpcPrefundSubnetMsg {
            subnet_id,
            amount: Amount::from_sat(1000),
            address: eth_addr,
        }
    }

    fn get_addresses() -> (bitcoin::Address, bitcoin::Address) {
        let multisig_address = bitcoin::Address::from_str(
            "bc1pzc5j0fyekrc9p63avup65y8h8rhp7m5ql5tg7590wuhhqdtlkfusng6er8",
        )
        .unwrap()
        .assume_checked();
        let release_address =
            bitcoin::Address::from_str("bcrt1qvr3jycfxtrkk8u6hp5caxc25tueek5f90mpnsv")
                .unwrap()
                .assume_checked();

        (multisig_address, release_address)
    }

    #[test]
    fn test_to_tx_structure() {
        let msg = create_test_msg();
        let (multisig_address, release_address) = get_addresses();

        // Generate transaction
        let tx = msg.to_tx(&multisig_address, &release_address).unwrap();

        // Check basic structure
        assert_eq!(
            tx.output.len(),
            2,
            "Transaction should have exactly 2 outputs"
        );

        // First output should be OP_RETURN
        assert!(
            tx.output[0].script_pubkey.is_op_return(),
            "First output should be OP_RETURN"
        );
        assert_eq!(
            tx.output[0].value,
            Amount::ZERO,
            "OP_RETURN output should have zero value"
        );

        // Second output should have the correct value and script
        assert!(
            tx.output[1].script_pubkey.is_p2tr(),
            "Second output should be p2tr"
        );
        assert_eq!(
            tx.output[1].value,
            Amount::from_sat(1000),
            "Second output should have correct value"
        );
    }

    #[test]
    fn test_from_tx_valid() {
        let original_msg = create_test_msg();
        let (multisig_address, release_address) = get_addresses();

        // Create transaction using to_tx
        let tx = original_msg
            .to_tx(&multisig_address, &release_address)
            .unwrap();

        // Parse it back using from_tx
        let parsed_msg = IpcPrefundSubnetMsg::from_tx(&tx).unwrap();

        // Verify all fields match
        assert_eq!(parsed_msg.subnet_id, original_msg.subnet_id);
        assert_eq!(parsed_msg.amount, original_msg.amount);
        assert_eq!(parsed_msg.address, original_msg.address);
    }

    #[test]
    fn test_from_tx_invalid_cases() {
        let (multisig_address, release_address) = get_addresses();

        // Test case 1: Empty transaction
        let empty_tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![],
            output: vec![],
        };
        assert!(matches!(
            IpcPrefundSubnetMsg::from_tx(&empty_tx),
            Err(IpcLibError::MsgParseError(IPC_PREFUND_SUBNET_TAG, _))
        ));

        // Test case 2: Transaction with only one output
        let tx = create_test_msg()
            .to_tx(&multisig_address, &release_address)
            .unwrap();
        let single_output_tx = Transaction {
            version: tx.version,
            lock_time: tx.lock_time,
            input: tx.input.clone(),
            output: vec![tx.output[0].clone()], // Only the OP_RETURN output
        };
        assert!(matches!(
            IpcPrefundSubnetMsg::from_tx(&single_output_tx),
            Err(IpcLibError::MsgParseError(IPC_PREFUND_SUBNET_TAG, _))
        ));

        // Test case 3: Wrong tag in OP_RETURN
        let mut wrong_tag_tx = tx.clone();
        let mut wrong_data = Vec::new();
        // same length as "IPC:PREFUND"
        let invalid_tag = "IPC:TEST123";
        wrong_data.extend_from_slice(invalid_tag.as_bytes()); // Different tag
        wrong_data.extend_from_slice(&[0u8; 32]); // txid
        wrong_data.extend_from_slice(&[0u8; 20]); // address
        let wrong_data: [u8; IpcPrefundSubnetMsg::DATA_LEN] = wrong_data.try_into().unwrap();
        wrong_tag_tx.output[0].script_pubkey = bitcoin_utils::make_op_return_script(wrong_data);
        assert!(matches!(
            IpcPrefundSubnetMsg::from_tx(&wrong_tag_tx),
            Err(IpcLibError::MsgParseError(IPC_PREFUND_SUBNET_TAG, _))
        ));
    }
}
