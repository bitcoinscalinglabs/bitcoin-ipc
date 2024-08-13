// subnet_interactor.rs
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

                    self.subnet.create_account(address.trim());
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

                    self.subnet.fund_account(address.trim(), amount);
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

                    match self.subnet.transfer(from.trim(), to.trim(), amount) {
                        Ok(_) => {}
                        Err(e) => println!("Transfer failed: {}", e),
                    }
                }
                "4" => {
                    let checkpoint = self.subnet.get_checkpoint();
                    let str_cp = hex::encode(checkpoint);
                    println!("Checkpoint: {:?}", str_cp);
                }
                "5" => self.subnet.print_state(),
                "6" => break,
                _ => println!("Invalid option. Please try again."),
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let subnet = SubnetSimulator::new(&args.subnet_name)?;
    let mut interactor = SubnetInteractor::new(subnet);

    interactor.interactive_interface();
    Ok(())
}
