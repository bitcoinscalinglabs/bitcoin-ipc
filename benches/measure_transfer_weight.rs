use bitcoin::Amount;
use bitcoin::Transaction;
use bitcoin_ipc::l1_manager::CreateChildArgs;
use bitcoin_ipc::l1_manager::L1Manager;
use bitcoin_ipc::subnet_simulator::SubnetSimulator;
use bitcoin_ipc::subnet_simulator::TransferEvent;
use core::time;
use csv::Writer;
use rand::{thread_rng, Rng};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::path::Path;
use thiserror::Error;

fn generate_random_filecoin_address() -> String {
    let base = "f3";
    let suffix: String = thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(42)
        .map(char::from)
        .collect();
    format!("{}{}", base, suffix)
}

fn generate_random_amount() -> Amount {
    let satoshis = thread_rng().gen_range(1001..2002);
    Amount::from_sat(satoshis)
}

fn generate_random_transfers(num_transfers: usize) -> BTreeSet<TransferEvent> {
    let mut transfers = BTreeSet::new();
    for _ in 0..num_transfers {
        let transfer_event = TransferEvent {
            deposit_address: generate_random_filecoin_address(),
            amount: generate_random_amount(),
        };
        transfers.insert(transfer_event);
    }
    transfers
}

fn delete_file_if_exists(file_path: &str) {
    let path = Path::new(file_path);

    if path.exists() {
        if let Err(e) = fs::remove_file(path) {
            eprintln!("Failed to delete the file: {}", e);
        }
    }
}

fn main() -> Result<(), TestWeightError> {
    delete_file_if_exists("outputs/transfer.csv");

    let mut manager = L1Manager::new()?;

    let existing_subnets = manager.update_and_get_subnets()?.len();
    let required_subnets = 10;
    if required_subnets - existing_subnets > 0 {
        for _ in 0..(required_subnets - existing_subnets) {
            let args = CreateChildArgs {
                required_number_of_validators: 1,
                required_collateral: 1000,
            };
            manager.create_child(args)?;
        }
        println!("Waiting for subnets to be created.");
        std::thread::sleep(time::Duration::from_secs(10));
    }

    for number_of_subnets in [1, 2, 5, 10] {
        for total_transfers in [
            1, 2, 3, 4, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000, 40000, 45000,
        ] {
            let all_subnets = manager.update_and_get_subnets()?;

            if all_subnets.len() < number_of_subnets {
                return Err(TestWeightError::NotEnoughSubnetsCreated);
            }
            let source_subnet = &all_subnets[0];
            let source_subnet_bitcoin_address = source_subnet.get_bitcoin_address()?;
            let source_subnet_simulator =
                SubnetSimulator::new(source_subnet.get_subnet_id().as_str())?;

            let mut remaining_transfers = total_transfers;
            let mut remaining_subnets = number_of_subnets;

            let mut transfer_map: BTreeMap<String, BTreeSet<TransferEvent>> = BTreeMap::new();
            for subnet in all_subnets.iter().take(number_of_subnets) {
                let transfers_to_subnet =
                    (remaining_transfers as f32 / remaining_subnets as f32).ceil() as usize;
                if transfers_to_subnet == 0 {
                    continue;
                }
                remaining_transfers -= transfers_to_subnet;
                remaining_subnets -= 1;
                let target_subnet_id = subnet.get_subnet_id();
                let transfers = generate_random_transfers(transfers_to_subnet);
                transfer_map.insert(target_subnet_id, transfers);
            }

            let (commit_tx, reveal_tx): (Transaction, Transaction) =
                bitcoin_ipc::ipc_lib::create_and_submit_transfer_tx(
                    source_subnet_bitcoin_address,
                    source_subnet.get_subnet_pk(),
                    &transfer_map,
                    all_subnets,
                    &source_subnet_simulator,
                    false,
                )?;

            let output = format!(
                "{},{},{},{}",
                number_of_subnets,
                total_transfers,
                commit_tx.vsize(),
                reveal_tx.vsize(),
            );

            let file = match OpenOptions::new()
                .append(true)
                .create(true)
                .open("outputs/transfer.csv")
            {
                Ok(f) => f,
                Err(e) => {
                    return Err(TestWeightError::Other(Box::new(e)));
                }
            };

            let mut wtr = Writer::from_writer(file);

            let metadata = match std::fs::metadata("outputs/transfer.csv") {
                Ok(m) => m,
                Err(e) => {
                    return Err(TestWeightError::Other(Box::new(e)));
                }
            };

            if metadata.len() == 0 {
                if let Err(e) = wtr.write_record([
                    "Number of subnets",
                    "Total transfers",
                    "Commit Tx vsize",
                    "Reveal Tx vsize",
                ]) {
                    return Err(TestWeightError::Other(Box::new(e)));
                };
            }

            if let Err(e) = wtr.write_record(output.split(',')) {
                return Err(TestWeightError::Other(Box::new(e)));
            };

            if let Err(e) = wtr.flush() {
                return Err(TestWeightError::Other(Box::new(e)));
            }
        }
    }

    Ok(())
}

#[derive(Error, Debug)]
pub enum TestWeightError {
    #[error(transparent)]
    SubnetSimulatorError(#[from] bitcoin_ipc::subnet_simulator::SubnetSimulatorError),

    #[error(transparent)]
    SubnetStateError(#[from] bitcoin_ipc::subnet_simulator::SubnetStateError),

    #[error(transparent)]
    IpcStateError(#[from] bitcoin_ipc::ipc_state::IpcStateError),

    #[error(transparent)]
    IpcLibError(#[from] bitcoin_ipc::ipc_lib::IpcLibError),

    #[error(transparent)]
    L1ManagerError(#[from] bitcoin_ipc::l1_manager::L1ManagerError),

    #[error("invalid id")]
    InvalidId,

    #[error("waiting for validators to join subnet")]
    WaitingForValidators,

    #[error("not enough IPC subnets have been created")]
    NotEnoughSubnetsCreated,

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),
}
