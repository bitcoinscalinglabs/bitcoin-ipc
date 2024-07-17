use bitcoin::Amount;
use bitcoin_ipc::bitcoin_utils;
use bitcoin_ipc::ipc_subnet_state::IPCSubnetState;

use std::io::{self};
use std::str::FromStr;

struct L1Manager {
    state: IPCSubnetState,
}

const L1_NAME: &str = "BTC";

impl L1Manager {
    fn new(state_file: String) -> Self {
        let state = IPCSubnetState::load_state(state_file.to_string()).unwrap_or_else(|_| {
            IPCSubnetState::new(L1_NAME.to_string(), L1_NAME.to_string(), "".to_string())
        });

        L1Manager { state }
    }

    fn create_child(&self) {
        let mut name = String::new();
        let mut pk = String::new();

        println!("Enter subnet name:");
        io::stdin()
            .read_line(&mut name)
            .expect("Failed to read subnet name");

        println!("Enter public key:");
        io::stdin()
            .read_line(&mut pk)
            .expect("Failed to read public key");

        let name = name.trim();
        let pk = pk.trim();

        let mut subnet_data = String::new();
        subnet_data.push_str(bitcoin_ipc::IPC_CREATE_SUBNET_TAG);
        subnet_data.push_str(&format!(":name={}:", name));
        subnet_data.push_str(&format!("url={}/{}:", L1_NAME, name));

        let pubkey = bitcoin::secp256k1::PublicKey::from_str(pk).expect("Invalid public key");
        let subnet_address =
            bitcoin_utils::get_address_from_public_key(pubkey, bitcoin_ipc::NETWORK);

        subnet_data.push_str(&format!("pk={}", pk));

        bitcoin_ipc::ipc_lib::create_child(&subnet_address, &subnet_data)
            .expect("Failed to create child");

        println!("Transaction for create child sent successfully, please wait for confirmation");
    }

    fn join_child(&self) {
        let mut ip = String::new();
        let mut pk = String::new();
        let mut collateral = String::new();
        let mut url = String::new();

        println!("Enter IP address:");
        io::stdin()
            .read_line(&mut ip)
            .expect("Failed to read IP address");

        println!("Enter public key:");
        io::stdin()
            .read_line(&mut pk)
            .expect("Failed to read public key");

        println!("Enter collateral (in satoshis):");
        io::stdin()
            .read_line(&mut collateral)
            .expect("Failed to read collateral");

        println!("Enter subnet url:");
        io::stdin()
            .read_line(&mut url)
            .expect("Failed to read subnet url");

        let ip = ip.trim();
        let pk = pk.trim();
        let collateral: u64 = collateral
            .trim()
            .parse()
            .expect("Invalid collateral amount");
        let url = url.trim();

        let mut validator_data = String::new();
        validator_data.push_str(bitcoin_ipc::IPC_JOIN_SUBNET_TAG);
        validator_data.push_str(&format!(":ip={}:", ip));

        let pubkey = bitcoin::secp256k1::PublicKey::from_str(&pk).expect("Invalid public key");
        let subnet_address =
            bitcoin_utils::get_address_from_public_key(pubkey, bitcoin_ipc::NETWORK);

        validator_data.push_str(&format!("pk={}:", pk));
        validator_data.push_str(&format!("collateral={}:", collateral));
        validator_data.push_str(&format!("url={}", url));

        bitcoin_ipc::ipc_lib::join_child(
            &subnet_address,
            Amount::from_sat(collateral),
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
                    self.state = IPCSubnetState::load_state(self.state.file_path.clone())
                        .expect("Failed to load state");
                    self.state.print_state();
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
    let state_file: &str = &format!("{}.json", L1_NAME);

    let mut manager = L1Manager::new(state_file.to_string());
    manager.interactive_interface();
}
