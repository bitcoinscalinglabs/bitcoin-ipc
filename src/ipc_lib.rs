use bitcoin::{key::Secp256k1, Amount, OutPoint, TxOut};

use crate::{
    bitcoin_utils::{
        collect_amount, commit_arbitrary_data, create_change_txout, init_rpc_client, init_wallet,
        reveal_arbitrary_data, test_and_submit,
    },
    utils,
};

pub fn create_child(
    subnet_address: bitcoin::Address,
    subnet_data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    println!("{:?}", subnet_data);
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env();

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
