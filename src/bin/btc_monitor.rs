use bitcoin::ScriptBuf;
use bitcoin_ipc::subnet_simulator::TransferEvent;
use bitcoin_ipc::{DELIMITER, IPC_DELETE_SUBNET_TAG, IPC_DEPOSIT_TAG, IPC_WITHDRAW_TAG};
use thiserror::Error;

use std::collections::{BTreeMap, BTreeSet};
use std::{str::FromStr, thread, time::Duration};

use bitcoin::{secp256k1::PublicKey, TxIn, XOnlyPublicKey};

use bitcoin::script::Instruction;
use bitcoin::Transaction;
use bitcoin_ipc::{bitcoin_utils, ipc_state::IPCState, subnet_simulator::SubnetSimulator, utils};

use bitcoincore_rpc::RpcApi;

fn concatenate_op_push_data(witness: &[u8]) -> Result<Vec<u8>, BtcMonitorError> {
    let mut concatenated_data = Vec::new();

    let script = ScriptBuf::from(witness.to_vec().clone());

    for instruction in script.instructions() {
        match instruction {
            Ok(Instruction::PushBytes(bytes)) => {
                concatenated_data.extend_from_slice(bytes.as_bytes());
            }
            Ok(Instruction::Op(op))
                if op == bitcoin::opcodes::all::OP_DROP || op == bitcoin::opcodes::OP_TRUE =>
            {
                // Do nothing, ignore these opcodes
            }
            // Return an error if any other instruction is encountered
            Ok(_) => {
                return Err(BtcMonitorError::UnsuportedOpCode);
            }
            Err(_) => {
                return Err(BtcMonitorError::ErrorParsingWitnessScript);
            }
        }
    }

    Ok(concatenated_data)
}

fn parse_create_command(
    witness_str: &str,
    tx_in: &TxIn,
) -> Result<IPCState, ParseIpcTransactionError> {
    let parts: Vec<&str> = witness_str.split(bitcoin_ipc::DELIMITER).collect();

    if parts.len() != 5 {
        return Err(ParseIpcTransactionError::InvalidWitnessFormat);
    }

    let required_number_of_validators: u64 = parts[1]
        .strip_prefix("required_number_of_validators=")
        .unwrap_or("")
        .trim()
        .parse()
        .map_err(|_| ParseIpcTransactionError::CannotParseNumberOfValidators)?;

    if required_number_of_validators == 0 {
        return Err(ParseIpcTransactionError::NumberOfValidatorsZero);
    }

    let required_collateral: u64 = parts[2]
        .strip_prefix("required_collateral=")
        .unwrap_or("")
        .trim()
        .parse()
        .map_err(|_| ParseIpcTransactionError::CollateralZero)?;

    let required_initial_funding: u64 = parts[3]
        .strip_prefix("required_initial_funding=")
        .unwrap_or("")
        .trim()
        .parse()
        .map_err(|_| ParseIpcTransactionError::Internal)?;

    let subnet_pk = match parts[4].strip_prefix("subnet_pk=") {
        Some(pk) => PublicKey::from_str(pk).map_err(|_| ParseIpcTransactionError::MissingPk)?,
        None => return Err(ParseIpcTransactionError::MissingPk),
    };

    let subnet_address = bitcoin_utils::get_address_from_x_only_public_key(
        XOnlyPublicKey::from(subnet_pk),
        bitcoin_ipc::NETWORK,
    );

    let subnet_id = format!("{}/{}", bitcoin_ipc::L1_NAME, tx_in.previous_output.txid);

    if required_collateral == 0 {
        return Err(ParseIpcTransactionError::CollateralZero);
    }

    let ipc_subnet_state = IPCState::new(
        subnet_id.to_string(),
        tx_in.previous_output.txid.to_string(),
        subnet_address.as_unchecked().clone(),
        subnet_pk,
        required_number_of_validators,
        required_collateral,
        required_initial_funding,
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

    let btc_address_str = match parts[2].strip_prefix("btc_address=") {
        Some(btc_address) => btc_address,
        None => return Err(ParseIpcTransactionError::MissingAddress),
    };

    let btc_address = match bitcoin::Address::from_str(btc_address_str) {
        Ok(a) => a,
        Err(_) => return Err(ParseIpcTransactionError::CannotParseBtcAddress),
    };

    let username = match parts[3].strip_prefix("username=") {
        Some(username) => username.to_string(),
        None => return Err(ParseIpcTransactionError::MissingUsername),
    };

    let subnet_id = match parts[4].strip_prefix("subnet_id=") {
        Some(subnet_id) => subnet_id,
        None => return Err(ParseIpcTransactionError::MissingId),
    };

    let file_name = format!("{}/ipc_state.json", subnet_id);
    let mut ipc_subnet_state = match IPCState::load_state(file_name) {
        Ok(state) => state,
        Err(_) => return Err(ParseIpcTransactionError::CannotReadIpcState),
    };

    match ipc_subnet_state.add_validator(ip.to_string(), username.clone(), btc_address) {
        Ok(_) => {}
        Err(_) => return Err(ParseIpcTransactionError::CannotWriteIpcState),
    };

    Ok(ipc_subnet_state)
}

fn parse_transfer_command(
    rpc: &bitcoincore_rpc::Client,
    wintess_str: &str,
    tx_in: &TxIn,
) -> Result<(), ParseIpcTransactionError> {
    let parts: Vec<&str> = wintess_str.split(bitcoin_ipc::DELIMITER).collect();
    let transfers_str = parts[1].strip_prefix("transfers=").unwrap_or("").trim();

    let transfers =
        match serde_json::from_str::<BTreeMap<String, BTreeSet<TransferEvent>>>(transfers_str)
            .map_err(|_| ParseIpcTransactionError::Internal)
        {
            Ok(transfers) => transfers,
            Err(_) => return Err(ParseIpcTransactionError::Internal),
        };

    let subnets = match IPCState::load_all() {
        Ok(subnets) => subnets,
        Err(_) => return Err(ParseIpcTransactionError::CannotReadIpcState),
    };

    let commit_tx_block_hash =
        match bitcoin_utils::find_block_hash_containing_txid(rpc, &tx_in.previous_output.txid) {
            Ok(hash) => hash,
            Err(_) => return Err(ParseIpcTransactionError::Internal),
        };

    let commit_tx =
        match rpc.get_raw_transaction(&tx_in.previous_output.txid, Some(&commit_tx_block_hash)) {
            Ok(tx) => tx,
            Err(_) => return Err(ParseIpcTransactionError::Internal),
        };

    for (target_subnet_id, transfers) in transfers {
        let subnet = subnets
            .iter()
            .find(|subnet| subnet.get_subnet_id() == target_subnet_id);

        let subnet_bitcoin_address = match subnet {
            Some(subnet) => subnet.get_bitcoin_address(),
            None => continue,
        };

        let total_amount = transfers
            .iter()
            .map(|transfer_event| transfer_event.a)
            .sum::<bitcoin::Amount>();

        let matching_output = commit_tx.output.iter().find(|output| {
            output.script_pubkey == subnet_bitcoin_address.script_pubkey()
                && total_amount == output.value
        });

        match matching_output {
            Some(output) => output,
            None => {
                return Err(ParseIpcTransactionError::Internal);
            }
        };

        let mut simulator = match SubnetSimulator::new(&target_subnet_id) {
            Ok(simulator) => simulator,
            Err(_) => return Err(ParseIpcTransactionError::CannotLaunchInteractor),
        };

        for transfer in transfers {
            match simulator.fund_account(&transfer.d, transfer.a.to_sat()) {
                Ok(_) => {}
                Err(_) => return Err(ParseIpcTransactionError::CannotDepositToAccount),
            };
        }
    }

    Ok(())
}

fn parse_withdraw_command(tx: &Transaction) -> Result<(), ParseIpcTransactionError> {
    for output in tx.output.iter().skip(2) {
        println!("Withdraw amount: {} --- CONFIRMED", output.value);
    }
    Ok(())
}

fn parse_deposit_command(
    tx: &Transaction,
    data: &[u8],
) -> Result<IPCState, ParseIpcTransactionError> {
    let subnets = match IPCState::load_all() {
        Ok(subnets) => subnets,
        Err(_) => return Err(ParseIpcTransactionError::CannotReadIpcState),
    };

    for subnet in subnets {
        let script_pubkey = subnet.get_bitcoin_address().script_pubkey();

        for output in tx.clone().output {
            if script_pubkey == output.script_pubkey {
                let mut simulator = match SubnetSimulator::new(&subnet.get_subnet_id()) {
                    Ok(simulator) => simulator,
                    Err(_) => return Err(ParseIpcTransactionError::CannotReadIpcState),
                };

                match simulator
                    .fund_account(&find_valid_utf8(data).to_string(), output.value.to_sat())
                {
                    Ok(_) => return Ok(subnet),
                    Err(_) => return Err(ParseIpcTransactionError::CannotDepositToAccount),
                }
            }
        }
    }
    Err(ParseIpcTransactionError::Internal)
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
        process_transaction(rpc, &tx, block_height)?;
    }

    *current_block_height = block_height;
    Ok(())
}

fn process_transaction(
    rpc: &bitcoincore_rpc::Client,
    tx: &bitcoin::Transaction,
    block_height: u64,
) -> Result<(), String> {
    println!("Checking transaction: {}", tx.compute_txid());

    for input in &tx.input {
        for witness in input.witness.iter() {
            let concatenated_data = match concatenate_op_push_data(witness) {
                Ok(data) => data,
                Err(_) => {
                    continue;
                }
            };
            let witness_str = find_valid_utf8(&concatenated_data);

            if witness_str.contains(bitcoin_ipc::IPC_CREATE_SUBNET_TAG) {
                println!(
                    "Transaction {} at block height {} contains the keyword '{:?}'",
                    tx.compute_txid(),
                    block_height,
                    bitcoin_ipc::IPC_CREATE_SUBNET_TAG
                );
                println!("Command: {}", witness_str);
                println!("Executing the CREATE command...");
                match parse_create_command(witness_str, input) {
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
            } else if witness_str.contains(bitcoin_ipc::IPC_TRANSFER_TAG) {
                println!(
                    "Transaction {} at block height {} contains the keyword '{:?}'",
                    tx.compute_txid(),
                    block_height,
                    bitcoin_ipc::IPC_TRANSFER_TAG
                );
                println!("Command: {}", witness_str);
                println!("Executing the TRANSFER command...");
                match parse_transfer_command(rpc, witness_str, input) {
                    Ok(_) => println!("TRANSFER Command successfully parsed"),
                    Err(e) => println!("TRANSFER Command could not be parsed. Error: {e}"),
                };
            }
        }
    }

    for output in &tx.output {
        let script = &output.script_pubkey;
        let mut instructions = script.instructions();
        if let Some(Ok(Instruction::Op(bitcoin::blockdata::opcodes::all::OP_RETURN))) =
            instructions.next()
        {
            if let Some(Ok(Instruction::PushBytes(data))) = instructions.next() {
                if data.len() > 32 {
                    let data_str = find_valid_utf8(data[..data.len() - 32].as_bytes());
                    if data_str.contains(bitcoin_ipc::IPC_CHECKPOINT_TAG)
                        && data_str.contains(bitcoin_ipc::DELIMITER)
                    {
                        let checkpoint = hex::encode(&data.as_bytes()[data.len() - 32..]);
                        let subnet = match find_subnet_that_signed_tx(rpc, tx) {
                            Ok(subnet) => {
                                println!("CHECKPOINT Command successfully parsed");
                                subnet
                            }
                            Err(e) => {
                                println!("CHECKPOINT Command could not be parsed. Error: {e}");
                                continue;
                            }
                        };
                        println!("Checkpoint found for subnet: {}", subnet.get_subnet_id());
                        println!("Checkpoint: {}", checkpoint);
                    }
                }

                if data.len() > IPC_DEPOSIT_TAG.len() + DELIMITER.len() {
                    let data_str =
                        find_valid_utf8(data[..IPC_DEPOSIT_TAG.len() + DELIMITER.len()].as_bytes());
                    if data_str.contains(IPC_DEPOSIT_TAG) && data_str.contains(DELIMITER) {
                        println!(
                            "Transaction {} at block height {} contains the keyword '{:?}'",
                            tx.compute_txid(),
                            block_height,
                            IPC_DEPOSIT_TAG
                        );
                        println!("Executing the DEPOSIT command...");
                        match parse_deposit_command(tx, data[data_str.len()..].as_bytes()) {
                            Ok(_) => println!("DEPOSIT Command successfully parsed"),
                            Err(e) => println!("DEPOSIT Command could not be parsed. Error: {e}"),
                        };
                    }
                }

                if data.len() > IPC_WITHDRAW_TAG.len() {
                    let data_str = find_valid_utf8(data.as_bytes());
                    if data_str.contains(bitcoin_ipc::IPC_WITHDRAW_TAG) {
                        println!(
                            "Transaction {} at block height {} contains the keyword '{:?}'",
                            tx.compute_txid(),
                            block_height,
                            bitcoin_ipc::IPC_WITHDRAW_TAG
                        );
                        println!("Executing the WITHDRAW command...");
                        match parse_withdraw_command(tx) {
                            Ok(_) => println!("WITHDRAW Command successfully parsed"),
                            Err(e) => println!("WITHDRAW Command could not be parsed. Error: {e}"),
                        };
                    }
                }

                if data.len() > IPC_DELETE_SUBNET_TAG.len() {
                    let data_str = find_valid_utf8(data.as_bytes());
                    if data_str.contains(bitcoin_ipc::IPC_DELETE_SUBNET_TAG) {
                        println!(
                            "Transaction {} at block height {} contains the keyword '{:?}'",
                            tx.compute_txid(),
                            block_height,
                            bitcoin_ipc::IPC_DELETE_SUBNET_TAG
                        );
                        println!("Executing the DELETE command...");
                        let subnet = match find_subnet_that_signed_tx(rpc, tx) {
                            Ok(subnet) => {
                                println!(
                                    "DELETE Command successfully parsed for subnet: {:?}",
                                    subnet.get_subnet_id()
                                );
                                subnet
                            }
                            Err(e) => {
                                println!("DELETE Command could not be parsed. Error: {e}");
                                continue;
                            }
                        };

                        delete_subnet_state(subnet);
                    }
                }
            }
        }
    }

    Ok(())
}

fn delete_subnet_state(subnet: IPCState) {
    match std::fs::remove_dir_all(subnet.get_subnet_id()) {
        Ok(_) => println!(
            "Deleted subnet state for subnet: {}",
            subnet.get_subnet_id()
        ),
        Err(_) => println!(
            "Failed to delete subnet state for subnet: {}",
            subnet.get_subnet_id()
        ),
    };
}

fn find_subnet_that_signed_tx(
    rpc: &bitcoincore_rpc::Client,
    tx: &bitcoin::Transaction,
) -> Result<IPCState, BtcMonitorError> {
    let subnets = match IPCState::load_all() {
        Ok(subnets) => subnets,
        Err(_) => return Err(BtcMonitorError::Internal),
    };

    for subnet in subnets {
        let public_key = XOnlyPublicKey::from(subnet.get_subnet_pk());

        match bitcoin_utils::verify_taproot_signature(rpc, tx, public_key) {
            Ok(is_valid) => {
                if is_valid {
                    return Ok(subnet);
                }
            }
            Err(_) => continue,
        }
    }
    Err(BtcMonitorError::Internal)
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

    #[error("Checkpoint processing error")]
    CheckpointError,

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    BtcCoreRpcError(#[from] bitcoincore_rpc::Error),

    #[error("unsupported opcode")]
    UnsuportedOpCode,

    #[error("error parsing witness script")]
    ErrorParsingWitnessScript,

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

    #[error("missing field subnet id")]
    MissingId,

    #[error("missing field pk")]
    MissingPk,

    #[error("missing field address")]
    MissingAddress,

    #[error("missing field ip")]
    MissingIP,

    #[error("missing field username")]
    MissingUsername,

    #[error("cannot parse btc address")]
    CannotParseBtcAddress,

    #[error("cannot write ipc state")]
    CannotWriteIpcState,

    #[error("cannot read ipc state")]
    CannotReadIpcState,

    #[error("cannot deposit to account")]
    CannotDepositToAccount,

    #[error("error")]
    Internal,
}
