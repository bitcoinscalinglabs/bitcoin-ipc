// subnet_interactor.rs
use thiserror::Error;

use bitcoin_ipc::subnet_simulator::SubnetSimulator;
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
    subnet_id: String,
}

impl SubnetInteractor {
    pub fn new(subnet_sim: SubnetSimulator) -> Self {
        println!(
            "Starting a Subnet Interactor for subnet {}",
            subnet_sim.subnet_id
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

                    self.subnet.create_account(&address);
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

                    self.subnet.fund_account(&address, amount);
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
                    let checkpoint = self.subnet.get_checkpoint();
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
        Err(e) => Err(e.into()),
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

    let subnet = match SubnetSimulator::new(&args.subnet_id) {
        Ok(subnet) => subnet,
        Err(e) => {
            println!("Could not start a Subnet Simulator. Error: {e}");
            return;
        }
    };
    let mut interactor = SubnetInteractor::new(subnet);

    interactor.interactive_interface();
}
