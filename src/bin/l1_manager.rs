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

    fn store_keypair(&self, keypair: &bitcoin::secp256k1::Keypair, name: &str) {
        let serialized = serde_json::to_string(&keypair).unwrap_or_else(|_| {
            println!("Failed to serialize keypair");
            "".to_string()
        });

        let file_path = &format!("{}/{}/keypair.yaml", bitcoin_ipc::L1_NAME, name);
        let path = std::path::Path::new(file_path);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("Failed to create directories");
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(file_path)
            .expect("Failed to open state file");

        file.write_all(serialized.as_bytes())
            .expect("Failed to write state to file");
    }

    fn create_child(&self) {
        let mut name = String::new();
        let mut required_number_of_validators = String::new();
        let mut required_collateral = String::new();

        println!("Enter subnet name:");
        io::stdin()
            .read_line(&mut name)
            .expect("Failed to read subnet name");

        println!("Enter required number of validators:");
        io::stdin()
            .read_line(&mut required_number_of_validators)
            .expect("Failed to read required number of validators");

        println!("Enter required collateral (in satoshis):");
        io::stdin()
            .read_line(&mut required_collateral)
            .expect("Failed to read required collateral");

        let name = name.trim();
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

        let key_pair = bitcoin_utils::generate_keypair(name.to_string());

        self.store_keypair(&key_pair, name);

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

        println!("Pick a subnet to join: ");

        subnets
            .iter()
            .enumerate()
            .for_each(|(index, subnet)| println!("{}. {}", index + 1, subnet.get_name()));

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

fn main() {
    L1Manager::new().interactive_interface();
}
