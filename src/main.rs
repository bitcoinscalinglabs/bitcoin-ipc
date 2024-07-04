use bitcoin::key::Secp256k1;
use bitcoin_ipc::{
    bitcoin_utils::get_address_from_private_key, bitcoin_utils::get_private_key,
    ipc_lib::create_child,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subnet_data = bitcoin_ipc::IPC_CREATE_SUBNET_TAG;

    let receiver_key: bitcoin::bip32::Xpriv = get_private_key(1, bitcoin_ipc::NETWORK);
    let subnet_address =
        get_address_from_private_key(&Secp256k1::new(), &receiver_key, bitcoin_ipc::NETWORK);

    create_child(subnet_address, subnet_data.as_bytes())?;

    Ok(())
}
