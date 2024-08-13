use thiserror::Error;

use bitcoin::Amount;

use crate::{
    bitcoin_utils::{self, init_rpc_client, init_wallet, test_and_submit, write_arbitrary_data},
    ipc_state::IPCState,
    subnet_simulator::SubnetSimulator,
    utils,
};

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
    subnet_address: &bitcoin::Address,
    subnet_data: &str,
) -> Result<(), IpcLibError> {
    println!("{:?}", subnet_data);
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;

    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;

    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let amount_to_send = Amount::from_btc(1.0)?;
    let fee: Amount = Amount::from_sat(200);

    let (commit_tx, reveal_tx) =
        write_arbitrary_data(&rpc, amount_to_send, fee, subnet_data, subnet_address)?;

    test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address)?;

    Ok(())
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
    subnet_address: &bitcoin::Address,
    collateral: Amount,
    validator_data: &str,
) -> Result<(), IpcLibError> {
    let fee: Amount = Amount::from_sat(200);

    // Init RPC connection and wallet
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let (commit_tx, reveal_tx) =
        write_arbitrary_data(&rpc, collateral, fee, validator_data, subnet_address)?;

    test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address)?;
    Ok(())
}

/// Submits a checkpoint of a subnet represented by IPCState and SubnetSimulator to the Bitcoin network.
///
/// This function creates a Bitcoin transaction that includes a checkpoint hash in an OP_RETURN output
/// This transaction gets signed by the subnetPK and the signature is added to the witness of the inputs
/// The transaction is then submitted to the Bitcoin network.
///
/// # Arguments
///
/// * `checkpoint_hash` - A string representing the hash of the checkpoint to be submitted.
/// * `ipc_state` - An instance of IPCState representing the state of the subnet.
/// * `simulator` - An instance of SubnetSimulator representing the state of the subnet.
pub fn submit_checkpoint(
    checkpoint_hash: [u8; 32],
    ipc_state: IPCState,
    simulator: SubnetSimulator,
) -> Result<(), SubmitCheckpointError> {
    let fee: Amount = Amount::from_sat(200);

    // Init RPC connection and wallet
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    println!("Submitting checkpoint for subnet: {}", ipc_state.get_url());
    let checkpoint_tx = bitcoin_utils::create_checkpoint_tx(
        &rpc,
        fee,
        ipc_state.get_name(),
        checkpoint_hash,
        simulator.get_keypair(),
    );

    let prevouts = bitcoin_utils::find_prevouts_for_tx(&rpc, checkpoint_tx.clone());

    // sign transaction with the subnetPK - the keypair of the subnet
    let signed_transaction = simulator.sign_transaction(checkpoint_tx.clone(), prevouts);

    test_and_submit(&rpc, vec![signed_transaction], miner_address);

    Ok(())
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

    #[error("internal error")]
    Internal,
}

#[derive(Error, Debug)]
pub enum SubmitCheckpointError {
    #[error("error when reading an environment variable")]
    EnvVarError(#[from] std::env::VarError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),

    #[error("internal error")]
    Internal,
}
