use bitcoin_ipc::utils;

use bitcoincore_rpc::{Auth, Client, RpcApi};
use std::{thread, time::Duration};

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env();

    let rpc = Client::new(
        &rpc_url,
        Auth::UserPass(rpc_user.to_string(), rpc_pass.to_string()),
    )?;

    let _ = rpc.load_wallet(&wallet_name);

    let mut blockchain_info;
    let mut current_block_height = 0;

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
                                if witness_str.contains("IPC:CREATE") {
                                    // Try to parse the rest of the command.
                                    println!("Transaction {} at block height {} contains the keyword 'IPC:CREATE'", tx.compute_txid(), block_height);
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
