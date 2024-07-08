use bitcoin::{key::Secp256k1, Amount};
use bitcoin_ipc::{
    bitcoin_utils::{get_address_from_private_key, get_private_key},
    ipc_lib::{create_child, join_child},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subnet_data = bitcoin_ipc::IPC_CREATE_SUBNET_TAG;

    let receiver_key: bitcoin::bip32::Xpriv = get_private_key(1, bitcoin_ipc::NETWORK);
    let subnet_address =
        get_address_from_private_key(&Secp256k1::new(), &receiver_key, bitcoin_ipc::NETWORK);

    create_child(&subnet_address, subnet_data)?;

    let collateral = Amount::from_btc(1.0)?;
    let validator_data = format!("{} IP:{}", bitcoin_ipc::IPC_JOIN_SUBNET_TAG, "...");
    join_child(&subnet_address, collateral, &validator_data)?;

    Ok(())
}
