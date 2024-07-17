// subnet_interactor.rs
use bitcoin_ipc::ipc_subnet_state::IPCSubnetState;
use clap::Parser;
use std::io::{self};

pub struct SubnetInteractor {
    state: IPCSubnetState,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    url: String,
}

impl SubnetInteractor {
    pub fn new(state: IPCSubnetState) -> Self {
        SubnetInteractor { state }
    }

    pub fn interactive_interface(&mut self) {
        loop {
            println!("Select an option:");
            println!("1. Create account");
            println!("2. Transfer funds");
            println!("3. Add child subnet");
            println!("4. Checkpoint state");
            println!("5. Print state");
            println!("6. Exit");

            let mut choice = String::new();
            io::stdin()
                .read_line(&mut choice)
                .expect("Failed to read line");
            match choice.trim() {
                "1" => {
                    let mut address = String::new();
                    let mut initial_balance = String::new();

                    println!("Enter account address:");
                    io::stdin()
                        .read_line(&mut address)
                        .expect("Failed to read account address");

                    println!("Enter initial balance:");
                    io::stdin()
                        .read_line(&mut initial_balance)
                        .expect("Failed to read initial balance");

                    let initial_balance: u64 = initial_balance
                        .trim()
                        .parse()
                        .expect("Invalid balance amount");

                    self.state.create_account(address.trim(), initial_balance);
                }
                "2" => {
                    let mut from = String::new();
                    let mut to = String::new();
                    let mut amount = String::new();

                    println!("Enter from account address:");
                    io::stdin()
                        .read_line(&mut from)
                        .expect("Failed to read from account address");

                    println!("Enter to account address:");
                    io::stdin()
                        .read_line(&mut to)
                        .expect("Failed to read to account address");

                    println!("Enter amount:");
                    io::stdin()
                        .read_line(&mut amount)
                        .expect("Failed to read amount");

                    let amount: u64 = amount.trim().parse().expect("Invalid amount");

                    match self.state.transfer(from.trim(), to.trim(), amount) {
                        Ok(_) => {}
                        Err(e) => println!("Transfer failed: {}", e),
                    }
                }
                "3" => {
                    let mut child_subnet_name = String::new();

                    println!("Enter child subnet name:");
                    io::stdin()
                        .read_line(&mut child_subnet_name)
                        .expect("Failed to read child subnet name");

                    self.state.add_child_subnet(child_subnet_name.trim());
                }
                "4" => {
                    let checkpoint = self.state.get_checkpoint();
                    println!("Checkpoint: {:?}", checkpoint);
                }
                "5" => self.state.print_state(),
                "6" => break,
                _ => println!("Invalid option. Please try again."),
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let clone = args.url.clone();

    let subnet_name = clone.split('/').last().unwrap_or("");

    let subnet_state = IPCSubnetState::load_state(args.url + "/" + subnet_name + ".json")?;

    let mut interactor = SubnetInteractor::new(subnet_state);

    interactor.interactive_interface();
    Ok(())
}
