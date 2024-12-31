use bitcoin::ScriptBuf;
use bitcoin::{Amount, Transaction, TxOut};
use thiserror::Error;

use crate::bitcoin_utils::{self, test_and_submit, write_arbitrary_data, CommitRevealFee};

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

/// Joins an existing subnet by attaching validator data to a Bitcoin transaction.
///
/// This function creates a Bitcoin transaction that includes specified validator data
/// and submits it to the Bitcoin network. The transaction involves creating and revealing
/// a script containing the data using the Taproot script-path. This process ensures
/// the data is embedded in the blockchain.
///
/// # Arguments
///
/// * `subnet_address` - A reference to a `bitcoin::Address` that represents the subnet's multisig address.
/// * `collateral` - An `Amount` representing the collateral to be locked by the subnet's multisig address.
/// * `validator_data` - A string slice that holds the validator data to be embedded in the transaction.
///   This data should contain:
///     - Validator's information, such as their IP, for discovery by other validators.
///
/// # Returns
///
/// This function returns a `Result`:
/// * `Ok(())` - If the transaction is successfully created and submitted.
/// * `Err(JoinChildError)` - If an error occurs during the process.
pub fn create_and_submit_join_child_tx(
    rpc: &bitcoincore_rpc::Client,
    subnet_address: &bitcoin::Address,
    collateral: Amount,
    initial_funding: Amount,
    validator_data: &str,
) -> Result<(), IpcLibError> {
    let output = TxOut {
        value: collateral + initial_funding,
        script_pubkey: subnet_address.script_pubkey(),
    };

    let commit_fee = bitcoin_utils::calculate_fee(rpc, 2, 3, 65);
    let reveal_fee = bitcoin_utils::calculate_fee(rpc, 1, 1, validator_data.as_bytes().len());

    let fee = CommitRevealFee::new(commit_fee, reveal_fee);

    let (commit_tx, reveal_tx) = write_arbitrary_data(
        rpc,
        collateral + initial_funding,
        fee,
        validator_data,
        subnet_address,
        vec![output],
        None,
    )?;

    match test_and_submit(rpc, vec![commit_tx, reveal_tx]) {
        Ok(_) => Ok(()),
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
