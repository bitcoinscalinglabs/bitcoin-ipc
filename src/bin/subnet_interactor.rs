// subnet_interactor.rs
use bitcoin_ipc::{ipc_state::IPCState, subnet_simulator::SubnetState};
use clap::Parser;
use std::io::{self};

pub struct SubnetInteractor {
    state: SubnetState,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    url: String,
}

impl SubnetInteractor {
    pub fn new(state: SubnetState) -> Self {
        SubnetInteractor { state }
    }

    pub fn interactive_interface(&mut self) {
        loop {
            println!("Select an option:");
            println!("1. Create account");
            println!("2. Fund account");
            println!("3. Transfer funds");
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

                    println!("Enter account address:");
                    io::stdin()
                        .read_line(&mut address)
                        .expect("Failed to read account address");

                    self.state.create_account(address.trim());
                }
                "2" => {
                    let mut address = String::new();
                    let mut amount = String::new();

                    println!("Enter account address:");
                    io::stdin()
                        .read_line(&mut address)
                        .expect("Failed to read account address");

                    println!("Enter amount to add:");
                    io::stdin()
                        .read_line(&mut amount)
                        .expect("Failed to read amount");

                    let amount: u64 = amount.trim().parse().expect("Invalid balance amount");

                    self.state.fund_account(address.trim(), amount);
                }
                "3" => {
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

    let ipc_state = IPCState::load_state(args.url + "/" + subnet_name + ".json")?;

    if !ipc_state.has_required_validators() {
        println!("Not enough validators to interact with subnet");
        return Ok(());
    }

    let subnet_state = SubnetState::new();

    let mut interactor = SubnetInteractor::new(subnet_state);

    interactor.interactive_interface();
    Ok(())
}
