use bitcoin::{Amount, Transaction, TxOut};
use bitcoin::{ScriptBuf, XOnlyPublicKey};
use ipc_serde::IPCSerialize;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

use crate::bitcoin_utils::{self, test_and_submit, write_arbitrary_data, CommitRevealFee};

// Temporary prelude module to re-export the necessary types
pub mod prelude {
    pub use super::{
        IPCCreateSubnetMsg, IPCMessage, IPCSerialize, IPCTag, IPC_CHECKPOINT_TAG,
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
pub enum IPCTag {
    CreateSubnet,
}

impl IPCTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CreateSubnet => IPC_CREATE_SUBNET_TAG,
        }
    }
}

impl std::str::FromStr for IPCTag {
    type Err = IPCSerializeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            IPC_CREATE_SUBNET_TAG => Ok(Self::CreateSubnet),
            _ => Err(IPCSerializeError::UnknownTag(s.to_string())),
        }
    }
}

// IPCSerialize trait

#[derive(Debug, Error)]
pub enum IPCSerializeError {
    #[error("Missing field: {0}")]
    MissingField(String),
    #[error("Error parsing field {0}: {1}")]
    ParseFieldError(String, String),
    #[error("Unknown IPC tag: {0}")]
    UnknownTag(String),
    #[error("Deserialization error: {0}")]
    DeserializationError(String),
}

pub trait IPCSerialize {
    fn ipc_serialize(&self) -> String;
    fn ipc_deserialize(s: &str) -> Result<Self, IPCSerializeError>
    where
        Self: Sized;
}

// IPCValidate trait

#[derive(Debug, Error)]
pub enum IPCValidateError {
    #[error("Invalid field {0}: {1}")]
    InvalidField(&'static str, String),
}

pub trait IPCValidate {
    fn validate(&self) -> Result<(), IPCValidateError>;
}

// IPC Messages

#[derive(Serialize, Deserialize, IPCSerialize, Debug, Clone)]
#[tag(IPC_CREATE_SUBNET_TAG)]
pub struct IPCCreateSubnetMsg {
    /// The minimum number of collateral required for validators in Satoshis
    pub min_validator_stake: u64,
    /// Minimum number of validators required to bootstrap the subnet
    pub min_validators: u64,
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

impl IPCValidate for IPCCreateSubnetMsg {
    fn validate(&self) -> Result<(), IPCValidateError> {
        if self.min_validators == 0 {
            return Err(IPCValidateError::InvalidField(
                "min_validators",
                "The minimum number of validators must be greater than 0".to_string(),
            ));
        }

        if self.whitelist.len() < self.min_validators as usize {
            return Err(IPCValidateError::InvalidField(
                "whitelist",
                "Number of whitelisted validators is less than the minimum required validators"
                    .to_string(),
            ));
        }

        if self.bottomup_check_period == 0 {
            return Err(IPCValidateError::InvalidField(
                "bottomup_check_period",
                "Must be greater than 0".to_string(),
            ));
        }

        if (self.active_validators_limit as u64) < self.min_validators {
            return Err(IPCValidateError::InvalidField(
                "active_validators_limit",
                "Must be greater than or equal to min_validators".to_string(),
            ));
        }

        if self.min_cross_msg_fee == Amount::ZERO {
            return Err(IPCValidateError::InvalidField(
                "min_cross_msg_fee",
                "Must be greater than 0".to_string(),
            ));
        }

        Ok(())
    }
}

// Define the IPCMessage enum
#[derive(Debug)]
pub enum IPCMessage {
    CreateSubnet(IPCCreateSubnetMsg),
}

impl IPCMessage {
    pub fn deserialize(s: &str) -> Result<Self, IPCSerializeError> {
        let tag = s
            .split(IPC_TAG_DELIMITER)
            .next()
            .ok_or_else(|| IPCSerializeError::DeserializationError("Missing tag".to_string()))?;

        // Temporary clippy warning because there is only one value
        #[allow(clippy::manual_map)]
        match IPCTag::from_str(tag)? {
            IPCTag::CreateSubnet => Ok(IPCMessage::CreateSubnet(
                IPCCreateSubnetMsg::ipc_deserialize(s)?,
            )),
        }
    }
}

/// Create Subnet IPC message is sent as a commit-reveal transaction pair.
/// Subnet ID is derived from the transaction ID of the commit transaction.
///
/// Creates a subnet ID from the commit txid
pub fn subnet_id_from_txid(txid: &bitcoin::Txid) -> String {
    format!("{}/{}", crate::L1_NAME, txid)
}

/// Creates a child subnet by attaching arbitrary data to a Bitcoin transaction.
///
/// This function creates a Bitcoin transaction that includes specified arbitrary data and
/// submits it to the Bitcoin network. The transaction involves creating and revealing
/// a script containing the data using the Taproot script-path. This process ensures
/// the data is embedded in the blockchain.
///
/// # Arguments
///
/// * `subnet_address` - A reference to a `bitcoin::Address` that represents the subnet's multisig address.
/// * `subnet_data` - A string slice that holds the data to be embedded in the transaction. This data should contain:
///     - A known tag indicating the creation of a new IPC Subnet.
///     - The subnet name.
///     - Any additional arbitrary data.
///
/// # Returns
///
/// This function returns a `Result`:
/// * `Ok(())` - If the transaction is successfully created and submitted.
/// * `Err(Box<dyn std::error::Error>)` - If an error occurs during the process.
pub fn create_and_submit_create_child_tx(
    rpc: &bitcoincore_rpc::Client,
    subnet_address: &bitcoin::Address,
    subnet_data: &str,
) -> Result<(Transaction, Transaction), IpcLibError> {
    let commit_fee = bitcoin_utils::calculate_fee(rpc, 2, 3, 65);
    let reveal_fee = bitcoin_utils::calculate_fee(rpc, 1, 1, subnet_data.as_bytes().len());

    let fee = CommitRevealFee::new(commit_fee, reveal_fee);

    let op_return_out = TxOut {
        value: Amount::ZERO,
        script_pubkey: ScriptBuf::new_op_return([]),
    };

    let (commit_tx, reveal_tx) = write_arbitrary_data(
        rpc,
        Amount::ZERO,
        fee,
        subnet_data,
        subnet_address,
        vec![op_return_out],
        None,
    )?;

    match test_and_submit(rpc, vec![commit_tx.clone(), reveal_tx.clone()]) {
        Ok(_) => Ok((commit_tx, reveal_tx)),
        Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
    }
}

#[derive(Error, Debug)]
pub enum IpcLibError {
    #[error("error when reading an environment variable")]
    EnvVarError(#[from] std::env::VarError),

    #[error("cannot parse the given amount")]
    AmountError(#[from] bitcoin::amount::ParseAmountError),

    #[error(transparent)]
    BitcoinUtilsError(#[from] crate::bitcoin_utils::BitcoinUtilsError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),

    #[error("Validators did not sign the transaction")]
    ValidatorsDidNotSignTx,

    #[error("Subnet id not found")]
    SubnetIdNotFound,

    #[error("internal error")]
    Internal,
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
        assert_eq!(IPCTag::CreateSubnet.as_str(), IPC_CREATE_SUBNET_TAG);
    }

    #[test]
    fn test_ipc_tag_from_str() {
        assert_eq!(
            IPCTag::from_str(IPC_CREATE_SUBNET_TAG).unwrap(),
            IPCTag::CreateSubnet
        );
        assert!(IPCTag::from_str("INVALID_TAG").is_err());
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

        let params = IPCCreateSubnetMsg {
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

        let params = IPCCreateSubnetMsg::ipc_deserialize(&serialized).unwrap();
        assert_eq!(params.min_validator_stake, 1000);
        assert_eq!(params.min_validators, 2);
        assert_eq!(params.bottomup_check_period, 10);
        assert_eq!(params.active_validators_limit, 20);
        assert_eq!(params.min_cross_msg_fee, Amount::from_sat(50));
        assert_eq!(params.whitelist, vec![validator1, validator2]);
    }
}
