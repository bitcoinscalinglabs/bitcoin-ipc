use bitcoin::address::NetworkUnchecked;
use bitcoin::hashes::Hash;
use bitcoin::Amount;
use bitcoin::Txid;
use bitcoin::XOnlyPublicKey;
use ipc_serde::IpcSerialize;
use log::trace;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

use crate::bitcoin_utils::{self, create_multisig_address, submit_to_mempool};
use crate::db;
use crate::NETWORK;

// Temporary prelude module to re-export the necessary types
pub mod prelude {
    pub use super::{
        IpcCreateSubnetMsg, IpcMessage, IpcSerialize, IpcTag, IPC_CHECKPOINT_TAG,
        IPC_CREATE_SUBNET_TAG, IPC_DELETE_SUBNET_TAG, IPC_DEPOSIT_TAG, IPC_JOIN_SUBNET_TAG,
        IPC_PREFUND_SUBNET_TAG, IPC_TAG_DELIMITER, IPC_TRANSFER_TAG, IPC_WITHDRAW_TAG,
    };
}

// Tag

pub const IPC_TAG_DELIMITER: &str = "#";
pub const IPC_CREATE_SUBNET_TAG: &str = "IPC:CREATE";
pub const IPC_PREFUND_SUBNET_TAG: &str = "IPC:PREFUND";
pub const IPC_JOIN_SUBNET_TAG: &str = "IPC:JOIN";
pub const IPC_DEPOSIT_TAG: &str = "IPC:DEPOSIT";
pub const IPC_CHECKPOINT_TAG: &str = "IPC:CHECKPOINT";
pub const IPC_TRANSFER_TAG: &str = "IPC:TRANSFER";
pub const IPC_WITHDRAW_TAG: &str = "IPC:WITHDRAW";
pub const IPC_DELETE_SUBNET_TAG: &str = "IPC:DELETE";

// Define the IPC tags enum
#[derive(Debug, PartialEq)]
pub enum IpcTag {
    CreateSubnet,
    JoinSubnet,
}

impl IpcTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CreateSubnet => IPC_CREATE_SUBNET_TAG,
            Self::JoinSubnet => IPC_JOIN_SUBNET_TAG,
        }
    }
}

impl std::str::FromStr for IpcTag {
    type Err = IpcSerializeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            IPC_CREATE_SUBNET_TAG => Ok(Self::CreateSubnet),
            IPC_JOIN_SUBNET_TAG => Ok(Self::JoinSubnet),
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

impl IpcCreateSubnetMsg {
    /// Creates a multisig address from the whitelisted public keys
    pub fn multisig_address_from_whitelist(&self) -> Result<bitcoin::Address, IpcLibError> {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let multisig_address = create_multisig_address(
            &secp,
            &self.whitelist.clone(),
            self.min_validators.into(),
            NETWORK,
        )?;

        Ok(multisig_address)
    }

    /// Submits the create subnet message to the Bitcoin network
    /// Using commit-reveal scheme
    /// Returns the subnet id — derived from the commit txid
    pub fn submit_to_bitcoin(
        &self,
        rpc: &bitcoincore_rpc::Client,
    ) -> Result<SubnetId, IpcLibError> {
        let multisig_address = self.multisig_address_from_whitelist()?;
        let subnet_data = self.ipc_serialize();

        info!(
            "Submitting create subnet msg to bitcoin. Multisig address = {}. Data={}",
            multisig_address, subnet_data
        );

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (commit_tx, reveal_tx) = bitcoin_utils::create_commit_reveal_txs(
            rpc,
            &secp,
            &multisig_address,
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

    pub fn save_to_db<D: db::Database>(
        &self,
        db: &D,
        block_height: u64,
        txid: bitcoin::Txid,
    ) -> Result<(), IpcLibError> {
        let subnet_id = SubnetId::from_txid(&txid);

        let genesis_info = db::SubnetGenesisInfo {
            create_subnet_msg: self.clone(),
            bootstrapped: false,
            genesis_block_height: block_height,
            boostrap_block_height: None,
            genesis_validators: Vec::with_capacity(0),
        };

        trace!("Saving {self:?} to DB, genesis_info={genesis_info:?}");

        let mut wtxn = db.write_txn()?;
        db.save_subnet_genesis_info(&mut wtxn, subnet_id, genesis_info)?;
        wtxn.commit()?;

        Ok(())
    }
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

        let new_validator = db::SubnetValidator {
            pubkey: self.pubkey,
            collateral: self.collateral,
            backup_address: self.backup_address.clone(),
            ip: self.ip,
            join_txid: txid,
        };
        trace!("Processing {self:?}, adding new validator {new_validator:?}");
        genesis_info.genesis_validators.push(new_validator);

        //
        // Check if the subnet is bootstrapped
        //
        if genesis_info.genesis_validators.len() as u16
            >= genesis_info.create_subnet_msg.min_validators
        {
            trace!("Subnet {} bootstrapped", self.subnet_id);
            genesis_info.bootstrapped = true;
            genesis_info.boostrap_block_height = Some(block_height);

            // TODO create subnet in db
        }

        let mut wtxn = db.write_txn()?;
        db.save_subnet_genesis_info(&mut wtxn, self.subnet_id, genesis_info)?;
        wtxn.commit()?;

        Ok(())
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

// Define the IPCMessage enum
#[derive(Debug)]
pub enum IpcMessage {
    CreateSubnet(IpcCreateSubnetMsg),
    JoinSubnet(IpcJoinSubnetMsg),
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
        }
    }
}

//
// Subnet ID
//

/// Create Subnet IPC message is sent as a commit-reveal transaction pair.
/// Subnet ID is derived from the transaction ID of the reveal transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubnetId(Txid);

#[derive(Debug, Error)]
pub enum SubnetIdError {
    #[error("Invalid Subnet Id format. Expected '{0}/<txid>', got '{1}'")]
    InvalidFormat(&'static str, String),
    #[error("Invalid transaction ID: {0}")]
    InvalidTxid(#[from] bitcoin::hashes::hex::HexToArrayError),
}

impl SubnetId {
    /// Creates a new SubnetId from a transaction ID
    pub fn from_txid(txid: &Txid) -> Self {
        Self(*txid)
    }

    /// Returns the transaction ID
    pub fn txid(&self) -> &Txid {
        &self.0
    }
}

impl Default for SubnetId {
    fn default() -> Self {
        Self(Txid::all_zeros())
    }
}

impl FromStr for SubnetId {
    type Err = SubnetIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('/').collect();
        if parts.len() != 2 || parts[0] != crate::L1_NAME {
            return Err(SubnetIdError::InvalidFormat(crate::L1_NAME, s.to_string()));
        }

        let txid = Txid::from_str(parts[1])?;
        Ok(SubnetId(txid))
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
    #[error(transparent)]
    IpcValidateError(#[from] IpcValidateError),

    #[error(transparent)]
    DbError(#[from] crate::db::DbError),

    #[error(transparent)]
    HeedError(#[from] heed::Error),

    #[error(transparent)]
    BitcoinUtilsError(#[from] crate::bitcoin_utils::BitcoinUtilsError),
}

#[cfg(test)]
mod tests {
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
            "{}subnet_id=BTC/4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
            IPC_TAG_DELIMITER
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

        assert_eq!(subnet_id.txid(), &txid);
    }

    #[test]
    fn test_subnet_id_from_str() {
        let subnet_id_str = format!(
            "{}/4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
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
                "{}/4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
                crate::L1_NAME
            )
        );
    }

    #[test]
    fn test_invalid_subnet_id() {
        // Test invalid txid
        let result = SubnetId::from_str(&format!("{}/invalid-txid", crate::L1_NAME));
        assert!(matches!(result, Err(SubnetIdError::InvalidTxid(_))));

        // Test missing prefix
        let result =
            SubnetId::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b");
        assert!(matches!(result, Err(SubnetIdError::InvalidFormat(_, _))));

        // Test wrong prefix
        let result = SubnetId::from_str(
            "wrongchain/4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
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
            "\"{}/4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b\"",
            crate::L1_NAME
        );
        assert_eq!(serialized, expected);

        // Test JSON deserialization
        let deserialized: SubnetId = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, subnet_id);
    }
}
