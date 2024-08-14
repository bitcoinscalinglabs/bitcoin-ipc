use clap::Parser;
use thiserror::Error;

use std::{thread, time::Duration};

use bitcoin_ipc::{ipc_lib, ipc_state::IPCState, subnet_simulator::SubnetSimulator};

fn checkpoint(subnet_name: String) {
    loop {
        let subnet = match IPCState::load_state(format!(
            "{}/{}/{}.json",
            bitcoin_ipc::L1_NAME,
            subnet_name,
            subnet_name
        )) {
            Ok(s) => s,
            Err(e) => {
                println!("Failed to load subnet state: {}", e);
                thread::sleep(Duration::from_secs(10));
                continue;
            }
        };

        if subnet.has_required_validators() {
            let mut simulator = match SubnetSimulator::new(&subnet_name) {
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

            if let Ok(_) = ipc_lib::create_and_submit_checkpoint_tx(hash, subnet.clone(), simulator)
            {
                println!("Checkpoint for {} submitted successfully", subnet.get_url());
            } else {
                println!("Failed to submit checkpoint for {}", subnet.get_url());
            }
        } else {
            println!(
                "Waiting for validators to join subnet: {}",
                subnet.get_url()
            );
        }

        thread::sleep(Duration::from_secs(100));
    }
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    subnet_name: String,
}

#[derive(Error, Debug)]
pub enum RelayerError {
    #[error(transparent)]
    SubnetSimulatorError(#[from] bitcoin_ipc::subnet_simulator::SubnetSimulatorError),

    #[error(transparent)]
    IpcStateError(#[from] bitcoin_ipc::ipc_state::IpcStateError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),
}

fn main() {
    let args = Args::parse();

    checkpoint(args.subnet_name);
}
