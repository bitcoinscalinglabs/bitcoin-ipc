use bitcoin_ipc::bitcoin_utils::{
    collect_amount, commit_arbitrary_data, create_change_txout, create_unspendable_internal_key,
    get_address_from_private_key, get_private_key, init_rpc_client, init_wallet,
    reveal_arbitrary_data, test_and_submit,
};
use bitcoin_ipc::utils;

use bitcoin::blockdata::transaction::{OutPoint, TxOut};
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{amount::Amount, Network};

const NETWORK: Network = Network::Regtest;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env();

    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;

    let (miner_address, _, _) = init_wallet(&rpc, NETWORK, &wallet_name)?;

    let amount_to_send = Amount::from_btc(1.0)?;
    let fee = Amount::from_sat(200);

    let input_info = collect_amount(&rpc, amount_to_send, fee).unwrap();

    let change = create_change_txout(&rpc, &input_info, amount_to_send, fee).unwrap();

    let secp = Secp256k1::new();

    let x_only_pubkey = create_unspendable_internal_key(&secp);

    // Commit Transaction : Include Arbitrary Data in the transaction
    let (commit_tx, script, taproot_spend_info) = commit_arbitrary_data(
        &rpc,
        input_info,
        amount_to_send,
        change,
        b"IPC:CREATE",
        &secp,
        x_only_pubkey,
    );

    let outpoint = OutPoint {
        txid: commit_tx.compute_txid(),
        vout: 0,
    };

    // Get subnet PK address
    let receiver_key = get_private_key(1, NETWORK);
    let subnet_address = get_address_from_private_key(&secp, &receiver_key, NETWORK);

    let output = TxOut {
        value: amount_to_send - fee,
        script_pubkey: subnet_address.script_pubkey(),
    };

    // Reveal Transaction : Reveal the Arbitrary Data
    let reveal_tx = reveal_arbitrary_data(outpoint, output, script, taproot_spend_info);

    test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address);

    Ok(())
}
