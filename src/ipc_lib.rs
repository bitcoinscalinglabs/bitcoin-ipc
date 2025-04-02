use bitcoin::address::NetworkUnchecked;
use bitcoin::hashes::Hash;
use bitcoin::Amount;
use bitcoin::Transaction;
use bitcoin::Txid;
use bitcoin::XOnlyPublicKey;
use ipc_serde::IpcSerialize;
use log::error;
use log::trace;
use log::warn;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use thiserror::Error;

use crate::bitcoin_utils::get_fee_rate;
use crate::bitcoin_utils::{self, submit_to_mempool};
use crate::db;
use crate::eth_utils::eth_addr_from_x_only_pubkey;
use crate::eth_utils::ETH_ADDR_LEN;
use crate::multisig;
use crate::multisig::create_subnet_multisig_address;
use crate::wallet;
use crate::NETWORK;

// Temporary prelude module to re-export the necessary types
pub mod prelude {
    pub use super::{
        IpcCreateSubnetMsg, IpcJoinSubnetMsg, IpcMessage, IpcSerialize, IpcTag, SubnetId,
        IPC_CHECKPOINT_TAG, IPC_CREATE_SUBNET_TAG, IPC_DELETE_SUBNET_TAG, IPC_FUND_SUBNET_TAG,
        IPC_JOIN_SUBNET_TAG, IPC_PREFUND_SUBNET_TAG, IPC_TAG_DELIMITER,
    };
}

pub type FvmAddress = fvm_shared::address::Address;

// Tag

pub const IPC_TAG_LENGTH: usize = 6;

// TODO make tags take less space
pub const IPC_TAG_DELIMITER: &str = "#";
pub const IPC_CREATE_SUBNET_TAG: &str = "IPCCRT";
pub const IPC_PREFUND_SUBNET_TAG: &str = "IPCPFD";
pub const IPC_JOIN_SUBNET_TAG: &str = "IPCJOI";
pub const IPC_FUND_SUBNET_TAG: &str = "IPCFND";
pub const IPC_CHECKPOINT_TAG: &str = "IPCCPT";
pub const IPC_TRANSFER_TAG: &str = "IPCTFR";
pub const IPC_DELETE_SUBNET_TAG: &str = "IPCDEL";

// Static assertion to verify tag lengths at compile time
const _: () = {
    assert!(IPC_CREATE_SUBNET_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_PREFUND_SUBNET_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_JOIN_SUBNET_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_FUND_SUBNET_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_CHECKPOINT_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_TRANSFER_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_DELETE_SUBNET_TAG.len() == IPC_TAG_LENGTH);
};

// Define the IPC tags enum
#[derive(Debug, PartialEq)]
pub enum IpcTag {
    CreateSubnet,
    JoinSubnet,
    PrefundSubnet,
    FundSubnet,
    CheckpointSubnet,
    BatchTransfer,
}

impl IpcTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CreateSubnet => IPC_CREATE_SUBNET_TAG,
            Self::JoinSubnet => IPC_JOIN_SUBNET_TAG,
            Self::PrefundSubnet => IPC_PREFUND_SUBNET_TAG,
            Self::FundSubnet => IPC_FUND_SUBNET_TAG,
            Self::CheckpointSubnet => IPC_CHECKPOINT_TAG,
            Self::BatchTransfer => IPC_TRANSFER_TAG,
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
            IPC_CHECKPOINT_TAG => Ok(Self::CheckpointSubnet),
            IPC_TRANSFER_TAG => Ok(Self::BatchTransfer),
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
    /// Each whitelisted key has the same weight
    pub fn multisig_address_from_whitelist(
        &self,
        subnet_id: &SubnetId,
    ) -> Result<bitcoin::Address, IpcLibError> {
        let secp = bitcoin::secp256k1::Secp256k1::new();

        let whitelist_weighted_keys = self
            .whitelist
            .clone()
            .into_iter()
            // Each key has the same weight
            .map(|xpk| (xpk, 1))
            .collect::<Vec<_>>();

        let multisig_address = create_subnet_multisig_address(
            &secp,
            subnet_id,
            &whitelist_weighted_keys,
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

        let power = multisig::collateral_to_power(
            &self.collateral,
            &genesis_info.create_subnet_msg.min_validator_stake,
        )?;

        let new_validator = db::SubnetValidator {
            pubkey: self.pubkey,
            subnet_address,
            collateral: self.collateral,
            power,
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
    // The total length of the op_return data - helper
    const DATA_LEN: usize = IPC_TAG_LENGTH + SubnetId::INNER_LEN + ETH_ADDR_LEN;

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
        let (tag, rest) = op_return_data.split_at(IPC_TAG_LENGTH);
        let (txid_bytes, addr_bytes) = rest.split_at(SubnetId::INNER_LEN);

        // Verify tag
        if tag != IPC_PREFUND_SUBNET_TAG.as_bytes() {
            return Err(err(format!(
                "Invalid tag: got '{}', expected '{}'",
                String::from_utf8_lossy(tag),
                IPC_PREFUND_SUBNET_TAG
            )));
        }

        // Convert txid bytes to Txid
        let txid20 = Txid20::from_slice(txid_bytes)
            .map_err(|e| err(format!("Invalid txid bytes: {}", e)))?;
        let subnet_id = SubnetId::from_txid20(&txid20);

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
        fee_rate: bitcoin::FeeRate,
        multisig_address: &bitcoin::Address,
        release_address: &bitcoin::Address,
    ) -> Result<Transaction, IpcLibError> {
        let secp = bitcoin::secp256k1::Secp256k1::new();

        //
        // Create the first output: op_return with
        // ipc tag, subnet_id (txid) and user's subnet address to fund
        //

        let prefund_tag: [u8; IPC_TAG_LENGTH] = IPC_PREFUND_SUBNET_TAG
            .as_bytes()
            .try_into()
            .expect("IPC_PREFUND_SUBNET_TAG has incorrect length");
        let subnet_id_txid: [u8; SubnetId::INNER_LEN] = self.subnet_id.txid20();
        let subnet_addr: [u8; ETH_ADDR_LEN] = self.address.into_array();

        // Construct op_return data
        let mut op_return_data = [0u8; Self::DATA_LEN];

        op_return_data[0..IPC_TAG_LENGTH].copy_from_slice(&prefund_tag);
        op_return_data[IPC_TAG_LENGTH..(IPC_TAG_LENGTH + SubnetId::INNER_LEN)]
            .copy_from_slice(&subnet_id_txid);
        op_return_data[(IPC_TAG_LENGTH + SubnetId::INNER_LEN)..].copy_from_slice(&subnet_addr);

        // Make op_return script and txout
        let op_return_script = bitcoin_utils::make_op_return_script(op_return_data);
        let data_value = op_return_script.minimal_non_dust_custom(fee_rate);

        let data_tx_out = bitcoin::TxOut {
            value: data_value,
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

        let fee_rate = get_fee_rate(rpc, None, None);
        let prefund_tx = self.to_tx(fee_rate, multisig_address, &release_address)?;

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
    const DATA_LEN: usize = Self::FUND_TAG_LEN + SubnetId::INNER_LEN + ETH_ADDR_LEN;

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
        let (txid_bytes, addr_bytes) = rest.split_at(SubnetId::INNER_LEN);

        // Verify tag
        if tag != IPC_FUND_SUBNET_TAG.as_bytes() {
            return Err(err(format!(
                "Invalid tag: got '{}', expected '{}'",
                String::from_utf8_lossy(tag),
                IPC_FUND_SUBNET_TAG
            )));
        }

        // Convert txid bytes to Txid
        let txid20 = Txid20::from_slice(txid_bytes)
            .map_err(|e| err(format!("Invalid txid bytes: {}", e)))?;
        let subnet_id = SubnetId::from_txid20(&txid20);

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
        fee_rate: bitcoin::FeeRate,
        multisig_address: &bitcoin::Address,
    ) -> Result<Transaction, IpcLibError> {
        //
        // Create the first output: op_return with
        // ipc tag, subnet_id (txid) and user's subnet address to fund
        //
        let fund_tag: [u8; Self::FUND_TAG_LEN] = IPC_FUND_SUBNET_TAG
            .as_bytes()
            .try_into()
            .expect("IPC_FUND_SUBNET_TAG has incorrect length");
        let subnet_id_txid: [u8; SubnetId::INNER_LEN] = self.subnet_id.txid20();
        let subnet_addr: [u8; ETH_ADDR_LEN] = self.address.into_array();

        // Construct op_return data
        let mut op_return_data = [0u8; Self::FUND_TAG_LEN + SubnetId::INNER_LEN + ETH_ADDR_LEN];

        op_return_data[0..Self::FUND_TAG_LEN].copy_from_slice(&fund_tag);
        op_return_data[Self::FUND_TAG_LEN..(Self::FUND_TAG_LEN + SubnetId::INNER_LEN)]
            .copy_from_slice(&subnet_id_txid);
        op_return_data[(Self::FUND_TAG_LEN + SubnetId::INNER_LEN)..].copy_from_slice(&subnet_addr);

        // Make op_return script and txout
        let op_return_script = bitcoin_utils::make_op_return_script(op_return_data);
        let data_value = op_return_script.minimal_non_dust_custom(fee_rate);
        let data_tx_out = bitcoin::TxOut {
            value: data_value,
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

        let fee_rate = get_fee_rate(rpc, None, None);
        let fund_tx = self.to_tx(fee_rate, multisig_address)?;

        // Construct, fund and sign the prefund transaction
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcWithdrawal {
    /// The amount to withdraw
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub amount: Amount,
    /// The address to withdraw to
    pub address: bitcoin::Address<NetworkUnchecked>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcCrossSubnetTransfer {
    /// The amount to transfer
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub amount: Amount,
    /// The destination subnet id.
    /// This is derived from the address
    /// if a subnet exists with that address
    //
    // TODO: should this be optional?
    // it would be considered invalid without it
    // but should we invalidate the entire checkpoint because of this?
    // maybe the subnet was killed in the meantime
    pub destination_subnet_id: SubnetId,
    /// The address of the subnet
    #[serde(skip_deserializing)]
    pub subnet_multisig_address: Option<bitcoin::Address<NetworkUnchecked>>,
    /// The address to transfer to
    pub subnet_user_address: alloy_primitives::Address,
}

/// Checkpoint message for a subnet
/// Note: currently the maximum number of withdrawals and transfers is 255 each
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcCheckpointSubnetMsg {
    /// The subnet id
    pub subnet_id: SubnetId,
    /// The checkpoint hash
    pub checkpoint_hash: bitcoin::hashes::sha256::Hash,
    /// The checkpoint height of child chain
    pub checkpoint_height: u64,
    /// Withdrawals
    #[serde(default)]
    pub withdrawals: Vec<IpcWithdrawal>,
    /// Cross-subnet transfers
    #[serde(default)]
    pub transfers: Vec<IpcCrossSubnetTransfer>,
    /// Optional change address (multisig)
    #[serde(skip_deserializing)]
    pub change_address: Option<bitcoin::Address<NetworkUnchecked>>,
}

impl IpcValidate for IpcCheckpointSubnetMsg {
    /// Validates the checkpoint message
    fn validate(&self) -> Result<(), IpcValidateError> {
        if self.checkpoint_height == 0 {
            return Err(IpcValidateError::InvalidField(
                "checkpoint_height",
                "Checkpoint height must be greater than zero".to_string(),
            ));
        }

        // TODO validate that checkpoint height no duplicates?

        // Ensure number of withdrawals and transfers doesn't exceed u8::MAX (255)
        if self.withdrawals.len() > u8::MAX as usize {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Number of withdrawals ({}) exceeds maximum allowed ({})",
                self.withdrawals.len(),
                u8::MAX
            )));
        }

        // Check if the withdrawals are valid
        for withdrawal in &self.withdrawals {
            if withdrawal.amount == Amount::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "withdrawal.amount",
                    "Withdrawal amount must be greater than zero".to_string(),
                ));
            }
            if !withdrawal.address.is_valid_for_network(NETWORK) {
                return Err(IpcValidateError::InvalidField(
                    "withdrawal.address",
                    format!(
                        "Bitcoin address {:?} must be for the current network",
                        withdrawal.address
                    ),
                ));
            }
        }

        if self.transfers.len() > u8::MAX as usize {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Number of transfers ({}) exceeds maximum allowed ({})",
                self.transfers.len(),
                u8::MAX
            )));
        }

        // Check if the transfers are valid
        for transfer in &self.transfers {
            if transfer.amount == Amount::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "transfer.amount",
                    "Transfer amount must be greater than zero".to_string(),
                ));
            }
        }

        // Check if the change address is valid
        if let Some(change_address) = &self.change_address {
            if !change_address.is_valid_for_network(NETWORK) {
                return Err(IpcValidateError::InvalidField(
                    "change_address",
                    format!(
                        "Bitcoin address {:?} must be for the current network",
                        change_address
                    ),
                ));
            }
        }

        Ok(())
    }
}

impl IpcCheckpointSubnetMsg {
    // Length of the withdrawal + transfer markers, 1 byte each
    const MARKERS_LEN: usize = 2;
    // u64 length
    const HEIGHT_LEN: usize = std::mem::size_of::<u64>();
    // The total length of the op_return data - helper
    const DATA_LEN: usize = IPC_TAG_LENGTH
        + SubnetId::INNER_LEN
        + bitcoin::hashes::sha256::Hash::LEN
        + Self::MARKERS_LEN
        + Self::HEIGHT_LEN;

    const TAG_OFFSET: usize = 0;
    const TXID_OFFSET: usize = Self::TAG_OFFSET + IPC_TAG_LENGTH;
    const HASH_OFFSET: usize = Self::TXID_OFFSET + SubnetId::INNER_LEN;
    const HEIGHT_OFFSET: usize = Self::HASH_OFFSET + bitcoin::hashes::sha256::Hash::LEN;
    const MARKERS_OFFSET: usize = Self::HEIGHT_OFFSET + Self::HEIGHT_LEN;

    fn make_metadata_tx_out(&self, fee_rate: bitcoin::FeeRate) -> bitcoin::TxOut {
        let mut op_return_data = [0u8; Self::DATA_LEN];

        // Copy tag
        op_return_data[Self::TAG_OFFSET..Self::TXID_OFFSET]
            .copy_from_slice(IPC_CHECKPOINT_TAG.as_bytes());

        // Copy subnet ID txid
        op_return_data[Self::TXID_OFFSET..Self::HASH_OFFSET]
            .copy_from_slice(&self.subnet_id.txid20());

        // Copy checkpoint hash
        op_return_data[Self::HASH_OFFSET..Self::HEIGHT_OFFSET]
            .copy_from_slice(&self.checkpoint_hash.to_byte_array());

        // Add checkpoint height
        op_return_data[Self::HEIGHT_OFFSET..Self::MARKERS_OFFSET]
            .copy_from_slice(&self.checkpoint_height.to_le_bytes());

        // Set marker values
        op_return_data[Self::MARKERS_OFFSET] = self.withdrawals.len().min(255) as u8; // Withdrawal count
        op_return_data[Self::MARKERS_OFFSET + 1] = self.transfers.len().min(255) as u8; // Transfer count

        let push_bytes: &bitcoin::script::PushBytes =
            (&op_return_data[..]).try_into().expect("the size is okay");

        let op_return_script = bitcoin_utils::make_op_return_script(push_bytes);
        let op_return_value = op_return_script.minimal_non_dust_custom(fee_rate);

        bitcoin::TxOut {
            value: op_return_value,
            script_pubkey: op_return_script,
        }
    }

    pub fn extract_markers_from_metadata_tx_out(
        tx_out: &bitcoin::TxOut,
    ) -> Result<(u8, u8), IpcLibError> {
        let err = |msg: String| IpcLibError::MsgParseError(IPC_CHECKPOINT_TAG, msg);

        // Get OP_RETURN data from first output
        let op_return_data = tx_out
            .script_pubkey
            .instructions_minimal()
            .find_map(|ins| match ins {
                Ok(bitcoin::blockdata::script::Instruction::PushBytes(data)) => {
                    Some(data.as_bytes())
                }
                _ => None,
            })
            .ok_or_else(|| err("First output must be OP_RETURN with pushdata".to_string()))?;

        // Check if the data length is correct
        if op_return_data.len() != Self::DATA_LEN {
            return Err(IpcLibError::MsgParseError(
                IPC_CHECKPOINT_TAG,
                format!(
					"Error while extracting markers, invalid metadata op_return data length: got {}, expected {}",
					op_return_data.len(),
					Self::DATA_LEN
				),
            ));
        }

        // Extract the markers
        let withdrawal_count = op_return_data[Self::MARKERS_OFFSET];
        let transfer_count = op_return_data[Self::MARKERS_OFFSET + 1];

        Ok((withdrawal_count, transfer_count))
    }

    fn make_batched_transfer_data(&self) -> Result<Vec<u8>, IpcLibError> {
        // Group transfers by destination subnet_id
        let mut transfers_by_subnet: HashMap<Txid20, Vec<(alloy_primitives::Address, Amount)>> =
            HashMap::new();

        for transfer in &self.transfers {
            let key = transfer.destination_subnet_id.txid20();
            let entry = transfers_by_subnet.entry(key).or_default();

            entry.push((transfer.subnet_user_address, transfer.amount));
        }

        let transfers_binary =
            bincode::serde::encode_to_vec(&transfers_by_subnet, bincode::config::standard())
                .map_err(|e| {
                    IpcLibError::from(IpcValidateError::InvalidMsg(format!(
                        "Failed to serialize transfers: {}",
                        e
                    )))
                })?;

        // Combine UTF-8 tag and binary data
        let mut complete_data = Vec::with_capacity(IPC_TRANSFER_TAG.len() + transfers_binary.len());
        complete_data.extend_from_slice(IPC_TRANSFER_TAG.as_bytes()); // UTF-8 tag prefix
        complete_data.extend_from_slice(&transfers_binary); // Binary data follows

        Ok(complete_data)
    }

    fn make_batched_transfer(
        &self,
        fee_rate: bitcoin::FeeRate,
        return_address: &bitcoin::Address,
    ) -> Result<(bitcoin::TxOut, bitcoin::Witness, bitcoin::TxOut), IpcLibError> {
        let batched_transfer_data = self.make_batched_transfer_data()?;
        let secp = bitcoin::secp256k1::Secp256k1::new();

        // Construct the script that will contain the data
        let commit_script = bitcoin_utils::make_push_data_script(batched_transfer_data.as_slice());

        let unspendable_pubkey = bitcoin_utils::unspenable_internal_key();
        let builder = bitcoin::taproot::TaprootBuilder::new()
            .add_leaf(0, commit_script.clone())
            .map_err(bitcoin_utils::BitcoinUtilsError::TaprootBuilderError)?;
        let commit_spend_info = builder
            .finalize(&secp, unspendable_pubkey)
            .map_err(|_| bitcoin_utils::BitcoinUtilsError::TaprootBuilderNotFinalizable)?;

        let commit_script_pubkey = bitcoin::script::ScriptBuf::new_p2tr(
            &secp,
            commit_spend_info.internal_key(),
            commit_spend_info.merkle_root(),
        );

        // Reveal transaction info

        let control_block = commit_spend_info
            .control_block(&(
                commit_script.clone(),
                bitcoin::taproot::LeafVersion::TapScript,
            ))
            .ok_or(bitcoin_utils::BitcoinUtilsError::CannotConstructControlBlock)?;

        let reveal_witness =
            bitcoin::Witness::from_slice(&[commit_script.to_bytes(), control_block.serialize()]);

        let reveal_tx_out = bitcoin::TxOut {
            value: return_address
                .script_pubkey()
                .minimal_non_dust_custom(fee_rate),
            script_pubkey: return_address.script_pubkey(),
        };

        // Make the reveal transaction to calculate weight

        let reveal_tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint {
                    // This will be replaced by the commit txid after
                    // constructing the checkpoint transaction
                    txid: bitcoin::Txid::all_zeros(),
                    vout: 0,
                },
                witness: reveal_witness.clone(),
                ..Default::default()
            }],
            output: vec![reveal_tx_out.clone()],
        };

        // Get the weight of the reveal transaction
        let reveal_tx_weight = reveal_tx.weight();

        // Get the reveal transaction fee from the current fee rate
        // FeeRate x Weight = Fee
        let reveal_tx_fee = fee_rate.fee_wu(reveal_tx_weight).ok_or(
            bitcoin_utils::BitcoinUtilsError::FeeRateOverflow(fee_rate, reveal_tx_weight),
        )?;

        trace!("reveal_tx_fee={reveal_tx_fee}");

        let commit_tx_out = bitcoin::TxOut {
            // Add enough sats to cover the reveal tx output and fee
            value: reveal_tx_out.value + reveal_tx_fee,
            script_pubkey: commit_script_pubkey,
        };

        Ok((commit_tx_out, reveal_witness, reveal_tx_out))
    }

    /// For each transfer, given the subnet id, set the subnet
    /// multisig address.
    pub fn update_subnets_for_transfer<D: db::Database>(
        &mut self,
        db: &D,
    ) -> Result<(), IpcLibError> {
        // For each transfer, look up the target subnet and get its multisig address
        for transfer in &mut self.transfers {
            if let Some(subnet_state) = db.get_subnet_state(transfer.destination_subnet_id)? {
                // Set the multisig address from the subnet state
                transfer.subnet_multisig_address =
                    Some(subnet_state.committee.multisig_address.clone());
            } else {
                return Err(IpcValidateError::InvalidField(
                    "destination_subnet_id",
                    format!("Subnet {} does not exist", transfer.destination_subnet_id),
                )
                .into());
            }
        }

        Ok(())
    }

    /// Returns the vout of the batched transfer commit output, if present
    /// Useful for constructing the reveal transaction
    fn batch_transfer_commit_outpoint(&self, txid: bitcoin::Txid) -> Option<bitcoin::OutPoint> {
        if self.transfers.is_empty() {
            return None;
        }

        // Calculate batch_transfer vout based on the number of withdrawals
        // position after the metadata output and all withdrawal outputs
        // i.e., 1 (metadata) + withdrawals.len() if present
        let vout = 1 + self.withdrawals.len() as u32;

        Some(bitcoin::OutPoint { txid, vout })
    }

    /// Makes a batched transfer reveal transaction
    pub fn make_reveal_batch_transfer_tx(
        &self,
        checkpoint_txid: Txid,
        fee_rate: bitcoin::FeeRate,
        return_address: &bitcoin::Address,
    ) -> Result<Option<Transaction>, IpcLibError> {
        if self.transfers.is_empty() {
            return Ok(None);
        }

        let checkpoint_tx_outpoint = self
            .batch_transfer_commit_outpoint(checkpoint_txid)
            .expect("Batched transfer commit output must be present");

        let (_, reveal_witness, reveal_tx_out) =
        // Send any sats in the reveal transaction
            self.make_batched_transfer(fee_rate, return_address)?;

        // Make the reveal transaction to calculate weight

        let reveal_tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: checkpoint_tx_outpoint,
                witness: reveal_witness,
                ..Default::default()
            }],
            output: vec![reveal_tx_out],
        };

        // #[cfg(test)]
        // dbg!(&reveal_tx);

        Ok(Some(reveal_tx))
    }

    /// Makes a unsigned checkpoint transaction that includes checkpoint data
    /// withdrawals and transfers
    pub fn to_checkpoint_psbt(
        &self,
        committee: &db::SubnetCommittee,
        fee_rate: bitcoin::FeeRate,
        unspent: &[bitcoincore_rpc::json::ListUnspentResultEntry],
    ) -> Result<bitcoin::Psbt, IpcLibError> {
        debug!(
            "Creating checkpoint transactions for subnet_id={}",
            self.subnet_id
        );

        let mut tx_outs = vec![];

        //
        // Add first output, the metadata
        //

        let data_tx_out = self.make_metadata_tx_out(fee_rate);
        tx_outs.push(data_tx_out);

        //
        // Add withdrawal outputs
        //
        for withdrawal in &self.withdrawals {
            tx_outs.push(bitcoin::TxOut {
                value: withdrawal.amount,
                script_pubkey: withdrawal
                    .address
                    .clone()
                    // safe to assume it's checked and panic otherwise
                    // because of the validation beforehand
                    .require_network(NETWORK)
                    .expect("Address must be valid for network")
                    .script_pubkey(),
            });
        }

        //
        // Add batched transfer output commit tx, if present
        //
        let has_transfers = !self.transfers.is_empty();
        if has_transfers {
            let (batch_transfer_tx_out, _, _) =
                self.make_batched_transfer(fee_rate, &committee.address_checked())?;

            // Push commit tx output
            tx_outs.push(batch_transfer_tx_out);

            // Add transfers outputs
            for transfer in &self.transfers {
                tx_outs.push(bitcoin::TxOut {
                    value: transfer.amount,
                    script_pubkey: transfer
                        .subnet_multisig_address
                        .clone()
                        // this should not happen as we fill it
                        // in update_subnets_for_transfer
                        // and it's always defined in the transaction outputs
                        .ok_or(IpcValidateError::InvalidField(
                            "transfer.subnet_multisig_address",
                            "subnet_multisig_address must be defined".to_string(),
                        ))?
                        // safe to assume it's checked and panic otherwise
                        // because of the validation beforehand
                        .require_network(NETWORK)
                        .expect("Address must be valid for network")
                        .script_pubkey(),
                });
            }
        }

        //
        // Create the checkpoint unsigned transaction
        //

        let committee_keys = committee.validator_weighted_keys();

        let checkpoint_tx = multisig::construct_spend_unsigned_transaction(
            &committee_keys,
            committee.threshold,
            &committee.address_checked(),
            unspent,
            &tx_outs,
            &fee_rate,
        )?;

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let checkpoint_psbt = multisig::construct_spend_psbt(
            &secp,
            &self.subnet_id,
            &committee_keys,
            committee.threshold,
            &committee.address_checked(),
            unspent,
            &tx_outs,
            &fee_rate,
        )?;

        debug!("Checkpoint TX: {checkpoint_tx:?}");
        // dbg!(&checkpoint_tx);
        // dbg!(&checkpoint_psbt);

        assert_eq!(
            checkpoint_tx.compute_txid(),
            checkpoint_psbt.unsigned_tx.compute_txid()
        );

        Ok(checkpoint_psbt)
    }

    /// Reconstructs an IpcCheckpointSubnetMsg from a checkpoint transaction.
    ///
    /// The checkpoint transaction has:
    /// 1. An OP_RETURN output with metadata in the format:
    ///    [checkpoint tag | 32-byte subnet ID txid | 32-byte checkpoint hash | 1-byte withdrawal count | 1-byte transfer count]
    /// 2. Withdrawal outputs for each withdrawal
    /// 3. Optional batch transfer commit output if transfers exist
    /// 4. Transfer outputs for each transfer
    ///
    /// Returns an error if the transaction doesn't match the expected format.
    pub fn from_checkpoint_tx<D: db::Database>(
        db: &D,
        tx: &Transaction,
    ) -> Result<Self, IpcLibError> {
        use bitcoin::blockdata::script::Instruction;

        // Helper closure for error creation
        let err = |msg: String| IpcLibError::MsgParseError(IPC_CHECKPOINT_TAG, msg);

        // Verify we have at least one output (the metadata output)
        if tx.output.is_empty() {
            return Err(err("Transaction must have at least one output".into()));
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

        // Verify tag
        let tag = &op_return_data[Self::TAG_OFFSET..Self::TXID_OFFSET];
        if tag != IPC_CHECKPOINT_TAG.as_bytes() {
            return Err(err(format!(
                "Invalid tag: got '{}', expected '{}'",
                String::from_utf8_lossy(tag),
                IPC_CHECKPOINT_TAG
            )));
        }

        // Extract subnet ID
        let txid_bytes = &op_return_data[Self::TXID_OFFSET..Self::HASH_OFFSET];
        let txid20 = Txid20::from_slice(txid_bytes)
            .map_err(|e| err(format!("Invalid subnet ID bytes: {}", e)))?;
        let subnet_id = SubnetId::from_txid20(&txid20);

        // Extract checkpoint hash
        let hash_bytes = &op_return_data[Self::HASH_OFFSET..Self::HEIGHT_OFFSET];
        let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_slice(hash_bytes)
            .map_err(|e| err(format!("Invalid checkpoint hash bytes: {}", e)))?;

        let height_bytes = &op_return_data[Self::HEIGHT_OFFSET..Self::MARKERS_OFFSET];
        let checkpoint_height =
            u64::from_le_bytes(height_bytes.try_into().map_err(|_| {
                err("Failed to convert checkpoint height bytes to u64".to_string())
            })?);

        // Extract marker values
        let withdrawals_count = op_return_data[Self::MARKERS_OFFSET] as usize;
        let transfers_count = op_return_data[Self::MARKERS_OFFSET + 1] as usize;

        // Check if we have enough outputs for all the withdrawals and transfers
        let expected_outputs = 1 + // metadata
               withdrawals_count + // withdrawals
               (if transfers_count > 0 { 1 } else { 0 }) + // batch transfer commit (if needed)
               transfers_count; // transfers

        if tx.output.len() < expected_outputs {
            return Err(err(format!(
                "Not enough outputs: got {}, expected at least {}",
                tx.output.len(),
                expected_outputs
            )));
        }

        // Parse withdrawals
        let mut withdrawals = Vec::with_capacity(withdrawals_count);
        for i in 0..withdrawals_count {
            let txout = &tx.output[1 + i]; // 1-based index (after metadata)
            let amount = txout.value;
            let address = bitcoin::Address::from_script(&txout.script_pubkey, NETWORK)
                .map_err(|_| err(format!("Could not parse address from output {}", 1 + i)))?;
            let address = address.into_unchecked();

            withdrawals.push(IpcWithdrawal { amount, address });
        }

        // Parse transfers
        let mut transfers = Vec::with_capacity(transfers_count);

        // If there are transfers, there should be a batch transfer commit output
        if transfers_count > 0 {
            // Start after metadata + withdrawals + batch commit
            let transfer_start_index = 1 + withdrawals_count + 1;

            for i in 0..transfers_count {
                let txout = &tx.output[transfer_start_index + i];
                let amount = txout.value;

                // For transfers, we can only extract the multisig address from the output
                // The subnet_id and user_address would need to be provided by the batch reveal transaction
                let subnet_multisig_address =
                    bitcoin::Address::from_script(&txout.script_pubkey, NETWORK).map_err(|_| {
                        err(format!(
                            "Could not parse address from transfer output {}",
                            transfer_start_index + i
                        ))
                    })?;
                let subnet_multisig_address = subnet_multisig_address.into_unchecked();

                let destination_subnet_id = match db
                    .get_subnet_by_multisig_address(&subnet_multisig_address)
                {
                    Ok(Some(subnet)) => subnet.id,
                    Ok(None) => {
                        error!("CheckpointSubnetMsg: Could not find subnet with multisig address {:?} for transfer.", subnet_multisig_address);
                        SubnetId::default()
                    }
                    Err(e) => {
                        return Err(err(format!(
                            "CheckpointSubnetMsg: Error while looking up subnet with multisig address {:?}: {}",
                            subnet_multisig_address, e
                        )));
                    }
                };

                // Note: We're creating placeholder values for subnet ID and user address
                // These will need to be filled in by parsing the batch reveal transaction
                transfers.push(IpcCrossSubnetTransfer {
                    amount,
                    destination_subnet_id,
                    subnet_multisig_address: Some(subnet_multisig_address),
                    // Placeholder, will be filled in batch transfer tx
                    subnet_user_address: alloy_primitives::Address::ZERO,
                });
            }
        }

        // Extract the change address if present (should be the last output)
        let change_address = if tx.output.len() > expected_outputs {
            let change_output = &tx.output[tx.output.len() - 1];
            let addr = bitcoin::Address::from_script(&change_output.script_pubkey, NETWORK)
                .map_err(|_| err("Could not parse change address from last output".to_string()))?;
            let addr = addr.into_unchecked();
            Some(addr)
        } else {
            None
        };

        Ok(Self {
            subnet_id,
            checkpoint_hash,
            checkpoint_height,
            withdrawals,
            transfers,
            change_address,
        })
    }

    /// Saves the checkpoint message to the database.
    ///
    /// This method validates the message against the subnet state, creates a checkpoint record,
    /// and stores it in the database. It also updates the last checkpoint number in the subnet state.
    ///
    /// Returns the created checkpoint record.
    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        txid: Txid,
        // batch_transfer_txid: Option<Txid>,
    ) -> Result<db::SubnetCheckpoint, IpcLibError> {
        // Get the current subnet state
        let mut subnet_state = db.get_subnet_state(self.subnet_id)?.ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!("Subnet ID {} does not exist", self.subnet_id))
        })?;

        let checkpoint_number = subnet_state.last_checkpoint_number.map_or(0, |n| n + 1);

        // Create a new checkpoint record
        let checkpoint = db::SubnetCheckpoint {
            checkpoint_number,
            checkpoint_hash: self.checkpoint_hash,
            checkpoint_height: self.checkpoint_height,
            block_height,
            txid,
            // Will be updated when batch transfer is confirmed
            batch_transfer_txid: None,
            batch_transfer_block_height: None,
            signed_committee_number: subnet_state.committee_number,
            // No committee rotation yet
            next_committee_number: subnet_state.committee_number,
        };

        // Update the checkpoint number in subnet state
        subnet_state.last_checkpoint_number = Some(checkpoint_number);

        // Begin a database transaction
        let mut wtxn = db.write_txn()?;
        // Save the updated subnet state
        db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet_state)?;
        // Save the checkpoint record
        db.save_checkpoint(&mut wtxn, self.subnet_id, &checkpoint, checkpoint_number)?;

        // Commit the transaction
        wtxn.commit()?;

        debug!(
            "Saved checkpoint #{} for subnet {} with txid {}",
            checkpoint_number, self.subnet_id, txid
        );

        Ok(checkpoint)
    }
}

/// Batch transfer message
/// It lacks important information so it must be
/// fetch from the checkpoint transaction and validated
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcBatchTransferMsg {
    /// Subnet ID
    pub subnet_id: SubnetId,
    /// The checkpoint txid
    pub checkpoint_txid: bitcoin::Txid,
    /// The checkpoint vout
    pub checkpoint_vout: u32,
    /// Cross-subnet transfers
    pub transfers: Vec<IpcCrossSubnetTransfer>,
}

impl IpcValidate for IpcBatchTransferMsg {
    /// Validates the batch transfer message
    fn validate(&self) -> Result<(), IpcValidateError> {
        if self.checkpoint_txid == bitcoin::Txid::all_zeros() {
            return Err(IpcValidateError::InvalidField(
                "checkpoint_txid",
                "Checkpoint txid must not be all zeros".to_string(),
            ));
        }

        if self.transfers.is_empty() {
            return Err(IpcValidateError::InvalidMsg(
                "Batch transfer message must have at least one transfer".to_string(),
            ));
        }

        // Check if the transfers are valid
        for transfer in &self.transfers {
            if transfer.amount == Amount::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "transfer.amount",
                    "Transfer amount must be greater than zero".to_string(),
                ));
            }

            if transfer.subnet_user_address == alloy_primitives::Address::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "transfer.subnet_user_address",
                    "Transfer address must not be zero".to_string(),
                ));
            }
        }

        Ok(())
    }
}

impl IpcBatchTransferMsg {
    /// Reconstructs an IpcBatchTransferMsg from a Bitcoin transaction.
    ///
    /// Given that:
    /// - The transaction is a batch transfer reveal transaction
    /// - The input points to a checkpoint transaction's batch transfer commit output
    /// - The witness contains our batch transfer data with IPC:TFR tag prefix
    ///
    /// Returns an error if the expected data is missing or malformed.
    pub fn from_tx<D: db::Database>(db: &D, tx: &Transaction) -> Result<Self, IpcLibError> {
        // Helper closure for error creation
        let err = |msg: String| IpcLibError::MsgParseError(IPC_TRANSFER_TAG, msg);

        // Verify we have at least one input
        if tx.input.len() != 1 {
            return Err(err("Transaction must have at least one input".into()));
        }

        // Get the checkpoint transaction reference
        let checkpoint_txid = tx.input[0].previous_output.txid;
        let checkpoint_vout = tx.input[0].previous_output.vout;

        // Extract witness data from the input
        if tx.input[0].witness.is_empty() {
            return Err(err("Transaction witness is empty".into()));
        }

        // Try to get the witness data
        let witness_data = match bitcoin_utils::concatenate_op_push_data(&tx.input[0].witness[0]) {
            Ok(data) => data,
            Err(_) => return Err(err("Failed to extract witness data".into())),
        };

        // Verify tag
        if witness_data.len() < IPC_TAG_LENGTH
            || &witness_data[..IPC_TAG_LENGTH] != IPC_TRANSFER_TAG.as_bytes()
        {
            return Err(err(format!(
                "Invalid tag: expected '{}' prefix",
                IPC_TRANSFER_TAG
            )));
        }

        // Deserialize the transfers
        let (transfers_by_subnet, _): (
            HashMap<Txid20, Vec<(alloy_primitives::Address, Amount)>>,
            usize,
        ) = bincode::serde::decode_from_slice(
            &witness_data[IPC_TRANSFER_TAG.len()..],
            bincode::config::standard(),
        )
        .map_err(|e| err(format!("Failed to deserialize transfers: {}", e)))?;

        // Collect all cross-subnet transfers with proper subnet info
        let transfers = transfers_by_subnet
            .into_iter()
            .filter_map(|(txid20, transfer_list)| {
                let destination_subnet_id = SubnetId::from_txid20(&txid20);

                // Try to get the subnet state
                match db.get_subnet_state(destination_subnet_id) {
                    Ok(Some(subnet)) => {
                        // If we have a subnet, return its address and transfers
                        let subnet_multisig_address = subnet.committee.multisig_address;
                        Some((destination_subnet_id, subnet_multisig_address, transfer_list))
                    }
                    _ => {
	                    // TODO how should we process this?
	                    // we have to find all previous subnet multisigs
                        warn!(
                            "Could not find subnet id={} while processing batch transfer, skipping.",
                            destination_subnet_id
                        );
                        None
                    }
                }
            })
            .flat_map(|(destination_subnet_id, subnet_multisig_address, transfer_list)| {
                // Convert each transfer in the list to an IpcCrossSubnetTransfer
                transfer_list.into_iter().map(move |(subnet_user_address, amount)| {
                    IpcCrossSubnetTransfer {
                        amount,
                        destination_subnet_id,
                        subnet_multisig_address: Some(subnet_multisig_address.clone()),
                        subnet_user_address,
                    }
                })
            })
            .collect::<Vec<_>>();

        if transfers.is_empty() {
            return Err(err("No valid transfers found".into()));
        }

        Ok(Self {
            // Filled in later
            subnet_id: SubnetId::default(),
            checkpoint_txid,
            checkpoint_vout,
            transfers,
        })
    }

    pub fn validate_for_checkpoint(
        &self,
        checkpoint_tx: &Transaction,
    ) -> Result<(), IpcValidateError> {
        // Verify we have the right checkpoint txid
        if checkpoint_tx.compute_txid() != self.checkpoint_txid {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Checkpoint txid mismatch: expected {}, got {}",
                self.checkpoint_txid,
                checkpoint_tx.compute_txid()
            )));
        }

        // Verify the batch transfer commit output exists at the expected vout
        if checkpoint_tx.output.len() <= self.checkpoint_vout as usize {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Checkpoint transaction doesn't have output at index {}",
                self.checkpoint_vout
            )));
        }

        let metadata_tx_out = checkpoint_tx
            .output
            .first()
            .ok_or(IpcValidateError::InvalidMsg(format!(
                "Checkpoint transaction doesn't have output at index {}",
                self.checkpoint_vout
            )))?;

        // Extract withdrawal and transfer counts from checkpoint metadata
        let (_withdrawal_count, transfer_count) =
            match IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(metadata_tx_out) {
                Ok(counts) => counts,
                Err(e) => {
                    return Err(IpcValidateError::InvalidMsg(format!(
                        "Failed to extract counts from checkpoint: {}",
                        e
                    )))
                }
            };

        // Check if the number of transfers matches
        if self.transfers.len() != transfer_count as usize {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Transfer count mismatch: batch has {}, checkpoint expected {}",
                self.transfers.len(),
                transfer_count
            )));
        }

        // TODO check transfers have exact output

        Ok(())
    }

    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<db::SubnetCheckpoint, IpcLibError> {
        let source_subnet_id = self.subnet_id;

        // Find the checkpoint's number in the source subnet
        let source_subnet_state = db.get_subnet_state(source_subnet_id)?.ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!("Source subnet {} not found", source_subnet_id))
        })?;

        let checkpoint_number = source_subnet_state.last_checkpoint_number.ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!("Subnet {} has no checkpoints", source_subnet_id))
        })?;

        // Get the checkpoint
        let mut checkpoint = db
            .get_checkpoint(source_subnet_id, checkpoint_number)?
            .ok_or_else(|| {
                IpcValidateError::InvalidMsg(format!(
                    "Checkpoint #{} for subnet {} not found",
                    checkpoint_number, source_subnet_id
                ))
            })?;

        // Update the checkpoint with the batch transfer information
        checkpoint.batch_transfer_txid = Some(txid);
        checkpoint.batch_transfer_block_height = Some(block_height);

        let next_checkpoint_number = source_subnet_state
            .last_checkpoint_number
            .map(|n| n + 1)
            .unwrap_or(0);

        // Start a database transaction
        let mut wtxn = db.write_txn()?;

        // Save the updated checkpoint
        db.save_checkpoint(
            &mut wtxn,
            source_subnet_id,
            &checkpoint,
            next_checkpoint_number,
        )?;

        // Process each transfer and create rootnet messages for destination subnets
        for transfer in &self.transfers {
            // Create a fund message for the transfer
            let fund_msg = IpcFundSubnetMsg {
                subnet_id: transfer.destination_subnet_id,
                amount: transfer.amount,
                address: transfer.subnet_user_address,
            };

            // Get the next nonce for the destination subnet
            let nonce = db.get_next_rootnet_msg_nonce_txn(&wtxn, transfer.destination_subnet_id)?;

            // Create the rootnet message
            let rootnet_msg = db::RootnetMessage::FundSubnet {
                msg: fund_msg,
                nonce,
                block_height,
                block_hash,
                txid,
            };

            // Add the rootnet message to the destination subnet
            db.add_rootnet_msg(&mut wtxn, transfer.destination_subnet_id, rootnet_msg)?;

            debug!(
                "Added fund message for subnet {} from batch transfer, amount: {}, address: {}",
                transfer.destination_subnet_id, transfer.amount, transfer.subnet_user_address
            );
        }

        // Commit all changes
        wtxn.commit()?;

        Ok(checkpoint)
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
    CheckpointSubnet(IpcCheckpointSubnetMsg),
    BatchTransfer(IpcBatchTransferMsg),
}

impl IpcMessage {
    pub fn from_witness(w: Vec<u8>) -> Result<Self, IpcSerializeError> {
        let tag = if w.len() >= IPC_TAG_LENGTH {
            &w[..IPC_TAG_LENGTH]
        } else {
            return Err(IpcSerializeError::DeserializationError(
                "Message too short for a valid tag".to_string(),
            ));
        };
        let tag = std::str::from_utf8(tag).map_err(|e| {
            IpcSerializeError::DeserializationError(format!("Could not deserialize tag: {}", e))
        })?;

        // we keep this a result since not all messages need it
        let wstr = std::str::from_utf8(&w).map_err(|_| {
            IpcSerializeError::DeserializationError("Could not deserialize witness".to_string())
        });

        match IpcTag::from_str(tag)? {
            IpcTag::CreateSubnet => Ok(IpcMessage::CreateSubnet(
                IpcCreateSubnetMsg::ipc_deserialize(wstr?)?,
            )),

            IpcTag::JoinSubnet => Ok(IpcMessage::JoinSubnet(IpcJoinSubnetMsg::ipc_deserialize(
                wstr?,
            )?)),
            //
            // The bellow messages will be processed from output
            //
            IpcTag::BatchTransfer => {
                Err(IpcSerializeError::DeserializationError("Skip".to_string()))
            }
            IpcTag::CheckpointSubnet => {
                Err(IpcSerializeError::DeserializationError("Skip".to_string()))
            }
            IpcTag::PrefundSubnet => {
                Err(IpcSerializeError::DeserializationError("Skip".to_string()))
            }
            IpcTag::FundSubnet => Err(IpcSerializeError::DeserializationError("Skip".to_string())),
        }
    }
}

//
// Subnet ID
//

pub const L1_DELEGATED_NAMESPACE: u64 = 10;

/// Create Subnet IPC message is sent as a commit-reveal transaction pair.
/// Subnet ID is derived from the transaction ID of the reveal transaction.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SubnetId(FvmAddress);

#[derive(Debug, Error)]
pub enum SubnetIdError {
    #[error("Invalid Subnet Id format. Expected '{0}/<addr>', got '{1}'")]
    InvalidFormat(&'static str, String),
    #[error("Invalid delegated address: {0}")]
    InvalidFvmAddress(#[from] fvm_shared::address::Error),
}

/// First 20 bytes of a transaction ID
pub type Txid20 = [u8; 20];

/// Helper methods for Txid20
pub trait Txid20Ext {
    /// Convert from hexadecimal string to Txid20
    fn from_hex(hex: &str) -> Result<Txid20, String>;

    /// Convert Txid20 to hexadecimal string
    fn to_hex(&self) -> String;

    /// Create a Txid20 from the first 20 bytes of a full Txid
    fn from_txid(txid: &Txid) -> Txid20;

    /// Create a Txid20 from a byte slice
    fn from_slice(slice: &[u8]) -> Result<Txid20, String>;

    /// Creates a Txid20 filled with zeros
    fn zeros() -> Txid20;

    /// Convert this Txid20 to a SubnetId
    fn to_subnet_id(&self) -> SubnetId;
}

impl Txid20Ext for Txid20 {
    fn from_hex(hex: &str) -> Result<Txid20, String> {
        if hex.len() != 40 {
            return Err(format!(
                "Invalid Txid20 hex length: {}, expected 40",
                hex.len()
            ));
        }

        let bytes = hex::decode(hex).map_err(|e| format!("Invalid hex: {}", e))?;

        Self::from_slice(&bytes)
    }

    fn to_hex(&self) -> String {
        hex::encode(self)
    }

    fn from_txid(txid: &Txid) -> Txid20 {
        let txid_bytes = txid.as_byte_array();
        let mut result = [0u8; 20];
        result.copy_from_slice(&txid_bytes[0..20]);
        result
    }

    fn from_slice(slice: &[u8]) -> Result<Txid20, String> {
        if slice.len() < 20 {
            return Err(format!(
                "Byte slice too short: length {}, expected at least 20 bytes",
                slice.len()
            ));
        }

        let mut result = [0u8; 20];
        result.copy_from_slice(&slice[0..20]);
        Ok(result)
    }

    fn zeros() -> Txid20 {
        [0u8; 20]
    }

    fn to_subnet_id(&self) -> SubnetId {
        SubnetId::from_txid20(self)
    }
}

impl SubnetId {
    pub const INNER_LEN: usize = 20;

    /// Creates a new SubnetId from a transaction ID
    pub fn from_txid(txid: &Txid) -> Self {
        let mut txid20: Txid20 = [0u8; 20];
        let txid_bytes = txid.as_byte_array();
        txid20.copy_from_slice(&txid_bytes[0..20]);

        let addr = FvmAddress::new_delegated(L1_DELEGATED_NAMESPACE, &txid20)
            .expect("txid is longer than 32 bytes, unreachable");
        Self(addr)
    }

    /// Creates a new SubnetId directly from a 20-byte transaction ID fragment
    pub fn from_txid20(txid20: &Txid20) -> Self {
        let addr = FvmAddress::new_delegated(L1_DELEGATED_NAMESPACE, txid20)
            .expect("txid20 is 20 bytes, should always be valid");
        Self(addr)
    }

    pub fn addr(&self) -> &FvmAddress {
        &self.0
    }

    // Getting the txid from the delegated address
    // It's impossible to create other types of addresses or with other data
    // So it's safe to panic to handle errors
    pub fn txid20(&self) -> Txid20 {
        let payload = self.0.payload();
        let sub_address = match payload {
            fvm_shared::address::Payload::Delegated(del_addr) => del_addr.subaddress(),
            _ => panic!("SubnetId doesn't have a delegated address"),
        };

        // Panic here since there's no other way to create a subnet id
        assert_eq!(
            sub_address.len(),
            Self::INNER_LEN,
            "SubnetId subaddress length mismatch: expected {}, got {}",
            Self::INNER_LEN,
            sub_address.len()
        );

        let mut txid20: Txid20 = [0u8; 20];
        txid20.copy_from_slice(&sub_address[..20]);

        txid20
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

impl std::fmt::Debug for SubnetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SubnetId({}/{})", crate::L1_NAME, self.0)
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
    use super::*;
    use crate::eth_utils::{
        delegated_fvm_to_eth_address, evm_address_to_delegated_fvm, set_fvm_network,
    };
    use crate::L1_NAME;
    use bitcoin::hex::DisplayHex;
    use fvm_shared::address::{set_current_network, Network as FvmNetwork};

    fn set_test_fvm_network() {
        set_current_network(FvmNetwork::Testnet);
    }

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
        set_test_fvm_network();
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
        set_test_fvm_network();

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
        set_test_fvm_network();

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
        set_test_fvm_network();

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
            "{}subnet_id={}/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei",
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
        set_test_fvm_network();

        let txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();
        let subnet_id = SubnetId::from_txid(&txid);
        let addr = subnet_id.addr();

        println!("{subnet_id} {addr}");

        let mut expected_txid20 = [0u8; 20];
        expected_txid20.copy_from_slice(&txid.as_byte_array()[0..20]);

        assert_eq!(subnet_id.txid20(), expected_txid20);
    }

    #[test]
    fn test_subnet_id_from_str() {
        set_test_fvm_network();
        let subnet_id_str = format!(
            "{}/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei",
            crate::L1_NAME
        );
        let subnet_id = SubnetId::from_str(&subnet_id_str).unwrap();

        assert_eq!(
            subnet_id.txid20().to_hex(),
            "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3"
        );
    }

    #[test]
    fn test_subnet_id_display() {
        set_test_fvm_network();
        let txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();
        let subnet_id = SubnetId::from_txid(&txid);

        assert_eq!(
            subnet_id.to_string(),
            format!(
                "{}/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei",
                crate::L1_NAME
            )
        );
    }

    #[test]
    fn test_invalid_subnet_id() {
        set_test_fvm_network();

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
        set_test_fvm_network();

        let txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();
        let subnet_id = SubnetId::from_txid(&txid);

        // Test JSON serialization
        let serialized = serde_json::to_string(&subnet_id).unwrap();
        let expected = format!(
            "\"{}/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei\"",
            crate::L1_NAME
        );
        assert_eq!(serialized, expected);

        // Test JSON deserialization
        let deserialized: SubnetId = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, subnet_id);
    }

    #[test]
    fn test_txid_to_subnet_id_to_eth_addr_conversion() {
        // Set FVM network for address formatting
        set_fvm_network();

        // 1. Start with a valid txid
        let txid =
            Txid::from_str("06426ef74c42f874ce7824c640074d0e0bd8a676b49e91ea567a5eb596cfb8cb")
                .unwrap();

        // 2. Create a SubnetId from the txid
        let subnet_id = SubnetId::from_txid(&txid);

        // Get the txid20 directly from Txid
        let txid20 = Txid20::from_txid(&txid);

        // Also get txid20 from subnet_id for comparison
        let subnet_txid20 = subnet_id.txid20();

        // These should match
        assert_eq!(
            txid20, subnet_txid20,
            "txid20 from Txid and from SubnetId should match"
        );

        // 3. Convert the txid20 to an Ethereum address
        let eth_addr = alloy_primitives::Address::from_slice(&txid20);

        // Verify the Ethereum address bytes match the txid20
        assert_eq!(
            eth_addr.as_slice(),
            &txid20,
            "Ethereum address bytes should match txid20"
        );

        // 4. Convert the Ethereum address to an FVM delegated address
        let eth_fvm_addr = evm_address_to_delegated_fvm(&eth_addr, L1_DELEGATED_NAMESPACE);

        // 5. Extract the Ethereum address back from the FVM address
        let recovered_eth_addr = delegated_fvm_to_eth_address(&eth_fvm_addr)
            .expect("Should be able to convert delegated address back to Ethereum address");

        // The recovered ETH address should match the original
        assert_eq!(
            recovered_eth_addr, eth_addr,
            "Recovered Ethereum address should match original"
        );

        // 6. Create a Subnet ID from the txid20
        let subnet_id_from_txid20 = SubnetId::from_txid20(&txid20);

        // 7. Create a Subnet ID directly from the Ethereum address
        let subnet_id_from_eth = {
            let txid20: Txid20 = eth_addr.into_array();
            SubnetId::from_txid20(&txid20)
        };

        // Both subnet IDs should match the original
        assert_eq!(
            subnet_id_from_txid20, subnet_id,
            "SubnetId from txid20 should match original"
        );
        assert_eq!(
            subnet_id_from_eth, subnet_id,
            "SubnetId from Ethereum address should match original"
        );

        // Verify the string representation
        println!("TXID: {}", txid);
        println!("Subnet ID: {}", subnet_id);
        println!("ETH Address: 0x{}", eth_addr.as_hex());
        println!("ETH FVM Address: {}", eth_fvm_addr);
        println!("Txid20 hex: {}", txid20.to_hex());
    }
}

#[cfg(test)]
mod prefund_msg_tests {
    use crate::DEFAULT_BTC_FEE_RATE;

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
        let tx = msg
            .to_tx(DEFAULT_BTC_FEE_RATE, &multisig_address, &release_address)
            .unwrap();

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
            .to_tx(DEFAULT_BTC_FEE_RATE, &multisig_address, &release_address)
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
            .to_tx(DEFAULT_BTC_FEE_RATE, &multisig_address, &release_address)
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
        // same length as "IPCPFD"
        let invalid_tag = "IPC123";
        // sanity check if we change tag length
        assert_eq!(invalid_tag.len(), IPC_TAG_LENGTH);
        wrong_data.extend_from_slice(invalid_tag.as_bytes()); // Different tag
        wrong_data.extend_from_slice(&[0u8; SubnetId::INNER_LEN]); // txid
        wrong_data.extend_from_slice(&[0u8; 20]); // address
        let wrong_data: [u8; IpcPrefundSubnetMsg::DATA_LEN] = wrong_data.try_into().unwrap();
        wrong_tag_tx.output[0].script_pubkey = bitcoin_utils::make_op_return_script(wrong_data);
        assert!(matches!(
            IpcPrefundSubnetMsg::from_tx(&wrong_tag_tx),
            Err(IpcLibError::MsgParseError(IPC_PREFUND_SUBNET_TAG, _))
        ));
    }
}

#[cfg(test)]
mod checkpoint_msg_tests {
    use super::*;
    use crate::{test_utils, DEFAULT_BTC_FEE_RATE};
    use std::str::FromStr;

    fn create_test_checkpoint_msg() -> IpcCheckpointSubnetMsg {
        // Generate a subnet with 3 validators
        let subnet_state = test_utils::generate_subnet(3);

        // Create a second subnet for cross-subnet transfers
        let destination_subnet = test_utils::generate_subnet(3);

        let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap();

        // Create a withdrawal
        let withdrawal = IpcWithdrawal {
            amount: Amount::from_sat(50000),
            address: bitcoin::Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
                .unwrap(),
        };

        // Create a cross-subnet transfer
        let transfer = IpcCrossSubnetTransfer {
            amount: Amount::from_sat(30000),
            destination_subnet_id: destination_subnet.id,
            subnet_multisig_address: Some(destination_subnet.committee.multisig_address.clone()),
            subnet_user_address: alloy_primitives::Address::from_str(
                "742d35Cc6634C0532925a3b844Bc454e4438f44e",
            )
            .unwrap(),
        };

        IpcCheckpointSubnetMsg {
            subnet_id: subnet_state.id,
            checkpoint_hash,
            checkpoint_height: 50,
            withdrawals: vec![withdrawal],
            transfers: vec![transfer],
            change_address: Some(subnet_state.committee.multisig_address.clone()),
        }
    }

    fn create_test_utxos(
        script_pub_key: bitcoin::ScriptBuf,
    ) -> Vec<bitcoincore_rpc::json::ListUnspentResultEntry> {
        vec![bitcoincore_rpc::json::ListUnspentResultEntry {
            txid: bitcoin::Txid::from_str(
                "f61b1742ca13176464adb3cb66050c00787bb3a4eead37e985f2df1e37718126",
            )
            .unwrap(),
            vout: 0,
            address: None,
            label: None,
            redeem_script: None,
            witness_script: None,
            script_pub_key,
            amount: bitcoin::Amount::from_sat(200000),
            confirmations: 10,
            spendable: true,
            solvable: true,
            descriptor: None,
            safe: true,
        }]
    }

    #[test]
    fn test_checkpoint_to_txs_basic() {
        let checkpoint_msg = create_test_checkpoint_msg();
        let subnet = test_utils::generate_subnet(3);
        let committee = &subnet.committee;
        let utxos = create_test_utxos(committee.address_checked().script_pubkey());

        let fee_rate = DEFAULT_BTC_FEE_RATE;

        // Generate transactions
        let checkpoint_psbt = checkpoint_msg
            .to_checkpoint_psbt(committee, fee_rate, &utxos)
            .unwrap();
        let checkpoint_tx = checkpoint_psbt.unsigned_tx.clone();

        let batch_tx = checkpoint_msg
            .make_reveal_batch_transfer_tx(
                checkpoint_tx.compute_txid(),
                fee_rate,
                &committee.address_checked(),
            )
            .unwrap();

        // First output should be OP_RETURN with metadata
        assert!(
            checkpoint_tx.output[0].script_pubkey.is_op_return(),
            "First output should be OP_RETURN"
        );

        // Extract OP_RETURN data from metadata output
        let op_return_data = checkpoint_tx.output[0]
            .script_pubkey
            .instructions_minimal()
            .find_map(|ins| match ins {
                Ok(bitcoin::blockdata::script::Instruction::PushBytes(data)) => {
                    Some(data.as_bytes())
                }
                _ => None,
            })
            .unwrap();

        // Check the withdrawal count marker
        assert_eq!(
            op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET],
            1,
            "Withdrawal count marker should be 1"
        );

        // Check the transfer count marker
        assert_eq!(
            op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 1],
            1,
            "Transfer count marker should be 1"
        );

        // Should have at least 2 outputs (OP_RETURN + withdrawal)
        assert_eq!(checkpoint_tx.output.len(), 5, "Should have 5 outputs");

        // Since we have transfers, we should have a batch transaction
        assert!(
            batch_tx.is_some(),
            "Should have a batch transfer transaction"
        );

        if let Some(reveal_tx) = batch_tx {
            // The reveal transaction should have one input
            assert_eq!(reveal_tx.input.len(), 1, "Reveal tx should have 1 input");

            // Input should point to checkpoint transaction
            assert_eq!(
                reveal_tx.input[0].previous_output.txid,
                checkpoint_tx.compute_txid(),
                "Reveal tx input should point to checkpoint tx"
            );

            // Should have witness data
            assert!(
                !reveal_tx.input[0].witness.is_empty(),
                "Reveal tx input should have witness data"
            );

            // Should have one output
            assert_eq!(reveal_tx.output.len(), 1, "Reveal tx should have 1 output");
        }
    }

    #[test]
    fn test_checkpoint_without_transfers() {
        // Create a subnet with 3 validators
        let subnet = test_utils::generate_subnet(3);
        let committee = &subnet.committee;
        let utxos = create_test_utxos(committee.address_checked().script_pubkey());

        // Create a simple checkpoint with only withdrawals
        let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap();

        let withdrawal = IpcWithdrawal {
            amount: Amount::from_sat(50000),
            address: bitcoin::Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
                .unwrap(),
        };
        let withdrawal2 = IpcWithdrawal {
            amount: Amount::from_sat(50000),
            address: bitcoin::Address::from_str("bcrt1qvr3jycfxtrkk8u6hp5caxc25tueek5f90mpnsv")
                .unwrap(),
        };

        let checkpoint_msg = IpcCheckpointSubnetMsg {
            subnet_id: subnet.id,
            checkpoint_hash,
            checkpoint_height: 50,
            withdrawals: vec![withdrawal, withdrawal2],
            transfers: vec![],
            change_address: Some(committee.multisig_address.clone()),
        };

        let fee_rate = DEFAULT_BTC_FEE_RATE;

        // Generate transactions
        let checkpoint_psbt = checkpoint_msg
            .to_checkpoint_psbt(committee, fee_rate, &utxos)
            .unwrap();
        let checkpoint_tx = checkpoint_psbt.unsigned_tx.clone();

        let batch_tx = checkpoint_msg
            .make_reveal_batch_transfer_tx(
                checkpoint_tx.compute_txid(),
                fee_rate,
                &committee.address_checked(),
            )
            .unwrap();

        assert!(
            batch_tx.is_none(),
            "No batch transaction should be created when there are no transfers"
        );

        // Verify structure
        assert_eq!(checkpoint_tx.output.len(), 4, "Should have 4 outputs");
        assert!(
            checkpoint_tx.output[0].script_pubkey.is_op_return(),
            "First output should be OP_RETURN"
        );

        // Extract OP_RETURN data
        let op_return_data = checkpoint_tx.output[0]
            .script_pubkey
            .instructions_minimal()
            .find_map(|ins| match ins {
                Ok(bitcoin::blockdata::script::Instruction::PushBytes(data)) => {
                    Some(data.as_bytes())
                }
                _ => None,
            })
            .unwrap();

        // Check the withdrawal count marker
        assert_eq!(
            op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET],
            2,
            "Withdrawal count marker should be 1"
        );

        // Check the transfer count marker
        assert_eq!(
            op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 1],
            0,
            "Transfer count marker should be 0"
        );
    }

    #[test]
    fn test_checkpoint_empty() {
        // Create a subnet with 3 validators
        let subnet = test_utils::generate_subnet(3);
        let committee = &subnet.committee;
        let utxos = create_test_utxos(committee.address_checked().script_pubkey());

        // Create a simple checkpoint with no withdrawals and no transfers
        let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap();

        // Create empty checkpoint message
        let checkpoint_msg = IpcCheckpointSubnetMsg {
            subnet_id: subnet.id,
            checkpoint_hash,
            checkpoint_height: 50,
            withdrawals: vec![],
            transfers: vec![],
            change_address: Some(committee.multisig_address.clone()),
        };

        let fee_rate = DEFAULT_BTC_FEE_RATE;

        // Generate transactions
        let checkpoint_psbt = checkpoint_msg
            .to_checkpoint_psbt(committee, fee_rate, &utxos)
            .unwrap();
        let checkpoint_tx = checkpoint_psbt.unsigned_tx.clone();

        let batch_tx = checkpoint_msg
            .make_reveal_batch_transfer_tx(
                checkpoint_tx.compute_txid(),
                fee_rate,
                &committee.address_checked(),
            )
            .unwrap();

        assert!(
            batch_tx.is_none(),
            "No batch transaction should be created when there are no transfers"
        );

        // Verify structure - should only have OP_RETURN metadata and change output
        assert_eq!(checkpoint_tx.output.len(), 2, "Should have 2 outputs");
        assert!(
            checkpoint_tx.output[0].script_pubkey.is_op_return(),
            "First output should be OP_RETURN"
        );

        // Extract OP_RETURN data
        let op_return_data = checkpoint_tx.output[0]
            .script_pubkey
            .instructions_minimal()
            .find_map(|ins| match ins {
                Ok(bitcoin::blockdata::script::Instruction::PushBytes(data)) => {
                    Some(data.as_bytes())
                }
                _ => None,
            })
            .unwrap();

        // Check the withdrawal count marker
        assert_eq!(
            op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET],
            0,
            "Withdrawal count marker should be 0"
        );

        // Check the transfer count marker
        assert_eq!(
            op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 1],
            0,
            "Transfer count marker should be 0"
        );

        // Verify no batch transaction is returned when there are no transfers
        assert!(
            batch_tx.is_none(),
            "No batch transaction should be created when there are no transfers"
        );
    }

    #[test]
    fn test_batch_transfer_reveal_tx_tag() {
        // Create a test checkpoint message with transfers
        let checkpoint_msg = create_test_checkpoint_msg();

        // Get the batched transfer data
        let transfer_data = checkpoint_msg.make_batched_transfer_data().unwrap();

        // Check that the transfer data starts with the correct tag
        let tag_bytes = IPC_TRANSFER_TAG.as_bytes();
        let prefix = &transfer_data[0..tag_bytes.len()];

        assert_eq!(
            prefix, tag_bytes,
            "Transfer data should start with IPC:TFR tag"
        );

        let tag_str = std::str::from_utf8(prefix).unwrap();
        assert_eq!(
            tag_str, IPC_TRANSFER_TAG,
            "Transfer tag should be valid UTF-8"
        );

        // Verify we have additional data after the tag
        assert!(
            transfer_data.len() > tag_bytes.len(),
            "Transfer data should contain more than just the tag"
        );
    }
}
