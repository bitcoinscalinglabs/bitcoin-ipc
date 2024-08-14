use thiserror::Error;

use bitcoin::Amount;
use bitcoin_ipc::bitcoin_utils;
use bitcoin_ipc::ipc_state::IPCState;
use std::fs::OpenOptions;
use std::io::{self, Write};

struct L1Manager {
    subnets: Vec<IPCState>,
}

impl L1Manager {
    fn new() -> Self {
        let subnets: Vec<IPCState> = IPCState::load_all().unwrap_or_else(|_| Vec::new());

        L1Manager { subnets }
    }

    fn store_keypair(
        &self,
        keypair: &bitcoin::secp256k1::Keypair,
        name: &str,
    ) -> Result<(), L1ManagerError> {
        let serialized = serde_json::to_string(&keypair).unwrap_or_else(|_| {
            println!("Failed to serialize keypair");
            "".to_string()
        });

        let file_path = &format!("{}/{}/keypair.json", bitcoin_ipc::L1_NAME, name);
        let path = std::path::Path::new(file_path);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(file_path)?;

        file.write_all(serialized.as_bytes())?;

        Ok(())
    }

    // fn create_state_file(name: &str) -> Result<(), L1ManagerError> {
    //     let state = SubnetState::new();
    //     let serialized = serde_json::to_string(state).unwrap_or_else(|_| {
    //         println!("Failed to serialize keypair");
    //         "".to_string()
    //     });

    //     let file_path = &format!("{}/{}/keypair.json", bitcoin_ipc::L1_NAME, name);
    //     let path = std::path::Path::new(file_path);

    //     if let Some(parent) = path.parent() {
    //         std::fs::create_dir_all(parent)?;
    //     }

    //     let mut file = OpenOptions::new()
    //         .write(true)
    //         .create(true)
    //         .truncate(true)
    //         .open(file_path)?;

    //     file.write_all(serialized.as_bytes())?;

    //     Ok(())
    // }

    fn create_child(&self) -> Result<(), L1ManagerError> {
        let name = get_user_input("Enter subnet name:")?;
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

        let key_pair = bitcoin_utils::generate_keypair(name.to_string())?;

        self.store_keypair(&key_pair, &name)?;

        let subnet_address = bitcoin_utils::get_address_from_private_key(
            key_pair.secret_key(),
            bitcoin_ipc::NETWORK,
        );

        subnet_data.push_str(&format!(
            "subnet_address={}{}",
            subnet_address.to_string(),
            bitcoin_ipc::DELIMITER
        ));

        subnet_data.push_str(&format!(
            "required_number_of_validators={}{}",
            required_number_of_validators,
            bitcoin_ipc::DELIMITER
        ));
        subnet_data.push_str(&format!("required_collateral={}", required_collateral));

        bitcoin_ipc::ipc_lib::create_and_submit_create_child_tx(&subnet_address, &subnet_data)?;

        Ok(())
    }

    fn choose_subnet(&self) -> Result<IPCState, L1ManagerError> {
        let subnets = IPCState::load_all()?;

        if subnets.len() == 0 {
            return Err(L1ManagerError::NoSubnetAvailable);
        }

        let mut prompt: String = format!(
            "Select a subnet (between 1 and {}) to deposit funds:\n",
            subnets.len()
        );

        for (i, subnet) in subnets.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, subnet.get_name()));
        }

        let choice = get_user_input(&prompt)?;

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

        Ok(subnets[choice - 1].clone())
    }

    fn join_child(&self) -> Result<(), L1ManagerError> {
        let ipc_state = self.choose_subnet()?;

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

    fn deposit(&self) -> Result<(), L1ManagerError> {
        let ipc_state = self.choose_subnet()?;

        let amount = get_user_input("Enter amount to deposit (in satoshis):")?;

        let amount: u64 = amount
            .parse()
            .map_err(|_| L1ManagerError::InvalidUserInput { field: "amount" })?;

        if amount < 200 {
            return Err(L1ManagerError::InvalidUserInput {
                field: "amount must be at least 200 satoshis",
            });
        }

        let subnet_address = &ipc_state.get_subnet_address()?;

        let target_address = get_user_input("Enter target address:")?;

        let mut deposit_data = String::new();
        deposit_data.push_str(bitcoin_ipc::IPC_DEPOSIT_TAG);
        deposit_data.push_str(&format!(
            "{}amount={}{}",
            bitcoin_ipc::DELIMITER,
            amount,
            bitcoin_ipc::DELIMITER
        ));
        deposit_data.push_str(&format!(
            "subnet_name={}{}",
            ipc_state.get_name(),
            bitcoin_ipc::DELIMITER
        ));
        deposit_data.push_str(&format!("target_address={}", target_address));

        bitcoin_ipc::ipc_lib::create_and_submit_deposit_tx(
            subnet_address,
            Amount::from_sat(amount),
            &deposit_data,
        )?;

        Ok(())
    }

    fn interactive_interface(&mut self) {
        let prompt = "Select an option:\n\
            1. Read state\n\
            2. Create child\n\
            3. Join child\n\
            4. Deposit\n\
            5. Exit";

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

                4 => match self.deposit() {
                    Ok(_) => {
                        println!("Transaction to deposit funds has been submited to bitcoin, please wait for confirmation.");
                    }
                    Err(e) => {
                        println!("An error occured, funds were not deposited. Error: {e}");
                    }
                },

                5 => break,

                _ => println!("Invalid option. Please try again."),
            }
            println!("===============")
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
    BitcoinUtilsError(#[from] bitcoin_ipc::bitcoin_utils::BitcoinUtilsError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),
}

fn main() {
    L1Manager::new().interactive_interface();
}
