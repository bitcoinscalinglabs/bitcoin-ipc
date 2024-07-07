use thiserror::Error;

use bitcoin::{key::Secp256k1, Amount, OutPoint, TxOut};

use crate::{
    bitcoin_utils::{
        collect_amount, commit_arbitrary_data, create_change_txout, init_rpc_client, init_wallet,
        reveal_arbitrary_data, test_and_submit,
    },
    utils,
};

pub fn create_child(
    subnet_address: &bitcoin::Address,
    subnet_data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    println!("{:?}", subnet_data);
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;

    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;

    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let amount_to_send = Amount::from_btc(1.0)?;
    let fee: Amount = Amount::from_sat(200);

    let input_info = collect_amount(&rpc, amount_to_send, fee).unwrap();

    let change = create_change_txout(&rpc, &input_info, amount_to_send, fee).unwrap();

    let secp = Secp256k1::new();

    // Commit Transaction : Include Arbitrary Data in the transaction
    let (commit_tx, script, taproot_spend_info) =
        commit_arbitrary_data(&rpc, input_info, amount_to_send, change, subnet_data, &secp);

    let commit_tx_outpoint = OutPoint {
        txid: commit_tx.compute_txid(),
        vout: 0,
    };

    // Get subnet PK address

    let output = TxOut {
        value: amount_to_send - fee,
        script_pubkey: subnet_address.script_pubkey(),
    };

    // Reveal Transaction : Reveal the Arbitrary Data
    let reveal_tx = reveal_arbitrary_data(commit_tx_outpoint, output, script, taproot_spend_info);

    test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address);

    Ok(())
}

pub fn join_child(
    subnet_address: &bitcoin::Address,
    collateral: Amount,
    validator_data: &str,
) -> Result<(), JoinChildError> {
    let fee: Amount = Amount::from_sat(200);
    let secp = Secp256k1::new();

    // Init RPC connection and wallet
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let inputs = collect_amount(&rpc, collateral, fee).unwrap();
    let change_ouput = create_change_txout(&rpc, &inputs, collateral, fee).unwrap();

    // Commit Transaction : Include Arbitrary Data in the transaction
    let (commit_tx, script, taproot_spend_info) = commit_arbitrary_data(
        &rpc,
        inputs,
        collateral,
        change_ouput,
        validator_data.as_bytes(),
        &secp,
    );

    let commit_tx_outpoint = OutPoint {
        txid: commit_tx.compute_txid(),
        vout: 0,
    };

    let output = TxOut {
        value: collateral - fee,
        script_pubkey: subnet_address.script_pubkey(),
    };

    // Reveal Transaction : Reveal the Arbitrary Data
    let reveal_tx = reveal_arbitrary_data(commit_tx_outpoint, output, script, taproot_spend_info);

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

// Orestis: I made `validator_data` of type &str, I think it makes more sense than &[u8]. Probably we should change this in create_child() as well.
// I also think we should merge commit_arbitrary_data() and reveal_arbitrary_data() into a common function write_arbitrary_data() or get_transactions_with_arbitrary_data(),
// because now there are a lot of dependencies between the two functions that are not so relevant for the code that calls them.
