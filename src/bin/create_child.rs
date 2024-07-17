use clap::Parser;
use std::str::FromStr;

use bitcoin_ipc::{bitcoin_utils, ipc_lib::create_child};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Subnet name
    #[arg(short, long)]
    name: String,

    /// Subnet public key
    #[arg(short, long)]
    pk: String,

    /// Subnet URL
    #[arg(short, long)]
    url: String,
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut subnet_data = String::new();
    subnet_data.push_str(bitcoin_ipc::IPC_CREATE_SUBNET_TAG);
    subnet_data.push_str(&format!("name={}:", args.name));
    subnet_data.push_str(&format!("url={}:", args.url));

    let pubkey = bitcoin::secp256k1::PublicKey::from_str(&args.pk)?;
    let subnet_address = bitcoin_utils::get_address_from_public_key(pubkey, bitcoin_ipc::NETWORK);

    subnet_data.push_str(&format!("pk={}", args.pk));

    create_child(&subnet_address, &subnet_data)?;
    Ok(())
}
