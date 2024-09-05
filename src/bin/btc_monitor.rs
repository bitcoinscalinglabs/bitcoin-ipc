use thiserror::Error;

use std::{thread, time::Duration};

use bitcoin::script::Instruction;
use bitcoin_ipc::{bitcoin_utils, ipc_state::IPCState, utils};

use bitcoincore_rpc::RpcApi;

fn parse_create_command(witness_str: &str) -> Result<IPCState, ParseIpcTransactionError> {
    let parts: Vec<&str> = witness_str.split(bitcoin_ipc::DELIMITER).collect();

    if parts.len() != 5 {
        return Err(ParseIpcTransactionError::InvalidWitnessFormat);
    }

    let name = match parts[1].strip_prefix("name=") {
        Some(name) => name,
        None => return Err(ParseIpcTransactionError::MissingName),
    };

    let subnet_address = match parts[2].strip_prefix("subnet_address=") {
        Some(subnet_address) => subnet_address,
        None => return Err(ParseIpcTransactionError::MissingPk),
    };

    let required_number_of_validators: u64 = parts[3]
        .strip_prefix("required_number_of_validators=")
        .unwrap_or("")
        .trim()
        .parse()
        .map_err(|_| ParseIpcTransactionError::CannotParseNumberOfValidators)?;

    if required_number_of_validators == 0 {
        return Err(ParseIpcTransactionError::NumberOfValidatorsZero);
    }

    let required_collateral: u64 = parts[4]
        .strip_prefix("required_collateral=")
        .unwrap_or("")
        .trim()
        .parse()
        .map_err(|_| ParseIpcTransactionError::CollateralZero)?;

    if required_collateral == 0 {
        return Err(ParseIpcTransactionError::CollateralZero);
    }

    let ipc_subnet_state = IPCState::new(
        name.to_string(),
        format!("{}/{}", bitcoin_ipc::L1_NAME, name.to_string()),
        subnet_address.to_string(),
        required_number_of_validators,
        required_collateral,
    );

    match ipc_subnet_state.save_state() {
        Ok(_) => {}
        Err(_) => return Err(ParseIpcTransactionError::CannotWriteIpcState),
    };

    Ok(ipc_subnet_state)
}

fn parse_join_command(witness_str: &str) -> Result<IPCState, ParseIpcTransactionError> {
    let parts: Vec<&str> = witness_str.split(bitcoin_ipc::DELIMITER).collect();

    if parts.len() != 5 {
        return Err(ParseIpcTransactionError::InvalidWitnessFormat);
    }

    let ip = match parts[1].strip_prefix("ip=") {
        Some(ip) => ip,
        None => return Err(ParseIpcTransactionError::MissingIP),
    };

    let pk = match parts[2].strip_prefix("pk=") {
        Some(pk) => pk,
        None => return Err(ParseIpcTransactionError::MissingPk),
    };

    let username = match parts[3].strip_prefix("username=") {
        Some(subnet_address) => subnet_address.to_string(),
        None => return Err(ParseIpcTransactionError::MissingUsername),
    };

    let subnet_name = match parts[4].strip_prefix("subnet_name=") {
        Some(subnet_name) => subnet_name,
        None => return Err(ParseIpcTransactionError::MissingName),
    };

    let file_name = format!(
        "{}/{}/{}.json",
        bitcoin_ipc::L1_NAME,
        subnet_name,
        subnet_name
    );
    let mut ipc_subnet_state = match IPCState::load_state(file_name) {
        Ok(state) => state,
        Err(_) => return Err(ParseIpcTransactionError::CannotReadIpcState),
    };

    match ipc_subnet_state.add_validator(ip.to_string(), username.clone(), pk.to_string()) {
        Ok(_) => {}
        Err(_) => return Err(ParseIpcTransactionError::CannotWriteIpcState),
    };

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

fn main() {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = match utils::load_env() {
        Ok(env) => env,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };

    let rpc = match bitcoin_utils::init_rpc_client(rpc_user, rpc_pass, rpc_url) {
        Ok(rpc) => rpc,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };

    let _ = rpc.load_wallet(&wallet_name);

    let config = match utils::load_config() {
        Ok(config) => config,
        Err(e) => {
            println!("Error loading config: {}", e);
            return;
        }
    };

    let mut current_block_height = match rpc.get_blockchain_info() {
        Ok(info) => info.blocks,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };

    loop {
        println!("Checking for new blocks...");
        if let Err(e) = check_new_blocks(&rpc, &config, &mut current_block_height) {
            println!("Error: {}", e);
            thread::sleep(Duration::from_secs(10));
        }

        thread::sleep(Duration::from_secs(config.listener_interval));
    }
}

fn check_new_blocks(
    rpc: &bitcoincore_rpc::Client,
    config: &utils::Config,
    current_block_height: &mut u64,
) -> Result<(), String> {
    let latest_block_height = match rpc.get_blockchain_info() {
        Ok(info) => info.blocks,
        Err(e) => {
            println!("Error: {}", e);
            return Err(e.to_string());
        }
    };

    if latest_block_height > *current_block_height {
        for block_height in (*current_block_height + 1)..=latest_block_height {
            if let Err(e) = process_block(rpc, config, block_height, current_block_height) {
                println!("Error processing block {}: {}", block_height, e);
                break;
            }
        }
    }
    Ok(())
}

fn process_block(
    rpc: &bitcoincore_rpc::Client,
    config: &utils::Config,
    block_height: u64,
    current_block_height: &mut u64,
) -> Result<(), String> {
    if block_height - *current_block_height < config.ipc_finalization_parameter {
        *current_block_height = block_height - 1;
        println!("Block not finalized, waiting for more blocks...");
        return Ok(());
    }

    let block_hash = rpc
        .get_block_hash(block_height)
        .map_err(|e| e.to_string())?;
    let block = rpc.get_block(&block_hash).map_err(|e| e.to_string())?;

    println!("Processing block: {}", block_height);

    for tx in block.txdata {
        process_transaction(&tx, block_height)?;
    }

    *current_block_height = block_height;
    Ok(())
}

fn process_transaction(tx: &bitcoin::Transaction, block_height: u64) -> Result<(), String> {
    println!("Checking transaction: {}", tx.compute_txid());

    for input in &tx.input {
        for witness in input.witness.iter() {
            let witness_str = find_valid_utf8(&witness[..]);

            if witness_str.contains(bitcoin_ipc::IPC_CREATE_SUBNET_TAG) {
                println!(
                    "Transaction {} at block height {} contains the keyword '{:?}'",
                    tx.compute_txid(),
                    block_height,
                    bitcoin_ipc::IPC_CREATE_SUBNET_TAG
                );
                println!("Command: {}", witness_str);
                println!("Executing the CREATE command...");
                match parse_create_command(witness_str) {
                    Ok(_) => println!("CREATE Command successfully parsed"),
                    Err(e) => println!("CREATE Command could not be parsed. Error: {e}"),
                };
            } else if witness_str.contains(bitcoin_ipc::IPC_JOIN_SUBNET_TAG) {
                println!(
                    "Transaction {} at block height {} contains the keyword '{:?}'",
                    tx.compute_txid(),
                    block_height,
                    bitcoin_ipc::IPC_JOIN_SUBNET_TAG
                );
                println!("Command: {}", witness_str);
                println!("Executing the JOIN command...");
                match parse_join_command(witness_str) {
                    Ok(_) => println!("JOIN Command successfully parsed"),
                    Err(e) => println!("JOIN Command could not be parsed. Error: {e}"),
                };
            }
        }
    }

    process_checkpoints(&tx)?;

    Ok(())
}

fn process_checkpoints(tx: &bitcoin::Transaction) -> Result<(), String> {
    for output in &tx.output {
        let script = &output.script_pubkey;
        let mut instructions = script.instructions();
        if let Some(Ok(Instruction::Op(bitcoin::blockdata::opcodes::all::OP_RETURN))) =
            instructions.next()
        {
            if let Some(Ok(Instruction::PushBytes(data))) = instructions.next() {
                if data.len() > 32 {
                    handle_checkpoint_data(data)?;
                }
            }
        }
    }
    Ok(())
}

fn handle_checkpoint_data(data: &bitcoin::script::PushBytes) -> Result<(), String> {
    if let Ok(data_str) = std::str::from_utf8(&data.as_bytes()[..data.len() - 32]) {
        if data_str.contains("n=")
            && data_str.contains("cp=")
            && data_str.contains(bitcoin_ipc::DELIMITER)
        {
            let parts: Vec<&str> = data_str.split(bitcoin_ipc::DELIMITER).collect();
            if parts.len() != 2 {
                return Err("Invalid checkpoint format".into());
            }

            let subnets = match IPCState::load_all() {
                Ok(subnets) => subnets,
                Err(_) => return Err("Could not load subnets".into()),
            };

            let name = parts[0].strip_prefix("n=").unwrap_or("");
            let checkpoint = hex::encode(&data[data.len() - 32..]);

            println!("Checkpoint for subnet: {}", name);

            subnets.iter().for_each(|subnet| {
                if subnet.get_name() == name {
                    match subnet.get_subnet_address() {
                        Ok(subnet_address) => println!("Subnet address: {}", subnet_address),
                        Err(_) => println!("Could not determine address for subnet"),
                    };
                }
            });

            println!("Checkpoint: {}", checkpoint);
        }
    }

    Ok(())
}

#[derive(Error, Debug)]
pub enum BtcMonitorError {
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),

    #[error(transparent)]
    IPCStateError(#[from] bitcoin_ipc::ipc_state::IpcStateError),

    #[error("unsupported operating system")]
    UnsuportedOperatingSystemError,

    #[error("Env var error")]
    EnvVarError(#[from] std::env::VarError),

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    BtcCoreRpcError(#[from] bitcoincore_rpc::Error),

    #[error("internal error")]
    Internal,
}

#[derive(Error, Debug)]
pub enum ParseIpcTransactionError {
    #[error("invalid witness format")]
    InvalidWitnessFormat,

    #[error("Cannot parse number of validators")]
    CannotParseNumberOfValidators,

    #[error("Cannot parse collateral")]
    CannotParseCollateral,

    #[error("cannot launch subnet interactor")]
    CannotLaunchInteractor,

    #[error("number of validators cannot be 0")]
    NumberOfValidatorsZero,

    #[error("required collateral cannot be 0")]
    CollateralZero,

    #[error("missing field name")]
    MissingName,

    #[error("missing field pk")]
    MissingPk,

    #[error("missing field ip")]
    MissingIP,

    #[error("missing field username")]
    MissingUsername,

    #[error("cannot write ipc state")]
    CannotWriteIpcState,

    #[error("cannot read ipc state")]
    CannotReadIpcState,
}
