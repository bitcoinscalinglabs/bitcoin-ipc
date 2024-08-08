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

    fn create_child(&self) {
        let mut name = String::new();
        let mut pk = String::new();
        let mut required_number_of_validators = String::new();
        let mut required_collateral = String::new();

        get_user_input(&mut name);

        println!("Enter public key:");
        io::stdin()
            .read_line(&mut pk)
            .expect("Failed to read public key");

        println!("Enter required number of validators:");
        io::stdin()
            .read_line(&mut required_number_of_validators)
            .expect("Failed to read required number of validators");

        println!("Enter required collateral (in satoshis):");
        io::stdin()
            .read_line(&mut required_collateral)
            .expect("Failed to read required collateral");

        let name = name.trim();
        let pk = pk.trim();
        let required_number_of_validators: u64 = required_number_of_validators
            .trim()
            .parse()
            .expect("Invalid number of validators");
        let required_collateral: u64 = required_collateral
            .trim()
            .parse()
            .expect("Invalid collateral amount");

        let mut subnet_data = String::new();
        subnet_data.push_str(bitcoin_ipc::IPC_CREATE_SUBNET_TAG);
        subnet_data.push_str(&format!(
            "{}name={}{}",
            bitcoin_ipc::DELIMITER,
            name,
            bitcoin_ipc::DELIMITER
        ));

        let pubkey = bitcoin::secp256k1::PublicKey::from_str(pk).expect("Invalid public key");
        let subnet_address =
            bitcoin_utils::get_address_from_public_key(pubkey, bitcoin_ipc::NETWORK);

        subnet_data.push_str(&format!("pk={}{}", pk, bitcoin_ipc::DELIMITER));
        subnet_data.push_str(&format!(
            "required_number_of_validators={}{}",
            required_number_of_validators,
            bitcoin_ipc::DELIMITER
        ));
        subnet_data.push_str(&format!("required_collateral={}", required_collateral));

        bitcoin_ipc::ipc_lib::create_child(&subnet_address, &subnet_data)
            .expect("Failed to create child");

        println!(
            "Transaction to create a child subnet has been submited to bitcoin, please wait for confirmation."
        );
    }

    fn join_child(&self) {
        let mut ip = String::new();
        let mut pk = String::new();
        let mut username = String::new();

        let subnets = IPCState::load_all().expect("Failed to load subnets");

        let available_subnets: Vec<&IPCState> = subnets
            .iter()
            .filter(|subnet| !subnet.has_required_validators())
            .collect();

        if available_subnets.is_empty() {
            println!("No subnets exist or all subnets have the required number of validators");
            return;
        }

        println!("Pick a subnet to join: ");

        available_subnets
            .iter()
            .enumerate()
            .for_each(|(index, subnet)| println!("{}. {}", index + 1, subnet.get_name()));

        println!("Subnets len {}", available_subnets.len());
        println!("isempty {}", available_subnets.is_empty());

        let mut choice = String::new();

        io::stdin()
            .read_line(&mut choice)
            .expect("Failed to read choice");

        let choice: usize = choice.trim().parse().expect("Invalid choice");

        if choice < 1 || choice > subnets.len() {
            println!("Invalid choice");
            return;
        }

        let ipc_state = &subnets[choice - 1];

        println!("Enter your IP address:");
        io::stdin()
            .read_line(&mut ip)
            .expect("Failed to read IP address");

        println!("Enter your public key:");
        io::stdin()
            .read_line(&mut pk)
            .expect("Failed to read public key");

        println!("Enter your username:");
        io::stdin()
            .read_line(&mut username)
            .expect("Failed to read username");

        let ip = ip.trim();
        let pk = pk.trim();
        let username = username.trim();

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

        bitcoin_ipc::ipc_lib::join_child(
            &ipc_state.get_subnet_address(),
            Amount::from_sat(ipc_state.get_required_collateral()),
            &validator_data,
        )
        .expect("Failed to join child");

        println!("Transaction to join a child subnet has been submited to bitcoin, please wait for confirmation.");
    }

    fn interactive_interface(&mut self) {
        loop {
            println!("Select an option:");
            println!("1. Read state");
            println!("2. Create child");
            println!("3. Join child");
            println!("4. Exit");

            let mut choice = String::new();
            io::stdin()
                .read_line(&mut choice)
                .expect("Failed to read line");
            match choice.trim() {
                "1" => {
                    self.subnets = IPCState::load_all().expect("Failed to load subnets");

                    self.subnets
                        .iter()
                        .for_each(|subnet| subnet.clone().print_state());
                }
                "2" => self.create_child(),
                "3" => self.join_child(),
                "4" => break,
                _ => println!("Invalid option. Please try again."),
            }
        }
    }
}

fn get_user_input(prompt: &String) -> Option<String> {
    println!("{}", prompt);
    let mut input = String::new();
    match io::stdin().read_line(&mut input) {
        Ok(_) => Some(input),
        Err(_) => None,
    }
}

fn main() {
    L1Manager::new().interactive_interface();
}
