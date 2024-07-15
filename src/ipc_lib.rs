use bitcoin::Amount;

use crate::{
    bitcoin_utils::{
        create_or_load_wallet, init_rpc_client, test_and_submit, write_arbitrary_data,
        LocalNodeError,
    },
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
pub fn create_child(
    subnet_address: &bitcoin::Address,
    subnet_data: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("{:?}", subnet_data);
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;

    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;

    let (miner_address, _) = create_or_load_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let amount_to_send = Amount::from_btc(1.0)?;
    let fee: Amount = Amount::from_sat(200);

    let (commit_tx, reveal_tx) =
        write_arbitrary_data(&rpc, amount_to_send, fee, subnet_data, subnet_address);

    test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address);

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
pub fn join_child(
    subnet_address: &bitcoin::Address,
    collateral: Amount,
    validator_data: &str,
) -> Result<(), LocalNodeError> {
    let fee: Amount = Amount::from_sat(200);

    // Init RPC connection and wallet
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _) = create_or_load_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    // let (commit_tx, reveal_tx) =
    //     write_arbitrary_data(&rpc, collateral, fee, validator_data, subnet_address);

    // test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address);

    let input_info = crate::bitcoin_utils::collect_amount(&rpc, collateral, fee).unwrap();
    let change =
        crate::bitcoin_utils::create_change_txout(&rpc, &input_info, collateral, fee).unwrap();

    match crate::bitcoin_utils::create_transactions_with_arbitrary_data(
        input_info,
        change,
        collateral,
        fee,
        subnet_address,
        validator_data,
    ) {
        Ok((commit_tx, reveal_tx)) => {
            test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address)
        }
        Err(_) => {}
    }

    Ok(())
}
