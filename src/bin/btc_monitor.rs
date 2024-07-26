use std::{
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use bitcoin_ipc::{ipc_state::IPCState, utils};

use bitcoincore_rpc::{Auth, Client, RpcApi};

fn parse_create_command(witness_str: &str) -> Result<IPCState, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = witness_str.split(bitcoin_ipc::DELIMITER).collect();

    if parts.len() != 5 {
        println!("Invalid input format");
        Err("Invalid input format")?;
    }
    let name = parts[1].strip_prefix("name=").unwrap_or("");
    let pk = parts[2].strip_prefix("pk=").unwrap_or("");
    let required_number_of_validators: u64 = parts[3]
        .strip_prefix("required_number_of_validators=")
        .unwrap_or("")
        .trim()
        .parse()
        .expect("Invalid number of validators");
    let required_collateral: u64 = parts[4]
        .strip_prefix("required_collateral=")
        .unwrap_or("")
        .trim()
        .parse()
        .expect("Invalid collateral amount");

    if name.is_empty()
        || pk.is_empty()
        || required_number_of_validators == 0
        || required_collateral == 0
    {
        println!("Invalid input format");
        Err("Invalid input format")?;
    }

    let ipc_subnet_state = IPCState::new(
        name.to_string(),
        format!("{}/{}", bitcoin_ipc::L1_NAME, name.to_string()),
        pk.to_string(),
        required_number_of_validators,
        required_collateral,
    );

    ipc_subnet_state.save_state();

    Ok(ipc_subnet_state)
}

fn parse_join_command(witness_str: &str) -> Result<IPCState, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = witness_str.split(bitcoin_ipc::DELIMITER).collect();

    if parts.len() != 5 {
        println!("Invalid input format");
        Err("Invalid input format")?;
    }

    let ip = parts[1].strip_prefix("ip=").unwrap_or("");
    let pk = parts[2].strip_prefix("pk=").unwrap_or("");
    let username: String = parts[3].strip_prefix("username=").unwrap_or("").to_string();
    let subnet_name: String = parts[4]
        .strip_prefix("subnet_name=")
        .unwrap_or("")
        .to_string();

    if ip.is_empty() || pk.is_empty() || username.is_empty() || subnet_name.is_empty() {
        println!("Invalid input format");
        Err("Invalid input format")?;
    }

    let file_name = format!(
        "{}/{}/{}.json",
        bitcoin_ipc::L1_NAME,
        subnet_name,
        subnet_name
    );
    let mut ipc_subnet_state = IPCState::load_state(file_name)?;

    ipc_subnet_state.add_validator(ip.to_string(), username.clone(), pk.to_string());

    // start the interactor after enough validators have joined.
    if ipc_subnet_state.has_required_validators() {
        let _subnet_interactor_handle = thread::spawn(move || {
            Command::new("gnome-terminal")
                .arg(format!("--title=subnet_interactor_{}", subnet_name))
                .arg("--")
                .arg("bash")
                .arg("-c")
                .arg(format!(
                    "cargo run --bin subnet_interactor -- --url {}; exec bash",
                    format!("{}/{}", bitcoin_ipc::L1_NAME, subnet_name)
                ))
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("Failed to start subnet_interactor");
        });
    }

    Ok(ipc_subnet_state)
}

fn find_valid_utf8(data: &[u8]) -> &str {
    let mut start = 0;
    while start < data.len() {
        match std::str::from_utf8(&data[start..]) {
            Ok(valid_str) => return valid_str,
            Err(_) => start += 1,
        }
    }
    ""
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
                            let witness_str = find_valid_utf8(&witness[..]);

                            println!("Witness: {:?}", witness_str);

                            match () {
                                _ if witness_str.contains(bitcoin_ipc::IPC_CREATE_SUBNET_TAG) => {
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
            current_block_height = latest_block_height;
        }

        thread::sleep(Duration::from_secs(10));
    }
}
