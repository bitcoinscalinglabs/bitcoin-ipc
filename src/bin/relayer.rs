use clap::Parser;
use thiserror::Error;

use std::{thread, time::Duration};

use bitcoin_ipc::{ipc_lib, ipc_state::IPCState, subnet_simulator::SubnetSimulator};

fn checkpoint(subnet_name: String) -> Result<(), RelayerError> {
    loop {
        let subnet = IPCState::load_state(format!(
            "{}/{}/{}.json",
            bitcoin_ipc::L1_NAME,
            subnet_name,
            subnet_name
        ))
        .unwrap();

        if subnet.has_required_validators() {
            let mut simulator = SubnetSimulator::new(&subnet_name)?;
            let hash = simulator.get_checkpoint();

            if let Ok(_) = ipc_lib::submit_checkpoint(hash, subnet.clone(), simulator) {
                println!("Checkpoint for {} submitted successfully", subnet.get_url());
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
    Other(#[from] Box<dyn std::error::Error>),
}

fn main() -> Result<(), RelayerError> {
    let args = Args::parse();

    checkpoint(args.subnet_name)?;

    Ok(())
}
