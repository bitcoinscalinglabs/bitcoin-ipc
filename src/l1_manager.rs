use thiserror::Error;

use crate::bitcoin_utils;
use crate::ipc_state::IPCState;
use bitcoin::{Amount, Txid};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use std::fs::OpenOptions;
use std::io::{self, Write};

pub struct L1Manager {
    subnets: Vec<IPCState>,
}

impl L1Manager {
    pub fn new() -> Result<Self, L1ManagerError> {
        let subnets: Vec<IPCState> = IPCState::load_all()?;

        Ok(L1Manager { subnets })
    }

    pub fn update_and_get_subnets(&mut self) -> Result<Vec<IPCState>, L1ManagerError> {
        self.subnets = IPCState::load_all()?;
        Ok(self.subnets.clone())
    }

    fn store_keypair(
        &self,
        keypair: &bitcoin::secp256k1::Keypair,
        subnet_id: &str,
    ) -> Result<(), L1ManagerError> {
        let serialized = serde_json::to_string(&keypair).unwrap_or_else(|_| {
            println!("Failed to serialize keypair");
            "".to_string()
        });

        let file_path = &format!("{}/keypair.yaml", subnet_id);
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

    pub fn parse_create_child_args() -> Result<CreateChildArgs, L1ManagerError> {
        let required_number_of_validators = get_user_input("Enter required number of validators:")?;
        let required_number_of_validators: u64 =
            required_number_of_validators.parse().map_err(|_| {
                L1ManagerError::InvalidUserInput {
                    field: "number of validators",
                }
            })?;

        let required_collateral = get_user_input(
            "Enter required collateral (in satoshis - should be greater than 1000 satoshis):",
        )?;
        let required_collateral: u64 =
            required_collateral
                .parse()
                .map_err(|_| L1ManagerError::InvalidUserInput {
                    field: "collateral amount",
                })?;

        if required_collateral < 1000 {
            return Err(L1ManagerError::InvalidUserInputWithError {
                field: "collateral amount",
                error: "Amount too low. Amount must be at least 1000 satoshis",
            });
        }

        let mut answer = get_user_input(
            format!(
                "Are you sure you want to create a child where validators will lock {:?} BTC as collateral? (press Enter to confirm/any other key+Enter to cancel)",
                Amount::from_sat(required_collateral).to_btc(),
            )
            .as_str(),
        )?;

        if !answer.is_empty() {
            return Err(L1ManagerError::InvalidUserInput {
                field: "Confiramtion",
            });
        }

        let required_initial_funding = get_user_input(
            "Enter required initial subnet funding that each validator will contribute (in satoshis - should be between 1 and 50 BTC for usability reasons):",
        )?;

        let required_initial_funding: u64 =
            required_initial_funding
                .parse()
                .map_err(|_| L1ManagerError::InvalidUserInput {
                    field: "funding amount",
                })?;

        if !(100_000_000..5_000_000_000).contains(&required_initial_funding) {
            return Err(L1ManagerError::InvalidUserInputWithError {
                field: "funding amount",
                error: "Amount not in specified range. Amount should be between 1 and 50 BTC",
            });
        }

        answer = get_user_input(
            format!(
                "Are you sure you want to create a child with {:?} BTC required initial funding? (press Enter to confirm/any other key+Enter to cancel)",
                Amount::from_sat(required_initial_funding).to_btc(),
            )
            .as_str(),
        )?;

        if !answer.is_empty() {
            return Err(L1ManagerError::InvalidUserInput {
                field: "Confiramtion",
            });
        }

        Ok(CreateChildArgs {
            required_number_of_validators,
            required_collateral,
            required_initial_funding,
        })
    }

    pub fn create_child(&self, args: CreateChildArgs) -> Result<(), L1ManagerError> {
        if args.required_collateral < 1000 {
            return Err(L1ManagerError::InvalidUserInput {
                field: "amount must be at least 1000 satoshis",
            });
        }

        let mut subnet_data = String::new();
        subnet_data.push_str(crate::IPC_CREATE_SUBNET_TAG);

        let seed = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(30)
            .map(char::from)
            .collect();

        let key_pair = bitcoin_utils::generate_keypair(seed)?;

        let subnet_address = bitcoin_utils::get_address_from_x_only_public_key(
            key_pair.x_only_public_key().0,
            crate::NETWORK,
        );

        subnet_data.push_str(&format!(
            "{}required_number_of_validators={}{}",
            crate::DELIMITER,
            args.required_number_of_validators,
            crate::DELIMITER
        ));
        subnet_data.push_str(&format!(
            "required_collateral={}{}",
            args.required_collateral,
            crate::DELIMITER
        ));

        subnet_data.push_str(&format!(
            "required_initial_funding={}{}",
            args.required_initial_funding,
            crate::DELIMITER
        ));

        subnet_data.push_str(&format!("subnet_pk={}", key_pair.public_key()));

        let (commit_tx, _) =
            crate::ipc_lib::create_and_submit_create_child_tx(&subnet_address, &subnet_data)?;

        let commit_tx_id: Txid = commit_tx.compute_txid();

        let subnet_id = format!("{}/{}", crate::L1_NAME, commit_tx_id);

        self.store_keypair(&key_pair, &subnet_id)?;

        Ok(())
    }

    fn choose_subnet(&self) -> Result<IPCState, L1ManagerError> {
        let subnets = IPCState::load_all()?;

        if subnets.is_empty() {
            return Err(L1ManagerError::NoSubnetAvailable);
        }

        let mut prompt: String = format!("Select a subnet (between 1 and {}):\n", subnets.len());

        for (i, subnet) in subnets.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, subnet.get_subnet_id()));
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

    pub fn parse_join_child_args(&self) -> Result<JoinChildArgs, L1ManagerError> {
        let ipc_state = self.choose_subnet()?;

        let ip = get_user_input("Enter validator's IP address:")?;
        let btc_address = get_user_input("Enter validator's btc address:")?;
        let username = get_user_input("Enter validator's name:")?;

        let answer = get_user_input(format!("Are you sure that you want to join this subnet? You need to lock {:?} BTC as collateral and contribute {:?} BTC as initial funding. (press Enter to confirm/any other key+Enter to cancel)",
            Amount::from_sat(ipc_state.get_required_collateral()).to_btc(),
            Amount::from_sat(ipc_state.get_required_initial_funding()).to_btc(),
        ).as_str())?;

        if !answer.is_empty() {
            return Err(L1ManagerError::InvalidUserInput {
                field: "Confiramtion",
            });
        }

        Ok(JoinChildArgs {
            ip,
            btc_address,
            username,
            ipc_state,
        })
    }

    pub fn join_child(&self, args: JoinChildArgs) -> Result<(), L1ManagerError> {
        let mut validator_data = String::new();
        validator_data.push_str(crate::IPC_JOIN_SUBNET_TAG);
        validator_data.push_str(&format!(
            "{}ip={}{}",
            crate::DELIMITER,
            args.ip,
            crate::DELIMITER
        ));

        validator_data.push_str(&format!(
            "btc_address={}{}",
            args.btc_address,
            crate::DELIMITER
        ));
        validator_data.push_str(&format!("username={}{}", args.username, crate::DELIMITER));
        validator_data.push_str(&format!("subnet_id={}", args.ipc_state.get_subnet_id()));

        let subnet_bitcoin_address = &args.ipc_state.get_bitcoin_address();
        crate::ipc_lib::create_and_submit_join_child_tx(
            subnet_bitcoin_address,
            Amount::from_sat(args.ipc_state.get_required_collateral()),
            Amount::from_sat(args.ipc_state.get_required_initial_funding()),
            &validator_data,
        )?;

        Ok(())
    }

    pub fn deposit(&self) -> Result<(), L1ManagerError> {
        let ipc_state = self.choose_subnet()?;

        let amount = get_user_input(
            "Enter amount to deposit (in satoshis - must be more than 1000 satoshis):",
        )?;

        let amount: u64 = amount
            .parse()
            .map_err(|_| L1ManagerError::InvalidUserInput { field: "amount" })?;

        if amount < 1000 {
            return Err(L1ManagerError::InvalidUserInputWithError {
                field: "amount",
                error: "Amount too low. Amount must be at least 1000 satoshis",
            });
        }

        let answer = get_user_input(
            format!(
                "Are you sure you want to deposit {:?} BTC to the subnet? (press Enter to confirm/any other key+Enter to cancel)",
                Amount::from_sat(amount).to_btc()
            )
            .as_str(),
        )?;

        if !answer.is_empty() {
            return Err(L1ManagerError::InvalidUserInput {
                field: "Confiramtion",
            });
        }

        let subnet_bitcoin_address = &ipc_state.get_bitcoin_address();

        let target_address = get_user_input("Enter target address:")?;

        crate::ipc_lib::create_and_submit_deposit_tx(
            subnet_bitcoin_address,
            Amount::from_sat(amount),
            &target_address,
        )?;

        Ok(())
    }
}

pub fn get_user_input(prompt: &str) -> Result<String, L1ManagerError> {
    println!("{prompt}");
    let mut input = String::new();
    match io::stdin().read_line(&mut input) {
        Ok(_) => Ok(input.trim().to_string()),
        Err(e) => Err(e.into()),
    }
}

pub struct CreateChildArgs {
    pub required_number_of_validators: u64,
    pub required_collateral: u64,
    pub required_initial_funding: u64,
}

pub struct JoinChildArgs {
    pub ip: String,
    pub btc_address: String,
    pub username: String,
    pub ipc_state: IPCState,
}

#[derive(Error, Debug)]
pub enum L1ManagerError {
    #[error("could not parse user input")]
    CannotReadUserInput(#[from] std::io::Error),

    #[error("invalid user input: {field}")]
    InvalidUserInput { field: &'static str },

    #[error("invalid user input: {field}, error: {error}")]
    InvalidUserInputWithError {
        field: &'static str,
        error: &'static str,
    },

    #[error("no child subnet is available")]
    NoSubnetAvailable,

    #[error(transparent)]
    IpcLibError(#[from] crate::ipc_lib::IpcLibError),

    #[error(transparent)]
    IpcStateError(#[from] crate::ipc_state::IpcStateError),

    #[error(transparent)]
    BitcoinUtilsError(#[from] crate::bitcoin_utils::BitcoinUtilsError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),
}
