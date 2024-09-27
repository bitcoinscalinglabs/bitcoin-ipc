use clap::Parser;
use thiserror::Error;

use tokio::{task, time};

use std::time::Duration;

use bitcoin_ipc::{ipc_lib, ipc_state::IPCState, subnet_simulator::SubnetSimulator, utils};

async fn get_subnet_and_simulator(
    subnet_id: &String,
) -> Result<(IPCState, SubnetSimulator), RelayerError> {
    let subnet = match IPCState::load_state(format!("{}/ipc_state.json", subnet_id)) {
        Ok(s) => s,
        Err(e) => {
            return Err(RelayerError::IpcStateError(e));
        }
    };

    if subnet.has_required_validators() {
        let simulator = match SubnetSimulator::new(subnet_id) {
            Ok(s) => s,
            Err(e) => {
                return Err(RelayerError::SubnetSimulatorError(e));
            }
        };

        Ok((subnet, simulator))
    } else {
        println!(
            "Waiting for validators to join subnet: {}",
            subnet.get_subnet_id()
        );
        Err(RelayerError::WaitingForValidators)
    }
}

async fn checkpoint(subnet_id: &String) -> Result<(), RelayerError> {
    let (subnet, mut simulator) = match get_subnet_and_simulator(subnet_id).await {
        Ok(s) => s,
        Err(e) => {
            return Err(e);
        }
    };

    let hash = match simulator.get_checkpoint() {
        Ok(h) => h,
        Err(e) => {
            return Err(RelayerError::SubnetStateError(e));
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

    Ok(())
}

async fn check_postbox(subnet_id: &String) -> Result<(), RelayerError> {
    let (subnet, mut simulator) = match get_subnet_and_simulator(subnet_id).await {
        Ok(s) => s,
        Err(e) => {
            return Err(e);
        }
    };

    let source_subnet_bitcoin_address = match subnet.get_bitcoin_address() {
        Ok(a) => a,
        Err(e) => {
            return Err(RelayerError::IpcStateError(e));
        }
    };

    let all_subnets = IPCState::load_all()?;

    {
        let transfers = simulator.get_postbox_transfers();

        println!("Handling Transfers: {:?}", transfers);

        if !transfers.is_empty() {
            {
                ipc_lib::create_and_submit_transfer_tx(
                    source_subnet_bitcoin_address,
                    subnet.get_subnet_pk(),
                    transfers,
                    all_subnets,
                    &simulator,
                )?;
            }

            {
                match simulator.empty_postbox_transfers() {
                    Ok(_) => {
                        println!(
                            "Handled transfers in postbox for subnet: {}",
                            subnet.get_subnet_id()
                        );
                    }
                    Err(e) => {
                        return Err(RelayerError::SubnetStateError(e));
                    }
                }
            }
        } else {
            println!("No transfers in postbox")
        }
    }

    {
        let withdraws = simulator.get_postbox_withdraws();
        if !withdraws.is_empty() {
            // TODO: batch and send withdraws here!

            match simulator.empty_postbox_withdraws() {
                Ok(_) => {
                    println!(
                        "Handled withdraws in postbox for subnet: {}",
                        subnet.get_subnet_id()
                    );
                }
                Err(e) => {
                    return Err(RelayerError::SubnetStateError(e));
                }
            }
        } else {
            println!("No withdraws in postbox")
        }
    }

    {
        let delete = simulator.get_postbox_delete();
        if delete.is_some() {
            // TODO: send delete tx here!

            match simulator.empty_postbox_delete() {
                Ok(_) => {
                    println!(
                        "Handled delete in postbox for subnet: {}",
                        subnet.get_subnet_id()
                    );
                }
                Err(e) => {
                    return Err(RelayerError::SubnetStateError(e));
                }
            }
        } else {
            println!("No delete in postbox")
        }
    }

    Ok(())
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
    SubnetStateError(#[from] bitcoin_ipc::subnet_simulator::SubnetStateError),

    #[error(transparent)]
    IpcStateError(#[from] bitcoin_ipc::ipc_state::IpcStateError),

    #[error(transparent)]
    IpcLibError(#[from] bitcoin_ipc::ipc_lib::IpcLibError),

    #[error("invalid id")]
    InvalidId,

    #[error("waiting for validators to join subnet")]
    WaitingForValidators,

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let config = match utils::load_config() {
        Ok(config) => config,
        Err(e) => {
            println!("Error loading config: {}", e);
            return;
        }
    };

    let checkpoint_interval = Duration::from_secs(config.checkpoint_interval);
    let postbox_interval = Duration::from_secs(config.postbox_interval);

    let mut subnet_id = args.subnet_id.clone();

    let checkpoint_task = task::spawn(async move {
        let mut interval = time::interval(checkpoint_interval);
        loop {
            interval.tick().await;
            if let Err(e) = checkpoint(&subnet_id).await {
                println!("Error submitting checkpoint: {}", e);
            }
        }
    });

    subnet_id = args.subnet_id.clone();

    let postbox_task = task::spawn(async move {
        let mut interval = time::interval(postbox_interval);
        loop {
            interval.tick().await;
            match check_postbox(&subnet_id).await {
                Ok(_) => println!("Postbox checked successfully"),
                Err(e) => println!("Error checking postbox: {}", e),
            }
        }
    });

    let _ = tokio::try_join!(checkpoint_task, postbox_task);

    println!("Relayer stopped");
}
