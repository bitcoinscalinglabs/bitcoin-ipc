use bitcoin::address::NetworkUnchecked;

use bitcoin::hashes::Hash;
use bitcoin::key::constants::{SCHNORR_PUBLIC_KEY_SIZE, SCHNORR_SIGNATURE_SIZE};
use bitcoin::Amount;
use bitcoin::Transaction;
use bitcoin::Txid;
use bitcoin::XOnlyPublicKey;
use heed::{RoTxn, RwTxn};
use log::error;
use log::trace;
use log::warn;
use log::{debug, info};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::str::FromStr;
use thiserror::Error;

use crate::eots::{signature_from_bytes, signature_to_bytes, EotsNonceCommitment, Signature64};
use crate::slash::SlashSignatureEntry;
use crate::bitcoin_utils::get_fee_rate;
use crate::bitcoin_utils::{self, submit_to_mempool, tagged_sha256, verify_schnorr_bytes};
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
        IpcCreateSubnetMsg, IpcJoinSubnetMsg, IpcJoinUeMsg, IpcMessage, IpcTag, SubnetId,
        IPC_CHECKPOINT_TAG, IPC_CREATE_SUBNET_TAG, IPC_DELETE_SUBNET_TAG, IPC_FUND_SUBNET_TAG,
        IPC_JOIN_SUBNET_TAG, IPC_JOIN_UE_TAG, IPC_PREFUND_SUBNET_TAG, IPC_TAG_DELIMITER,
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
pub const IPC_JOIN_UE_TAG: &str = "IPCJUE";

// Static assertion to verify tag lengths at compile time
const _: () = {
    assert!(IPC_JOIN_UE_TAG.len() == IPC_TAG_LENGTH);
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

use alloy_primitives::{I256, U256};

/// Compact length-prefixed encoding for U256 and I256, usable as `#[serde(with = "...")]`.
///
/// Wire format for unsigned (compact_amount::u256):
///   [1-byte length N][N bytes big-endian value, no leading zeros]
///
/// Wire format for signed (compact_amount::i256):
///   [1-byte header: high bit = sign, low 7 bits = N][N bytes big-endian absolute value]
mod compact_amount {
    use alloy_primitives::{Sign, I256, U256};
    use serde::{de, Deserializer, Serializer};

    fn encode_magnitude(abs: U256) -> Vec<u8> {
        let bytes = abs.to_be_bytes::<32>();
        let leading = bytes.iter().take_while(|&&b| b == 0).count();
        let payload = &bytes[leading..];
        let mut buf = Vec::with_capacity(1 + payload.len());
        buf.push(payload.len() as u8);
        buf.extend_from_slice(payload);
        buf
    }

    fn decode_magnitude<E: de::Error>(v: &[u8]) -> Result<U256, E> {
        if v.is_empty() {
            return Err(E::custom("empty compact amount"));
        }
        let n = (v[0] & 0x7F) as usize; // low 7 bits = length; high bit reserved for i256 sign
        if n > 32 {
            return Err(E::custom("compact amount length > 32"));
        }
        if v.len() != 1 + n {
            return Err(E::custom("compact amount length mismatch"));
        }
        let mut be = [0u8; 32];
        if n > 0 {
            be[32 - n..].copy_from_slice(&v[1..]);
        }
        Ok(U256::from_be_bytes(be))
    }

    pub mod u256 {
        use super::*;

        pub fn serialize<S: Serializer>(v: &U256, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                serde::Serialize::serialize(v, s)
            } else {
                s.serialize_bytes(&encode_magnitude(*v))
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
            if d.is_human_readable() {
                return serde::Deserialize::deserialize(d);
            }
            struct V;
            impl<'de> de::Visitor<'de> for V {
                type Value = U256;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(f, "compact U256")
                }
                fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<U256, E> {
                    decode_magnitude(v)
                }
            }
            d.deserialize_bytes(V)
        }
    }

    pub mod i256 {
        use super::*;

        pub fn serialize<S: Serializer>(v: &I256, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                return s.collect_str(v);
            }
            let (sign, abs) = v.into_sign_and_abs();
            let mut buf = encode_magnitude(abs);
            if sign == Sign::Negative {
                buf[0] |= 0x80;
            }
            s.serialize_bytes(&buf)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<I256, D::Error> {
            if d.is_human_readable() {
                struct StrV;
                impl de::Visitor<'_> for StrV {
                    type Value = I256;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(f, "decimal string for I256")
                    }
                    fn visit_str<E: de::Error>(self, v: &str) -> Result<I256, E> {
                        v.parse().map_err(de::Error::custom)
                    }
                }
                return d.deserialize_str(StrV);
            }
            struct V;
            impl<'de> de::Visitor<'de> for V {
                type Value = I256;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(f, "compact I256")
                }
                fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<I256, E> {
                    if v.is_empty() {
                        return Err(E::custom("empty compact I256"));
                    }
                    let negative = (v[0] & 0x80) != 0;
                    let abs = decode_magnitude(v)?;
                    let sign = if negative {
                        Sign::Negative
                    } else {
                        Sign::Positive
                    };
                    I256::checked_from_sign_and_abs(sign, abs)
                        .ok_or_else(|| E::custom("I256 overflow (negative zero)"))
                }
            }
            d.deserialize_bytes(V)
        }
    }
}

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
    JoinUe,
}

impl IpcTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CreateSubnet => IPC_CREATE_SUBNET_TAG,
            Self::JoinSubnet => IPC_JOIN_SUBNET_TAG,
            Self::JoinUe => IPC_JOIN_UE_TAG,
            Self::PrefundSubnet => IPC_PREFUND_SUBNET_TAG,
            Self::FundSubnet => IPC_FUND_SUBNET_TAG,
            Self::CheckpointSubnet => IPC_CHECKPOINT_TAG,
            Self::BatchTransfer => IPC_TRANSFER_TAG,
            Self::StakeCollateral => IPC_STAKE_TAG,
            Self::UnstakeCollateral => IPC_UNSTAKE_TAG,
            Self::KillSubnet => IPC_KILL_SUBNET_TAG,
        }
    }

    /// The IPC tag that `data` starts with, if any
    pub fn from_payload(data: &[u8]) -> Option<Self> {
        let tag = std::str::from_utf8(data.get(..IPC_TAG_LENGTH)?).ok()?;
        Self::from_str(tag).ok()
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
            IPC_JOIN_UE_TAG => Ok(Self::JoinUe),
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
    #[error("Serialization error: {0}")]
    SerializationError(String),
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

pub const MAX_POWER_UNIT_EXPONENT: u8 = 15;
/// The most whitelisted keys and the largest `active_validators_limit`.
pub const MAX_WHITELIST_SIZE: usize = 350;
pub const MAX_RELATIVE_TIMELOCK: u64 = 65_535;
pub const P2TR_DUST_LIMIT: Amount = Amount::from_sat(330);

/// The subnet parameters, in the postcard order of the create transaction.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcCreateSubnetMsg {
    /// The minimum collateral of a join, in satoshis
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub min_validator_stake: Amount,
    /// `p`: one unit of power is `10^p` satoshis
    pub power_unit_exponent: u8,
    /// Number of accepted joins that bootstraps the subnet (`boot_validators` in the spec)
    pub min_validators: u16,
    /// The bottom up checkpoint period in number of subnet blocks
    pub bottomup_check_period: u64,
    /// The max number of active validators in subnet
    pub active_validators_limit: u16,
    /// Minimum fee for cross-net messages in subnet (in Satoshis)
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub min_cross_msg_fee: Amount,
    /// The keys of `C_wlist`
    pub whitelist: Vec<XOnlyPublicKey>,
    /// `K`: the length of an epoch in Bitcoin blocks
    pub epoch_length: u64,
    /// The fewest epochs for which a collateral seats its owner
    pub min_timeout: u32,
    /// The most epochs for which a collateral seats its owner
    pub max_timeout: u32,
    /// The smallest deposit that is credited, in satoshis
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub min_deposit: Amount,
    /// Subnet blocks for which a kill proposal stays open
    pub kill_vote_period: u64,
    /// Epochs a subnet marked to be killed stays operational
    pub kill_delay_period: u64,
}

impl IpcValidate for IpcCreateSubnetMsg {
    fn validate(&self) -> Result<(), IpcValidateError> {
        let field = |name, what: &str| IpcValidateError::InvalidField(name, what.to_string());

        if self.power_unit_exponent > MAX_POWER_UNIT_EXPONENT {
            return Err(field("power_unit_exponent", "Must be at most 15"));
        }
        if self.min_validator_stake < self.power_unit() {
            return Err(field("min_validator_stake", "Must be at least one power unit"));
        }
        if self.min_validators == 0 {
            return Err(field("min_validators", "Must be greater than 0"));
        }
        let mut keys = self.whitelist.clone();
        keys.sort();
        keys.dedup();
        if keys.len() != self.whitelist.len() {
            return Err(field("whitelist", "Keys must be distinct"));
        }
        if self.whitelist.len() < self.min_validators as usize {
            return Err(field("whitelist", "Must hold at least min_validators keys"));
        }
        if self.whitelist.len() > MAX_WHITELIST_SIZE {
            return Err(field("whitelist", "Must hold at most 350 keys"));
        }
        if self.bottomup_check_period == 0 {
            return Err(field("bottomup_check_period", "Must be greater than 0"));
        }
        if self.active_validators_limit < self.min_validators {
            return Err(field("active_validators_limit", "Must be at least min_validators"));
        }
        if usize::from(self.active_validators_limit) > MAX_WHITELIST_SIZE {
            return Err(field("active_validators_limit", "Must be at most 350"));
        }
        if self.min_cross_msg_fee == Amount::ZERO {
            return Err(field("min_cross_msg_fee", "Must be greater than 0"));
        }
        if self.epoch_length == 0 {
            return Err(field("epoch_length", "Must be greater than 0"));
        }
        if self.epoch_length.saturating_mul(3) > MAX_RELATIVE_TIMELOCK {
            return Err(field("epoch_length", "3K must be at most 65535"));
        }
        if self.min_timeout == 0 {
            return Err(field("min_timeout", "Must be greater than 0"));
        }
        if self.max_timeout < self.min_timeout {
            return Err(field("max_timeout", "Must be at least min_timeout"));
        }
        if self.min_deposit < P2TR_DUST_LIMIT {
            return Err(field("min_deposit", "Must be at least the 330 sat dust limit"));
        }
        if self.kill_vote_period == 0 {
            return Err(field("kill_vote_period", "Must be greater than 0"));
        }

        Ok(())
    }
}

/// Decodes `bytes` as the exact postcard encoding of a `T`: no byte is left over and the
/// decoded value re-encodes to the same bytes, which rejects non-minimal varints.
fn decode_exact<T: Serialize + DeserializeOwned>(bytes: &[u8]) -> Result<T, IpcSerializeError> {
    let err = IpcSerializeError::DeserializationError;
    let (value, rest): (T, &[u8]) =
        postcard::take_from_bytes(bytes).map_err(|e| err(e.to_string()))?;
    if !rest.is_empty() {
        return Err(err(format!("{} bytes left after the message", rest.len())));
    }
    let encoded = postcard::to_stdvec(&value).map_err(|e| err(e.to_string()))?;
    if encoded != bytes {
        return Err(err(
            "Message is not the exact encoding of its fields".to_string()
        ));
    }
    Ok(value)
}

impl IpcCreateSubnetMsg {
    pub fn to_bytes(&self) -> Result<Vec<u8>, IpcSerializeError> {
        let serialized = postcard::to_stdvec(self)
            .map_err(|e| IpcSerializeError::SerializationError(e.to_string()))?;
        Ok([IPC_CREATE_SUBNET_TAG.as_bytes(), &serialized].concat())
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, IpcSerializeError> {
        if data.len() < IPC_TAG_LENGTH
            || &data[..IPC_TAG_LENGTH] != IPC_CREATE_SUBNET_TAG.as_bytes()
        {
            return Err(IpcSerializeError::DeserializationError(
                "Invalid message tag".to_string(),
            ));
        }

        decode_exact(&data[IPC_TAG_LENGTH..])
    }

    /// Submits the create subnet message to the Bitcoin network
    /// Using commit-reveal scheme
    /// Returns the subnet id — derived from the commit txid
    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
    ) -> Result<SubnetId, IpcLibError> {
        let subnet_data = self.to_bytes().map_err(|e| {
            error!("Could not serialize create subnet msg: {}", e);
            IpcValidateError::InvalidMsg("Could not serialize message".to_string())
        })?;

        info!("Submitting create subnet msg to bitcoin. Data={:?}", self);

        // The address to return any non-dust values
        let return_address = wallet::get_new_address(rpc)?;

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (commit_tx, reveal_tx) = bitcoin_utils::create_commit_reveal_txs(
            rpc,
            &secp,
            &return_address,
            &subnet_data,
            None,
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
    pub fn save_to_db<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &mut RwTxn,
        block_height: u64,
        txid: bitcoin::Txid,
    ) -> Result<db::SubnetGenesisInfo, IpcLibError> {
        let subnet_id = SubnetId::from_txid(&txid);
        if db.get_subnet_genesis_info_txn(txn, subnet_id)?.is_some() {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {subnet_id} exists, the create is ignored"
            ))
            .into());
        }

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
        let mut wtxn = db.nested_write_txn(txn)?;
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
        let configuration = self.whitelist_configuration();
        Ok(create_subnet_multisig_address(
            &secp,
            subnet_id,
            &configuration.keys,
            configuration.threshold,
            NETWORK,
        )?)
    }

    pub fn power_unit(&self) -> Amount {
        Amount::from_sat(10u64.pow(u32::from(self.power_unit_exponent)))
    }

    pub fn power_of(&self, collateral: &Amount) -> Result<multisig::Power, multisig::MultisigError> {
        multisig::collateral_to_power(collateral, &self.min_validator_stake, &self.power_unit())
    }

    pub fn cutoff_height(&self, h_0: u64, epoch: u64) -> u64 {
        h_0 + epoch * self.epoch_length
    }

    /// The whitelist configuration `C_wlist`: every whitelisted key with power 1, threshold
    /// `⌊2 · min_validators / 3⌋ + 1`.
    pub fn whitelist_configuration(&self) -> multisig::Configuration {
        multisig::Configuration {
            keys: self.whitelist.iter().map(|xpk| (*xpk, 1)).collect(),
            threshold: multisig::multisig_threshold(self.min_validators.into()),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcJoinSubnetMsg {
    /// The subnet id of the subnet to join
    pub subnet_id: SubnetId,
    /// The amount of collateral to lock in the subnet
    #[serde(skip)]
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
    pub fn to_bytes(&self) -> Result<Vec<u8>, IpcSerializeError> {
        let serialized = postcard::to_stdvec(self)
            .map_err(|e| IpcSerializeError::SerializationError(e.to_string()))?;
        Ok([IPC_JOIN_SUBNET_TAG.as_bytes(), &serialized].concat())
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, IpcSerializeError> {
        if data.len() < IPC_TAG_LENGTH || &data[..IPC_TAG_LENGTH] != IPC_JOIN_SUBNET_TAG.as_bytes()
        {
            return Err(IpcSerializeError::DeserializationError(
                "Invalid message tag".to_string(),
            ));
        }

        decode_exact(&data[IPC_TAG_LENGTH..])
    }

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
        if subnet.is_killed_or_pending() {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {} is killed or marked for killing, cannot join.",
                self.subnet_id
            )));
        }
        if !subnet.is_activated() {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {} is in epoch 0: it accepts no join until its activation.",
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

        Ok(())
    }

    /// Refuses the join of a member of the current configuration or of `members`, the multisig
    /// members of the next configuration as filled so far.
    pub fn validate_for_members(
        &self,
        subnet: &db::SubnetState,
        members: &BTreeMap<XOnlyPublicKey, db::SubnetValidator>,
    ) -> Result<(), IpcValidateError> {
        if subnet.latest_committee().is_validator(&self.pubkey)
            || members.contains_key(&self.pubkey)
        {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Validator with public key '{}' already registered in subnet {}",
                self.pubkey, self.subnet_id
            )));
        }
        Ok(())
    }

    /// Validates the join, including the collateral, which is output 1's value and so not checked in `validate`.
    pub fn validate_for_subnet(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
        subnet: &Option<db::SubnetState>,
    ) -> Result<(), IpcValidateError> {
        if self.collateral == Amount::ZERO {
            return Err(IpcValidateError::InvalidField(
                "collateral",
                "Collateral amount must be greater than zero".to_string(),
            ));
        }
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
        let subnet_data = self.to_bytes().map_err(|e| {
            error!("Could not serialize join subnet msg: {}", e);
            IpcValidateError::InvalidMsg("Could not serialize message".to_string())
        })?;

        info!(
            "Submitting join subnet msg to bitcoin. Multisig address = {}. Data={:?}",
            multisig_address, self
        );

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (commit_tx, reveal_tx) = bitcoin_utils::create_commit_reveal_txs(
            rpc,
            &secp,
            multisig_address,
            &subnet_data,
            Some(self.collateral),
            None,
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
    pub fn save_to_db<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &mut RwTxn,
        block_height: u64,
        _block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<Option<db::SubnetState>, IpcLibError> {
        let mut genesis_info = db.get_subnet_genesis_info_txn(txn, self.subnet_id)?.ok_or(
            IpcValidateError::InvalidMsg(format!("subnet id={} does not exist", self.subnet_id)),
        )?;

        let subnet_state = db.get_subnet_state_txn(txn, self.subnet_id).map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            IpcValidateError::InvalidMsg(e.to_string())
        })?;

        self.validate_for_subnet(&genesis_info, &subnet_state)?;

        let subnet_address = eth_addr_from_x_only_pubkey(self.pubkey);

        let power = genesis_info.create_subnet_msg.power_of(&self.collateral)?;
        // A join with unilateral exit passed this check before its collateral was recorded
        if !crate::epochs::is_ue_validator(db, txn, self.subnet_id, txid, &self.pubkey)? {
            crate::collateral::refuse_excess_power(
                db,
                txn,
                &genesis_info,
                subnet_state.as_ref(),
                block_height,
                None,
                self.collateral,
            )?;
        }

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
            let members = crate::epochs::next_multisig_members(db, txn, &genesis_info, &subnet)?;
            self.validate_for_members(&subnet, &members)?;

            subnet
                .pending_multisig_changes
                .push(db::PendingMultisigChange::Join(new_validator));
            let mut wtxn = db.nested_write_txn(txn)?;
            db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet)?;
            wtxn.commit()?;

            Ok(None)
        }
        //
        // Handle pre-bootstrap case
        //
        else {
            genesis_info.genesis_validators.push(new_validator);

            // Write to DB
            let mut wtxn = db.nested_write_txn(txn)?;

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

/// A join whose collateral is locked under the collateral script instead of the subnet multisig.
#[derive(Debug, Clone)]
pub struct IpcJoinUeMsg {
    /// The join fields, `collateral` from the commit transaction
    pub join: IpcJoinSubnetMsg,
    pub timeout: u32,
    pub pk_eots: bitcoin::XOnlyPublicKey,
    pub nonce_commitment: EotsNonceCommitment,
    /// Signature of `pk_eots` on `committed_bytes`
    pub nonce_commitment_signature: Signature64,
    /// Signature of the validator key on `unsigned_bytes_of`
    pub signature: bitcoin::secp256k1::schnorr::Signature,
}

pub const JOIN_UE_SIGNATURE_TAG: &[u8] = b"IPCJUE/signature";

pub const JOIN_UE_NONCE_COMMITMENT_TAG: &[u8] = b"IPCJUE/nonce_commitment";

impl IpcJoinUeMsg {
    const TAG: &str = IPC_JOIN_UE_TAG;

    pub fn new_signed(
        secp: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
        join: IpcJoinSubnetMsg,
        timeout: u32,
        secret_key: &bitcoin::secp256k1::SecretKey,
        eots_secret_key: &bitcoin::secp256k1::SecretKey,
        nonce_commitment: EotsNonceCommitment,
    ) -> Self {
        let pk_eots = eots_secret_key.x_only_public_key(secp).0;
        let committed = Self::committed_bytes(&join, timeout, &pk_eots, &nonce_commitment);
        let nonce_commitment_signature = signature_from_bytes(
            &secp
                .sign_schnorr(
                    &Self::digest_of(JOIN_UE_NONCE_COMMITMENT_TAG, &committed),
                    &eots_secret_key.keypair(secp),
                )
                .serialize(),
        );
        let unsigned = Self::unsigned_bytes_of(&committed, &nonce_commitment_signature);
        let signature = secp.sign_schnorr(
            &Self::digest_of(JOIN_UE_SIGNATURE_TAG, &unsigned),
            &secret_key.keypair(secp),
        );
        Self {
            join,
            timeout,
            pk_eots,
            nonce_commitment,
            nonce_commitment_signature,
            signature,
        }
    }

    fn committed_bytes(
        join: &IpcJoinSubnetMsg,
        timeout: u32,
        pk_eots: &bitcoin::XOnlyPublicKey,
        nonce_commitment: &EotsNonceCommitment,
    ) -> Vec<u8> {
        let body = postcard::to_stdvec(&(join, timeout, pk_eots.serialize(), nonce_commitment))
            .expect("join message fields serialize without error");
        [Self::TAG.as_bytes(), &body].concat()
    }

    fn unsigned_bytes_of(committed: &[u8], nonce_commitment_signature: &Signature64) -> Vec<u8> {
        [
            committed,
            &signature_to_bytes(nonce_commitment_signature),
        ]
        .concat()
    }

    fn unsigned_bytes(&self) -> Vec<u8> {
        Self::unsigned_bytes_of(
            &Self::committed_bytes(
                &self.join,
                self.timeout,
                &self.pk_eots,
                &self.nonce_commitment,
            ),
            &self.nonce_commitment_signature,
        )
    }

    fn digest_of(tag: &[u8], data: &[u8]) -> bitcoin::secp256k1::Message {
        bitcoin::secp256k1::Message::from_digest(tagged_sha256(tag, data))
    }

    pub fn collateral_script(
        &self,
        configuration: crate::multisig::Configuration,
    ) -> crate::collateral::CollateralScript {
        crate::collateral::CollateralScript::new(
            self.join.pubkey,
            self.pk_eots,
            self.timeout,
            configuration,
        )
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, IpcSerializeError> {
        let mut bytes = self.unsigned_bytes();
        bytes.extend_from_slice(&self.signature.serialize());
        Ok(bytes)
    }

    /// Parses the message and verifies both signatures.
    pub fn from_bytes(data: &[u8]) -> Result<Self, IpcSerializeError> {
        let err = IpcSerializeError::DeserializationError;
        if data.len() < IPC_TAG_LENGTH || &data[..IPC_TAG_LENGTH] != Self::TAG.as_bytes() {
            return Err(err("Invalid message tag".to_string()));
        }
        let ((join, timeout, pk_eots, nonce_commitment), rest): (
            (IpcJoinSubnetMsg, u32, [u8; 32], EotsNonceCommitment),
            &[u8],
        ) = postcard::take_from_bytes(&data[IPC_TAG_LENGTH..]).map_err(|e| err(e.to_string()))?;
        if rest.len() != 2 * SCHNORR_SIGNATURE_SIZE {
            return Err(err(format!(
                "Expected two {SCHNORR_SIGNATURE_SIZE}-byte signatures after the message, found {} bytes",
                rest.len()
            )));
        }
        let pk_eots = bitcoin::XOnlyPublicKey::from_slice(&pk_eots)
            .map_err(|e| err(format!("Invalid EOTS key: {e}")))?;
        let committed = &data[..data.len() - 2 * SCHNORR_SIGNATURE_SIZE];
        if Self::committed_bytes(&join, timeout, &pk_eots, &nonce_commitment) != committed {
            return Err(err(
                "Message is not the exact encoding of its fields".to_string()
            ));
        }
        let unsigned = &data[..data.len() - SCHNORR_SIGNATURE_SIZE];
        let nonce_commitment_signature: [u8; 64] = rest[..SCHNORR_SIGNATURE_SIZE]
            .try_into()
            .expect("checked length");
        verify_schnorr_bytes(
            &nonce_commitment_signature,
            tagged_sha256(JOIN_UE_NONCE_COMMITMENT_TAG, committed),
            &pk_eots,
        )
        .map_err(|e| err(format!("Invalid nonce commitment signature: {e}")))?;
        let signature: [u8; 64] = rest[SCHNORR_SIGNATURE_SIZE..]
            .try_into()
            .expect("checked length");
        verify_schnorr_bytes(
            &signature,
            tagged_sha256(JOIN_UE_SIGNATURE_TAG, unsigned),
            &join.pubkey,
        )
        .map_err(|e| err(format!("Invalid signature: {e}")))?;
        Ok(Self {
            join,
            timeout,
            pk_eots,
            nonce_commitment,
            nonce_commitment_signature: signature_from_bytes(&nonce_commitment_signature),
            signature: bitcoin::secp256k1::schnorr::Signature::from_slice(&signature)
                .expect("a verified signature parses"),
        })
    }

    /// Submits the commit and reveal transactions, the collateral naming the first configuration
    /// of `context`. Returns the reveal txid.
    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
        context: &crate::collateral::JoinContext,
    ) -> Result<Txid, IpcLibError> {
        use bitcoincore_rpc::RpcApi;

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let data = self.to_bytes().map_err(|e| {
            error!("Could not serialize join-ue msg: {}", e);
            IpcValidateError::InvalidMsg("Could not serialize message".to_string())
        })?;
        let (_, configuration) = context
            .named
            .first()
            .ok_or_else(|| IpcValidateError::InvalidMsg("No configuration to name".to_string()))?;
        let collateral = bitcoin::TxOut {
            value: self.join.collateral,
            script_pubkey: self
                .collateral_script(configuration.clone())
                .script_pubkey(&secp)?,
        };
        let return_address = wallet::get_new_address(rpc)?;

        info!("Submitting join-ue msg to bitcoin. Data={:?}", self);

        let (commit_tx, reveal_tx) = bitcoin_utils::create_commit_reveal_txs(
            rpc,
            &secp,
            &return_address,
            &data,
            None,
            Some(collateral),
        )?;

        let next_height = rpc
            .get_block_count()
            .map_err(bitcoin_utils::BitcoinUtilsError::from)?
            + 1;
        crate::collateral::check_join_ue(&secp, &reveal_tx, 0, &commit_tx, next_height, context)?;

        let reveal_txid = reveal_tx.compute_txid();
        debug!(
            "Join-ue commit_txid={} reveal_txid={}",
            commit_tx.compute_txid(),
            reveal_txid
        );

        submit_to_mempool(rpc, vec![commit_tx, reveal_tx])?;
        info!(
            "Submitted join-ue msg for subnet_id={} join_txid={}",
            self.join.subnet_id, reveal_txid
        );
        Ok(reveal_txid)
    }
}

impl IpcValidate for IpcJoinUeMsg {
    fn validate(&self) -> Result<(), IpcValidateError> {
        self.join.validate()?;
        if self.timeout >= crate::collateral::LOCKTIME_THRESHOLD {
            return Err(IpcValidateError::InvalidField(
                "timeout",
                format!(
                    "Timeout must be a block height below {}",
                    crate::collateral::LOCKTIME_THRESHOLD
                ),
            ));
        }
        if self.pk_eots == self.join.pubkey {
            return Err(IpcValidateError::InvalidField(
                "pk_eots",
                "The EOTS key must differ from the validator key".to_string(),
            ));
        }
        if self.nonce_commitment.start < 1 || self.nonce_commitment.count < 1 {
            return Err(IpcValidateError::InvalidField(
                "nonce_commitment",
                "The EOTS nonce commitment needs start >= 1 and count >= 1".to_string(),
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
    pub fn save_to_db<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &mut RwTxn,
        block_height: u64,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let mut genesis_info = db.get_subnet_genesis_info_txn(txn, self.subnet_id)?.ok_or(
            IpcValidateError::InvalidMsg(format!("subnet id={} does not exist", self.subnet_id)),
        )?;

        self.validate_for_genesis_info(&genesis_info)?;

        trace!("Processing {self:?}, adding new genesis_balance entry");

        genesis_info.add_genesis_balance_entry(self.address, self.amount, txid, block_height);

        let mut wtxn = db.nested_write_txn(txn)?;
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

    pub fn validate_for_subnet(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
        subnet: &db::SubnetState,
    ) -> Result<(), IpcValidateError> {
        if subnet.is_killed_or_pending() {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {} is killed or marked for killing, cannot fund.",
                self.subnet_id
            )));
        }
        let min_deposit = genesis_info.create_subnet_msg.min_deposit;
        if self.amount < min_deposit {
            return Err(IpcValidateError::InvalidField(
                "amount",
                format!(
                    "Deposit must be at least {min_deposit}, supplied {}",
                    self.amount
                ),
            ));
        }

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

    pub fn save_to_db<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &mut RwTxn,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let genesis_info = db.get_subnet_genesis_info_txn(txn, self.subnet_id)?.ok_or(
            IpcValidateError::InvalidMsg(format!("subnet id={} does not exist", self.subnet_id)),
        )?;
        let subnet_state =
            db.get_subnet_state_txn(txn, self.subnet_id)?
                .ok_or(IpcValidateError::InvalidMsg(format!(
                    "subnet id={} is not bootstrapped",
                    self.subnet_id
                )))?;

        self.validate_for_subnet(&genesis_info, &subnet_state)?;

        trace!("Processing {self:?}, adding new rootnet message");

        // Get next nonce

        let nonce = db.get_next_rootnet_msg_nonce_txn(txn, self.subnet_id)?;
        let mut wtxn = db.nested_write_txn(txn)?;
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

/// ERC20 token registration metadata (IPC:ETR).
/// Does not include home_subnet — that is tracked separately (e.g. as the DB key,
/// or derived from the checkpoint's OP_RETURN subnet ID).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcErcTokenRegistration {
    /// The token contract address on the home subnet
    pub home_token_address: alloy_primitives::Address,
    /// ERC20 name
    pub name: String,
    /// ERC20 symbol
    pub symbol: String,
    /// ERC20 decimals
    pub decimals: u8,
    /// T.totalSupply() at registration time.
    /// Seeds the balance ledger for S_home.
    #[serde(default, with = "compact_amount::u256")]
    pub initial_supply: U256,
}

/// ERC20 supply adjustment (IPC:ETS — ERC Token Supply).
/// Encodes a mint (positive delta) or burn (negative delta) event on S_home.
/// The monitor accepts EMS records only from the token's home subnet.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcErcSupplyAdjustment {
    /// The token contract address on the home subnet
    pub home_token_address: alloy_primitives::Address,
    /// Supply change. Positive = mint, negative = burn.
    #[serde(with = "compact_amount::i256")]
    pub delta: I256,
}

/// ERC20 cross-subnet transfer (IPC:ETX).
/// In the wire format (BatchTransferWireData), destination_subnet_id is the map key
/// and is NOT repeated in each record — reconstructed on deserialization.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IpcCrossSubnetErcTransfer {
    /// The home subnet where the token originates
    pub home_subnet_id: SubnetId,
    /// The token contract address on the home subnet
    pub home_token_address: alloy_primitives::Address,
    /// Transfer amount
    pub amount: U256,
    /// The destination subnet for this transfer
    pub destination_subnet_id: SubnetId,
    /// The recipient address on the destination subnet
    pub recipient: alloy_primitives::Address,
}

/// Wire-only inner type for ERC transfers, grouped by destination_subnet_id.
/// destination_subnet_id is the map key — not repeated in each record.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct IpcCrossSubnetErcTransferWire {
    home_subnet_id: SubnetId,
    home_token_address: alloy_primitives::Address,
    #[serde(with = "compact_amount::u256")]
    amount: U256,
    recipient: alloy_primitives::Address,
}

/// Wire format for the batch transfer witness blob.
/// Record ordering: transfers, token_registrations, supply_adjustments, erc_transfers.
/// (ETR before ETS before ETX — guarantees metadata and supply are available before transfers.)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct BatchTransferWireData {
    transfers: BTreeMap<Txid20, Vec<(alloy_primitives::Address, Amount)>>,
    /// ERC20 token registrations (IPC:ETR)
    token_registrations: Vec<IpcErcTokenRegistration>,
    /// ERC20 supply adjustments (IPC:ETS) — mint/burn on S_home
    #[serde(default)]
    token_supply_adjustments: Vec<IpcErcSupplyAdjustment>,
    /// ERC20 cross-subnet transfers grouped by destination subnet (IPC:ETX)
    token_transfers: BTreeMap<Txid20, Vec<IpcCrossSubnetErcTransferWire>>,
    slash_signatures: Vec<SlashSignatureEntry>,
}

/// The transfer batches of `transfers`: one per destination subnet, in ascending `txid20`
/// order, with the sum of its transfers.
pub(crate) fn transfer_batches(transfers: &[IpcCrossSubnetTransfer]) -> BTreeMap<Txid20, Amount> {
    let mut batches = BTreeMap::new();
    for transfer in transfers {
        *batches
            .entry(transfer.destination_subnet_id.txid20())
            .or_insert(Amount::ZERO) += transfer.amount;
    }
    batches
}

/// The configuration that signs a checkpoint's fund inputs: its keys, threshold and address.
#[derive(Debug, Clone)]
pub struct CheckpointSigner {
    pub keys: Vec<multisig::WeightedKey>,
    pub threshold: multisig::Power,
    pub address: bitcoin::Address,
}

impl CheckpointSigner {
    pub fn of_committee(committee: &db::SubnetCommittee) -> Self {
        Self {
            keys: committee.validator_weighted_keys(),
            threshold: committee.threshold,
            address: committee.address_checked(),
        }
    }

    pub fn of_configuration(
        subnet_id: &SubnetId,
        configuration: &multisig::Configuration,
    ) -> Result<Self, IpcLibError> {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let address = create_subnet_multisig_address(
            &secp,
            subnet_id,
            &configuration.keys,
            configuration.threshold,
            NETWORK,
        )?;
        Ok(Self {
            keys: configuration.keys.clone(),
            threshold: configuration.threshold,
            address,
        })
    }
}

/// Checkpoint message for a subnet
/// Note: currently the maximum number of withdrawals and transfer batches is 255 each
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
    /// ERC20 token registrations (IPC:ETR)
    #[serde(default)]
    pub token_registrations: Vec<IpcErcTokenRegistration>,
    /// ERC20 supply adjustments (IPC:ETS) — mint/burn on S_home
    #[serde(default)]
    pub token_supply_adjustments: Vec<IpcErcSupplyAdjustment>,
    /// ERC20 cross-subnet transfers (IPC:ETX)
    #[serde(default)]
    pub token_transfers: Vec<IpcCrossSubnetErcTransfer>,
    #[serde(default)]
    pub slash_signatures: Vec<SlashSignatureEntry>,
    /// Unstakes for validators leaving the subnet
    #[serde(default, skip_deserializing)]
    pub unstakes: Vec<IpcUnstake>,
    /// Optional change address (multisig)
    #[serde(skip_deserializing)]
    pub change_address: Option<bitcoin::Address<NetworkUnchecked>>,
    /// The flag marking the subnet killed, ie. the last checkpoint
    #[serde(default, skip_deserializing)]
    pub is_kill_checkpoint: bool,
    /// The flag marking the rotation checkpoint of an epoch
    #[serde(default, skip_deserializing)]
    pub is_rotation_checkpoint: bool,
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

        // Check if the transfers are valid
        for transfer in &self.transfers {
            if transfer.amount == Amount::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "transfer.amount",
                    "Transfer amount must be greater than zero".to_string(),
                ));
            }
        }

        let batches = transfer_batches(&self.transfers).len();
        if batches > u8::MAX as usize {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Number of batch transfer destination subnets ({}) exceeds maximum allowed ({})",
                batches,
                u8::MAX
            )));
        }

        // ERC fields (token_registrations, token_supply_adjustments, token_transfers)
        // are validated in IpcBatchTransferMsg::validate(), which is the message that
        // actually carries them on Bitcoin (the batch transfer reveal tx, not the checkpoint tx).

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

/// Output indices of a checkpoint transaction: the OP_RETURN output, the withdrawals, the
/// commitment output if the checkpoint has batch data, the transfer batches, the unstakes, then
/// the change. The builder, the parser and the batch transfer reveal check all use it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CheckpointOutputs {
    pub withdrawals: usize,
    pub has_commitment: bool,
    pub batches: usize,
    pub unstakes: usize,
}

impl CheckpointOutputs {
    pub(crate) fn withdrawal(&self, i: usize) -> usize {
        1 + i
    }

    /// Index of the batch transfer commitment output, if the checkpoint has one
    pub(crate) fn commitment(&self) -> usize {
        self.withdrawal(self.withdrawals)
    }

    /// Index of the output that pays transfer batch `i` (the batches in ascending `txid20`
    /// order of their destinations)
    pub(crate) fn transfer(&self, i: usize) -> usize {
        self.commitment() + usize::from(self.has_commitment) + i
    }

    pub(crate) fn unstake(&self, i: usize) -> usize {
        self.transfer(self.batches) + i
    }

    /// Index of the change output, if the checkpoint has one: the number of outputs before it
    pub(crate) fn change(&self) -> usize {
        self.unstake(self.unstakes)
    }
}

impl IpcCheckpointSubnetMsg {
    // Length of the withdrawal + transfer + unstake markers, 1 byte each
    const MARKERS_LEN: usize = 3;
    // u64 length
    const HEIGHT_LEN: usize = std::mem::size_of::<u64>();
    // u64 length
    const COMMITTEE_CONF_LEN: usize = std::mem::size_of::<u64>();
    const FLAGS_LEN: usize = std::mem::size_of::<u8>();
    pub const FLAG_KILL: u8 = 0x01;
    pub const FLAG_ROTATION: u8 = 0x04;
    // The total length of the op_return data - helper
    const DATA_LEN: usize = IPC_TAG_LENGTH
        + SubnetId::INNER_LEN
        + bitcoin::hashes::sha256::Hash::LEN
        + Self::MARKERS_LEN
        + Self::HEIGHT_LEN
        + Self::COMMITTEE_CONF_LEN
        + Self::FLAGS_LEN;

    const TAG_OFFSET: usize = 0;
    const TXID_OFFSET: usize = Self::TAG_OFFSET + IPC_TAG_LENGTH;
    const HASH_OFFSET: usize = Self::TXID_OFFSET + SubnetId::INNER_LEN;
    const HEIGHT_OFFSET: usize = Self::HASH_OFFSET + bitcoin::hashes::sha256::Hash::LEN;
    const MARKERS_OFFSET: usize = Self::HEIGHT_OFFSET + Self::HEIGHT_LEN;
    const COMMITTEE_CONF_OFFSET: usize = Self::MARKERS_OFFSET + Self::MARKERS_LEN;
    const FLAGS_OFFSET: usize = Self::COMMITTEE_CONF_OFFSET + Self::COMMITTEE_CONF_LEN;

    fn flags(&self) -> u8 {
        (if self.is_kill_checkpoint {
            Self::FLAG_KILL
        } else {
            0
        }) | (if self.is_rotation_checkpoint {
            Self::FLAG_ROTATION
        } else {
            0
        })
    }

    pub(crate) fn make_metadata_tx_out(&self, fee_rate: bitcoin::FeeRate) -> bitcoin::TxOut {
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
        op_return_data[Self::MARKERS_OFFSET + 1] =
            u8::try_from(transfer_batches(&self.transfers).len())
                .expect("validate: at most 255 destination subnets"); // Transfer batch count
        op_return_data[Self::MARKERS_OFFSET + 2] = self.unstakes.len().min(255) as u8; // Unstake count
                                                                                       // Add committee configuration number
        op_return_data[Self::COMMITTEE_CONF_OFFSET..Self::FLAGS_OFFSET]
            .copy_from_slice(&self.next_committee_configuration_number.to_le_bytes());

        op_return_data[Self::FLAGS_OFFSET] = self.flags();

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

        let killed = op_return_data[Self::FLAGS_OFFSET] & Self::FLAG_KILL != 0;

        Ok((withdrawal_count, transfer_count, unstake_count, killed))
    }

    pub(crate) fn checkpoint_outputs(&self) -> CheckpointOutputs {
        CheckpointOutputs {
            withdrawals: self.withdrawals.len(),
            has_commitment: self.has_batch_data(),
            batches: transfer_batches(&self.transfers).len(),
            unstakes: self.unstakes.len(),
        }
    }

    /// Whether the checkpoint carries batch transfer data, and so a commitment output
    pub(crate) fn has_batch_data(&self) -> bool {
        !self.transfers.is_empty()
            || !self.token_registrations.is_empty()
            || !self.token_supply_adjustments.is_empty()
            || !self.token_transfers.is_empty()
            || !self.slash_signatures.is_empty()
    }

    pub(crate) fn make_batched_transfer_data(&self) -> Result<Vec<u8>, IpcLibError> {
        // Group native transfers by destination subnet_id
        let mut transfers_by_subnet: BTreeMap<Txid20, Vec<(alloy_primitives::Address, Amount)>> =
            BTreeMap::new();
        for transfer in &self.transfers {
            let key = transfer.destination_subnet_id.txid20();
            transfers_by_subnet
                .entry(key)
                .or_default()
                .push((transfer.subnet_user_address, transfer.amount));
        }

        // Group ERC transfers by destination subnet_id
        let mut erc_by_subnet: BTreeMap<Txid20, Vec<IpcCrossSubnetErcTransferWire>> =
            BTreeMap::new();
        for etx in &self.token_transfers {
            let wire = IpcCrossSubnetErcTransferWire {
                home_subnet_id: etx.home_subnet_id,
                home_token_address: etx.home_token_address,
                amount: etx.amount,
                recipient: etx.recipient,
            };
            erc_by_subnet
                .entry(etx.destination_subnet_id.txid20())
                .or_default()
                .push(wire);
        }

        let mut slash_signatures = self.slash_signatures.clone();
        slash_signatures.sort_by_key(|e| (e.txid, e.vout));
        let wire_data = BatchTransferWireData {
            transfers: transfers_by_subnet,
            token_registrations: self.token_registrations.clone(),
            token_supply_adjustments: self.token_supply_adjustments.clone(),
            token_transfers: erc_by_subnet,
            slash_signatures,
        };

        let binary: Vec<u8> = postcard::to_stdvec(&wire_data).map_err(|e| {
            IpcLibError::from(IpcValidateError::InvalidMsg(format!(
                "Failed to serialize batch transfer data: {}",
                e
            )))
        })?;

        // Combine UTF-8 tag and binary data
        let mut complete_data = Vec::with_capacity(IPC_TRANSFER_TAG.len() + binary.len());
        complete_data.extend_from_slice(IPC_TRANSFER_TAG.as_bytes()); // UTF-8 tag prefix
        complete_data.extend_from_slice(&binary); // Binary data follows

        Ok(complete_data)
    }

    /// The commitment scriptPubKey of batch transfer `data` and its revealing witness
    fn batch_transfer_commitment(
        data: &[u8],
    ) -> Result<(bitcoin::ScriptBuf, bitcoin::Witness), IpcLibError> {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let commit_script = bitcoin_utils::make_push_data_script(data);

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

        let control_block = commit_spend_info
            .control_block(&(
                commit_script.clone(),
                bitcoin::taproot::LeafVersion::TapScript,
            ))
            .ok_or(bitcoin_utils::BitcoinUtilsError::CannotConstructControlBlock)?;

        let reveal_witness =
            bitcoin::Witness::from_slice(&[commit_script.to_bytes(), control_block.serialize()]);
        Ok((commit_script_pubkey, reveal_witness))
    }

    /// The batch transfer reveal with `witness`, its commitment outpoint set later
    fn batch_transfer_reveal(witness: bitcoin::Witness, output: bitcoin::TxOut) -> Transaction {
        Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint {
                    txid: bitcoin::Txid::all_zeros(),
                    vout: 0,
                },
                witness,
                ..Default::default()
            }],
            output: vec![output],
        }
    }

    /// The weight of the batch transfer reveal of this checkpoint's data
    pub fn batch_transfer_reveal_weight(&self) -> Result<bitcoin::Weight, IpcLibError> {
        let (_, witness) = Self::batch_transfer_commitment(&self.make_batched_transfer_data()?)?;
        let output = bitcoin::TxOut {
            value: Amount::ZERO,
            script_pubkey: bitcoin::ScriptBuf::from_bytes(vec![0; 34]),
        };
        Ok(Self::batch_transfer_reveal(witness, output).weight())
    }

    fn make_batched_transfer(
        &self,
        fee_rate: bitcoin::FeeRate,
        return_address: &bitcoin::Address,
    ) -> Result<(bitcoin::TxOut, bitcoin::Witness, bitcoin::TxOut), IpcLibError> {
        let (commit_script_pubkey, reveal_witness) =
            Self::batch_transfer_commitment(&self.make_batched_transfer_data()?)?;

        // The reveal output must clear the node's dust limit at its default relay fee too
        let reveal_tx_out = bitcoin::TxOut {
            value: return_address
                .script_pubkey()
                .minimal_non_dust_custom(fee_rate)
                .max(return_address.script_pubkey().minimal_non_dust()),
            script_pubkey: return_address.script_pubkey(),
        };

        let reveal_tx = Self::batch_transfer_reveal(reveal_witness.clone(), reveal_tx_out.clone());

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

        // returns: txOut of commitTx, witness of  batchTransferTx, txOut of revealTx
        Ok((commit_tx_out, reveal_witness, reveal_tx_out))
    }

    /// For each transfer, given the subnet id, set the subnet
    /// multisig address.
    pub fn update_subnets_for_transfer<D: db::DatabaseCore>(
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

    /// The commitment output of the checkpoint `txid`, which the batch transfer reveal spends,
    /// if the checkpoint has batch data
    fn batch_transfer_commit_outpoint(&self, txid: bitcoin::Txid) -> Option<bitcoin::OutPoint> {
        self.has_batch_data().then(|| bitcoin::OutPoint {
            txid,
            vout: self.checkpoint_outputs().commitment() as u32,
        })
    }

    /// Makes a batched transfer reveal transaction
    pub fn make_reveal_batch_transfer_tx(
        &self,
        checkpoint_txid: Txid,
        fee_rate: bitcoin::FeeRate,
        return_address: &bitcoin::Address,
    ) -> Result<Option<Transaction>, IpcLibError> {
        if !self.has_batch_data() {
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
        self.build_checkpoint_psbt(
            &CheckpointSigner::of_committee(committee),
            &next_committee.address_checked(),
            committee != next_committee || self.is_kill_checkpoint,
            fee_rate,
            unspent,
        )
    }

    /// The checkpoint PSBT: fund inputs from `unspent`, locked under `signer`, change to
    /// `change_address`, and with `exhaust_unspent` every input of `unspent` (a rotation or
    /// the kill checkpoint).
    pub fn build_checkpoint_psbt(
        &self,
        signer: &CheckpointSigner,
        change_address: &bitcoin::Address,
        exhaust_unspent: bool,
        fee_rate: bitcoin::FeeRate,
        unspent: &[bitcoincore_rpc::json::ListUnspentResultEntry],
    ) -> Result<bitcoin::Psbt, IpcLibError> {
        debug!(
            "Creating checkpoint transactions for subnet_id={}",
            self.subnet_id
        );

        let committee_change_address = change_address.clone();

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
        //
        let outputs = self.checkpoint_outputs();
        if self.has_batch_data() {
            debug_assert_eq!(tx_outs.len(), outputs.commitment());
            let (commit_tx_out, _, _) = self.make_batched_transfer(fee_rate, &signer.address)?;
            tx_outs.push(commit_tx_out);

            for (txid20, total_amount) in transfer_batches(&self.transfers) {
                let multisig_addr = self
                    .transfers
                    .iter()
                    .find(|t| t.destination_subnet_id.txid20() == txid20)
                    .and_then(|t| t.subnet_multisig_address.clone())
                    .ok_or(IpcValidateError::InvalidField(
                        "transfer.subnet_multisig_address",
                        "subnet_multisig_address must be defined".to_string(),
                    ))?;
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
        debug_assert_eq!(tx_outs.len(), outputs.change());

        //
        // Create the checkpoint unsigned transaction
        //

        let committee_keys = signer.keys.clone();

        let mut checkpoint_tx = multisig::construct_spend_unsigned_transaction(
            &committee_keys,
            signer.threshold,
            &committee_change_address,
            unspent,
            exhaust_unspent,
            &tx_outs,
            &fee_rate,
            multisig::CHECKPOINT_CONSOLIDATION_LIMIT,
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
                // Distribute the change proportionally among unstake outputs
                let mut distributed_amount = Amount::ZERO;
                for (i, unstake) in self.unstakes.iter().enumerate() {
                    let output_index = outputs.unstake(i);
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
                    change_amount, outputs.unstakes
                );

                // Reconstruct the transaction with updated tx_outs
                checkpoint_tx = multisig::construct_spend_unsigned_transaction(
                    &committee_keys,
                    signer.threshold,
                    &committee_change_address,
                    unspent,
                    exhaust_unspent,
                    &tx_outs,
                    &fee_rate,
                    multisig::CHECKPOINT_CONSOLIDATION_LIMIT,
                )?;
            }
        }

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let checkpoint_psbt = multisig::construct_spend_psbt(
            &secp,
            &self.subnet_id,
            &committee_keys,
            signer.threshold,
            &committee_change_address,
            unspent,
            exhaust_unspent,
            &tx_outs,
            &fee_rate,
            multisig::CHECKPOINT_CONSOLIDATION_LIMIT,
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
    ///    [checkpoint tag | 20-byte subnet ID txid | 32-byte checkpoint hash | 8-byte checkpoint height | 1-byte withdrawal count | 1-byte transfer batch count | 1-byte unstake count | 8-byte configuration number | 1-byte killed flag]
    /// 2. Withdrawal outputs for each withdrawal
    /// 3. The batch transfer commitment output, if the checkpoint has batch data
    /// 4. One transfer output per destination subnet
    /// 5. Unstake outputs, then the change ([`CheckpointOutputs`])
    ///
    /// Returns an error if the transaction doesn't match the expected format.
    pub fn from_checkpoint_tx<D: db::DatabaseCore>(
        db: &D,
        txn: &RoTxn,
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
        let batch_transfers_count = op_return_data[Self::MARKERS_OFFSET + 1] as usize;
        let unstakes_count = op_return_data[Self::MARKERS_OFFSET + 2] as usize;

        // Extract committee configuration number
        let committee_conf_bytes = &op_return_data[Self::COMMITTEE_CONF_OFFSET..Self::FLAGS_OFFSET];
        let next_committee_configuration_number =
            u64::from_le_bytes(committee_conf_bytes.try_into().map_err(|_| {
                err("Failed to convert committee configuration number bytes to u64".to_string())
            })?);

        let flags = op_return_data[Self::FLAGS_OFFSET];
        if flags & !(Self::FLAG_KILL | Self::FLAG_ROTATION) != 0 {
            return Err(err(format!("Unknown flag bits set: {flags:#04x}")));
        }
        let kill_checkpoint = flags & Self::FLAG_KILL != 0;
        let rotation_checkpoint = flags & Self::FLAG_ROTATION != 0;

        // Without transfer batches, the output count tells whether there is a commitment output.
        // Only a kill checkpoint with unstakes has no change: they take its remainder.
        let without_commitment = CheckpointOutputs {
            withdrawals: withdrawals_count,
            has_commitment: false,
            batches: batch_transfers_count,
            unstakes: unstakes_count,
        };
        let has_change = !(kill_checkpoint && unstakes_count > 0);
        let outputs = CheckpointOutputs {
            has_commitment: batch_transfers_count > 0
                || tx.output.len() > without_commitment.change() + usize::from(has_change),
            ..without_commitment
        };
        let expected_outputs = outputs.change();

        if tx.output.len() < expected_outputs {
            return Err(err(format!(
                "Not enough outputs: got {}, expected at least {}",
                tx.output.len(),
                expected_outputs
            )));
        }

        let subnet = db
            .get_subnet_state_txn(txn, subnet_id)?
            .ok_or(err(format!("Subnet {} not found", subnet_id)))?;

        // Parse withdrawals
        let mut withdrawals = Vec::with_capacity(withdrawals_count);
        for i in 0..withdrawals_count {
            let txout = &tx.output[outputs.withdrawal(i)];
            let amount = txout.value;
            let address =
                bitcoin::Address::from_script(&txout.script_pubkey, NETWORK).map_err(|_| {
                    err(format!(
                        "Could not parse address from withdrawal output {}",
                        outputs.withdrawal(i)
                    ))
                })?;
            let address = address.into_unchecked();

            withdrawals.push(IpcWithdrawal { amount, address });
        }

        // Parse unstakes
        let mut unstakes = Vec::with_capacity(unstakes_count);
        for i in 0..unstakes_count {
            let txout = &tx.output[outputs.unstake(i)];
            let amount = txout.value;
            let address =
                bitcoin::Address::from_script(&txout.script_pubkey, NETWORK).map_err(|_| {
                    err(format!(
                        "Could not parse address from unstake output {}",
                        outputs.unstake(i)
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

        if batch_transfers_count > 0 {
            for i in 0..batch_transfers_count {
                let transfer_output_index = outputs.transfer(i);
                let txout = &tx.output[transfer_output_index];

                // For transfers, we can only extract the multisig address from the output
                // The subnet_id and user_address would need to be provided by the batch reveal transaction
                let subnet_multisig_address =
                    bitcoin::Address::from_script(&txout.script_pubkey, NETWORK).map_err(|_| {
                        err(format!(
                            "Could not parse address from transfer output {}",
                            transfer_output_index
                        ))
                    })?;
                let subnet_multisig_address = subnet_multisig_address.into_unchecked();

                match db.get_subnet_by_multisig_address_txn(txn, &subnet_multisig_address) {
                    Ok(Some(subnet)) => {
                        trace!(
                            "Found subnet {} for multisig address {:?}",
                            subnet.id,
                            subnet_multisig_address
                        );
                    }
                    Ok(None) => {
                        error!("CheckpointSubnetMsg: Could not find subnet with multisig address {:?} for transfer.", subnet_multisig_address);
                    }
                    Err(e) => {
                        return Err(err(format!(
                            "CheckpointSubnetMsg: Error while looking up subnet with multisig address {:?}: {}",
                            subnet_multisig_address, e
                        )));
                    }
                };
            }
        }

        // Extract the change address if present (should be the last output)
        let change_address = if tx.output.len() > expected_outputs {
            let change_output = &tx.output[outputs.change()];
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
            transfers: vec![],
            token_registrations: vec![],
            token_supply_adjustments: vec![],
            token_transfers: vec![],
            slash_signatures: vec![],
            change_address,
            is_kill_checkpoint: kill_checkpoint,
            is_rotation_checkpoint: rotation_checkpoint,
        })
    }

    /// Saves the checkpoint message to the database.
    ///
    /// This method validates the message against the subnet state, creates a checkpoint record,
    /// and stores it in the database. It also updates the last checkpoint number in the subnet state.
    ///
    /// Returns the created checkpoint record.
    pub fn save_to_db<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &mut RwTxn,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<db::SubnetCheckpoint, IpcLibError> {
        // Get the current subnet state
        let mut subnet_state = db
            .get_subnet_state_txn(txn, self.subnet_id)?
            .ok_or_else(|| {
                IpcValidateError::InvalidMsg(format!("Subnet ID {} does not exist", self.subnet_id))
            })?;

        let checkpoint_number = subnet_state.last_checkpoint_number.map_or(0, |n| n + 1);
        let epoch = subnet_state.epoch;

        // The adopted configuration is `Some` for a rotation checkpoint
        let (last_change_number, committee_number, checkpoint_epoch, adopted) =
            if self.is_rotation_checkpoint {
                if subnet_state.last_rotation_epoch >= epoch {
                    return Err(IpcValidateError::InvalidField(
                        "flags",
                        format!(
                            "Epoch {epoch} of subnet {} awaits no rotation checkpoint",
                            self.subnet_id
                        ),
                    )
                    .into());
                }
                let number = crate::epochs::epoch_committee_number(db, txn, self.subnet_id, epoch)?
                    .ok_or_else(|| {
                        IpcValidateError::InvalidMsg(format!(
                            "Subnet {} has no configuration of epoch {epoch}",
                            self.subnet_id
                        ))
                    })?;
                let configuration = subnet_state.latest_committee().clone();
                (
                    configuration.configuration_number,
                    number,
                    epoch,
                    Some(configuration),
                )
            } else if subnet_state.last_rotation_epoch == 0 {
                (0, crate::epochs::WHITELIST_COMMITTEE_NUMBER, 0, None)
            } else {
                (
                    subnet_state.committee.configuration_number,
                    subnet_state.committee_number,
                    subnet_state.last_rotation_epoch,
                    None,
                )
            };

        if !self.is_kill_checkpoint {
            let change_multisig = match &adopted {
                Some(configuration) => configuration.multisig_address.clone(),
                // `C_wlist` signs until the rotation checkpoint of epoch 1
                None if subnet_state.last_rotation_epoch == 0 => db
                    .get_subnet_genesis_info_txn(txn, self.subnet_id)?
                    .ok_or_else(|| {
                        IpcValidateError::InvalidMsg(format!(
                            "Subnet ID {} does not exist",
                            self.subnet_id
                        ))
                    })?
                    .create_subnet_msg
                    .multisig_address_from_whitelist(&self.subnet_id)?
                    .into_unchecked(),
                None => subnet_state.committee.multisig_address.clone(),
            };
            if self.change_address.as_ref() != Some(&change_multisig) {
                return Err(IpcValidateError::InvalidField(
                    "change_address",
                    "The change does not pay the multisig of the checkpoint's configuration"
                        .to_string(),
                )
                .into());
            }
        }

        let applied = self.next_committee_configuration_number;
        if applied != 0
            && db
                .get_stake_change_txn(txn, self.subnet_id, applied)?
                .is_none()
        {
            return Err(IpcValidateError::InvalidField(
                "next_committee_configuration_number",
                format!("Stake change for committee configuration number {applied} does not exist"),
            )
            .into());
        }
        // `C_wlist` signs the fund inputs until the rotation checkpoint of epoch 1
        let signed_committee_number = if subnet_state.last_rotation_epoch == 0 {
            crate::epochs::WHITELIST_COMMITTEE_NUMBER
        } else {
            subnet_state.committee_number
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
            signed_committee_number,
            next_committee_number: committee_number,
            next_configuration_number: last_change_number,
            is_kill_checkpoint: self.is_kill_checkpoint,
            epoch: checkpoint_epoch,
            is_rotation_checkpoint: self.is_rotation_checkpoint,
        };

        // Update the checkpoint number in subnet state
        subnet_state.last_checkpoint_number = Some(checkpoint_number);

        if let Some(configuration) = &adopted {
            subnet_state.adopt_committee(checkpoint_epoch, committee_number, configuration.clone());
        }

        // Update subnet state if killed
        if self.is_kill_checkpoint {
            info!(
                "Marking subnet {} as killed at block height {} with txid {}",
                self.subnet_id, block_height, txid
            );

            subnet_state.killed = db::SubnetKillState::Killed {
                parent_height: block_height,
            };
        }

        // Begin a database transaction
        let mut wtxn = db.nested_write_txn(txn)?;
        // Save the updated subnet state
        db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet_state)?;
        // Save the checkpoint record
        db.save_checkpoint(&mut wtxn, self.subnet_id, &checkpoint, checkpoint_number)?;
        if applied != 0 {
            db.confirm_stake_changes(&mut wtxn, self.subnet_id, applied, block_height, block_hash)?;
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

/// The recorded checkpoint of `subnet_id` with transaction `txid`, searched from the last one.
fn checkpoint_record<D: db::DatabaseCore>(
    db: &D,
    txn: &RoTxn,
    subnet_id: SubnetId,
    txid: Txid,
) -> Result<Option<db::SubnetCheckpoint>, IpcLibError> {
    let Some(last) = db
        .get_subnet_state_txn(txn, subnet_id)?
        .and_then(|s| s.last_checkpoint_number)
    else {
        return Ok(None);
    };
    for number in (0..=last).rev() {
        if let Some(checkpoint) = db.get_checkpoint_txn(txn, subnet_id, number)? {
            if checkpoint.txid == txid {
                return Ok(Some(checkpoint));
            }
        }
    }
    Ok(None)
}

/// Validate ERC batch data (registrations, supply adjustments, transfers) against the
/// current DB state.  Called by both the provider (before generating the PSBT) and the
/// monitor (before writing state).  Returns `Err` on the first violation; the caller
/// must treat any error as a fully invalid batch and take no action.
///
/// Validation order: registrations -> mints -> burns -> transfers. A shared
/// `final_balances` map is updated in-memory as each operation is applied, so a
/// token that is registered, minted, burned, and transferred all in one checkpoint
/// is accounted for correctly.
pub fn validate_erc_batch_data<D: db::DatabaseCore>(
    source_subnet_id: SubnetId,
    token_registrations: &[IpcErcTokenRegistration],
    token_supply_adjustments: &[IpcErcSupplyAdjustment],
    token_transfers: &[IpcCrossSubnetErcTransfer],
    db: &D,
    txn: &RoTxn,
) -> Result<(), IpcValidateError> {
    // key: "{home_subnet_id}:{home_token_address}"
    let mut final_balances: std::collections::HashMap<String, U256> =
        std::collections::HashMap::new();

    // 1. Registrations — must be new tokens, no duplicates within the batch.
    for etr in token_registrations {
        let key = format!("{}:{}", source_subnet_id, etr.home_token_address);
        if final_balances.contains_key(&key) {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Token ({},{}) registered more than once in the same checkpoint",
                source_subnet_id, etr.home_token_address
            )));
        }
        let existing = db
            .get_token_balance_txn(
                txn,
                source_subnet_id,
                etr.home_token_address,
                source_subnet_id,
            )
            .map_err(|e| IpcValidateError::InvalidMsg(e.to_string()))?;
        if existing > U256::ZERO {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Token ({},{}) is already registered (existing balance: {})",
                source_subnet_id, etr.home_token_address, existing
            )));
        }
        if etr.initial_supply == U256::ZERO {
            trace!(
                "Zero initial_supply for token ({},{}) on subnet {}",
                source_subnet_id,
                etr.home_token_address,
                source_subnet_id
            );
        }
        final_balances.insert(key, etr.initial_supply);
    }

    // 2. Mints (positive ETS delta) — increase the running balance.
    for ets in token_supply_adjustments {
        if ets.delta.is_negative() {
            continue;
        }
        let (_, mint_amount) = ets.delta.into_sign_and_abs();
        if mint_amount == U256::ZERO {
            trace!(
                "Zero-delta mint for token ({},{}) on subnet {} — no-op",
                source_subnet_id,
                ets.home_token_address,
                source_subnet_id
            );
            continue;
        }
        let key = format!("{}:{}", source_subnet_id, ets.home_token_address);
        let bal = match final_balances.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let fetched = db
                    .get_token_balance_txn(
                        txn,
                        source_subnet_id,
                        ets.home_token_address,
                        source_subnet_id,
                    )
                    .map_err(|e| IpcValidateError::InvalidMsg(e.to_string()))?;
                e.insert(fetched)
            }
        };
        *bal = bal.saturating_add(mint_amount);
    }

    // 3. Burns (negative ETS delta) — must not exceed running balance.
    for ets in token_supply_adjustments {
        if !ets.delta.is_negative() {
            continue;
        }
        let (_, burn_amount) = ets.delta.into_sign_and_abs();
        let key = format!("{}:{}", source_subnet_id, ets.home_token_address);
        let bal = match final_balances.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let fetched = db
                    .get_token_balance_txn(
                        txn,
                        source_subnet_id,
                        ets.home_token_address,
                        source_subnet_id,
                    )
                    .map_err(|e| IpcValidateError::InvalidMsg(e.to_string()))?;
                e.insert(fetched)
            }
        };
        if *bal < burn_amount {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Burn exceeds balance for token ({},{}): has {}, burn {}",
                source_subnet_id, ets.home_token_address, bal, burn_amount
            )));
        }
        *bal -= burn_amount;
    }

    // 4. Transfers (ETX) — must not exceed running balance on the source subnet.
    for etx in token_transfers {
        if etx.amount == U256::ZERO {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Zero-amount ETX for token ({},{}) on subnet {}",
                etx.home_subnet_id, etx.home_token_address, source_subnet_id
            )));
        }
        let key = format!("{}:{}", etx.home_subnet_id, etx.home_token_address);
        let bal = match final_balances.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let fetched = db
                    .get_token_balance_txn(
                        txn,
                        etx.home_subnet_id,
                        etx.home_token_address,
                        source_subnet_id,
                    )
                    .map_err(|e| IpcValidateError::InvalidMsg(e.to_string()))?;
                e.insert(fetched)
            }
        };
        if *bal < etx.amount {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Insufficient balance for token ({},{}): has {}, needs {}",
                etx.home_subnet_id, etx.home_token_address, bal, etx.amount
            )));
        }
        *bal -= etx.amount;
    }

    Ok(())
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
    /// ERC20 token registrations (IPC:ETR)
    pub token_registrations: Vec<IpcErcTokenRegistration>,
    /// ERC20 supply adjustments (IPC:ETS)
    pub token_supply_adjustments: Vec<IpcErcSupplyAdjustment>,
    /// ERC20 cross-subnet transfers (IPC:ETX)
    pub token_transfers: Vec<IpcCrossSubnetErcTransfer>,
    pub slash_signatures: Vec<SlashSignatureEntry>,
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

        if self.transfers.is_empty()
            && self.token_registrations.is_empty()
            && self.token_supply_adjustments.is_empty()
            && self.token_transfers.is_empty()
            && self.slash_signatures.is_empty()
        {
            return Err(IpcValidateError::InvalidMsg(
                "Batch transfer message must have at least one transfer, token registration, ERC transfer or slash pre-signature entry".to_string(),
            ));
        }
        SlashSignatureEntry::check_order(&self.slash_signatures)
            .map_err(|e| IpcValidateError::InvalidField("slash_signatures", e))?;

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

        // Check if the token registrations are valid
        for etr in &self.token_registrations {
            if etr.home_token_address == alloy_primitives::Address::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "token_registration.home_token_address",
                    "Token address must not be zero".to_string(),
                ));
            }
            if etr.name.is_empty() {
                return Err(IpcValidateError::InvalidField(
                    "token_registration.name",
                    "Token name must not be empty".to_string(),
                ));
            }
            if etr.symbol.is_empty() {
                return Err(IpcValidateError::InvalidField(
                    "token_registration.symbol",
                    "Token symbol must not be empty".to_string(),
                ));
            }
        }

        // Check if the ERC transfers are valid
        for etx in &self.token_transfers {
            if etx.home_token_address == alloy_primitives::Address::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "erc_transfer.home_token_address",
                    "ERC transfer token address must not be zero".to_string(),
                ));
            }
            if etx.recipient == alloy_primitives::Address::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "erc_transfer.recipient",
                    "ERC transfer recipient must not be zero".to_string(),
                ));
            }
            if etx.amount == U256::ZERO {
                return Err(IpcValidateError::InvalidField(
                    "erc_transfer.amount",
                    "ERC transfer amount must be greater than zero".to_string(),
                ));
            }
        }

        Ok(())
    }
}

impl IpcBatchTransferMsg {
    /// Deserializes the witness data into a BatchTransferWireData struct.
    /// Witness data must be already processed to extract the pushdata.
    pub(crate) fn witness_to_wire_data(
        witness_data: &[u8],
    ) -> Result<BatchTransferWireData, IpcLibError> {
        if witness_data.len() < IPC_TAG_LENGTH
            || &witness_data[..IPC_TAG_LENGTH] != IPC_TRANSFER_TAG.as_bytes()
        {
            return Err(IpcLibError::MsgParseError(
                IPC_TRANSFER_TAG,
                format!("Invalid tag: expected '{}' prefix", IPC_TRANSFER_TAG),
            ));
        }

        decode_exact(&witness_data[IPC_TRANSFER_TAG.len()..]).map_err(|e| {
            IpcLibError::MsgParseError(
                IPC_TRANSFER_TAG,
                format!("Failed to deserialize batch transfer data: {e}"),
            )
        })
    }

    /// Reconstructs an IpcBatchTransferMsg from the reveal input whose executed leaf holds `data`.
    ///
    /// Returns an error if the data is malformed.
    pub fn from_reveal_input(input: &bitcoin::TxIn, data: &[u8]) -> Result<Self, IpcLibError> {
        let err = |msg: String| IpcLibError::MsgParseError(IPC_TRANSFER_TAG, msg);

        let checkpoint_txid = input.previous_output.txid;
        let checkpoint_vout = input.previous_output.vout;

        let wire_data = Self::witness_to_wire_data(data)
            .map_err(|e| err(format!("Failed to deserialize batch transfer data: {}", e)))?;

        let transfers = wire_data
            .transfers
            .into_iter()
            .flat_map(|(txid20, transfer_list)| {
                let destination_subnet_id = SubnetId::from_txid20(&txid20);
                transfer_list
                    .into_iter()
                    .map(
                        move |(subnet_user_address, amount)| IpcCrossSubnetTransfer {
                            amount,
                            destination_subnet_id,
                            subnet_multisig_address: None,
                            subnet_user_address,
                        },
                    )
            })
            .collect::<Vec<_>>();

        let erc_transfers = wire_data
            .token_transfers
            .into_iter()
            .flat_map(|(txid20, records)| {
                let destination_subnet_id = SubnetId::from_txid20(&txid20);
                records.into_iter().map(move |w| IpcCrossSubnetErcTransfer {
                    home_subnet_id: w.home_subnet_id,
                    home_token_address: w.home_token_address,
                    amount: w.amount,
                    destination_subnet_id,
                    recipient: w.recipient,
                })
            })
            .collect::<Vec<_>>();

        if transfers.is_empty()
            && wire_data.token_registrations.is_empty()
            && wire_data.token_supply_adjustments.is_empty()
            && erc_transfers.is_empty()
            && wire_data.slash_signatures.is_empty()
        {
            return Err(err("No valid data found in batch transfer".into()));
        }

        Ok(Self {
            // Filled in by `process`
            subnet_id: SubnetId::default(),
            checkpoint_txid,
            checkpoint_vout,
            transfers,
            token_registrations: wire_data.token_registrations,
            token_supply_adjustments: wire_data.token_supply_adjustments,
            token_transfers: erc_transfers,
            slash_signatures: wire_data.slash_signatures,
        })
    }

    /// Applies the batch to its checkpoint and returns the updated checkpoint record. A batch
    /// already applied is a no-op.
    pub fn process<D: db::DatabaseCore>(
        &mut self,
        db: &D,
        txn: &mut RwTxn,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<db::SubnetCheckpoint, IpcLibError> {
        self.validate()?;
        let checkpoint_tx = db
            .get_transaction_txn(txn, &self.checkpoint_txid)?
            .ok_or_else(|| {
                IpcValidateError::InvalidMsg(format!(
                    "Can't get Checkpoint Txid {} for BatchTransferMsg: not found in database",
                    self.checkpoint_txid
                ))
            })?;
        let checkpoint_msg = IpcCheckpointSubnetMsg::from_checkpoint_tx(db, txn, &checkpoint_tx)
            .map_err(|e| {
                IpcValidateError::InvalidMsg(format!(
                    "BatchTransferMsg has invalid CheckpointMsg as previous tx: {e}"
                ))
            })?;
        trace!("BatchTransfer: CheckpointMsg {:?}", checkpoint_msg);
        checkpoint_msg.validate()?;
        self.subnet_id = checkpoint_msg.subnet_id;

        let checkpoint = checkpoint_record(db, txn, self.subnet_id, self.checkpoint_txid)?
            .ok_or_else(|| {
                IpcValidateError::InvalidMsg(format!(
                    "Checkpoint {} of subnet {} is not recorded",
                    self.checkpoint_txid, self.subnet_id
                ))
            })?;
        match checkpoint.batch_transfer_txid {
            Some(applied) if applied == txid => {
                debug!("Batch transfer {txid} is already applied");
                return Ok(checkpoint);
            }
            Some(other) => {
                return Err(IpcValidateError::InvalidMsg(format!(
                    "Checkpoint {} already has the batch transfer {other}",
                    self.checkpoint_txid
                ))
                .into())
            }
            None => {}
        }

        self.validate_for_checkpoint(db, txn, &checkpoint_tx)?;
        self.save_to_db(db, txn, checkpoint, block_height, block_hash, txid)
    }

    /// The collateral outputs the entries make slashable now, with their valid signatures.
    pub(crate) fn newly_slashable<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &RoTxn,
    ) -> Result<Vec<(db::UeCollateral, Vec<(u16, Signature64)>)>, IpcLibError> {
        let mut newly = Vec::new();
        for entry in &self.slash_signatures {
            let checked = check_slash_entry(db, txn, self.subnet_id, entry)?;
            if let Some(index) = checked.invalid.first() {
                return Err(IpcValidateError::InvalidMsg(format!(
                    "Slash pre-signature entry {}: signature {index} is invalid",
                    entry.outpoint()
                ))
                .into());
            }
            if checked.slashable {
                continue;
            }
            if checked.reaches_threshold() {
                newly.push((checked.record, checked.valid));
            } else {
                debug!(
                    "Slash pre-signatures of {} reach power {} of threshold {}: not slashable yet",
                    entry.outpoint(),
                    checked.power,
                    checked.script.configuration.threshold
                );
            }
        }
        Ok(newly)
    }

    pub fn validate_for_checkpoint<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &RoTxn,
        checkpoint_tx: &Transaction,
    ) -> Result<(), IpcLibError> {
        let invalid = |msg: String| IpcLibError::from(IpcValidateError::InvalidMsg(msg));

        if checkpoint_tx.compute_txid() != self.checkpoint_txid {
            return Err(invalid(format!(
                "Checkpoint txid mismatch: expected {}, got {}",
                self.checkpoint_txid,
                checkpoint_tx.compute_txid()
            )));
        }

        let metadata_tx_out = checkpoint_tx
            .output
            .first()
            .ok_or_else(|| invalid("Checkpoint transaction has no outputs".to_string()))?;
        let (withdrawals, batch_count, unstakes, _) =
            IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(metadata_tx_out)
                .map_err(|e| invalid(format!("Failed to extract counts from checkpoint: {e}")))?;
        let outputs = CheckpointOutputs {
            withdrawals: withdrawals as usize,
            has_commitment: true,
            batches: batch_count as usize,
            unstakes: unstakes as usize,
        };

        let commit_index = outputs.commitment();
        if self.checkpoint_vout as usize != commit_index {
            return Err(invalid(format!(
                "Reveal spends checkpoint output {}, the commitment output is {}",
                self.checkpoint_vout, commit_index
            )));
        }

        let batches = transfer_batches(&self.transfers);
        if batches.len() != batch_count as usize {
            return Err(invalid(format!(
                "Destination count mismatch: batch has {}, checkpoint expected {}",
                batches.len(),
                batch_count
            )));
        }

        for (i, (txid20, total)) in batches.iter().enumerate() {
            let destination_subnet_id = SubnetId::from_txid20(txid20);
            let output = checkpoint_tx
                .output
                .get(outputs.transfer(i))
                .ok_or_else(|| {
                    invalid(format!(
                        "Checkpoint has no output for destination {destination_subnet_id}"
                    ))
                })?;
            if output.value != *total {
                return Err(invalid(format!(
                    "Destination {destination_subnet_id} output pays {}, the batch transfers {total}",
                    output.value
                )));
            }
            let destination = db
                .get_subnet_state_txn(txn, destination_subnet_id)?
                .ok_or_else(|| {
                    invalid(format!(
                        "Destination subnet {destination_subnet_id} not found"
                    ))
                })?;
            // Accept the previous configuration without enforcing the delayed-arrival window.
            let previous = match destination.committee_number.checked_sub(1) {
                Some(number) => db.get_committee_txn(txn, destination_subnet_id, number)?,
                None => None,
            };
            let pays = |committee: &db::SubnetCommittee| {
                committee.address_checked().script_pubkey() == output.script_pubkey
            };
            if !pays(&destination.committee) && !previous.as_ref().is_some_and(pays) {
                return Err(invalid(format!(
                    "Destination {destination_subnet_id} output does not pay its multisig"
                )));
            }
        }

        Ok(())
    }

    pub fn save_to_db<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &mut RwTxn,
        mut checkpoint: db::SubnetCheckpoint,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<db::SubnetCheckpoint, IpcLibError> {
        let source_subnet_id = self.subnet_id;

        // Validate ERC batch data before touching the DB.
        // Any violation means the entire BatchTransferMsg is malformed — ignore it.
        validate_erc_batch_data(
            source_subnet_id,
            &self.token_registrations,
            &self.token_supply_adjustments,
            &self.token_transfers,
            db,
            txn,
        )?;
        let newly_slashable = self.newly_slashable(db, txn)?;

        checkpoint.batch_transfer_txid = Some(txid);
        checkpoint.batch_transfer_block_height = Some(block_height);

        let mut wtxn = db.nested_write_txn(txn)?;

        db.save_checkpoint(
            &mut wtxn,
            source_subnet_id,
            &checkpoint,
            checkpoint.checkpoint_number,
        )?;

        // Process each transfer and create rootnet messages for destination subnets
        for transfer in &self.transfers {
            if !receives_transfers(db, &wtxn, transfer.destination_subnet_id)? {
                debug!(
                    "Destination subnet {} is not active: fund of {} skipped",
                    transfer.destination_subnet_id, transfer.amount
                );
                continue;
            }
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

        // Save ERC token registrations (IPC:ETR) — fan out to all known subnets
        // except the source (home) subnet itself.
        // Also seed the balance ledger: balance(homeSubnet, token, S_home) = initialSupply.
        if !self.token_registrations.is_empty() {
            let all_subnets = db.get_all_subnets_txn(&wtxn)?;
            for etr in &self.token_registrations {
                // Seed balance for S_home with initial_supply (validation already
                // confirmed this is a new token).
                db.set_token_balance(
                    &mut wtxn,
                    source_subnet_id,
                    etr.home_token_address,
                    source_subnet_id,
                    etr.initial_supply,
                )?;
                debug!(
                    "Stored balance for token {} on home subnet {}",
                    etr.home_token_address, source_subnet_id
                );

                for subnet in &all_subnets {
                    if subnet.id == source_subnet_id {
                        continue;
                    }
                    let nonce = db.get_next_rootnet_msg_nonce_txn(&wtxn, subnet.id)?;
                    let rootnet_msg = db::RootnetMessage::ErcRegistration {
                        home_subnet_id: source_subnet_id,
                        registration: etr.clone(),
                        block_height,
                        block_hash,
                        nonce,
                        txid,
                    };
                    db.add_rootnet_msg(&mut wtxn, subnet.id, rootnet_msg)?;
                    debug!(
                        "Added ERC token registration for subnet {}, token: {}",
                        subnet.id, etr.home_token_address
                    );
                }
                debug!(
                    "Stored ERC token registration to the DB, token: {}",
                    etr.home_token_address
                );
            }
        }

        // Process ERC supply adjustments (IPC:ETS) — mints first, then burns.
        // Order must match validate_erc_batch_data() so that a batch containing
        // both a burn and a mint for the same token is applied consistently.
        for ems in self
            .token_supply_adjustments
            .iter()
            .filter(|e| !e.delta.is_negative())
        {
            let current_balance = db.get_token_balance_txn(
                &wtxn,
                source_subnet_id,
                ems.home_token_address,
                source_subnet_id,
            )?;
            let (_, mint_amount) = ems.delta.into_sign_and_abs();
            let new_balance = current_balance.saturating_add(mint_amount);
            db.set_token_balance(
                &mut wtxn,
                source_subnet_id,
                ems.home_token_address,
                source_subnet_id,
                new_balance,
            )?;
            debug!(
                "Applied mint of amount {} for token {} on subnet {}",
                mint_amount, ems.home_token_address, source_subnet_id
            );
        }

        for ems in self
            .token_supply_adjustments
            .iter()
            .filter(|e| e.delta.is_negative())
        {
            let current_balance = db.get_token_balance_txn(
                &wtxn,
                source_subnet_id,
                ems.home_token_address,
                source_subnet_id,
            )?;
            let (_, burn_amount) = ems.delta.into_sign_and_abs();
            let Some(new_balance) = current_balance.checked_sub(burn_amount) else {
                error!(
                    "ETS burn underflow for token {} on subnet {}: balance {}, burn {} \
                     — should have been caught by validate_erc_batch_data()",
                    ems.home_token_address, source_subnet_id, current_balance, burn_amount,
                );
                return Err(IpcValidateError::InvalidMsg(format!(
                    "ETS burn underflow for token {}: balance {}, burn {}",
                    ems.home_token_address, current_balance, burn_amount
                ))
                .into());
            };
            db.set_token_balance(
                &mut wtxn,
                source_subnet_id,
                ems.home_token_address,
                source_subnet_id,
                new_balance,
            )?;
            debug!(
                "Applied burn of amount {} for token {} on subnet {}",
                burn_amount, ems.home_token_address, source_subnet_id
            );
        }

        // Save ERC transfers (IPC:ETX) — firewall: check source has sufficient balance
        for etx in &self.token_transfers {
            if etx.amount == U256::ZERO {
                debug!(
                    "Skipping zero-amount ETX for token ({},{}) on subnet {}",
                    etx.home_subnet_id, etx.home_token_address, source_subnet_id
                );
                continue;
            }
            if !receives_transfers(db, &wtxn, etx.destination_subnet_id)? {
                debug!(
                    "Destination subnet {} is not active: ERC transfer of {} skipped, the \
                     amount stays with subnet {}",
                    etx.destination_subnet_id, etx.amount, source_subnet_id
                );
                continue;
            }
            let src_balance = db.get_token_balance_txn(
                &wtxn,
                etx.home_subnet_id,
                etx.home_token_address,
                source_subnet_id,
            )?;

            // Debit source
            let Some(new_src) = src_balance.checked_sub(etx.amount) else {
                error!(
                    "ETX underflow for token {} from subnet {} to {}: balance {}, amount {} \
                     — should have been caught by validate_erc_batch_data()",
                    etx.home_token_address,
                    source_subnet_id,
                    etx.destination_subnet_id,
                    src_balance,
                    etx.amount,
                );
                return Err(IpcValidateError::InvalidMsg(format!(
                    "ETX underflow for token {}: balance {}, amount {}",
                    etx.home_token_address, src_balance, etx.amount
                ))
                .into());
            };
            db.set_token_balance(
                &mut wtxn,
                etx.home_subnet_id,
                etx.home_token_address,
                source_subnet_id,
                new_src,
            )?;

            // Credit destination
            let dst_balance = db.get_token_balance_txn(
                &wtxn,
                etx.home_subnet_id,
                etx.home_token_address,
                etx.destination_subnet_id,
            )?;
            let new_dst = dst_balance.checked_add(etx.amount).unwrap_or_else(|| {
                error!(
                    "Balance overflow crediting subnet {} for token {} — clamping to MAX",
                    etx.destination_subnet_id, etx.home_token_address
                );
                U256::MAX
            });
            db.set_token_balance(
                &mut wtxn,
                etx.home_subnet_id,
                etx.home_token_address,
                etx.destination_subnet_id,
                new_dst,
            )?;

            // Store rootnet message for destination subnet
            let nonce = db.get_next_rootnet_msg_nonce_txn(&wtxn, etx.destination_subnet_id)?;
            let rootnet_msg = db::RootnetMessage::ErcTransfer {
                msg: etx.clone(),
                block_height,
                block_hash,
                nonce,
                txid,
            };
            db.add_rootnet_msg(&mut wtxn, etx.destination_subnet_id, rootnet_msg)?;
            debug!(
                "Stored ERC transfer for subnet {}, token: {}, recipient: {}",
                etx.destination_subnet_id, etx.home_token_address, etx.recipient
            );
        }

        for (record, signatures) in &newly_slashable {
            db.add_slash_presigs(
                &mut wtxn,
                &db::SlashPresigRecord {
                    subnet_id: source_subnet_id,
                    outpoint: record.outpoint,
                    signatures: signatures.clone(),
                    block_height,
                    batch_transfer_txid: txid,
                },
            )?;
            info!(
                "Collateral {} of validator {} in subnet {} is slashable",
                record.outpoint, record.pubkey, source_subnet_id
            );
        }

        // Commit all changes
        wtxn.commit()?;

        debug!(
            "Stored BatchTransfer data for checkpoint #{} for subnet {}",
            checkpoint.checkpoint_number, self.subnet_id
        );

        Ok(checkpoint)
    }
}

pub struct CheckedSlashEntry {
    pub record: db::UeCollateral,
    pub script: crate::collateral::CollateralScript,
    pub valid: Vec<(u16, Signature64)>,
    pub invalid: Vec<u16>,
    pub power: u64,
    pub slashable: bool,
}

impl CheckedSlashEntry {
    pub fn reaches_threshold(&self) -> bool {
        self.power >= u64::from(self.script.configuration.threshold)
    }

    /// The valid signatures in ascending order of index, until their powers reach the threshold.
    pub fn signatures_to_threshold(&self) -> Vec<(u16, Signature64)> {
        let keys = self.script.configuration.sorted_keys();
        let threshold = u64::from(self.script.configuration.threshold);
        let mut valid = self.valid.clone();
        valid.sort_by_key(|(index, _)| *index);
        let mut power = 0;
        valid
            .into_iter()
            .take_while(|(index, _)| {
                let below = power < threshold;
                power += keys
                    .get(usize::from(*index))
                    .map_or(0, |(_, p)| u64::from(*p));
                below
            })
            .collect()
    }
}

/// Checks a slash pre-signature entry against the configuration named by the collateral script.
pub fn check_slash_entry<D: db::DatabaseCore>(
    db: &D,
    txn: &RoTxn,
    subnet_id: SubnetId,
    entry: &SlashSignatureEntry,
) -> Result<CheckedSlashEntry, IpcLibError> {
    let outpoint = entry.outpoint();
    let record = db
        .get_ue_collateral_by_outpoint_txn(txn, &outpoint)?
        .filter(|r| r.subnet_id == subnet_id)
        .ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!(
                "Slash pre-signature entry names {outpoint}, not a collateral of subnet {subnet_id}"
            ))
        })?;
    let script = crate::collateral::collateral_script_of(db, txn, &record)?;
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (valid, invalid, power) = crate::slash::check_slash_signatures(
        &secp,
        &script,
        outpoint,
        record.value,
        &entry.signatures,
    )?;
    let slashable = db
        .get_slash_presigs_txn(txn, subnet_id, &outpoint)?
        .is_some();
    Ok(CheckedSlashEntry {
        record,
        script,
        valid,
        invalid,
        power,
        slashable,
    })
}

fn receives_transfers<D: db::DatabaseCore>(
    db: &D,
    txn: &RoTxn,
    subnet_id: SubnetId,
) -> Result<bool, IpcLibError> {
    Ok(db
        .get_subnet_state_txn(txn, subnet_id)?
        .is_some_and(|subnet| subnet.is_activated()))
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

        if subnet.is_killed_or_pending() {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {} is killed or marked for killing, cannot change stake.",
                self.subnet_id
            )));
        }

        Ok(())
    }

    /// Validates the stake against `members`, the multisig members of the next configuration as
    /// filled so far, and returns the validator's collateral after it.
    pub fn validate_for_members(
        &self,
        members: &BTreeMap<XOnlyPublicKey, db::SubnetValidator>,
    ) -> Result<Amount, IpcValidateError> {
        let member = members.get(&self.pubkey).ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!(
                "Validator with public key {} is not a multisig member of subnet {}",
                self.pubkey, self.subnet_id
            ))
        })?;

        let total_collateral: Amount = members.values().map(|m| m.collateral).sum();
        let diff = self.amount.to_sat() as f64 / total_collateral.to_sat() as f64 * 100.0;
        if diff < Self::MIN_STAKE_DIFF {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Stake addition must change the total collateral by at least {:.2}%. Current change: {:.2}%",
                Self::MIN_STAKE_DIFF, diff
            )));
        }

        member
            .collateral
            .checked_add(self.amount)
            .ok_or_else(|| IpcValidateError::InvalidMsg("collateral value overflow".to_string()))
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
    pub fn from_tx<D: db::DatabaseCore>(
        db: &D,
        txn: &RoTxn,
        tx: &Transaction,
    ) -> Result<Self, IpcLibError> {
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

        let subnet_id = match db.get_subnet_by_multisig_address_txn(txn, &multisig_address) {
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

    pub fn save_to_db<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &mut RwTxn,
        block_height: u64,
        _block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let genesis_info = db.get_subnet_genesis_info_txn(txn, self.subnet_id)?.ok_or(
            IpcValidateError::InvalidMsg(format!("subnet id={} does not exist", self.subnet_id)),
        )?;

        let mut subnet_state = db
            .get_subnet_state_txn(txn, self.subnet_id)
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

        let members = crate::epochs::next_multisig_members(db, txn, &genesis_info, &subnet_state)?;
        let new_collateral = self.validate_for_members(&members)?;
        crate::collateral::refuse_excess_power(
            db,
            txn,
            &genesis_info,
            Some(&subnet_state),
            block_height,
            Some(&self.pubkey),
            new_collateral,
        )?;

        subnet_state
            .pending_multisig_changes
            .push(db::PendingMultisigChange::Stake {
                pubkey: self.pubkey,
                amount: self.amount,
            });
        let mut wtxn = db.nested_write_txn(txn)?;
        db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet_state)?;
        wtxn.commit()?;
        info!(
            "Subnet {} stake of {} by {} in block {} ({txid}), applied at the next cutoff",
            self.subnet_id, self.amount, self.pubkey, block_height
        );

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

        if subnet.is_killed_or_pending() {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Subnet {} is killed or marked for killing, cannot change stake.",
                self.subnet_id
            )));
        }

        if self.pubkey.is_none() {
            return Err(IpcValidateError::InvalidField(
                "pubkey",
                "Validator public key is required".to_string(),
            ));
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

    /// Validates the unstake against `members`, the multisig members of the next configuration as
    /// filled so far, and returns whether the validator leaves.
    pub fn validate_for_members(
        &self,
        genesis_info: &db::SubnetGenesisInfo,
        members: &BTreeMap<XOnlyPublicKey, db::SubnetValidator>,
    ) -> Result<bool, IpcLibError> {
        let pubkey = self.pubkey.ok_or_else(|| {
            IpcValidateError::InvalidField("pubkey", "Validator public key is required".to_string())
        })?;
        let member = members.get(&pubkey).ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!(
                "Validator with public key {pubkey} is not a multisig member of subnet {}",
                self.subnet_id
            ))
        })?;
        let rest = member.collateral.checked_sub(self.amount).ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!(
                "Amount {} is greater than current collateral {}",
                self.amount, member.collateral
            ))
        })?;
        let leaves = match genesis_info.create_subnet_msg.power_of(&rest) {
            Ok(_) => false,
            Err(crate::multisig::MultisigError::InsufficientCollateral) => true,
            Err(e) => return Err(e.into()),
        };
        if leaves && self.amount != member.collateral {
            return Err(IpcValidateError::InvalidMsg(format!(
                "Validator {pubkey} would keep less than the minimum stake after unstaking {}: unstake all {}",
                self.amount, member.collateral
            ))
            .into());
        }
        Ok(leaves)
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
    pub fn from_tx<D: db::DatabaseCore>(
        db: &D,
        txn: &RoTxn,
        tx: &Transaction,
    ) -> Result<Self, IpcLibError> {
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

        let subnet = match db.get_subnet_by_multisig_address_txn(txn, &multisig_address) {
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

        // The multisig members of the next configuration as filled so far are among these keys
        let pending_joins =
            subnet
                .pending_multisig_changes
                .iter()
                .filter_map(|change| match change {
                    db::PendingMultisigChange::Join(validator) => Some(validator),
                    _ => None,
                });
        let validator = subnet
            .latest_committee()
            .validators
            .iter()
            .chain(pending_joins)
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

    pub fn save_to_db<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &mut RwTxn,
        block_height: u64,
        _block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let genesis_info = db.get_subnet_genesis_info_txn(txn, self.subnet_id)?.ok_or(
            IpcValidateError::InvalidMsg(format!("subnet id={} does not exist", self.subnet_id)),
        )?;

        let mut subnet_state = db
            .get_subnet_state_txn(txn, self.subnet_id)
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

        let members = crate::epochs::next_multisig_members(db, txn, &genesis_info, &subnet_state)?;
        let leaves = self.validate_for_members(&genesis_info, &members)?;

        subnet_state
            .pending_multisig_changes
            .push(db::PendingMultisigChange::Unstake {
                pubkey,
                amount: self.amount,
            });
        let mut wtxn = db.nested_write_txn(txn)?;
        db.save_subnet_state(&mut wtxn, self.subnet_id, &subnet_state)?;
        wtxn.commit()?;
        info!(
            "Subnet {} unstake of {} by {pubkey} in block {} ({txid}), applied at the next cutoff{}",
            self.subnet_id,
            self.amount,
            block_height,
            if leaves { ": the validator leaves" } else { "" }
        );

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
    pub fn from_tx<D: db::DatabaseCore>(
        db: &D,
        txn: &RoTxn,
        tx: &Transaction,
    ) -> Result<Self, IpcLibError> {
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

        let subnet = match db.get_subnet_by_multisig_address_txn(txn, &multisig_address) {
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

    pub fn save_to_db<D: db::DatabaseCore>(
        &self,
        db: &D,
        txn: &mut RwTxn,
        block_height: u64,
        block_hash: bitcoin::BlockHash,
        txid: Txid,
    ) -> Result<(), IpcLibError> {
        let genesis_info = db.get_subnet_genesis_info_txn(txn, self.subnet_id)?.ok_or(
            IpcValidateError::InvalidMsg(format!("subnet id={} does not exist", self.subnet_id)),
        )?;

        let subnet_state = db
            .get_subnet_state_txn(txn, self.subnet_id)
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
        let mut valid_kill_requests =
            db.get_valid_kill_requests_txn(txn, self.subnet_id, block_height)?;

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

        let mut wtxn = db.nested_write_txn(txn)?;

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
                    // if there was a checkpoint, set the marked for kill checkpoint number
                    if let Some(last_checkpoint) = updated_subnet_state.last_checkpoint_number {
                        updated_subnet_state.marked_for_kill_checkpoint_number =
                            Some(last_checkpoint);
                    }
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
    JoinUe {
        input: usize,
        msg: IpcJoinUeMsg,
    },
}

impl IpcMessage {
    /// The messages of `tx`'s reveal inputs, in input order, with the input index.
    pub fn from_reveal_inputs(tx: &Transaction) -> Vec<(usize, Self)> {
        tx.input
            .iter()
            .enumerate()
            .filter_map(|(index, input)| {
                let found = bitcoin_utils::reveal_input(input)?;
                if let Err(e) = found.check_exact() {
                    warn!(
                        "Skipping reveal input {index} of transaction {}: {e}",
                        tx.compute_txid()
                    );
                    return None;
                }
                match Self::from_reveal_input(tx, index, &found.data) {
                    Ok(msg) => Some((index, msg)),
                    Err(IpcLibError::MsgParseError("tag", e)) => {
                        trace!("Input {index}: not an IPC message: {e}");
                        None
                    }
                    Err(e) => {
                        warn!(
                            "Skipping IPC message in transaction {} input {index}: {e}",
                            tx.compute_txid()
                        );
                        None
                    }
                }
            })
            .collect()
    }

    /// The message of the reveal input `tx.input[index]`, whose executed leaf holds `data`.
    pub fn from_reveal_input(
        tx: &Transaction,
        index: usize,
        data: &[u8],
    ) -> Result<Self, IpcLibError> {
        let tag = if data.len() >= IPC_TAG_LENGTH {
            &data[..IPC_TAG_LENGTH]
        } else {
            return Err(IpcLibError::MsgParseError(
                "tag",
                "Message too short for a valid tag".to_string(),
            ));
        };
        let tag = std::str::from_utf8(tag).map_err(|e| {
            IpcLibError::MsgParseError("tag", format!("Could not deserialize tag: {}", e))
        })?;
        let tag =
            IpcTag::from_str(tag).map_err(|e| IpcLibError::MsgParseError("tag", e.to_string()))?;
        let parse_err =
            |e: IpcSerializeError| IpcLibError::MsgParseError(tag.as_str(), e.to_string());

        match tag {
            IpcTag::CreateSubnet => {
                if tx.input.len() != 1 || tx.output.len() != 1 {
                    return Err(IpcLibError::MsgParseError(
                        tag.as_str(),
                        format!(
                            "a create reveal has exactly one input and one output, found {} and {}",
                            tx.input.len(),
                            tx.output.len()
                        ),
                    ));
                }
                Ok(IpcMessage::CreateSubnet(
                    IpcCreateSubnetMsg::from_bytes(data).map_err(parse_err)?,
                ))
            }
            IpcTag::JoinSubnet => Ok(IpcMessage::JoinSubnet(
                IpcJoinSubnetMsg::from_bytes(data).map_err(parse_err)?,
            )),
            IpcTag::JoinUe => Ok(IpcMessage::JoinUe {
                input: index,
                msg: IpcJoinUeMsg::from_bytes(data).map_err(parse_err)?,
            }),
            IpcTag::BatchTransfer => Ok(IpcMessage::BatchTransfer(
                IpcBatchTransferMsg::from_reveal_input(&tx.input[index], data)?,
            )),
            // Carried by outputs
            IpcTag::CheckpointSubnet
            | IpcTag::PrefundSubnet
            | IpcTag::FundSubnet
            | IpcTag::StakeCollateral
            | IpcTag::UnstakeCollateral
            | IpcTag::KillSubnet => Err(IpcLibError::MsgParseError(
                tag.as_str(),
                "not a reveal message".to_string(),
            )),
        }
    }
}

//
// Subnet ID
//

pub const L1_DELEGATED_NAMESPACE: u64 = 10;

/// Create Subnet IPC message is sent as a commit-reveal transaction pair.
/// Subnet ID is derived from the transaction ID of the reveal transaction.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

    #[error(transparent)]
    CollateralError(#[from] crate::collateral::CollateralError),
}

#[cfg(test)]
mod tests;
