use bitcoin::address::NetworkUnchecked;
use bitcoin::Amount;
use bitcoin::Transaction;
use bitcoin_ipc::bitcoin_utils;
use bitcoin_ipc::ipc_state::IPCState;
use bitcoin_ipc::subnet_simulator::SubnetSimulator;
use csv::Writer;
use rand::{thread_rng, Rng};
use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::path::Path;
use thiserror::Error;

fn generate_random_seed() -> String {
    thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(20)
        .map(char::from)
        .collect()
}

fn generate_random_amount() -> Amount {
    let satoshis = thread_rng().gen_range(1001..2002);
    Amount::from_sat(satoshis)
}

fn generate_random_withdraws(
    num_withdraws: usize,
) -> BTreeMap<bitcoin::Address<NetworkUnchecked>, Amount> {
    let mut withdraws = BTreeMap::new();
    for _ in 0..num_withdraws {
        let keypair = match bitcoin_utils::generate_keypair(generate_random_seed()) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("Failed to generate keypair: {}", e);
                continue;
            }
        };

        let address = bitcoin_utils::get_address_from_x_only_public_key(
            keypair.x_only_public_key().0,
            bitcoin_ipc::NETWORK,
        )
        .as_unchecked()
        .clone();

        let amount = generate_random_amount();
        withdraws.insert(address, amount);
    }
    withdraws
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
    delete_file_if_exists("outputs/withdraw.csv");

    for number_of_withdraws in [
        1, 2, 3, 4, 5, 10, 20, 50, 100, 200, 500, 1000, 2000,
        2300, // 10000, 20000, 40000, 45000,
    ] {
        println!("Number of withdraws: {}", number_of_withdraws);
        let all_subnets = IPCState::load_all()?;

        let withdraws = generate_random_withdraws(number_of_withdraws);

        let source_subnet = &all_subnets[0];
        let source_subnet_bitcoin_address = source_subnet.get_bitcoin_address()?;

        let simulator = match SubnetSimulator::new(source_subnet.get_subnet_id().as_str()) {
            Ok(s) => s,
            Err(e) => {
                return Err(TestWeightError::SubnetSimulatorError(e));
            }
        };

        let withdraw_tx: Transaction = bitcoin_ipc::ipc_lib::create_and_submit_withdraw_tx(
            source_subnet_bitcoin_address,
            source_subnet.get_subnet_pk(),
            &withdraws,
            &simulator,
            false,
        )?;

        let output = format!("{},{}", number_of_withdraws, withdraw_tx.vsize(),);

        let file = match OpenOptions::new()
            .append(true)
            .create(true)
            .open("outputs/withdraw.csv")
        {
            Ok(f) => f,
            Err(e) => {
                return Err(TestWeightError::Other(Box::new(e)));
            }
        };

        let mut wtr = Writer::from_writer(file);

        let metadata = match std::fs::metadata("outputs/withdraw.csv") {
            Ok(m) => m,
            Err(e) => {
                return Err(TestWeightError::Other(Box::new(e)));
            }
        };

        if metadata.len() == 0 {
            if let Err(e) = wtr.write_record(["Number of withdraws", "Withdraw Tx vsize"]) {
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
