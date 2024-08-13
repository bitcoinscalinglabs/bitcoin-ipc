use thiserror::Error;

use bitcoin::Amount;
use bitcoin_ipc::bitcoin_utils;
use bitcoin_ipc::ipc_state::IPCState;

use std::io::{self};
use std::str::FromStr;

struct L1Manager {
    subnets: Vec<IPCState>,
}

impl L1Manager {
    fn new() -> Self {
        let subnets: Vec<IPCState> = IPCState::load_all().unwrap_or_else(|_| Vec::new());

        L1Manager { subnets }
    }

    fn create_child(&self) -> Result<(), L1ManagerError> {
        let name = get_user_input("Enter subnet name:")?;

        let pubkey_str = get_user_input("Enter subnet's public key:")?;
        let pubkey = bitcoin::secp256k1::PublicKey::from_str(&pubkey_str).map_err(|_| {
            L1ManagerError::InvalidUserInput {
                field: "public key",
            }
        })?;

        let required_number_of_validators = get_user_input("Enter required number of validators:")?;
        let required_number_of_validators: u64 =
            required_number_of_validators.parse().map_err(|_| {
                L1ManagerError::InvalidUserInput {
                    field: "number of validators",
                }
            })?;

        let required_collateral = get_user_input("Enter required collateral (in satoshis):")?;
        let required_collateral: u64 =
            required_collateral
                .parse()
                .map_err(|_| L1ManagerError::InvalidUserInput {
                    field: "collateral amount",
                })?;

        let mut subnet_data = String::new();
        subnet_data.push_str(bitcoin_ipc::IPC_CREATE_SUBNET_TAG);
        subnet_data.push_str(&format!(
            "{}name={}{}",
            bitcoin_ipc::DELIMITER,
            name,
            bitcoin_ipc::DELIMITER
        ));
        let subnet_address =
            bitcoin_utils::get_address_from_public_key(pubkey, bitcoin_ipc::NETWORK);
        subnet_data.push_str(&format!("pk={}{}", pubkey_str, bitcoin_ipc::DELIMITER));
        subnet_data.push_str(&format!(
            "required_number_of_validators={}{}",
            required_number_of_validators,
            bitcoin_ipc::DELIMITER
        ));
        subnet_data.push_str(&format!("required_collateral={}", required_collateral));

        bitcoin_ipc::ipc_lib::create_and_submit_create_child_tx(&subnet_address, &subnet_data)?;

        Ok(())
    }

    fn join_child(&self) -> Result<(), L1ManagerError> {
        let subnets = IPCState::load_all()?;

        let available_subnets: Vec<&IPCState> = subnets
            .iter()
            .filter(|subnet| !subnet.has_required_validators())
            .collect();

        if available_subnets.is_empty() {
            return Err(L1ManagerError::NoSubnetAvailable);
        }

        available_subnets
            .iter()
            .enumerate()
            .for_each(|(index, subnet)| println!("{}. {}", index + 1, subnet.get_name()));

        let choice = get_user_input(&format!(
            "Pick a subnet (between 1 and  {}) to join: ",
            available_subnets.len()
        ))?;
        let choice: usize = choice
            .parse()
            .map_err(|_| L1ManagerError::InvalidUserInput {
                field: "invalid choice",
            })?;
        if choice < 1 || choice > subnets.len() {
            return Err(L1ManagerError::InvalidUserInput {
                field: "invalid choice",
            });
        }

        let ipc_state = &subnets[choice - 1];

        let ip = get_user_input("Enter validator's IP address:")?;
        let pk = get_user_input("Enter validator's public key:")?;
        let username = get_user_input("Enter validator's name:")?;

        let mut validator_data = String::new();
        validator_data.push_str(bitcoin_ipc::IPC_JOIN_SUBNET_TAG);
        validator_data.push_str(&format!(
            "{}ip={}{}",
            bitcoin_ipc::DELIMITER,
            ip,
            bitcoin_ipc::DELIMITER
        ));
        validator_data.push_str(&format!("pk={}{}", pk, bitcoin_ipc::DELIMITER));
        validator_data.push_str(&format!("username={}{}", username, bitcoin_ipc::DELIMITER));
        validator_data.push_str(&format!("subnet_name={}", ipc_state.get_name()));

        let subnet_address = &ipc_state.get_subnet_address()?;
        bitcoin_ipc::ipc_lib::create_and_submit_join_child_tx(
            subnet_address,
            Amount::from_sat(ipc_state.get_required_collateral()),
            &validator_data,
        )?;

        Ok(())
    }

    fn interactive_interface(&mut self) {
        let prompt = "Select an option:\n\
            1. Read state\n\
            2. Create child\n\
            3. Join child\n\
            4. Exit";

        loop {
            let choice = match get_user_input(prompt) {
                Ok(c) => c,
                Err(_) => {
                    println!("Invalid option. Please try again.");
                    continue;
                }
            };
            let choice: usize = match choice.parse() {
                Ok(c) => c,
                Err(_) => {
                    println!("Invalid option. Please try again.");
                    continue;
                }
            };

            match choice {
                1 => {
                    match IPCState::load_all() {
                        Ok(subnets) => {
                            subnets
                                .iter()
                                .for_each(|subnet| subnet.clone().print_state());
                            self.subnets = subnets;
                        }
                        Err(_) => {
                            println!("An error occured while reading the state.");
                        }
                    };
                }

                2 => match self.create_child() {
                    Ok(_) => {
                        println!("Transaction to create a child subnet has been submited to bitcoin, please wait for confirmation.");
                    }
                    Err(e) => {
                        println!("An error occured, child subnet was not created. Error: {e}");
                    }
                },

                3 => match self.join_child() {
                    Ok(_) => {
                        println!("Transaction to join a child subnet has been submited to bitcoin, please wait for confirmation.");
                    }
                    Err(e) => {
                        println!("An error occured, child subnet was not joined. Error: {e}");
                    }
                },

                4 => break,

                _ => println!("Invalid option. Please try again."),
            }
        }
    }
}

fn get_user_input(prompt: &str) -> Result<String, L1ManagerError> {
    println!("{prompt}");
    let mut input = String::new();
    match io::stdin().read_line(&mut input) {
        Ok(_) => Ok(input.trim().to_string()),
        Err(e) => return Err(e.into()),
    }
}

#[derive(Error, Debug)]
pub enum L1ManagerError {
    #[error("could not parse user input")]
    CannotReadUserInput(#[from] std::io::Error),

    #[error("invalid user input: {field}")]
    InvalidUserInput { field: &'static str },

    #[error("no child subnet is available")]
    NoSubnetAvailable,

    #[error(transparent)]
    IpcLibError(#[from] bitcoin_ipc::ipc_lib::IpcLibError),

    #[error(transparent)]
    IpcStateError(#[from] bitcoin_ipc::ipc_state::IpcStateError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),
}

fn main() {
    L1Manager::new().interactive_interface();
}
