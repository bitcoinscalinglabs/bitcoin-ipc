// subnet_interactor.rs
use thiserror::Error;

use bitcoin_ipc::{ipc_state::IPCState, subnet_simulator::SubnetSimulator};
use clap::Parser;
use std::io::{self};

/// A SubnetInteractor is responsible for interacting with the given subnet,
/// enabling the user of the subnet to use the functionality provided by the subnet.
///
/// This implementation uses a SubnetSimulator, instead of a distributed subnet.
/// Hence, the SubnetInteractor simply calls the interface of the SubnetSimulator.
/// It is implemented as a wrapper around a SubnetSimulator object.
///
/// In an implementation with a real distributed subnet, the SubnetInteractor
/// must know how to contact each subnet validator.
pub struct SubnetInteractor {
    subnet: SubnetSimulator,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    subnet_name: String,
}

impl SubnetInteractor {
    pub fn new(subnet_sim: SubnetSimulator) -> Self {
        println!(
            "Starting a Subnet Interactor for subnet {}",
            subnet_sim.subnet_name
        );
        SubnetInteractor { subnet: subnet_sim }
    }

    pub fn interactive_interface(&mut self) {
        loop {
            let prompt = "Select an option:\n\
                                1. Create account\n\
                                2. Fund account\n\
                                3. Transfer funds\n\
                                4. Checkpoint state\n\
                                5. Print state\n\
                                6. Exit";

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
                    let address = match get_user_input("Enter account address:") {
                        Ok(a) => a,
                        Err(e) => {
                            println!("Failed to read account address: {}", e);
                            continue;
                        }
                    };

                    match self.subnet.create_account(&address) {
                        Ok(_) => {}
                        Err(e) => println!("Account creation failed: {}", e),
                    }
                }
                2 => {
                    let address = match get_user_input("Enter account address:") {
                        Ok(a) => a,
                        Err(e) => {
                            println!("Failed to read account address: {}", e);
                            continue;
                        }
                    };
                    let amount = match get_user_input("Enter amount to add:") {
                        Ok(a) => a,
                        Err(e) => {
                            println!("Failed to read amount: {}", e);
                            continue;
                        }
                    };

                    let amount: u64 = match amount.parse() {
                        Ok(a) => a,
                        Err(e) => {
                            println!("Invalid balance amount: {}", e);
                            continue;
                        }
                    };

                    match self.subnet.fund_account(&address, amount) {
                        Ok(_) => {}
                        Err(e) => println!("Account funding failed: {}", e),
                    }
                }
                3 => {
                    let from = match get_user_input("Enter from account address:") {
                        Ok(a) => a,
                        Err(e) => {
                            println!("Failed to read from account address: {}", e);
                            continue;
                        }
                    };
                    let to = match get_user_input("Enter to account address:") {
                        Ok(a) => a,
                        Err(e) => {
                            println!("Failed to read to account address: {}", e);
                            continue;
                        }
                    };

                    let amount = match get_user_input("Enter amount:") {
                        Ok(a) => a,
                        Err(e) => {
                            println!("Failed to read amount: {}", e);
                            continue;
                        }
                    };
                    let amount: u64 = match amount.parse() {
                        Ok(a) => a,
                        Err(e) => {
                            println!("Invalid amount: {}", e);
                            continue;
                        }
                    };
                    match self.subnet.transfer(&from, &to, amount) {
                        Ok(_) => {}
                        Err(e) => println!("Transfer failed: {}", e),
                    }
                }
                4 => {
                    let checkpoint = match self.subnet.get_checkpoint() {
                        Ok(cp) => cp,
                        Err(e) => {
                            println!("Failed to get checkpoint: {}", e);
                            continue;
                        }
                    };
                    let str_cp = hex::encode(checkpoint);
                    println!("Checkpoint: {:?}", str_cp);
                }
                5 => self.subnet.print_state(),
                6 => break,
                _ => println!("Invalid option. Please try again."),
            }
        }
    }
}

fn get_user_input(prompt: &str) -> Result<String, SubnetInteractorError> {
    println!("{prompt}");
    let mut input = String::new();
    match io::stdin().read_line(&mut input) {
        Ok(_) => Ok(input.trim().to_string()),
        Err(e) => return Err(e.into()),
    }
}

#[derive(Error, Debug)]
pub enum SubnetInteractorError {
    #[error(transparent)]
    SubnetSimulatorError(#[from] bitcoin_ipc::subnet_simulator::SubnetSimulatorError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),

    #[error("could not parse user input")]
    CannotReadUserInput(#[from] std::io::Error),
}

fn main() {
    let args = Args::parse();

    let file_name = format!(
        "{}/{}/{}.json",
        bitcoin_ipc::L1_NAME,
        args.subnet_name.clone(),
        args.subnet_name.clone()
    );

    let ipc_state = match IPCState::load_state(file_name) {
        Ok(state) => state,
        Err(e) => {
            println!("Subnet doesn't exist. Error: {e}");
            return;
        }
    };

    if !ipc_state.has_required_validators() {
        println!("Subnet does not have the required number of validators. Please wait for more validators to join.");
        return;
    }

    let subnet = match SubnetSimulator::new(&args.subnet_name) {
        Ok(subnet) => subnet,
        Err(e) => {
            println!("Could not start a Subnet Simulator. Error: {e}");
            return;
        }
    };
    let mut interactor = SubnetInteractor::new(subnet);

    interactor.interactive_interface();
}
