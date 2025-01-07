use bitcoin::Amount;
use bitcoin::Txid;
use bitcoin::XOnlyPublicKey;
use ipc_serde::IpcSerialize;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

use crate::bitcoin_utils::{self, create_multisig_address, submit_to_mempool};
use crate::NETWORK;

// Temporary prelude module to re-export the necessary types
pub mod prelude {
    pub use super::{
        IpcCreateSubnetMsg, IpcMessage, IpcSerialize, IpcTag, IPC_CHECKPOINT_TAG,
        IPC_CREATE_SUBNET_TAG, IPC_DELETE_SUBNET_TAG, IPC_DEPOSIT_TAG, IPC_JOIN_SUBNET_TAG,
        IPC_TAG_DELIMITER, IPC_TRANSFER_TAG, IPC_WITHDRAW_TAG,
    };
}

// Tag

pub const IPC_TAG_DELIMITER: &str = "#";
pub const IPC_CREATE_SUBNET_TAG: &str = "IPC:CREATE";
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
}

impl IpcTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CreateSubnet => IPC_CREATE_SUBNET_TAG,
        }
    }
}

impl std::str::FromStr for IpcTag {
    type Err = IpcSerializeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            IPC_CREATE_SUBNET_TAG => Ok(Self::CreateSubnet),
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
    pub min_validator_stake: u64,
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
        )?;

        let commit_txid = commit_tx.compute_txid();
        let reveal_txid = reveal_tx.compute_txid();
        debug!(
            "Create subnet commit_txid={} reveal_txid={}",
            commit_txid, reveal_txid
        );

        match submit_to_mempool(rpc, vec![commit_tx.clone(), reveal_tx.clone()]) {
            Ok(_) => {
                let subnet_id = SubnetId::from_txid(&commit_txid);
                info!("Submitted create subnet msg for subnet_id={}", subnet_id);
                Ok(subnet_id)
            }
            Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
        }
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

// Define the IPCMessage enum
#[derive(Debug)]
pub enum IpcMessage {
    CreateSubnet(IpcCreateSubnetMsg),
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
        }
    }
}

//
// Subnet ID
//

/// Create Subnet IPC message is sent as a commit-reveal transaction pair.
/// Subnet ID is derived from the transaction ID of the commit transaction.
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

// pub fn create_pre_fund_tx(
//     rpc: &bitcoincore_rpc::Client,
//     subnet_id: String,
//     multisig_address: bitcoin::Address,
//     amount: Amount,
// ) -> Result<Transaction, IpcLibError> {
//     // TODO explain 65 + look if other values are possible
//     let witness_bytes = 65;
//     let input = bitcoin_utils::collect_wallet_outpoints_for_amount(rpc, amount, witness_bytes)?;

//     // Create inputs with timelock sequence
//     // Set relative timelock of 6 blocks using BIP68
//     let timelock_sequence = Sequence::from_height(PRE_FUND_TIMELOCK_BLOCKS);
//     let input_vec: Vec<TxIn> = input
//         .into_iter()
//         .map(|input| TxIn {
//             previous_output: input,
//             script_sig: ScriptBuf::new(),
//             sequence: timelock_sequence,
//             witness: Witness::default(),
//         })
//         .collect();

//     // Create change output
//     let change = bitcoin_utils::create_change_txout(rpc, &input, amount, fee, None)?;

//     //  Create collateral output
//     let collateral_output = TxOut {
//         value: amount,
//         script_pubkey: multisig_address.script_pubkey(),
//     };

//     let tx = Transaction {
//         version: transaction::Version::TWO,
//         lock_time: LockTime::ZERO,
//         input: input_vec,
//         output: vec![collateral_output, change],
//     };

//     // 8. Sign the transaction
//     let signed_tx = bitcoin_utils::sign_transaction_safe(rpc, tx)?;

//     Ok(signed_tx)
// }

#[derive(Error, Debug)]
pub enum IpcLibError {
    #[error(transparent)]
    BitcoinUtilsError(#[from] crate::bitcoin_utils::BitcoinUtilsError),
}

#[derive(PartialEq, Eq)]
pub enum IpcTransactionType {
    CreateChild,
    JoinChild,
    Deposit,
    Checkpoint,
    Transfer,
    Withdraw,
    Delete,
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
            min_validator_stake: 1000,
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
        assert_eq!(params.min_validator_stake, 1000);
        assert_eq!(params.min_validators, 2);
        assert_eq!(params.bottomup_check_period, 10);
        assert_eq!(params.active_validators_limit, 20);
        assert_eq!(params.min_cross_msg_fee, Amount::from_sat(50));
        assert_eq!(params.whitelist, vec![validator1, validator2]);
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
