use thiserror::Error;

use bitcoin::Amount;

use crate::{
    bitcoin_utils::{init_rpc_client, init_wallet, test_and_submit, write_arbitrary_data},
    utils,
};

pub fn create_child(
    subnet_address: &bitcoin::Address,
    subnet_data: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("{:?}", subnet_data);
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;

    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;

    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let amount_to_send = Amount::from_btc(1.0)?;
    let fee: Amount = Amount::from_sat(200);

    let (commit_tx, reveal_tx) =
        write_arbitrary_data(&rpc, amount_to_send, fee, subnet_data, subnet_address);

    test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address);

    Ok(())
}

pub fn join_child(
    subnet_address: &bitcoin::Address,
    collateral: Amount,
    validator_data: &str,
) -> Result<(), JoinChildError> {
    let fee: Amount = Amount::from_sat(200);

    // Init RPC connection and wallet
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let (commit_tx, reveal_tx) =
        write_arbitrary_data(&rpc, collateral, fee, validator_data, subnet_address);

    test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address);
    Ok(())
}

#[derive(Error, Debug)]
pub enum JoinChildError {
    #[error("no child subnet with address `{0}` was found")]
    SubnetNotFound(bitcoin::Address),

    #[error("error when reading an environment variable")]
    EnvVarError(#[from] std::env::VarError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),

    #[error("internal error")]
    Internal,
}
