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

        println!("Enter subnet name:");
        io::stdin()
            .read_line(&mut name)
            .expect("Failed to read subnet name");

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
        subnet_data.push_str(&format!(":name={}:", name));

        let pubkey = bitcoin::secp256k1::PublicKey::from_str(pk).expect("Invalid public key");
        let subnet_address =
            bitcoin_utils::get_address_from_public_key(pubkey, bitcoin_ipc::NETWORK);

        subnet_data.push_str(&format!("pk={}:", pk));
        subnet_data.push_str(&format!(
            "required_number_of_validators={}:",
            required_number_of_validators
        ));
        subnet_data.push_str(&format!("required_collateral={}", required_collateral));

        bitcoin_ipc::ipc_lib::create_child(&subnet_address, &subnet_data)
            .expect("Failed to create child");

        println!("Transaction for create child sent successfully, please wait for confirmation");
    }

    fn join_child(&self) {
        let mut ip = String::new();
        let mut pk = String::new();
        let mut name = String::new();

        println!("Enter IP address:");
        io::stdin()
            .read_line(&mut ip)
            .expect("Failed to read IP address");

        println!("Enter public key:");
        io::stdin()
            .read_line(&mut pk)
            .expect("Failed to read public key");

        println!("Enter subnet name:");
        io::stdin()
            .read_line(&mut name)
            .expect("Failed to read subnet name");

        let ip = ip.trim();
        let pk = pk.trim();
        let name = name.trim();

        let mut validator_data = String::new();
        validator_data.push_str(bitcoin_ipc::IPC_JOIN_SUBNET_TAG);
        validator_data.push_str(&format!(":ip={}:", ip));

        let pubkey = bitcoin::secp256k1::PublicKey::from_str(&pk).expect("Invalid public key");
        let subnet_address =
            bitcoin_utils::get_address_from_public_key(pubkey, bitcoin_ipc::NETWORK);

        validator_data.push_str(&format!("pk={}:", pk));
        validator_data.push_str(&format!("name={}", name));

        let ipc_state = IPCState::load_state(format!(
            "{}/{}/{}.json",
            bitcoin_ipc::L1_NAME,
            name.to_string(),
            name.to_string()
        ))
        .expect("Failed to load state");

        println!("{}", validator_data);

        bitcoin_ipc::ipc_lib::join_child(
            &subnet_address,
            Amount::from_sat(ipc_state.get_required_collateral()),
            &validator_data,
        )
        .expect("Failed to join child");

        println!("Transaction for join child sent successfully, please wait for confirmation");
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

fn main() {
    L1Manager::new().interactive_interface();
}
