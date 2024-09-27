use bitcoin::Amount;
use bitcoin::Transaction;
use bitcoin_ipc::ipc_state::IPCState;
use bitcoin_ipc::subnet_simulator::SubnetSimulator;
use bitcoin_ipc::subnet_simulator::TransferEvent;
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
    delete_file_if_exists("output.csv");

    for number_of_subnets in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
        for transfers_per_subnet in [1, 5, 10, 25, 50, 100, 200, 300, 500, 750, 1000] {
            let all_subnets = IPCState::load_all()?;

            let mut transfer_map: BTreeMap<String, BTreeSet<TransferEvent>> = BTreeMap::new();
            {
                for target_subnet_index in 0..number_of_subnets {
                    let target_subnet_id =
                        all_subnets[target_subnet_index as usize].get_subnet_id();
                    let transfers = generate_random_transfers(transfers_per_subnet);
                    transfer_map.insert(target_subnet_id, transfers);
                }
            }

            let source_subnet = &all_subnets[0];
            let source_subnet_bitcoin_address = source_subnet.get_bitcoin_address()?;

            let simulator = match SubnetSimulator::new(source_subnet.get_subnet_id().as_str()) {
                Ok(s) => s,
                Err(e) => {
                    return Err(TestWeightError::SubnetSimulatorError(e));
                }
            };

            let (commit_tx, reveal_tx): (Transaction, Transaction) =
                bitcoin_ipc::ipc_lib::create_and_submit_transfer_tx(
                    source_subnet_bitcoin_address,
                    source_subnet.get_subnet_pk(),
                    &transfer_map,
                    all_subnets,
                    &simulator,
                )?;

            let output = format!(
                "{},{},{},{}",
                number_of_subnets,
                transfers_per_subnet,
                commit_tx.vsize(),
                reveal_tx.vsize(),
            );

            let file = match OpenOptions::new()
                .append(true)
                .create(true)
                .open("output.csv")
            {
                Ok(f) => f,
                Err(e) => {
                    return Err(TestWeightError::Other(Box::new(e)));
                }
            };

            let mut wtr = Writer::from_writer(file);

            let metadata = match std::fs::metadata("output.csv") {
                Ok(m) => m,
                Err(e) => {
                    return Err(TestWeightError::Other(Box::new(e)));
                }
            };

            if metadata.len() == 0 {
                if let Err(e) = wtr.write_record([
                    "Number of Subnets",
                    "Transfers per Subnet",
                    "Commit Tx vsize",
                    "Reveal Tx vsize",
                ]) {
                    return Err(TestWeightError::Other(Box::new(e)));
                };
            }

            // Write the output row
            if let Err(e) = wtr.write_record(output.split(',')) {
                return Err(TestWeightError::Other(Box::new(e)));
            };

            // Flush and finish writing to the file
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

    #[error("invalid id")]
    InvalidId,

    #[error("waiting for validators to join subnet")]
    WaitingForValidators,

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),
}
