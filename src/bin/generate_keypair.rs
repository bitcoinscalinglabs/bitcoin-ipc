use bitcoin::key::Secp256k1;
use bitcoin_ipc::bitcoin_utils::get_private_key;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secp = &Secp256k1::new();

    let private_key = get_private_key(1, bitcoin_ipc::NETWORK);
    let public_key: bitcoin::secp256k1::PublicKey = private_key.to_keypair(secp).public_key();

    let keypair_string = format!(
        "private_key:\n{}\npublic_key:\n{}\n",
        private_key.to_string(),
        public_key.to_string()
    );
    print!("{}", keypair_string);

    Ok(())
}
