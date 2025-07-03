use bitcoin::address::NetworkUnchecked;

use bitcoin::hashes::Hash;
use bitcoin::key::constants::{SCHNORR_PUBLIC_KEY_SIZE, SCHNORR_SIGNATURE_SIZE};
use bitcoin::Amount;
use bitcoin::Transaction;
use bitcoin::Txid;
use bitcoin::XOnlyPublicKey;
use ipc_serde::IpcSerialize;
use log::error;
use log::trace;
use log::warn;
use log::{debug, info};
use num_traits::Zero;
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
pub const IPC_STAKE_TAG: &str = "IPCSTK";
pub const IPC_UNSTAKE_TAG: &str = "IPCUST";
pub const IPC_KILL_SUBNET_TAG: &str = "IPCKIL";

// Static assertion to verify tag lengths at compile time
const _: () = {
    assert!(IPC_CREATE_SUBNET_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_PREFUND_SUBNET_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_JOIN_SUBNET_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_FUND_SUBNET_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_CHECKPOINT_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_TRANSFER_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_DELETE_SUBNET_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_STAKE_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_UNSTAKE_TAG.len() == IPC_TAG_LENGTH);
    assert!(IPC_KILL_SUBNET_TAG.len() == IPC_TAG_LENGTH);
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
    StakeCollateral,
    UnstakeCollateral,
    KillSubnet,
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
            Self::StakeCollateral => IPC_STAKE_TAG,
            Self::UnstakeCollateral => IPC_UNSTAKE_TAG,
            Self::KillSubnet => IPC_KILL_SUBNET_TAG,
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
            IPC_STAKE_TAG => Ok(Self::StakeCollateral),
            IPC_UNSTAKE_TAG => Ok(Self::UnstakeCollateral),
            IPC_KILL_SUBNET_TAG => Ok(Self::KillSubnet),
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
    pub fn validate_pre_bootstrap(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
    ) -> Result<(), IpcValidateError> {
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
                format!(
                    "Validator with public key '{}' already registered in subnet",
                    self.pubkey
                ),
            ));
        }

        Ok(())
    }

    /// Validates the join subnet message, for the given genesis info
    /// and current subnet state
    pub fn validate_post_bootstrap(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
        subnet: &db::SubnetState,
    ) -> Result<(), IpcValidateError> {
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

        // Check if the validator with this public key is already registered
        if subnet.committee.is_validator(&self.pubkey) {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Validator with public key '{}' already registered in subnet",
                self.pubkey
            )));
        }

        // Check if the validator with this public key is already registered
        // and waiting in the next committee
        if subnet
            .waiting_committee
            .as_ref()
            .is_some_and(|c| c.is_validator(&self.pubkey))
        {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Validator with public key '{}' already registered in subnet, waiting for next committee",
                self.pubkey
            )));
        }

        Ok(())
    }

    /// Validates the join subnet message, for the given genesis info
    pub fn validate_for_subnet(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
        subnet: &Option<db::SubnetState>,
    ) -> Result<(), IpcValidateError> {
        if let Some(subnet) = subnet {
            if subnet.id != self.subnet_id {
                return Err(IpcValidateError::InvalidField(
                    "subnet_id",
                    format!(
                        "Subnet ID mismatch: expected {}, got {}",
                        subnet.id, self.subnet_id
                    ),
                ));
            }

            self.validate_post_bootstrap(genesis_info, subnet)
        } else {
            self.validate_pre_bootstrap(genesis_info)
        }
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
    /// Returning SubnetState *only* if the subnet is bootstrapped
    ///
    /// It will return None if processed for an already bootstrapped subnet
    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<Option<db::SubnetState>, IpcLibError> {
        let mut genesis_info =
            db.get_subnet_genesis_info(self.subnet_id)?
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "subnet id={} does not exist",
                    self.subnet_id
                )))?;

        let subnet_state = db.get_subnet_state(self.subnet_id).map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            IpcValidateError::InvalidMsg(e.to_string())
        })?;

        self.validate_for_subnet(&genesis_info, &subnet_state)?;

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

        //
        // Handle post-bootstrap case
        //
        if let Some(mut subnet) = subnet_state {
            if subnet.is_validator(&self.pubkey) {
                return Err(IpcValidateError::InvalidField(
                    "pubkey",
                    format!(
                        "Validator with public key '{}' already registered in subnet",
                        self.pubkey
                    ),
                )
                .into());
            }

            let mut next_committee = subnet
                .waiting_committee
                .unwrap_or_else(|| subnet.committee.clone());

            if next_committee.is_validator(&self.pubkey) {
                return Err(IpcValidateError::InvalidField(
                    "pubkey",
                    format!(
						"Validator with public key '{}' already registered in subnet, waiting in next committee.",
						self.pubkey
					),
                )
                .into());
            }

            next_committee.join_new_validator(&subnet.id, &new_validator)?;
            subnet.waiting_committee = Some(next_committee.clone());

            let stake_change_configuration_number =
                db.get_next_stake_change_configuration_number(self.subnet_id)?;

            let validator_subnet_address = eth_addr_from_x_only_pubkey(self.pubkey);

            // Join
            let stake_change_join = db::StakeChangeRequest {
                change: db::StakingChange::Join {
                    pubkey: self.pubkey.public_key(bitcoin::secp256k1::Parity::Even),
                },
                validator_xpk: self.pubkey,
                validator_subnet_address,
                configuration_number: stake_change_configuration_number,
                committee_after_change: next_committee.clone(),

                block_height,
                block_hash,
                checkpoint_block_height: None,
                checkpoint_block_hash: None,
                txid,
            };
            // Deposit
            let stake_change = db::StakeChangeRequest {
                change: db::StakingChange::Deposit {
                    amount: self.collateral,
                },
                validator_xpk: self.pubkey,
                validator_subnet_address,
                configuration_number: stake_change_configuration_number + 1,
                committee_after_change: next_committee.clone(),

                block_height,
                block_hash,
                checkpoint_block_height: None,
                checkpoint_block_hash: None,
                txid,
            };

            // Write to DB
            let mut wtxn = db.write_txn()?;
            db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet)?;
            db.add_stake_change(&mut wtxn, self.subnet_id, stake_change_join)?;
            db.add_stake_change(&mut wtxn, self.subnet_id, stake_change)?;
            wtxn.commit()?;

            Ok(None)
        }
        //
        // Handle pre-bootstrap case
        //
        else {
            genesis_info.genesis_validators.push(new_validator);

            // Write to DB
            let mut wtxn = db.write_txn()?;

            //
            // Check if the subnet is bootstrapped
            //
            if genesis_info.enough_to_bootstrap() {
                info!("Subnet ID: {} has been bootstrapped", self.subnet_id);
                genesis_info.bootstrapped = true;
                genesis_info.genesis_block_height = Some(block_height);

                // Save the newly create subnet state
                let subnet_state = genesis_info.to_subnet();
                db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet_state)?;
                db.save_subnet_genesis_info(&mut wtxn, self.subnet_id, &genesis_info)?;
                db.save_committee(
                    &mut wtxn,
                    self.subnet_id,
                    subnet_state.committee_number,
                    &subnet_state.committee,
                )?;
                wtxn.commit()?;
                Ok(Some(subnet_state))
            } else {
                db.save_subnet_genesis_info(&mut wtxn, self.subnet_id, &genesis_info)?;
                wtxn.commit()?;
                Ok(None)
            }
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
pub struct IpcUnstake {
    /// The amount to unstake
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub amount: Amount,
    /// The address to send collateral to
    /// For now, this is the same as the validator's
    /// backup_address they specify upon joining
    pub address: bitcoin::Address<NetworkUnchecked>,
    /// The pubkey of the validator in question
    pub pubkey: bitcoin::XOnlyPublicKey,
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
    /// Committee configuration number
    pub next_committee_configuration_number: u64,
    /// Withdrawals
    #[serde(default)]
    pub withdrawals: Vec<IpcWithdrawal>,
    /// Cross-subnet transfers
    #[serde(default)]
    pub transfers: Vec<IpcCrossSubnetTransfer>,
    /// Unstakes for validators leaving the subnet
    #[serde(default, skip_deserializing)]
    pub unstakes: Vec<IpcUnstake>,
    /// Optional change address (multisig)
    #[serde(skip_deserializing)]
    pub change_address: Option<bitcoin::Address<NetworkUnchecked>>,
    /// The flag marking the subnet killed, ie. the last checkpoint
    #[serde(default, skip_deserializing)]
    pub is_kill_checkpoint: bool,
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

        // Ensure number of unstakes, withdrawals and transfers doesn't exceed u8::MAX (255)
        if self.unstakes.len() > u8::MAX as usize {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Number of unstakes ({}) exceeds maximum allowed ({})",
                self.unstakes.len(),
                u8::MAX
            )));
        }

        // Check if the unstakes are valid
        for unstake in &self.unstakes {
            if unstake.amount == Amount::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "unstake.amount",
                    "Unstake amount must be greater than zero".to_string(),
                ));
            }
            if !unstake.address.is_valid_for_network(NETWORK) {
                return Err(IpcValidateError::InvalidField(
                    "unstake.address",
                    format!(
                        "Bitcoin address {:?} must be for the current network",
                        unstake.address
                    ),
                ));
            }
        }

        // Ensure number of withdrawals doesn't exceed u8::MAX (255)
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

        if self.is_kill_checkpoint && self.change_address.is_some() {
            return Err(IpcValidateError::InvalidField(
                "change_address",
                "Change address must not be set for kill checkpoint".to_string(),
            ));
        }

        Ok(())
    }
}

impl IpcCheckpointSubnetMsg {
    // Length of the withdrawal + transfer + unstake markers, 1 byte each
    const MARKERS_LEN: usize = 3;
    // u64 length
    const HEIGHT_LEN: usize = std::mem::size_of::<u64>();
    // u64 length
    const COMMITTEE_CONF_LEN: usize = std::mem::size_of::<u64>();
    // bool length
    const KILLED_LEN: usize = 1;
    // The total length of the op_return data - helper
    const DATA_LEN: usize = IPC_TAG_LENGTH
        + SubnetId::INNER_LEN
        + bitcoin::hashes::sha256::Hash::LEN
        + Self::MARKERS_LEN
        + Self::HEIGHT_LEN
        + Self::COMMITTEE_CONF_LEN
        + Self::KILLED_LEN;

    const TAG_OFFSET: usize = 0;
    const TXID_OFFSET: usize = Self::TAG_OFFSET + IPC_TAG_LENGTH;
    const HASH_OFFSET: usize = Self::TXID_OFFSET + SubnetId::INNER_LEN;
    const HEIGHT_OFFSET: usize = Self::HASH_OFFSET + bitcoin::hashes::sha256::Hash::LEN;
    const MARKERS_OFFSET: usize = Self::HEIGHT_OFFSET + Self::HEIGHT_LEN;
    const COMMITTEE_CONF_OFFSET: usize = Self::MARKERS_OFFSET + Self::MARKERS_LEN;
    const KILLED_OFFSET: usize = Self::COMMITTEE_CONF_OFFSET + Self::COMMITTEE_CONF_LEN;

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
        op_return_data[Self::MARKERS_OFFSET + 2] = self.unstakes.len().min(255) as u8; // Unstake count

        // Add committee configuration number
        op_return_data[Self::COMMITTEE_CONF_OFFSET..Self::KILLED_OFFSET]
            .copy_from_slice(&self.next_committee_configuration_number.to_le_bytes());

        // Add killed flag
        op_return_data[Self::KILLED_OFFSET] = if self.is_kill_checkpoint { 1 } else { 0 };

        // Required to do since [u8; 78] for some reason doesn't implement pusbytes
        let push_bytes: &bitcoin::script::PushBytes =
            (&op_return_data[..]).try_into().expect("the size is okay");

        let op_return_script = bitcoin_utils::make_op_return_script(push_bytes);
        let op_return_value = op_return_script
            .minimal_non_dust_custom(fee_rate)
            .max(op_return_script.minimal_non_dust());

        bitcoin::TxOut {
            value: op_return_value,
            script_pubkey: op_return_script,
        }
    }

    pub fn extract_markers_from_metadata_tx_out(
        tx_out: &bitcoin::TxOut,
    ) -> Result<(u8, u8, u8, bool), IpcLibError> {
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
        let unstake_count = op_return_data[Self::MARKERS_OFFSET + 2];

        // Extract the killed flag
        let killed = op_return_data[Self::KILLED_OFFSET] != 0;

        Ok((withdrawal_count, transfer_count, unstake_count, killed))
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

        let commit_tx_value =
            // Provide at least the minimal value for the output
            commit_script_pubkey.minimal_non_dust_custom(fee_rate)
            // At least the minimal broadcastable
        	.max(commit_script_pubkey.minimal_non_dust())
         	// Add enough sats to cover the reveal tx output and fee
            .max(reveal_tx_out.value + reveal_tx_fee);

        debug!("checkpoint batch_transfer commit_tx_value={commit_tx_value}");

        let commit_tx_out = bitcoin::TxOut {
            value: commit_tx_value,
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

        // Calculate batch_transfer vout based on the number of withdrawals and unstakes
        // position after the metadata output, all withdrawal outputs and all unstake outputs
        // i.e., 1 (metadata) + withdrawals.len() + unstakes.len() if present
        let vout = 1 + self.withdrawals.len() as u32 + self.unstakes.len() as u32;

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
        debug!("batch_transfer_reveal_tx={reveal_tx:?}");

        Ok(Some(reveal_tx))
    }

    /// Makes a unsigned checkpoint transaction that includes checkpoint data
    /// withdrawals and transfers
    ///
    /// When the next_committee is different from the current committee,
    /// all of the UTXOs are exhausted.
    pub fn to_checkpoint_psbt(
        &self,
        committee: &db::SubnetCommittee,
        next_committee: &db::SubnetCommittee,
        fee_rate: bitcoin::FeeRate,
        unspent: &[bitcoincore_rpc::json::ListUnspentResultEntry],
    ) -> Result<bitcoin::Psbt, IpcLibError> {
        debug!(
            "Creating checkpoint transactions for subnet_id={}",
            self.subnet_id
        );

        // Check if committee rotation is happening
        let exhaust_unspent = committee != next_committee || self.is_kill_checkpoint;
        let committee_change_address = next_committee.address_checked();

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

            // Group transfers by subnet id (txid20)
            let mut transfers_by_subnet: HashMap<
                Txid20,
                (Amount, bitcoin::Address<NetworkUnchecked>),
            > = HashMap::new();

            for transfer in &self.transfers {
                let subnet_txid20 = transfer.destination_subnet_id.txid20();

                if let Some(multisig_addr) = &transfer.subnet_multisig_address {
                    let entry = transfers_by_subnet
                        .entry(subnet_txid20)
                        .or_insert((Amount::ZERO, multisig_addr.clone()));

                    // Add to the total amount for this subnet
                    entry.0 += transfer.amount;
                } else {
                    return Err(IpcValidateError::InvalidField(
                        "transfer.subnet_multisig_address",
                        "subnet_multisig_address must be defined".to_string(),
                    )
                    .into());
                }
            }

            // Add transfer outputs - one per unique subnet
            for (_, (total_amount, multisig_addr)) in transfers_by_subnet {
                tx_outs.push(bitcoin::TxOut {
                    value: total_amount,
                    script_pubkey: multisig_addr
                        .require_network(NETWORK)
                        .expect("Address must be valid for network")
                        .script_pubkey(),
                });
            }
        }

        //
        // Add unstake outputs
        //
        for unstake in &self.unstakes {
            tx_outs.push(bitcoin::TxOut {
                value: unstake.amount,
                script_pubkey: unstake
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
        // Create the checkpoint unsigned transaction
        //

        let committee_keys = committee.validator_weighted_keys();

        let mut checkpoint_tx = multisig::construct_spend_unsigned_transaction(
            &committee_keys,
            committee.threshold,
            &committee_change_address,
            unspent,
            exhaust_unspent,
            &tx_outs,
            &fee_rate,
        )?;

        // dbg!(&checkpoint_tx);

        let change_amount = checkpoint_tx
            .output
            .iter()
            .find(|out| out.script_pubkey == committee_change_address.script_pubkey())
            .map_or(Amount::ZERO, |out| out.value);

        debug!(
            "Checkpoint has {} of change going to {}",
            change_amount, committee_change_address
        );

        // If this is a kill checkpoint, we should not have any change
        // we split it among the unstake outputs proportionally
        if self.is_kill_checkpoint && change_amount > Amount::ZERO && !self.unstakes.is_empty() {
            debug!(
                "Kill checkpoint, splitting the leftover funds {} among unstake outputs.",
                change_amount
            );

            // Get the total amount in unstake outputs to calculate proportions
            let total_unstake_amount: Amount = self.unstakes.iter().map(|u| u.amount).sum();

            if total_unstake_amount > Amount::ZERO {
                // Modify the last N tx_outs which are the unstake outputs
                let num_unstakes = self.unstakes.len();
                let unstake_start_index = tx_outs.len() - num_unstakes;

                // Distribute the change proportionally among unstake outputs
                let mut distributed_amount = Amount::ZERO;
                for (i, unstake) in self.unstakes.iter().enumerate() {
                    let output_index = unstake_start_index + i;
                    if output_index < tx_outs.len() {
                        let proportion =
                            unstake.amount.to_sat() as f64 / total_unstake_amount.to_sat() as f64;
                        let additional_amount = if i == self.unstakes.len() - 1 {
                            // For the last unstake, give remaining to avoid rounding issues
                            change_amount - distributed_amount
                        } else {
                            Amount::from_sat((change_amount.to_sat() as f64 * proportion) as u64)
                        };

                        tx_outs[output_index].value += additional_amount;
                        distributed_amount += additional_amount;

                        debug!(
                            "Added {} to unstake output {} (proportion: {:.4})",
                            additional_amount, i, proportion
                        );
                    }
                }

                debug!(
                    "Redistributed {} change among {} unstake outputs",
                    change_amount, num_unstakes
                );

                // Reconstruct the transaction with updated tx_outs
                checkpoint_tx = multisig::construct_spend_unsigned_transaction(
                    &committee_keys,
                    committee.threshold,
                    &committee_change_address,
                    unspent,
                    exhaust_unspent,
                    &tx_outs,
                    &fee_rate,
                )?;
            }
        }

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let checkpoint_psbt = multisig::construct_spend_psbt(
            &secp,
            &self.subnet_id,
            &committee_keys,
            committee.threshold,
            &committee_change_address,
            unspent,
            exhaust_unspent,
            &tx_outs,
            &fee_rate,
        )?;

        debug!("Checkpoint TX: {checkpoint_tx:?}");
        // dbg!(&checkpoint_tx);
        // dbg!(&checkpoint_psbt);

        // assert_eq!(
        //     checkpoint_tx.compute_txid(),
        //     checkpoint_psbt.unsigned_tx.compute_txid()
        // );

        Ok(checkpoint_psbt)
    }

    /// Reconstructs an IpcCheckpointSubnetMsg from a checkpoint transaction.
    ///
    /// The checkpoint transaction has:
    /// 1. An OP_RETURN output with metadata in the format:
    ///    [checkpoint tag | 32-byte subnet ID txid | 32-byte checkpoint hash | 8-byte checkpoint height | 1-byte withdrawal count | 1-byte transfer count | 1-byte unstake count | 8-byte committee configuration number | 1-byte killed flag]
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
        let unstakes_count = op_return_data[Self::MARKERS_OFFSET + 2] as usize;

        // Extract committee configuration number
        let committee_conf_bytes =
            &op_return_data[Self::COMMITTEE_CONF_OFFSET..Self::KILLED_OFFSET];
        let next_committee_configuration_number =
            u64::from_le_bytes(committee_conf_bytes.try_into().map_err(|_| {
                err("Failed to convert committee configuration number bytes to u64".to_string())
            })?);

        // Extract killed flag
        let kill_checkpoint = op_return_data[Self::KILLED_OFFSET] != 0;

        // Check if we have enough outputs for all the withdrawals, unstakes and transfers
        let expected_outputs = 1 + // metadata
               withdrawals_count + // withdrawals
               unstakes_count + // unstakes
               (if transfers_count > 0 { 1 } else { 0 }) + // batch transfer commit (if needed)
               transfers_count; // transfers

        if tx.output.len() < expected_outputs {
            return Err(err(format!(
                "Not enough outputs: got {}, expected at least {}",
                tx.output.len(),
                expected_outputs
            )));
        }

        let subnet = db
            .get_subnet_state(subnet_id)?
            .ok_or(err(format!("Subnet {} not found", subnet_id)))?;

        // Parse withdrawals
        let mut withdrawals = Vec::with_capacity(withdrawals_count);
        for i in 0..withdrawals_count {
            let txout = &tx.output[1 + i]; // 1-based index (after metadata)
            let amount = txout.value;
            let address =
                bitcoin::Address::from_script(&txout.script_pubkey, NETWORK).map_err(|_| {
                    err(format!(
                        "Could not parse address from withdrawal output {}",
                        1 + i
                    ))
                })?;
            let address = address.into_unchecked();

            withdrawals.push(IpcWithdrawal { amount, address });
        }

        // Parse unstakes
        let mut unstakes = Vec::with_capacity(unstakes_count);
        for i in 0..unstakes_count {
            let txout = &tx.output[1 + withdrawals_count + i]; // After metadata and withdrawals
            let amount = txout.value;
            let address =
                bitcoin::Address::from_script(&txout.script_pubkey, NETWORK).map_err(|_| {
                    err(format!(
                        "Could not parse address from unstake output {}",
                        1 + withdrawals_count + i
                    ))
                })?;
            let address = address.into_unchecked();

            let validator = subnet
                .committee
                .validators
                .iter()
                .find(|v| v.backup_address == address);

            if validator.is_none() {
                warn!("Subnet {} checkpoint: unstake present for a non-validator to address {}, skipping...", subnet_id, address.assume_checked());
                continue;
            }

            let validator = validator.unwrap();
            let pubkey = validator.pubkey;

            unstakes.push(IpcUnstake {
                amount,
                address,
                pubkey,
            });
        }

        // Parse transfers
        let mut transfers = Vec::with_capacity(transfers_count);

        // If there are transfers, there should be a batch transfer commit output
        if transfers_count > 0 {
            // Start after metadata + withdrawals + unstakes + batch commit
            let transfer_start_index = 1 + withdrawals_count + unstakes_count + 1;

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
            next_committee_configuration_number,
            unstakes,
            withdrawals,
            transfers,
            change_address,
            is_kill_checkpoint: kill_checkpoint,
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
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<db::SubnetCheckpoint, IpcLibError> {
        // Get the current subnet state
        let mut subnet_state = db.get_subnet_state(self.subnet_id)?.ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!("Subnet ID {} does not exist", self.subnet_id))
        })?;

        let checkpoint_number = subnet_state.last_checkpoint_number.map_or(0, |n| n + 1);
        let last_committee_configuration_number = subnet_state.committee.configuration_number;

        let stake_change = if
        // If next_committee_configuration_number is zero, it "could mean" no change
        self.next_committee_configuration_number.is_zero()
            || self.next_committee_configuration_number == last_committee_configuration_number
        {
            // No committee rotation, no stake change
            None
        } else {
            if self.next_committee_configuration_number < last_committee_configuration_number {
                return Err(IpcValidateError::InvalidField(
                    "next_committee_configuration_number",
                    format!(
                        "Next committee configuration number {} is less than the last one {}",
                        self.next_committee_configuration_number,
                        last_committee_configuration_number
                    ),
                )
                .into());
            }

            let stake_change =
                db.get_stake_change(self.subnet_id, self.next_committee_configuration_number)?;

            if stake_change.is_none() {
                return Err(IpcValidateError::InvalidField(
                    "next_committee_configuration_number",
                    format!(
                        "Stake change for committee configuration number {} does not exist",
                        self.next_committee_configuration_number
                    ),
                )
                .into());
            }

            debug!(
                "Processing stake changes for checkpoint. Up to stake change {:?}",
                stake_change
            );

            stake_change
        };

        let next_committee = stake_change.clone().map(|sc| sc.committee_after_change);

        let (next_committee_number, next_configuration_number) =
            if let Some(stake_change) = stake_change {
                // Increment the committee number
                // And use the new configuration number
                (
                    subnet_state.committee_number + 1,
                    stake_change.configuration_number,
                )
            } else {
                // No committee rotation, use the current committee number
                (
                    subnet_state.committee_number,
                    subnet_state.committee.configuration_number,
                )
            };

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
            next_committee_number,
            next_configuration_number,
            is_kill_checkpoint: self.is_kill_checkpoint,
        };

        // Update the checkpoint number in subnet state
        subnet_state.last_checkpoint_number = Some(checkpoint_number);

        // Update subnet state with the new committee
        if let Some(next_committee) = &next_committee {
            subnet_state.rotate_to_committee(next_committee.clone());
        }

        // Update subnet state if killed
        if self.is_kill_checkpoint {
            info!(
                "Marking subnet {} as killed at block height {} with txid {}",
                self.subnet_id, block_height, txid
            );

            subnet_state.killed = db::SubnetKillState::Killed;
        }

        // Begin a database transaction
        let mut wtxn = db.write_txn()?;
        // Save the updated subnet state
        db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet_state)?;
        // Save the checkpoint record
        db.save_checkpoint(&mut wtxn, self.subnet_id, &checkpoint, checkpoint_number)?;
        // Save new committee if changed
        if let Some(next_committee) = &next_committee {
            db.save_committee(
                &mut wtxn,
                self.subnet_id,
                next_committee_number,
                next_committee,
            )?;

            // Update the stake changes
            db.confirm_stake_changes(
                &mut wtxn,
                self.subnet_id,
                self.next_committee_configuration_number,
                block_height,
                block_hash,
            )?;
        }

        // Commit the transaction
        wtxn.commit()?;

        debug!(
            "Saved checkpoint #{} for subnet {} with txid {}. Checkpoint = {:?}",
            checkpoint_number, self.subnet_id, txid, checkpoint
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
        let (_, transfer_count, _, _) =
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcStakeCollateralMsg {
    /// The subnet id of the subnet to stake
    /// This is derived from 2nd output
    /// that is sent to the subnet multisig address
    pub subnet_id: SubnetId,
    /// The amount to add to the validator stake
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub amount: bitcoin::Amount,
    /// The pubkey of the validator
    pub pubkey: bitcoin::XOnlyPublicKey,
}

impl IpcStakeCollateralMsg {
    const MIN_STAKE_DIFF: f64 = 5.0;

    // The total length of the op_return data - helper
    const DATA_LEN: usize = IPC_TAG_LENGTH + SCHNORR_PUBLIC_KEY_SIZE;

    pub fn validate_for_subnet(&self, subnet: &db::SubnetState) -> Result<(), IpcValidateError> {
        // should never happen
        if subnet.id != self.subnet_id {
            return Err(IpcValidateError::InvalidField(
                "subnet_id",
                format!(
                    "Subnet ID mismatch: expected {}, got {}",
                    subnet.id, self.subnet_id
                ),
            ));
        }

        if subnet.killed != db::SubnetKillState::NotKilled {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {} is killed or marked for killing, cannot change stake.",
                self.subnet_id
            )));
        }

        // Check if the validator with this public key is already registered
        if !subnet.latest_committee().is_validator(&self.pubkey) {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Validator with public key '{}' must already be a validator in subnet {}",
                self.pubkey, self.subnet_id,
            )));
        }

        // Check diff of the collateral
        // TODO this is a temporary solution, should revisit
        let curr_collateral = subnet.total_collateral();
        let new_collateral = curr_collateral + self.amount;
        let diff = ((new_collateral.to_sat() as f64 - curr_collateral.to_sat() as f64)
            / curr_collateral.to_sat() as f64)
            * 100.0;

        if diff < Self::MIN_STAKE_DIFF {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Stake addition must change the total collateral by at least {:.2}%. Current change: {:.2}%",
                Self::MIN_STAKE_DIFF, diff
            )));
        }

        Ok(())
    }

    pub fn to_tx(
        &self,
        fee_rate: bitcoin::FeeRate,
        multisig_address: &bitcoin::Address,
    ) -> Result<Transaction, IpcLibError> {
        //
        // Create the first output: op_return with
        // ipc tag, xonly pubkey of validator
        //
        let tag: [u8; IPC_TAG_LENGTH] = IPC_STAKE_TAG
            .as_bytes()
            .try_into()
            .expect("IPC_STAKE_TAG has incorrect length");
        let pubkey: [u8; SCHNORR_PUBLIC_KEY_SIZE] = self.pubkey.serialize();

        // Construct op_return data
        let mut op_return_data = [0u8; Self::DATA_LEN];

        op_return_data[0..IPC_TAG_LENGTH].copy_from_slice(&tag);
        op_return_data[IPC_TAG_LENGTH..].copy_from_slice(&pubkey);

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

        let stake_tx_out = bitcoin::TxOut {
            value: self.amount,
            script_pubkey: multisig_address.script_pubkey(),
        };

        // Construct transaction

        let tx_outs = vec![data_tx_out, stake_tx_out];
        let tx = bitcoin_utils::create_tx_from_txouts(tx_outs);
        debug!("Stake TX: {tx:?}");

        Ok(tx)
    }

    /// Parses an IpcStakeCollateralMsg from a Bitcoin transaction
    pub fn from_tx<D: db::Database>(db: &D, tx: &Transaction) -> Result<Self, IpcLibError> {
        use bitcoin::blockdata::script::Instruction;

        // Helper closure for error creation
        let err = |msg: String| IpcLibError::MsgParseError(IPC_STAKE_TAG, msg);

        // We expect minimum two outputs:
        // 1. OP_RETURN with IPC tag and validator pubkey
        // 2. Amount sent to the multisig address
        if tx.output.len() < 2 {
            return Err(err(format!(
                "Expected at least 2 outputs for stake collateral message, found {}",
                tx.output.len()
            )));
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

        // Check data length
        if op_return_data.len() != Self::DATA_LEN {
            return Err(err(format!(
                "Invalid OP_RETURN data length: expected {}, got {}",
                Self::DATA_LEN,
                op_return_data.len()
            )));
        }

        // Check IPC tag
        let tag_bytes = &op_return_data[0..IPC_TAG_LENGTH];
        let expected_tag = IPC_STAKE_TAG.as_bytes();
        if tag_bytes != expected_tag {
            return Err(err(format!(
                "Invalid IPC tag: expected {:?}, got {:?}",
                expected_tag, tag_bytes
            )));
        }

        // Extract validator pubkey
        let pubkey_bytes = &op_return_data[IPC_TAG_LENGTH..];
        let pubkey = XOnlyPublicKey::from_slice(pubkey_bytes)
            .map_err(|e| err(format!("Invalid validator pubkey: {}", e)))?;

        // Extract stake amount from the second output
        let amount = tx.output[1].value;

        let multisig_address = bitcoin::Address::from_script(&tx.output[1].script_pubkey, NETWORK)
            .map_err(|_| err("Could not parse address from output 1".to_string()))?
            .into_unchecked();

        let subnet_id = match db.get_subnet_by_multisig_address(&multisig_address) {
            Ok(Some(subnet)) => subnet.id,
            Ok(None) => {
                error!(
                    "StakeCollateralMsg: Could not find subnet with multisig address {:?}.",
                    multisig_address
                );
                return Err(err(format!(
                    "StakeCollateralMsg: Could not find subnet with multisig address {:?}",
                    multisig_address
                )));
            }
            Err(e) => {
                error!(
	                "StakeCollateralMsg: Error while looking up subnet with multisig address {:?}: {}",
	                multisig_address, e
	            );
                return Err(err(format!(
                    "StakeCollateralMsg: Error while looking up subnet with multisig address {:?}: {}",
                    multisig_address, e
                )));
            }
        };

        // Construct the message
        Ok(Self {
            subnet_id,
            amount,
            pubkey,
        })
    }

    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
        multisig_address: &bitcoin::Address,
    ) -> Result<Txid, IpcLibError> {
        info!(
            "Submitting stake collateral msg to bitcoin. Multisig address = {}. Amount={}",
            multisig_address, self.amount
        );

        let fee_rate = get_fee_rate(rpc, None, None);
        let tx = self.to_tx(fee_rate, multisig_address)?;

        // Construct, fund and sign the stake transaction
        let tx = crate::wallet::fund_tx(rpc, tx, None)?;
        trace!("Stake msg funded TX: {tx:?}");
        let tx = crate::wallet::sign_tx(rpc, tx)?;
        trace!("Stake msg signed TX: {tx:?}");

        // Submit the stake transaction to the mempool

        let txid = tx.compute_txid();
        match submit_to_mempool(rpc, vec![tx]) {
            Ok(_) => {
                info!(
                    "Submitted stake collateral msg for subnet_id={} txid={} amount={}",
                    self.subnet_id, txid, self.amount
                );
                Ok(txid)
            }
            Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
        }
    }

    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let genesis_info =
            db.get_subnet_genesis_info(self.subnet_id)?
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "subnet id={} does not exist",
                    self.subnet_id
                )))?;

        let mut subnet_state = db
            .get_subnet_state(self.subnet_id)
            .map_err(|e| {
                error!("Error getting subnet info from Db: {}", e);
                IpcValidateError::InvalidMsg(e.to_string())
            })?
            // should never happen
            .ok_or_else(|| {
                error!("Subnet {} not found, unexpected", self.subnet_id);
                IpcValidateError::InvalidMsg(format!("Subnet {} not found.", self.subnet_id))
            })?;

        self.validate_for_subnet(&subnet_state)?;

        // we clone and modify the validator
        let mut validator = subnet_state
            .latest_committee()
            .validators
            .iter()
            .find(|v| v.pubkey == self.pubkey)
            .ok_or(IpcValidateError::InvalidMsg(format!(
                "Validator with public key {} not part of committee",
                self.pubkey
            )))?
            .clone();

        let new_collateral = self.amount + validator.collateral;
        let new_power = multisig::collateral_to_power(
            &new_collateral,
            &genesis_info.create_subnet_msg.min_validator_stake,
        )?;

        validator.collateral = new_collateral;
        validator.power = new_power;

        // Update the next committee or create one if it doesn't exist
        let mut next_committee = subnet_state
            .waiting_committee
            .unwrap_or_else(|| subnet_state.committee.clone());

        // Modify the validator in the next committee
        next_committee.modify_validator(&self.subnet_id, &validator)?;

        // Update the subnet state with the modified next committee
        subnet_state.waiting_committee = Some(next_committee.clone());

        let stake_change_configuration_number =
            db.get_next_stake_change_configuration_number(self.subnet_id)?;

        // Create a stake change request to track this change
        let stake_change = db::StakeChangeRequest {
            change: db::StakingChange::Deposit {
                amount: self.amount,
            },
            validator_xpk: self.pubkey,
            validator_subnet_address: validator.subnet_address,
            configuration_number: stake_change_configuration_number,
            committee_after_change: next_committee.clone(),
            block_height,
            block_hash,
            checkpoint_block_height: None,
            checkpoint_block_hash: None,
            txid,
        };

        let mut wtxn = db.write_txn()?;

        // Save the updated subnet state to the database
        db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet_state)?;
        // Add the stake change to the database
        db.add_stake_change(&mut wtxn, self.subnet_id, stake_change)?;

        wtxn.commit()?;

        Ok(())
    }
}

impl IpcValidate for IpcStakeCollateralMsg {
    fn validate(&self) -> Result<(), IpcValidateError> {
        // Check subnet_id is not all zeros
        if self.subnet_id == SubnetId::default() {
            return Err(IpcValidateError::InvalidField(
                "subnet_id",
                "Subnet ID cannot be all zeros".to_string(),
            ));
        }

        // Check collateral is not zero
        if self.amount == Amount::ZERO {
            return Err(IpcValidateError::InvalidField(
                "amount",
                "Amount must be greater than zero".to_string(),
            ));
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcUnstakeCollateralMsg {
    /// The subnet id of the subnet to stake
    /// This is derived from 2nd output
    /// that is sent to the subnet multisig address
    pub subnet_id: SubnetId,
    /// The amount to remove from the validator stake
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub amount: bitcoin::Amount,
    /// The pubkey of the validator
    #[serde(skip_deserializing)]
    pub pubkey: Option<bitcoin::XOnlyPublicKey>,
}

impl IpcUnstakeCollateralMsg {
    const TAG: &str = IPC_UNSTAKE_TAG;

    // The total length of the op_return data - helper
    const DATA_LEN: usize = IPC_TAG_LENGTH + SCHNORR_SIGNATURE_SIZE + Amount::SIZE;

    pub fn validate_for_subnet(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
        subnet: &db::SubnetState,
    ) -> Result<(), IpcValidateError> {
        // should never happen
        if subnet.id != self.subnet_id || genesis_info.subnet_id != self.subnet_id {
            return Err(IpcValidateError::InvalidField(
                "subnet_id",
                format!(
                    "Subnet ID mismatch: expected {}, got {}",
                    subnet.id, self.subnet_id
                ),
            ));
        }

        if subnet.killed != db::SubnetKillState::NotKilled {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {} is killed or marked for killing, cannot change stake.",
                self.subnet_id
            )));
        }

        let committee = subnet.latest_committee();

        // Check if the validator with this public key is validator
        // NOTE: mandatory check here
        let pubkey = match self.pubkey {
            Some(pubkey) => {
                if !committee.is_validator(&pubkey) {
                    return Err(IpcValidateError::InvalidMsg(format!(
                        "Validator with public key '{}' must already be a validator in subnet {}",
                        pubkey, self.subnet_id,
                    )));
                }

                pubkey
            }
            None => {
                return Err(IpcValidateError::InvalidField(
                    "pubkey",
                    "Validator public key is required".to_string(),
                ));
            }
        };

        // Check if the amount is not more than the current collateral
        if self.amount > subnet.total_collateral() {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Unstake amount {} is greater than current collateral {}",
                self.amount,
                subnet.total_collateral()
            )));
        }

        // Check rest of collateral is not less than the minimum required
        // for a validator to participate in the committee
        let validator = committee
            .validators
            .iter()
            .find(|v| v.pubkey == pubkey)
            .ok_or(IpcValidateError::InvalidMsg(format!(
                "Validator with public key {} not part of committee",
                pubkey
            )))?;

        let new_collateral =
            validator
                .collateral
                .checked_sub(self.amount)
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "Amount {} is greater than current validator collateral {}",
                    self.amount, validator.collateral
                )))?;

        let (_, sufficient_to_participate) = match multisig::collateral_to_power(
            &new_collateral,
            &genesis_info.create_subnet_msg.min_validator_stake,
        ) {
            Ok(power) => (power, true),
            Err(crate::multisig::MultisigError::InsufficientCollateral) => (0, false),
            Err(_) => {
                return Err(IpcValidateError::InvalidMsg(
                    "Collateral too high".to_string(),
                ))
            }
        };

        if !sufficient_to_participate && self.amount != validator.collateral {
            return Err(IpcValidateError::InvalidMsg(format!(
				"Validator with public key {} has insufficient collateral to participate in the committee after unstaking {}. Please unstake all {}.",
				pubkey, self.amount, validator.collateral,
			)));
        }

        // TODO Check diff of the collateral
        // TODO this is a temporary solution, should revisit
        // let curr_collateral = subnet.total_collateral();
        // let new_collateral = curr_collateral + self.amount;
        // let diff = ((new_collateral.to_sat() as f64 - curr_collateral.to_sat() as f64)
        //     / curr_collateral.to_sat() as f64)
        //     * 100.0;

        // if diff < Self::MIN_STAKE_DIFF {
        //     return Err(IpcValidateError::InvalidMsg(format!(
        //         "Stake addition must change the total collateral by at least {:.2}%. Current change: {:.2}%",
        //         Self::MIN_STAKE_DIFF, diff
        //     )));
        // }

        Ok(())
    }

    /// See `make_signature` for information
    fn make_signature_msg(
        prev_txid: &Txid,
        pubkey: &bitcoin::XOnlyPublicKey,
        amount: &bitcoin::Amount,
        subnet_id: &SubnetId,
    ) -> bitcoin::secp256k1::Message {
        let mut msg = Vec::new();
        msg.extend_from_slice(prev_txid.as_byte_array().as_slice());
        msg.extend_from_slice(&pubkey.serialize());
        msg.extend_from_slice(&amount.to_sat().to_be_bytes());
        msg.extend_from_slice(subnet_id.txid20().as_ref());

        let msg = bitcoin::hashes::sha256::Hash::hash(&msg);
        bitcoin::secp256k1::Message::from_digest(msg.to_byte_array())
    }

    /// Returns a signature to be embedded in the OP_RETURN output
    /// Confirming it's valid and signed by the validator making the request
    ///
    /// The signature is signing the following message:
    /// prev_txid || xpubkey || unstake_amount || subnet_id
    ///
    /// This is to prevent replay attacks, since the tx inputs
    /// are not enforced or verified
    pub(crate) fn make_signature(
        &self,
        prev_txid: &Txid,
        secret_key: bitcoin::secp256k1::SecretKey,
    ) -> bitcoin::secp256k1::schnorr::Signature {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (pubkey, _) = secret_key.x_only_public_key(&secp);
        let msg = Self::make_signature_msg(prev_txid, &pubkey, &self.amount, &self.subnet_id);

        // Sign the message with the secret key
        // and return the signature
        secp.sign_schnorr(&msg, &secret_key.keypair(&secp))
    }

    pub fn to_tx(
        &self,
        fee_rate: bitcoin::FeeRate,
        multisig_address: &bitcoin::Address,
        signature: bitcoin::secp256k1::schnorr::Signature,
    ) -> Result<Transaction, IpcLibError> {
        //
        // Create the first output: op_return with
        // ipc tag, schnorr signature, amount
        //
        let tag: [u8; IPC_TAG_LENGTH] = Self::TAG
            .as_bytes()
            .try_into()
            .expect("IPC_UNSTAKE_TAG has incorrect length");
        let amount = self.amount.to_sat().to_be_bytes();

        let sig_bytes = signature.as_ref();

        // Construct op_return data
        let mut op_return_data = [0u8; Self::DATA_LEN];

        op_return_data[0..IPC_TAG_LENGTH].copy_from_slice(&tag);
        op_return_data[IPC_TAG_LENGTH..IPC_TAG_LENGTH + SCHNORR_SIGNATURE_SIZE]
            .copy_from_slice(sig_bytes);
        op_return_data[IPC_TAG_LENGTH + SCHNORR_SIGNATURE_SIZE..].copy_from_slice(&amount);

        // Required to do since [u8; 78] for some reason doesn't implement pusbytes
        let push_bytes: &bitcoin::script::PushBytes =
            (&op_return_data[..]).try_into().expect("the size is okay");

        // Make op_return script and txout
        let op_return_script = bitcoin_utils::make_op_return_script(push_bytes);
        let data_value = op_return_script.minimal_non_dust_custom(fee_rate);
        let data_tx_out = bitcoin::TxOut {
            value: data_value,
            script_pubkey: op_return_script,
        };

        //
        // Create second output: amount sent to the subnet multisig address
        //
        let multisig_tx_out = bitcoin::TxOut {
            // TODO check this dust calculation?
            value: multisig_address
                .script_pubkey()
                .minimal_non_dust_custom(fee_rate),
            script_pubkey: multisig_address.script_pubkey(),
        };

        // Construct transaction
        let tx_outs = vec![data_tx_out, multisig_tx_out];
        let tx = bitcoin_utils::create_tx_from_txouts(tx_outs);
        debug!("Unstake TX: {tx:?}");

        Ok(tx)
    }

    /// Parses an IpcUnstakeCollateralMsg from a Bitcoin transaction
    pub fn from_tx<D: db::Database>(db: &D, tx: &Transaction) -> Result<Self, IpcLibError> {
        use bitcoin::blockdata::script::Instruction;

        // Helper closure for error creation
        let err = |msg: String| IpcLibError::MsgParseError(Self::TAG, msg);

        // We expect at least two outputs:
        // 1. OP_RETURN with IPC tag, signature, and amount
        // 2. Output to the subnet multisig address
        if tx.output.len() < 2 {
            return Err(err(format!(
                "Expected at least 2 outputs for unstake collateral message, found {}",
                tx.output.len()
            )));
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

        // Check data length
        if op_return_data.len() != Self::DATA_LEN {
            return Err(err(format!(
                "Invalid OP_RETURN data length: expected {}, got {}",
                Self::DATA_LEN,
                op_return_data.len()
            )));
        }

        // Check IPC tag
        let tag_bytes = &op_return_data[0..IPC_TAG_LENGTH];
        let expected_tag = Self::TAG.as_bytes();
        if tag_bytes != expected_tag {
            return Err(err(format!(
                "Invalid IPC tag: expected {:?}, got {:?}",
                expected_tag, tag_bytes
            )));
        }

        // Extract signature
        let sig_bytes = &op_return_data[IPC_TAG_LENGTH..IPC_TAG_LENGTH + SCHNORR_SIGNATURE_SIZE];
        let signature = bitcoin::secp256k1::schnorr::Signature::from_slice(sig_bytes)
            .map_err(|e| err(format!("Invalid schnorr signature: {}", e)))?;

        // Extract amount
        let amount_bytes = &op_return_data[IPC_TAG_LENGTH + SCHNORR_SIGNATURE_SIZE..];
        let amount_sat = u64::from_be_bytes(
            amount_bytes
                .try_into()
                .map_err(|_| err("Could not parse amount from OP_RETURN data".to_string()))?,
        );
        let amount = Amount::from_sat(amount_sat);

        let multisig_address = bitcoin::Address::from_script(&tx.output[1].script_pubkey, NETWORK)
            .map_err(|_| err("Could not parse address from output 1".to_string()))?
            .into_unchecked();

        let subnet = match db.get_subnet_by_multisig_address(&multisig_address) {
            Ok(Some(subnet)) => subnet,
            Ok(None) => {
                error!(
                    "StakeCollateralMsg: Could not find subnet with multisig address {:?}.",
                    multisig_address
                );
                return Err(err(format!(
                    "StakeCollateralMsg: Could not find subnet with multisig address {:?}",
                    multisig_address
                )));
            }
            Err(e) => {
                error!(
	                "StakeCollateralMsg: Error while looking up subnet with multisig address {:?}: {}",
	                multisig_address, e
	            );
                return Err(err(format!(
                    "StakeCollateralMsg: Error while looking up subnet with multisig address {:?}: {}",
                    multisig_address, e
                )));
            }
        };

        let secp = bitcoin::secp256k1::Secp256k1::new();

        let prev_txid = tx
            .input
            .first()
            .ok_or(err("No inputs in the transaction".to_string()))?
            .previous_output
            .txid;

        let validator = subnet
            .committee
            .validators
            .iter()
            .find(|v| {
                let msg = IpcUnstakeCollateralMsg::make_signature_msg(
                    &prev_txid, &v.pubkey, &amount, &subnet.id,
                );

                // dbg!(prev_txid);
                // dbg!(v.pubkey);
                // dbg!(amount);
                // dbg!(subnet.id);
                // dbg!(msg);

                secp.verify_schnorr(&signature, &msg, &v.pubkey).is_ok()
            })
            .ok_or(IpcValidateError::InvalidMsg(format!(
                "Signature doesn't match any of the validators in the committee for subnet {}",
                subnet.id
            )))?;

        debug!(
            "Unstake request matches validator xpk {} from signature",
            validator.pubkey
        );

        // Construct the message
        Ok(Self {
            subnet_id: subnet.id,
            amount,
            pubkey: Some(validator.pubkey),
        })
    }

    /// Submits the unstake transaction to Bitcoin
    /// It accepts the current subnet multisig address to send some non-dust amount
    /// to identify the subnet
    ///
    /// First output of the transaction includes the signature of the validator
    /// thus we accept the secret key of the validator
    ///
    /// The signature also includes the prev_txid (of the first input) to prevent
    /// replay attacks. This function first funds the tx
    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
        multisig_address: &bitcoin::Address,
        secret_key: bitcoin::secp256k1::SecretKey,
    ) -> Result<Txid, IpcLibError> {
        info!(
            "Submitting unstake collateral msg to bitcoin. Multisig address = {} Validator XPK = {:?} Amount={}",
            multisig_address, self.pubkey, self.amount
        );

        let signature = bitcoin::secp256k1::schnorr::Signature::from_slice(&[0; 64])
            .expect("All-zero valid signature");

        let fee_rate = get_fee_rate(rpc, None, None);
        let tx = self.to_tx(fee_rate, multisig_address, signature)?;

        // Construct, fund and sign the stake transaction
        let mut tx = crate::wallet::fund_tx(rpc, tx, None)?;
        trace!("Unstake msg funded TX (empty signature): {tx:?}");

        let first_prev_txid = tx
            .input
            .first()
            .ok_or(IpcLibError::MsgParseError(
                IPC_UNSTAKE_TAG,
                "No inputs in the transaction".to_string(),
            ))?
            .previous_output
            .txid;

        // Update the signature with the correct one
        let signature = self.make_signature(&first_prev_txid, secret_key);
        let tx_with_signature = self.to_tx(fee_rate, multisig_address, signature)?;

        // Update our funded tx's first output which has the updated signature
        tx.output[0] = tx_with_signature.output[0].clone();

        let tx = crate::wallet::sign_tx(rpc, tx)?;
        trace!("Unstake msg signed TX: {tx:?}");

        // Submit the transaction to the mempool

        let txid = tx.compute_txid();
        match submit_to_mempool(rpc, vec![tx]) {
            Ok(_) => {
                info!(
                    "Submitted unstake collateral msg for subnet_id={} txid={} amount={}",
                    self.subnet_id, txid, self.amount
                );
                Ok(txid)
            }
            Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
        }
    }

    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let genesis_info =
            db.get_subnet_genesis_info(self.subnet_id)?
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "subnet id={} does not exist",
                    self.subnet_id
                )))?;

        let mut subnet_state = db
            .get_subnet_state(self.subnet_id)
            .map_err(|e| {
                error!("Error getting subnet info from Db: {}", e);
                IpcValidateError::InvalidMsg(e.to_string())
            })?
            // should never happen
            .ok_or_else(|| {
                error!("Subnet {} not found, unexpected", self.subnet_id);
                IpcValidateError::InvalidMsg(format!("Subnet {} not found.", self.subnet_id))
            })?;

        self.validate_for_subnet(&genesis_info, &subnet_state)?;

        // checked before in validate_for_subnet
        let pubkey = self.pubkey.expect("pubkey should be present");

        let committee = subnet_state.latest_committee();

        // we clone and modify the validator
        let mut validator = committee
            .validators
            .iter()
            .find(|v| v.pubkey == pubkey)
            .ok_or(IpcValidateError::InvalidMsg(format!(
                "Validator with public key {} not part of committee",
                pubkey
            )))?
            .clone();

        let new_collateral =
            validator
                .collateral
                .checked_sub(self.amount)
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "Amount {} is greater than current collateral {}",
                    self.amount, validator.collateral
                )))?;

        use crate::multisig::MultisigError;

        // The amount user specified to unstake
        // in case the remaining collateral is too low, we will withdraw all
        // of the funds
        let (new_power, sufficient_to_participate) = match multisig::collateral_to_power(
            &new_collateral,
            &genesis_info.create_subnet_msg.min_validator_stake,
        ) {
            Ok(power) => (power, true),
            Err(MultisigError::InsufficientCollateral) => (0, false),
            Err(e) => return Err(e.into()),
        };

        // Update the next committee or create one if it doesn't exist
        let mut next_committee = subnet_state.latest_committee().clone();

        if new_power == 0 || !sufficient_to_participate {
            // Remove the validator from the committee if power is zero
            next_committee.remove_validator(&self.subnet_id, &pubkey)?;
            info!(
                "Subnet={} Validator XPK {} left the committee by withdrawing {} collateral",
                self.subnet_id, pubkey, self.amount
            );
        } else {
            // Modify the validator in the next committee
            validator.collateral = new_collateral;
            validator.power = new_power;
            next_committee.modify_validator(&self.subnet_id, &validator)?;
            info!(
                "Subnet={} Validator XPK {} updated collateral to {}",
                self.subnet_id, pubkey, self.amount
            );
        }

        // Update the subnet state with the modified next committee
        subnet_state.waiting_committee = Some(next_committee.clone());

        let stake_change_configuration_number =
            db.get_next_stake_change_configuration_number(self.subnet_id)?;

        // Create a stake change request to track this change
        let stake_change = db::StakeChangeRequest {
            change: db::StakingChange::Withdraw {
                amount: self.amount,
            },
            validator_xpk: pubkey,
            validator_subnet_address: validator.subnet_address,
            configuration_number: stake_change_configuration_number,
            committee_after_change: next_committee.clone(),
            block_height,
            block_hash,
            checkpoint_block_height: None,
            checkpoint_block_hash: None,
            txid,
        };

        let mut wtxn = db.write_txn()?;

        // Save the updated subnet state to the database
        db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet_state)?;
        // Add the stake change to the database
        db.add_stake_change(&mut wtxn, self.subnet_id, stake_change)?;

        wtxn.commit()?;

        Ok(())
    }
}

impl IpcValidate for IpcUnstakeCollateralMsg {
    fn validate(&self) -> Result<(), IpcValidateError> {
        // Check subnet_id is not all zeros
        if self.subnet_id == SubnetId::default() {
            return Err(IpcValidateError::InvalidField(
                "subnet_id",
                "Subnet ID cannot be all zeros".to_string(),
            ));
        }

        // Check collateral is not zero
        if self.amount == Amount::ZERO {
            return Err(IpcValidateError::InvalidField(
                "amount",
                "Amount must be greater than zero".to_string(),
            ));
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcKillSubnetMsg {
    /// The subnet id of the subnet to stake
    /// This is derived from 2nd output
    /// that is sent to the subnet multisig address
    pub subnet_id: SubnetId,
    /// The pubkey of the validator who submitted the kill request
    #[serde(skip_deserializing)]
    pub pubkey: Option<bitcoin::XOnlyPublicKey>,
}

impl IpcKillSubnetMsg {
    const TAG: &str = IPC_KILL_SUBNET_TAG;

    // The total length of the op_return data - helper
    const DATA_LEN: usize = IPC_TAG_LENGTH + SCHNORR_SIGNATURE_SIZE;

    pub fn validate_for_subnet(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
        subnet: &db::SubnetState,
    ) -> Result<(), IpcValidateError> {
        // should never happen
        if subnet.id != self.subnet_id || genesis_info.subnet_id != self.subnet_id {
            return Err(IpcValidateError::InvalidField(
                "subnet_id",
                format!(
                    "Subnet ID mismatch: expected {}, got {}",
                    subnet.id, self.subnet_id
                ),
            ));
        }

        let committee = subnet.latest_committee();

        // Check if the validator with this public key is validator
        // NOTE: mandatory check here
        if let Some(pubkey) = self.pubkey {
            if !committee.is_validator(&pubkey) {
                return Err(IpcValidateError::InvalidMsg(format!(
                    "Validator with public key '{}' must be a validator in subnet {}",
                    pubkey, self.subnet_id,
                )));
            }
        } else {
            return Err(IpcValidateError::InvalidField(
                "pubkey",
                "Validator public key is required".to_string(),
            ));
        }

        Ok(())
    }

    /// See `make_signature` for information
    fn make_signature_msg(
        prev_txid: &Txid,
        pubkey: &bitcoin::XOnlyPublicKey,
        subnet_id: &SubnetId,
    ) -> bitcoin::secp256k1::Message {
        let mut msg = Vec::new();
        msg.extend_from_slice(prev_txid.as_byte_array().as_slice());
        msg.extend_from_slice(&pubkey.serialize());
        msg.extend_from_slice(subnet_id.txid20().as_ref());

        let msg = bitcoin::hashes::sha256::Hash::hash(&msg);
        bitcoin::secp256k1::Message::from_digest(msg.to_byte_array())
    }

    /// Returns a signature to be embedded in the OP_RETURN output
    /// Confirming it's valid and signed by the validator making the request
    ///
    /// The signature is signing the following message:
    /// prev_txid || xpubkey || subnet_id
    ///
    /// This is to prevent replay attacks, since the tx inputs
    /// are not enforced or verified
    pub(crate) fn make_signature(
        &self,
        prev_txid: &Txid,
        secret_key: bitcoin::secp256k1::SecretKey,
    ) -> bitcoin::secp256k1::schnorr::Signature {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (pubkey, _) = secret_key.x_only_public_key(&secp);
        let msg = Self::make_signature_msg(prev_txid, &pubkey, &self.subnet_id);

        // Sign the message with the secret key
        // and return the signature
        secp.sign_schnorr(&msg, &secret_key.keypair(&secp))
    }

    pub fn to_tx(
        &self,
        fee_rate: bitcoin::FeeRate,
        multisig_address: &bitcoin::Address,
        signature: bitcoin::secp256k1::schnorr::Signature,
    ) -> Result<Transaction, IpcLibError> {
        //
        // Create the first output: op_return with
        // ipc tag, schnorr signature, amount
        //
        let tag: [u8; IPC_TAG_LENGTH] = IPC_KILL_SUBNET_TAG
            .as_bytes()
            .try_into()
            .expect("IPC_KILL_SUBNET_TAG has incorrect length");

        let sig_bytes = signature.as_ref();

        // Construct op_return data
        let mut op_return_data = [0u8; Self::DATA_LEN];

        op_return_data[0..IPC_TAG_LENGTH].copy_from_slice(&tag);
        op_return_data[IPC_TAG_LENGTH..IPC_TAG_LENGTH + SCHNORR_SIGNATURE_SIZE]
            .copy_from_slice(sig_bytes);

        // Required to do since [u8; 78] for some reason doesn't implement pusbytes
        let push_bytes: &bitcoin::script::PushBytes = (&op_return_data).into();

        // Make op_return script and txout
        let op_return_script = bitcoin_utils::make_op_return_script(push_bytes);
        let data_value = op_return_script.minimal_non_dust_custom(fee_rate);
        let data_tx_out = bitcoin::TxOut {
            value: data_value,
            script_pubkey: op_return_script,
        };

        //
        // Create second output: amount sent to the subnet multisig address
        //
        let multisig_tx_out = bitcoin::TxOut {
            // TODO check this dust calculation?
            value: multisig_address
                .script_pubkey()
                .minimal_non_dust_custom(fee_rate),
            script_pubkey: multisig_address.script_pubkey(),
        };

        // Construct transaction
        let tx_outs = vec![data_tx_out, multisig_tx_out];
        let tx = bitcoin_utils::create_tx_from_txouts(tx_outs);
        debug!("Kill TX: {tx:?}");

        Ok(tx)
    }

    /// Parses an IpcKillSubnetMsg from a Bitcoin transaction
    pub fn from_tx<D: db::Database>(db: &D, tx: &Transaction) -> Result<Self, IpcLibError> {
        use bitcoin::blockdata::script::Instruction;

        // Helper closure for error creation
        let err = |msg: String| IpcLibError::MsgParseError(Self::TAG, msg);

        // We expect at least two outputs:
        // 1. OP_RETURN with IPC tag, signature, and amount
        // 2. Output to the subnet multisig address
        if tx.output.len() < 2 {
            return Err(err(format!(
                "Expected at least 2 outputs for kill subnet message, found {}",
                tx.output.len()
            )));
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

        // Check data length
        if op_return_data.len() != Self::DATA_LEN {
            return Err(err(format!(
                "Invalid OP_RETURN data length: expected {}, got {}",
                Self::DATA_LEN,
                op_return_data.len()
            )));
        }

        // Check IPC tag
        let tag_bytes = &op_return_data[0..IPC_TAG_LENGTH];
        let expected_tag = Self::TAG.as_bytes();
        if tag_bytes != expected_tag {
            return Err(err(format!(
                "Invalid IPC tag: expected {:?}, got {:?}",
                expected_tag, tag_bytes
            )));
        }

        // Extract signature
        let sig_bytes = &op_return_data[IPC_TAG_LENGTH..IPC_TAG_LENGTH + SCHNORR_SIGNATURE_SIZE];
        let signature = bitcoin::secp256k1::schnorr::Signature::from_slice(sig_bytes)
            .map_err(|e| err(format!("Invalid schnorr signature: {}", e)))?;

        let multisig_address = bitcoin::Address::from_script(&tx.output[1].script_pubkey, NETWORK)
            .map_err(|_| err("Could not parse address from output 1".to_string()))?
            .into_unchecked();

        let subnet = match db.get_subnet_by_multisig_address(&multisig_address) {
            Ok(Some(subnet)) => subnet,
            Ok(None) => {
                error!(
                    "KillSubnetMsg: Could not find subnet with multisig address {:?}.",
                    multisig_address
                );
                return Err(err(format!(
                    "KillSubnetMsg: Could not find subnet with multisig address {:?}",
                    multisig_address
                )));
            }
            Err(e) => {
                error!(
                    "KillSubnetMsg: Error while looking up subnet with multisig address {:?}: {}",
                    multisig_address, e
                );
                return Err(err(format!(
                    "KillSubnetMsg: Error while looking up subnet with multisig address {:?}: {}",
                    multisig_address, e
                )));
            }
        };

        let secp = bitcoin::secp256k1::Secp256k1::new();

        let prev_txid = tx
            .input
            .first()
            .ok_or(err("No inputs in the transaction".to_string()))?
            .previous_output
            .txid;

        let validator = subnet
            .committee
            .validators
            .iter()
            .find(|v| {
                let msg = IpcKillSubnetMsg::make_signature_msg(&prev_txid, &v.pubkey, &subnet.id);

                secp.verify_schnorr(&signature, &msg, &v.pubkey).is_ok()
            })
            .ok_or(IpcValidateError::InvalidMsg(format!(
                "Signature doesn't match any of the validators in the committee for subnet {}",
                subnet.id
            )))?;

        debug!(
            "Kill request matches validator xpk {} from signature",
            validator.pubkey
        );

        // Construct the message
        Ok(Self {
            subnet_id: subnet.id,
            pubkey: Some(validator.pubkey),
        })
    }

    /// Submits the kill subnet transaction to Bitcoin
    /// It accepts the current subnet multisig address to send some non-dust amount
    /// to identify the subnet
    ///
    /// First output of the transaction includes the signature of the validator
    /// thus we accept the secret key of the validator
    ///
    /// The signature also includes the prev_txid (of the first input) to prevent
    /// replay attacks. This function first funds the tx
    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
        multisig_address: &bitcoin::Address,
        secret_key: bitcoin::secp256k1::SecretKey,
    ) -> Result<Txid, IpcLibError> {
        info!(
            "Submitting kill subnet msg to bitcoin. Multisig address = {} Validator XPK = {:?}",
            multisig_address, self.pubkey
        );

        let signature = bitcoin::secp256k1::schnorr::Signature::from_slice(&[0; 64])
            .expect("All-zero valid signature");

        let fee_rate = get_fee_rate(rpc, None, None);
        let tx = self.to_tx(fee_rate, multisig_address, signature)?;

        // Construct, fund and sign the stake transaction
        let mut tx = crate::wallet::fund_tx(rpc, tx, None)?;
        trace!("Kill subnet msg funded TX (empty signature): {tx:?}");

        let first_prev_txid = tx
            .input
            .first()
            .ok_or(IpcLibError::MsgParseError(
                Self::TAG,
                "No inputs in the transaction".to_string(),
            ))?
            .previous_output
            .txid;

        // Update the signature with the correct one
        let signature = self.make_signature(&first_prev_txid, secret_key);
        let tx_with_signature = self.to_tx(fee_rate, multisig_address, signature)?;

        // Update our funded tx's first output which has the updated signature
        tx.output[0] = tx_with_signature.output[0].clone();

        let tx = crate::wallet::sign_tx(rpc, tx)?;
        trace!("Kill subnet msg signed TX: {tx:?}");

        // Submit the transaction to the mempool

        let txid = tx.compute_txid();
        match submit_to_mempool(rpc, vec![tx]) {
            Ok(_) => {
                info!(
                    "Submitted kill subnet msg for subnet_id={} txid={}",
                    self.subnet_id, txid,
                );
                Ok(txid)
            }
            Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
        }
    }

    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let genesis_info =
            db.get_subnet_genesis_info(self.subnet_id)?
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "subnet id={} does not exist",
                    self.subnet_id
                )))?;

        let subnet_state = db
            .get_subnet_state(self.subnet_id)
            .map_err(|e| {
                error!("Error getting subnet info from Db: {}", e);
                IpcValidateError::InvalidMsg(e.to_string())
            })?
            // should never happen
            .ok_or_else(|| {
                error!("Subnet {} not found, unexpected", self.subnet_id);
                IpcValidateError::InvalidMsg(format!("Subnet {} not found.", self.subnet_id))
            })?;

        self.validate_for_subnet(&genesis_info, &subnet_state)?;

        // checked before in validate_for_subnet
        let pubkey = self.pubkey.expect("pubkey should be present");

        let committee = subnet_state.latest_committee();

        // we clone the validator to verify it exists
        let _validator = committee
            .validators
            .iter()
            .find(|v| v.pubkey == pubkey)
            .ok_or(IpcValidateError::InvalidMsg(format!(
                "Validator with public key {} not part of committee",
                pubkey
            )))?
            .clone();

        // Get all valid kill requests not including the new one to be added
        let mut valid_kill_requests = db.get_valid_kill_requests(self.subnet_id, block_height)?;

        // Create the kill request
        let kill_request = db::KillRequest {
            validator_xpk: pubkey,
            block_height,
            block_hash,
            txid,
        };

        if valid_kill_requests
            .iter()
            .any(|req| req.validator_xpk == kill_request.validator_xpk)
        {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Validator {} has already submitted a kill request for subnet {}",
                kill_request.validator_xpk.clone(),
                self.subnet_id
            ))
            .into());
        }

        // Append the new kill request
        valid_kill_requests.push(kill_request.clone());

        let mut wtxn = db.write_txn()?;

        // Add the kill request to the database
        db.add_kill_request(&mut wtxn, self.subnet_id, kill_request)?;

        // Calculate total power of validators who have submitted kill requests
        let mut kill_request_power = 0u32;
        for kill_req in &valid_kill_requests {
            if let Some(validator) = committee
                .validators
                .iter()
                .find(|v| v.pubkey == kill_req.validator_xpk)
            {
                kill_request_power += validator.power;
            }
        }

        // Check if we have reached 2/3 majority
        let total_power = committee.total_power();
        let threshold = multisig::multisig_threshold(total_power);

        if kill_request_power >= threshold {
            use db::SubnetKillState::*;

            match subnet_state.killed {
                NotKilled => {
                    info!(
						"Kill request majority reached for subnet {}: {}/{} power, marking subnet pending killed.",
						self.subnet_id, kill_request_power, total_power
					);
                    // Mark the subnet as to be killed
                    let mut updated_subnet_state = subnet_state;
                    updated_subnet_state.killed = ToBeKilled;
                    db.save_subnet_state(&mut wtxn, self.subnet_id, &updated_subnet_state)?;
                }
                ToBeKilled => {
                    info!(
						"Kill request majority reached for subnet {}: {}/{} power, but subnet is already pending killed.",
						self.subnet_id, kill_request_power, total_power
					);
                }
                Killed { .. } => {
                    info!(
						"Kill request majority reached for subnet {}: {}/{} power, but subnet is already killed.",
						self.subnet_id, kill_request_power, total_power
					);
                }
            }
        } else {
            info!(
                "Kill request added for subnet {} but majority not reached: {}/{} power (threshold: {})",
                self.subnet_id, kill_request_power, total_power, threshold
            );
        }

        wtxn.commit()?;

        Ok(())
    }
}

impl IpcValidate for IpcKillSubnetMsg {
    fn validate(&self) -> Result<(), IpcValidateError> {
        // Check subnet_id is not all zeros
        if self.subnet_id == SubnetId::default() {
            return Err(IpcValidateError::InvalidField(
                "subnet_id",
                "Subnet ID cannot be all zeros".to_string(),
            ));
        }

        Ok(())
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
    StakeCollateral(IpcStakeCollateralMsg),
    UnstakeCollateral(IpcUnstakeCollateralMsg),
    KillSubnet(IpcKillSubnetMsg),
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
            IpcTag::StakeCollateral => {
                Err(IpcSerializeError::DeserializationError("Skip".to_string()))
            }
            IpcTag::UnstakeCollateral => {
                Err(IpcSerializeError::DeserializationError("Skip".to_string()))
            }
            IpcTag::KillSubnet => Err(IpcSerializeError::DeserializationError("Skip".to_string())),
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
    use bitcoin::{Amount, XOnlyPublicKey};
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
    use crate::{
        db::Database,
        test_utils::{self, create_test_db},
        DEFAULT_BTC_FEE_RATE,
    };
    use std::str::FromStr;

    #[test]
    #[test_retry::retry(3)]
    fn test_checkpoint_with_unstakes() {
        // Set up test database
        let db = create_test_db();

        // Create a test subnet
        let subnet = test_utils::generate_subnet(3);
        let committee = &subnet.committee;
        let utxos = create_test_utxos(committee.address_checked().script_pubkey());

        // dbg!(&subnet);

        // Save subnets to database
        {
            let mut wtxn = db.write_txn().expect("Should create transaction");
            db.save_subnet_state(&mut wtxn, subnet.id, &subnet)
                .expect("Should save subnet");
            wtxn.commit().unwrap();
        }

        // Create checkpoint message with unstakes
        let mut checkpoint_msg = create_test_checkpoint_msg();
        // Update subnet i
        checkpoint_msg.subnet_id = subnet.id;
        // Remove transfers for this test
        checkpoint_msg.transfers.clear();
        // Update unstake address
        checkpoint_msg.unstakes.first_mut().unwrap().address = subnet
            .committee
            .validators
            .first()
            .unwrap()
            .backup_address
            .clone();

        // Verify the test checkpoint message has unstakes
        assert!(!checkpoint_msg.unstakes.is_empty());
        let original_unstakes = checkpoint_msg.unstakes.clone();

        // Generate the transaction
        let checkpoint_psbt = checkpoint_msg
            .to_checkpoint_psbt(committee, committee, DEFAULT_BTC_FEE_RATE, &utxos)
            .expect("Should create checkpoint PSBT");

        let checkpoint_tx = checkpoint_psbt.extract_tx().expect("must extract tx");

        dbg!(&checkpoint_tx);

        // Parse the transaction back to a checkpoint message
        let parsed_msg = IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &checkpoint_tx)
            .expect("Should parse checkpoint message");
        dbg!(&parsed_msg);

        // Verify unstakes were correctly serialized and deserialized
        assert_eq!(parsed_msg.unstakes.len(), original_unstakes.len());
        for (i, unstake) in parsed_msg.unstakes.iter().enumerate() {
            assert_eq!(unstake.amount, original_unstakes[i].amount);
            assert_eq!(unstake.address, original_unstakes[i].address);
            // Note: pubkey is not preserved in the checkpoint transaction
        }

        // Verify metadata and markers
        let (withdrawal_count, transfer_count, unstake_count, killed) =
            IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&checkpoint_tx.output[0])
                .expect("Should extract markers");

        assert_eq!(withdrawal_count as usize, checkpoint_msg.withdrawals.len());
        assert_eq!(transfer_count as usize, checkpoint_msg.transfers.len());
        assert_eq!(unstake_count as usize, original_unstakes.len());
        assert_eq!(killed, checkpoint_msg.is_kill_checkpoint);
    }

    fn create_test_checkpoint_msg() -> IpcCheckpointSubnetMsg {
        // Generate a subnet with 3 validators
        let subnet_state = test_utils::generate_subnet(3);

        // Create a second subnet for cross-subnet transfers
        let destination_subnet = test_utils::generate_subnet(3);

        let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap();

        // Create an unstake
        let unstake = IpcUnstake {
            amount: Amount::from_sat(40000),
            address: bitcoin::Address::from_str("bcrt1qgtzpfqlfkz4nhkpvz2enqtucm2pzhdvlssxnss")
                .unwrap(),
            pubkey: bitcoin::XOnlyPublicKey::from_str(
                "b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0",
            )
            .unwrap(),
        };

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
            unstakes: vec![unstake],
            withdrawals: vec![withdrawal],
            transfers: vec![transfer],
            change_address: Some(subnet_state.committee.multisig_address.clone()),
            next_committee_configuration_number: 30, // arbitrary test number
            is_kill_checkpoint: false,
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
            .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
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

        // Check the unstake count marker
        assert_eq!(
            op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 2],
            1,
            "Unstake count marker should be 1"
        );

        assert_eq!(checkpoint_tx.output.len(), 6, "Should have 6 outputs");

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
            unstakes: vec![],
            change_address: Some(committee.multisig_address.clone()),
            next_committee_configuration_number: 1,
            is_kill_checkpoint: false,
        };

        let fee_rate = DEFAULT_BTC_FEE_RATE;

        // Generate transactions
        let checkpoint_psbt = checkpoint_msg
            .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
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
            unstakes: vec![],
            change_address: Some(committee.multisig_address.clone()),
            next_committee_configuration_number: 1,
            is_kill_checkpoint: false,
        };

        let fee_rate = DEFAULT_BTC_FEE_RATE;

        // Generate transactions
        let checkpoint_psbt = checkpoint_msg
            .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
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

    #[test]
    #[test_retry::retry(3)]
    fn test_checkpoint_batch_transfers_to_same_subnet() {
        // Create a subnet with 3 validators - this will be our source subnet
        let source_subnet = test_utils::generate_subnet(3);
        let committee = &source_subnet.committee;
        let utxos = create_test_utxos(committee.address_checked().script_pubkey());

        // Create a destination subnet for transfers
        let destination_subnet = test_utils::generate_subnet(3);

        let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap();

        // Create two transfers to the same destination subnet but different addresses
        let transfer1 = IpcCrossSubnetTransfer {
            amount: Amount::from_sat(30000),
            destination_subnet_id: destination_subnet.id,
            subnet_multisig_address: Some(destination_subnet.committee.multisig_address.clone()),
            subnet_user_address: alloy_primitives::Address::from_str(
                "742d35Cc6634C0532925a3b844Bc454e4438f44e",
            )
            .unwrap(),
        };

        let transfer2 = IpcCrossSubnetTransfer {
            amount: Amount::from_sat(40000),
            destination_subnet_id: destination_subnet.id,
            subnet_multisig_address: Some(destination_subnet.committee.multisig_address.clone()),
            subnet_user_address: alloy_primitives::Address::from_str(
                "1111111111111111111111111111111111111111", // Different address
            )
            .unwrap(),
        };

        // Create a second destination subnet for a third transfer
        let destination_subnet2 = test_utils::generate_subnet(3);

        let transfer3 = IpcCrossSubnetTransfer {
            amount: Amount::from_sat(50000),
            destination_subnet_id: destination_subnet2.id,
            subnet_multisig_address: Some(destination_subnet2.committee.multisig_address.clone()),
            subnet_user_address: alloy_primitives::Address::from_str(
                "2222222222222222222222222222222222222222",
            )
            .unwrap(),
        };

        let checkpoint_msg = IpcCheckpointSubnetMsg {
            subnet_id: source_subnet.id,
            checkpoint_hash,
            checkpoint_height: 50,
            transfers: vec![transfer1.clone(), transfer2.clone(), transfer3.clone()],
            withdrawals: vec![],
            unstakes: vec![],
            change_address: Some(committee.multisig_address.clone()),
            next_committee_configuration_number: 1,
            is_kill_checkpoint: false,
        };

        let fee_rate = DEFAULT_BTC_FEE_RATE;

        // Generate transactions
        let checkpoint_psbt = checkpoint_msg
            .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
            .unwrap();
        let checkpoint_tx = checkpoint_psbt.unsigned_tx.clone();

        // Verify transaction output structure:
        // 1. OP_RETURN metadata
        // 2. Batch transfer commit output
        // 3. First destination subnet output (combined from transfer1 + transfer2)
        // 4. Second destination subnet output (from transfer3)
        // 5. Change output
        assert_eq!(checkpoint_tx.output.len(), 5, "Should have 5 outputs");

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

        // Check the transfer count marker is 3
        assert_eq!(
            op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 1],
            3,
            "Transfer count marker should be 3"
        );

        // Check that there's a batch transfer commit output
        let batch_tx_out = &checkpoint_tx.output[1]; // Second output should be batch transfer commit
        assert!(
            batch_tx_out.script_pubkey.is_p2tr(),
            "Batch transfer commit output should be P2TR"
        );

        // Third output should be to the first destination subnet
        let combined_output = &checkpoint_tx.output[2];
        assert_eq!(
            combined_output.script_pubkey,
            destination_subnet
                .committee
                .multisig_address
                .assume_checked()
                .script_pubkey(),
            "Output should be to the first destination subnet multisig address"
        );

        // The combined output value should be transfer1 + transfer2
        let expected_value = transfer1.amount + transfer2.amount;
        assert_eq!(
            combined_output.value, expected_value,
            "Combined output value should equal sum of transfer1 and transfer2"
        );

        // Fourth output should be to the second destination subnet
        let second_subnet_output = &checkpoint_tx.output[3];
        assert_eq!(
            second_subnet_output.script_pubkey,
            destination_subnet2
                .committee
                .multisig_address
                .assume_checked()
                .script_pubkey(),
            "Output should be to the second destination subnet multisig address"
        );

        // The second subnet output value should match transfer3
        assert_eq!(
            second_subnet_output.value, transfer3.amount,
            "Second subnet output value should equal transfer3"
        );

        // Make the batch reveal transaction
        let batch_tx = checkpoint_msg
            .make_reveal_batch_transfer_tx(
                checkpoint_tx.compute_txid(),
                fee_rate,
                &committee.address_checked(),
            )
            .unwrap();

        // Verify we have a batch transaction
        assert!(
            batch_tx.is_some(),
            "Should have a batch transfer transaction"
        );
    }

    #[test]
    fn test_stake_collateral_from_tx() {
        let db = crate::test_utils::create_test_db();
        let subnet_id = crate::test_utils::generate_subnet_id();
        let subnet_state = crate::db::SubnetState {
            id: subnet_id,
            committee_number: 1,
            committee: crate::db::SubnetCommittee {
                configuration_number: 0,
                threshold: 1,
                validators: vec![],
                multisig_address: bitcoin::Address::from_str(
                    "bcrt1p2wqu7w8n8mnw37sl40u6y9tlpyy9hy6d2k4wt6r8ut7897ejl8fsgdhxzg",
                )
                .unwrap(),
            },
            waiting_committee: None,
            last_checkpoint_number: None,
            killed: db::SubnetKillState::NotKilled,
        };

        {
            let mut wtxn = db.write_txn().unwrap();
            db.save_subnet_state(&mut wtxn, subnet_id, &subnet_state)
                .expect("saves subnet");
            wtxn.commit().expect("commits transaction");
        }

        let txbytes = hex::decode("020000000001019c40ba653f54403e8bfda357ccd6fff74746dea98d22316cd194544c27fe81ac0000000000fdffffff030000000000000000286a2649504353544b851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf460ec5300000000002251205381cf38f33ee6e8fa1fabf9a2157f09085b934d55aae5e867e2fc72fb32f9d31e03b229010000002251209acb5b5b3729a8f9ca2c2df76a6e600974f858352dbbca140e7e7d732e9da145024730440220316c97ebc27947c85e5ddec43d35075dc029b244188161265c4f540ceabd40d302200a7940cb366e46f1ba744ea84f1c34b6ade3fd3425f424099f19289fb64321ff012103fe6bbd509c039530b43c6c93524ec4d3df82da5de278096c095694166ccb828200000000").expect("Failed to parse transaction hex");

        let tx: bitcoin::Transaction =
            bitcoin::consensus::deserialize(&txbytes).expect("Failed to deserialize transaction");

        let msg = IpcStakeCollateralMsg::from_tx(&db, &tx).expect("Should parse tx as msg");

        assert_eq!(msg.amount, Amount::from_sat(5500000));
        assert_eq!(
            msg.pubkey,
            XOnlyPublicKey::from_str(
                "851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4"
            )
            .unwrap()
        );

        dbg!(&msg);
    }

    #[test]
    fn test_unstake_collateral_from_tx() {
        use crate::db::SubnetValidators;
        use crate::DEFAULT_BTC_FEE_RATE;

        let db = crate::test_utils::create_test_db();

        // Create test data
        let mut subnet_state = crate::test_utils::generate_subnet(4);
        let subnet_id = subnet_state.id;

        let keypairs = crate::test_utils::generate_keypairs(1);
        let keypair = keypairs.first().expect("should generate");
        let (xpk, _) = keypair.x_only_public_key();

        let prev_txid = bitcoin::Txid::all_zeros();

        // Update the subnet to use our validator
        subnet_state
            .committee
            .validators
            .first_mut()
            .unwrap()
            .pubkey = xpk;

        subnet_state.committee.multisig_address = subnet_state
            .committee
            .validators
            .multisig_address(&subnet_id);

        {
            let mut wtxn = db.write_txn().unwrap();
            db.save_subnet_state(&mut wtxn, subnet_id, &subnet_state)
                .expect("saves subnet");
            wtxn.commit().expect("commits transaction");
        }

        let amount = bitcoin::Amount::from_sat(5500000);

        // Create the unstake message
        let unstake_msg = IpcUnstakeCollateralMsg {
            subnet_id,
            amount,
            pubkey: Some(xpk),
        };

        let msg =
            IpcUnstakeCollateralMsg::make_signature_msg(&prev_txid, &xpk, &amount, &subnet_id);

        dbg!(&subnet_state.committee);
        dbg!(prev_txid);
        dbg!(xpk);
        dbg!(amount);
        dbg!(subnet_id);
        dbg!(msg);

        let signature = unstake_msg.make_signature(&prev_txid, keypair.secret_key());

        // Generate a transaction from the message
        let mut tx = unstake_msg
            .to_tx(
                DEFAULT_BTC_FEE_RATE,
                &subnet_state.committee.multisig_address.assume_checked(),
                signature,
            )
            .expect("Should create transaction");

        tx.input = vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint {
                txid: prev_txid,
                vout: 0,
            },
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: bitcoin::Sequence::MAX,
            witness: bitcoin::Witness::new(),
        }];

        // Test parsing the transaction back into a message
        let parsed_msg =
            IpcUnstakeCollateralMsg::from_tx(&db, &tx).expect("Should parse tx as msg");

        // Verify the parsed message matches the original
        assert_eq!(parsed_msg.subnet_id, subnet_id);
        assert_eq!(parsed_msg.amount, amount);
        assert_eq!(parsed_msg.pubkey, Some(xpk));

        // Print debug info
        println!("Original subnet_id: {}", subnet_id);
        println!("Parsed subnet_id  : {}", parsed_msg.subnet_id);

        // Test with hexadecimal transaction (easier to debug if needed)
        let serialized_tx = bitcoin::consensus::serialize(&tx);
        let hex_tx = hex::encode(&serialized_tx);
        println!("Transaction hex: {}", hex_tx);

        let tx_from_hex: bitcoin::Transaction =
            bitcoin::consensus::deserialize(&hex::decode(hex_tx).unwrap()).unwrap();

        let parsed_msg_from_hex = IpcUnstakeCollateralMsg::from_tx(&db, &tx_from_hex)
            .expect("Should parse tx from hex as msg");

        assert_eq!(parsed_msg_from_hex.subnet_id, subnet_id);
        assert_eq!(parsed_msg_from_hex.amount, amount);
        assert_eq!(parsed_msg_from_hex.pubkey, Some(xpk));
    }

    #[test]
    fn test_unstake_collateral_unknown_signature() {
        use crate::DEFAULT_BTC_FEE_RATE;

        let db = crate::test_utils::create_test_db();

        // Create test data
        let subnet_state = crate::test_utils::generate_subnet(4);
        let subnet_id = subnet_state.id;

        // Intentionaly don't add this validator to the subnet committee
        let keypairs = crate::test_utils::generate_keypairs(1);
        let keypair = keypairs.first().expect("should generate");
        let (xpk, _) = keypair.x_only_public_key();

        let prev_txid = bitcoin::Txid::all_zeros();

        {
            let mut wtxn = db.write_txn().unwrap();
            db.save_subnet_state(&mut wtxn, subnet_id, &subnet_state)
                .expect("saves subnet");
            wtxn.commit().expect("commits transaction");
        }

        let amount = bitcoin::Amount::from_sat(5500000);

        // Create the unstake message
        let unstake_msg = IpcUnstakeCollateralMsg {
            subnet_id,
            amount,
            pubkey: Some(xpk),
        };

        let msg =
            IpcUnstakeCollateralMsg::make_signature_msg(&prev_txid, &xpk, &amount, &subnet_id);

        dbg!(&subnet_state.committee);
        dbg!(prev_txid);
        dbg!(xpk);
        dbg!(amount);
        dbg!(subnet_id);
        dbg!(msg);

        let signature = unstake_msg.make_signature(&prev_txid, keypair.secret_key());

        // Generate a transaction from the message
        let mut tx = unstake_msg
            .to_tx(
                DEFAULT_BTC_FEE_RATE,
                &subnet_state.committee.multisig_address.assume_checked(),
                signature,
            )
            .expect("Should create transaction");

        tx.input = vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint {
                txid: prev_txid,
                vout: 0,
            },
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: bitcoin::Sequence::MAX,
            witness: bitcoin::Witness::new(),
        }];

        // Test parsing the transaction back into a message
        let parsed_msg = IpcUnstakeCollateralMsg::from_tx(&db, &tx);
        assert!(parsed_msg.is_err());

        let expected_error_msg = format!(
            "Signature doesn't match any of the validators in the committee for subnet {}",
            subnet_id
        );

        if let Err(IpcLibError::IpcValidateError(crate::ipc_lib::IpcValidateError::InvalidMsg(
            msg,
        ))) = parsed_msg
        {
            assert_eq!(msg, expected_error_msg);
        } else {
            panic!(
                "Expected IpcValidateError::InvalidMsg but got: {:?}",
                parsed_msg
            );
        }
    }

    #[test]
    fn test_kill_checkpoint_change_distribution() {
        let subnet = test_utils::generate_subnet(3);

        // Clone the multisig address early to avoid borrow issues
        let committee_address = subnet.committee.multisig_address.clone();

        // Create unstakes with different amounts
        let unstake1 = IpcUnstake {
            amount: Amount::from_sat(300_000), // 30% of total
            address: subnet.committee.validators[0].backup_address.clone(),
            pubkey: subnet.committee.validators[0].pubkey.clone(),
        };
        let unstake2 = IpcUnstake {
            amount: Amount::from_sat(500_000), // 50% of total
            address: subnet.committee.validators[1].backup_address.clone(),
            pubkey: subnet.committee.validators[1].pubkey.clone(),
        };
        let unstake3 = IpcUnstake {
            amount: Amount::from_sat(200_000), // 20% of total
            address: subnet.committee.validators[2].backup_address.clone(),
            pubkey: subnet.committee.validators[2].pubkey.clone(),
        };

        let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_byte_array([42u8; 32]);

        // Create a kill checkpoint with unstakes
        let checkpoint_msg = IpcCheckpointSubnetMsg {
            subnet_id: subnet.id,
            checkpoint_hash,
            checkpoint_height: 50,
            withdrawals: vec![],
            transfers: vec![],
            unstakes: vec![unstake1.clone(), unstake2.clone(), unstake3.clone()],
            change_address: None, // Kill checkpoints shouldn't have change address
            next_committee_configuration_number: 1,
            is_kill_checkpoint: true,
        };

        // Create test UTXOs with extra funds that will become change
        let extra_change = Amount::from_sat(100_000); // This should be distributed among unstakes
        let script_pub_key = committee_address
            .clone()
            .require_network(NETWORK)
            .unwrap()
            .script_pubkey();
        let unspent = vec![bitcoincore_rpc::json::ListUnspentResultEntry {
            txid: bitcoin::Txid::from_str(
                "f61b1742ca13176464adb3cb66050c00787bb3a4eead37e985f2df1e37718126",
            )
            .unwrap(),
            vout: 0,
            address: None,
            label: None,
            redeem_script: None,
            witness_script: None,
            script_pub_key: script_pub_key.clone(),
            amount: checkpoint_msg
                .unstakes
                .iter()
                .map(|u| u.amount)
                .sum::<Amount>()
                + extra_change
                + Amount::from_sat(4_000), // Add some buffer for fees
            descriptor: None,
            spendable: true,
            solvable: true,
            safe: true,
            confirmations: 6,
        }];

        // Generate the checkpoint PSBT
        let psbt_result = checkpoint_msg.to_checkpoint_psbt(
            &subnet.committee,
            &subnet.committee, // Same committee (no rotation)
            DEFAULT_BTC_FEE_RATE,
            &unspent,
        );

        assert!(psbt_result.is_ok(), "Failed to create checkpoint PSBT");
        let psbt = psbt_result.unwrap();

        // Check that there's no change output in the transaction
        let committee_script = committee_address
            .require_network(NETWORK)
            .unwrap()
            .script_pubkey();
        let change_outputs: Vec<_> = psbt
            .unsigned_tx
            .output
            .iter()
            .filter(|out| out.script_pubkey == committee_script)
            .collect();

        dbg!(&psbt.unsigned_tx);
        dbg!(&change_outputs);

        assert_eq!(
            change_outputs.len(),
            0,
            "Kill checkpoint should not have change output"
        );

        // Verify that unstake outputs have been increased
        // The unstake outputs should be outputs 1, 2, 3 (after metadata at index 0)
        let total_original_unstakes: Amount =
            checkpoint_msg.unstakes.iter().map(|u| u.amount).sum();
        let total_final_unstakes: Amount = psbt.unsigned_tx.output[1..4]
            .iter()
            .map(|out| out.value)
            .sum();

        // The final unstakes should be larger than original due to distributed change
        assert!(
            total_final_unstakes > total_original_unstakes,
            "Unstake outputs should have increased due to change distribution"
        );

        // Verify proportional distribution
        let expected_increase_1 = Amount::from_sat((extra_change.to_sat() as f64 * 0.3) as u64); // 30%
        let expected_increase_2 = Amount::from_sat((extra_change.to_sat() as f64 * 0.5) as u64); // 50%
        let expected_increase_3 = Amount::from_sat((extra_change.to_sat() as f64 * 0.2) as u64); // 20%

        let actual_1 = psbt.unsigned_tx.output[1].value;
        let actual_2 = psbt.unsigned_tx.output[2].value;
        let actual_3 = psbt.unsigned_tx.output[3].value;

        // Allow some tolerance for rounding

        dbg!(actual_1 - (unstake1.amount + expected_increase_1));
        dbg!(actual_2 - (unstake2.amount + expected_increase_2));
        dbg!(actual_3 - (unstake3.amount + expected_increase_3));

        assert!((actual_1 - (unstake1.amount + expected_increase_1)) < Amount::from_sat(200));
        assert!((actual_2 - (unstake2.amount + expected_increase_2)) < Amount::from_sat(200));
        assert!((actual_3 - (unstake3.amount + expected_increase_3)) < Amount::from_sat(200));

        // Verify total is conserved (minus fees)
        let total_input: Amount = unspent.iter().map(|u| u.amount).sum();
        let total_output: Amount = psbt.unsigned_tx.output.iter().map(|out| out.value).sum();
        assert!(
            total_input >= total_output,
            "Total output should not exceed total input"
        );
    }

    #[test]
    fn test_unstake_collateral_replay_attack() {
        use crate::db::SubnetValidators;
        use crate::DEFAULT_BTC_FEE_RATE;

        let db = crate::test_utils::create_test_db();

        // Create test data
        let mut subnet_state = crate::test_utils::generate_subnet(4);
        let subnet_id = subnet_state.id;

        let keypairs = crate::test_utils::generate_keypairs(1);
        let keypair = keypairs.first().expect("should generate");
        let (xpk, _) = keypair.x_only_public_key();

        let prev_txid = bitcoin::Txid::all_zeros();

        // Update the subnet to use our validator
        subnet_state
            .committee
            .validators
            .first_mut()
            .unwrap()
            .pubkey = xpk;

        subnet_state.committee.multisig_address = subnet_state
            .committee
            .validators
            .multisig_address(&subnet_id);

        {
            let mut wtxn = db.write_txn().unwrap();
            db.save_subnet_state(&mut wtxn, subnet_id, &subnet_state)
                .expect("saves subnet");
            wtxn.commit().expect("commits transaction");
        }

        let amount = bitcoin::Amount::from_sat(5500000);

        // Create the unstake message
        let unstake_msg = IpcUnstakeCollateralMsg {
            subnet_id,
            amount,
            pubkey: Some(xpk),
        };

        let msg =
            IpcUnstakeCollateralMsg::make_signature_msg(&prev_txid, &xpk, &amount, &subnet_id);

        dbg!(&subnet_state.committee);
        dbg!(prev_txid);
        dbg!(xpk);
        dbg!(amount);
        dbg!(subnet_id);
        dbg!(msg);

        let signature = unstake_msg.make_signature(&prev_txid, keypair.secret_key());

        // Generate a transaction from the message
        let mut tx = unstake_msg
            .to_tx(
                DEFAULT_BTC_FEE_RATE,
                &subnet_state.committee.multisig_address.assume_checked(),
                signature,
            )
            .expect("Should create transaction");

        tx.input = vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint {
                // Intentionally set to a different txid to simulate a replay attack
                txid: Txid::from_str(
                    "5e66f5f4e961aa5aa35d226c100e1931d5f4ee99e149a3a9549edad279f77236",
                )
                .expect("valid txid"),
                vout: 0,
            },
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: bitcoin::Sequence::MAX,
            witness: bitcoin::Witness::new(),
        }];

        // Test parsing the transaction back into a message
        let parsed_msg = IpcUnstakeCollateralMsg::from_tx(&db, &tx);
        assert!(parsed_msg.is_err());

        // Even though our validator is in the committee, the prev_txid is different
        // it will not match the signature

        let expected_error_msg = format!(
            "Signature doesn't match any of the validators in the committee for subnet {}",
            subnet_id
        );

        if let Err(IpcLibError::IpcValidateError(crate::ipc_lib::IpcValidateError::InvalidMsg(
            msg,
        ))) = parsed_msg
        {
            assert_eq!(msg, expected_error_msg);
        } else {
            panic!(
                "Expected IpcValidateError::InvalidMsg but got: {:?}",
                parsed_msg
            );
        }
    }
}
