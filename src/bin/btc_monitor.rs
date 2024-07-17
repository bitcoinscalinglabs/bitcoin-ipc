use std::{thread, time::Duration};

use bitcoin_ipc::{ipc_subnet_state::IPCSubnetState, utils};

use bitcoincore_rpc::{Auth, Client, RpcApi};

fn parse_create_command(witness_str: &str) -> Result<IPCSubnetState, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = witness_str.split(':').collect();

    if parts.len() != 5 {
        println!("Invalid input format");
        Err("Invalid input format")?;
    }

    let name = parts[2].strip_prefix("name=").unwrap_or("");
    let url = parts[3].strip_prefix("url=").unwrap_or("");
    let pk = parts[4].strip_prefix("pk=").unwrap_or("");

    if name.is_empty() || url.is_empty() || pk.is_empty() {
        println!("Invalid input format");
        Err("Invalid input format")?;
    }

    let ipc_subnet_state = IPCSubnetState::new(name.to_string(), url.to_string(), pk.to_string());

    let mut parent = ipc_subnet_state.clone().get_parent();

    parent.add_child_subnet(name);

    ipc_subnet_state.save_state();

    Ok(parent)
}

fn parse_join_command(witness_str: &str) -> Result<IPCSubnetState, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = witness_str.split(':').collect();

    if parts.len() != 6 {
        println!("Invalid input format");
        Err("Invalid input format")?;
    }

    let ip = parts[2].strip_prefix("ip=").unwrap_or("");
    let pk = parts[3].strip_prefix("pk=").unwrap_or("");
    let collateral: u64 = parts[4]
        .strip_prefix("collateral=")
        .unwrap_or("")
        .trim()
        .parse()
        .expect("Invalid collateral amount");
    let url: String = parts[5].strip_prefix("url=").unwrap_or("").to_string();

    if ip.is_empty() || pk.is_empty() || url.is_empty() {
        println!("Invalid input format");
        Err("Invalid input format")?;
    }

    let file_name = format!("{}/{}.json", url, url.split("/").last().unwrap_or_default());
    let mut ipc_subnet_state = IPCSubnetState::load_state(file_name)?;

    ipc_subnet_state.add_validator(ip.to_string(), collateral);

    Ok(ipc_subnet_state)
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;

    let rpc = Client::new(
        &rpc_url,
        Auth::UserPass(rpc_user.to_string(), rpc_pass.to_string()),
    )?;

    let _ = rpc.load_wallet(&wallet_name);

    let mut blockchain_info;

    let mut current_block_height = rpc.get_blockchain_info()?.blocks;

    loop {
        println!("Checking for new blocks...");
        blockchain_info = rpc.get_blockchain_info()?;
        let latest_block_height = blockchain_info.blocks;

        // Check for new blocks
        if latest_block_height > current_block_height {
            for block_height in (current_block_height + 1)..=latest_block_height {
                println!("Checking block height: {}", block_height);

                let block_hash = rpc.get_block_hash(block_height)?;
                let block = rpc.get_block(&block_hash)?;

                for tx in block.txdata {
                    println!("Checking transaction: {}", tx.compute_txid());
                    for input in &tx.input {
                        for witness in input.witness.iter() {
                            let witness_slice: Vec<u8> = witness.iter().map(|&x| x as u8).collect();

                            if let Ok(witness_str) = std::str::from_utf8(&witness_slice) {
                                match () {
                                    _ if witness_str
                                        .contains(bitcoin_ipc::IPC_CREATE_SUBNET_TAG) =>
                                    {
                                        println!("Transaction {} at block height {} contains the keyword '{:?}'", tx.compute_txid(), block_height, bitcoin_ipc::IPC_CREATE_SUBNET_TAG);
                                        println!("Command: {}", witness_str);
                                        println!("Executing the CREATE command...");
                                        let _ = parse_create_command(witness_str)?;
                                        println!("CREATE Command executed successfully");
                                    }
                                    _ if witness_str.contains(bitcoin_ipc::IPC_JOIN_SUBNET_TAG) => {
                                        println!("Transaction {} at block height {} contains the keyword '{:?}'", tx.compute_txid(), block_height, bitcoin_ipc::IPC_JOIN_SUBNET_TAG);
                                        println!("Command: {}", witness_str);
                                        println!("Executing the JOIN command...");
                                        let _ = parse_join_command(witness_str)?;
                                        println!("JOIN Command executed successfully");
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            current_block_height = latest_block_height;
        }

        thread::sleep(Duration::from_secs(10));
    }
}
