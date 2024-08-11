use bitcoin::key::Secp256k1;
use bitcoin_ipc::{bitcoin_utils::generate_private_key, ipc_lib};

pub fn main() -> Result<(), ipc_lib::Error> {
    let secp = &Secp256k1::new();

    let private_key = generate_private_key(1, bitcoin_ipc::NETWORK)?;
    let public_key: bitcoin::secp256k1::PublicKey = private_key.to_keypair(secp).public_key();

    let keypair_string = format!(
        "private_key:\n{}\npublic_key:\n{}\n",
        private_key.to_string(),
        public_key.to_string()
    );
    print!("{}", keypair_string);

    Ok(())
}
