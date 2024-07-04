use clap::Parser;
use std::str::FromStr;

use bitcoin::address::Address;
use bitcoin_ipc::ipc_lib::create_child;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Subnet name
    #[arg(short, long)]
    name: String,

    /// Subnet public key
    #[arg(short, long)]
    pk: String,
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut subnet_data = String::new();
    subnet_data.push_str(bitcoin_ipc::IPC_CREATE_SUBNET_TAG);
    subnet_data.push_str(&format!("name={}", args.name));

    let pubkey = bitcoin::secp256k1::PublicKey::from_str(&args.pk)?;
    let btc_pubkey = bitcoin::PublicKey::new(pubkey);
    let subnet_address = Address::p2pkh(btc_pubkey, bitcoin_ipc::NETWORK);

    create_child(subnet_address, subnet_data.as_bytes())?;
    Ok(())
}
