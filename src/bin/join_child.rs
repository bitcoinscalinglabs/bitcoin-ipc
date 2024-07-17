use bitcoin::Amount;
use clap::Parser;
use std::str::FromStr;

use bitcoin_ipc::{bitcoin_utils, ipc_lib::join_child};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// IP address
    #[arg(short, long)]
    ip: String,

    /// Subnet public key
    #[arg(short, long)]
    pk: String,

    /// Subnet collateral
    #[arg(short, long)]
    collateral: u64,

    /// url
    #[arg(short, long)]
    url: String,
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut validator_data = String::new();

    validator_data.push_str(bitcoin_ipc::IPC_JOIN_SUBNET_TAG);
    validator_data.push_str(&format!("ip={}:", args.ip));

    let pubkey = bitcoin::secp256k1::PublicKey::from_str(&args.pk)?;
    let subnet_address = bitcoin_utils::get_address_from_public_key(pubkey, bitcoin_ipc::NETWORK);

    validator_data.push_str(&format!("pk={}:", args.pk));
    validator_data.push_str(&format!("collateral={}:", args.collateral));
    validator_data.push_str(&format!("url={}", args.url));

    join_child(
        &subnet_address,
        Amount::from_sat(args.collateral),
        &validator_data,
    )?;
    Ok(())
}
