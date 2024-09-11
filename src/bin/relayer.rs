use clap::Parser;
use thiserror::Error;

use std::{thread, time::Duration};

use bitcoin_ipc::{ipc_lib, ipc_state::IPCState, subnet_simulator::SubnetSimulator};

fn checkpoint(subnet_id: String) -> Result<(), RelayerError> {
    let subnet_address = match subnet_id.split("/").last() {
        Some(subnet_address) => subnet_address,
        None => return Err(RelayerError::InvalidId),
    };

    loop {
        let subnet = match IPCState::load_state(format!("{}/{}.json", subnet_id, subnet_address)) {
            Ok(s) => s,
            Err(e) => {
                println!("Failed to load subnet state: {}", e);
                thread::sleep(Duration::from_secs(10));
                continue;
            }
        };

        if subnet.has_required_validators() {
            let mut simulator = match SubnetSimulator::new(&subnet_id) {
                Ok(s) => s,
                Err(e) => {
                    println!("Failed to start simulator: {}", e);
                    thread::sleep(Duration::from_secs(10));
                    continue;
                }
            };
            let hash = match simulator.get_checkpoint() {
                Ok(h) => h,
                Err(e) => {
                    println!("Failed to get checkpoint: {}", e);
                    thread::sleep(Duration::from_secs(10));
                    continue;
                }
            };

            if ipc_lib::submit_checkpoint(hash, subnet.clone(), simulator).is_ok() {
                println!(
                    "Checkpoint for {} submitted successfully",
                    subnet.get_subnet_id()
                )
            } else {
                println!("Failed to submit checkpoint for {}", subnet.get_subnet_id());
            }
        } else {
            println!(
                "Waiting for validators to join subnet: {}",
                subnet.get_subnet_id()
            );
        }

        thread::sleep(Duration::from_secs(100));
    }
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    subnet_id: String,
}

#[derive(Error, Debug)]
pub enum RelayerError {
    #[error(transparent)]
    SubnetSimulatorError(#[from] bitcoin_ipc::subnet_simulator::SubnetSimulatorError),

    #[error(transparent)]
    IpcStateError(#[from] bitcoin_ipc::ipc_state::IpcStateError),

    #[error("invalid id")]
    InvalidId,

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),
}

fn main() {
    let args = Args::parse();

    match checkpoint(args.subnet_id) {
        Ok(_) => println!("Relayer stopped"),
        Err(e) => println!("Relayer error: {}", e),
    }
}
