use std::str::FromStr;

use bitcoin::{
    secp256k1::{self, schnorr},
    Amount, Transaction, Weight,
};
use num_traits::ToPrimitive;

use crate::ipc_lib::{
    IpcCheckpointSubnetMsg, IpcCrossSubnetTransfer, IpcUnstake, IpcValidate, IpcWithdrawal,
};
use crate::multisig::{self, WeightedKey};
use crate::test_utils::create_rand_ipc_cross_subnet_transfer;
use crate::test_utils::{
    create_rand_ipc_unstake, create_rand_ipc_withdrawal, create_rand_utxo_entry, generate_subnet,
};
use crate::DEFAULT_BTC_FEE_RATE;

/// Create a dummy schnorr signature for testing purposes only
pub fn create_dummy_schnorr_signature() -> schnorr::Signature {
    let sig_bytes = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
        0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89,
        0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
        0x89, 0xab, 0xcd, 0xef,
    ];
    schnorr::Signature::from_slice(&sig_bytes).unwrap()
}

/// Create signature sets for the threshold number of signers
/// Returns signatures for exactly the threshold number of validators
pub fn create_threshold_signatures(
    committee_keys: &[WeightedKey],
    threshold: u32,
    num_inputs: usize,
) -> Vec<Vec<bitcoin::secp256k1::schnorr::Signature>> {
    let threshold = threshold as usize;
    let mut signature_sets = Vec::with_capacity(committee_keys.len());

    for i in 0..committee_keys.len() {
        if i < threshold {
            // This validator signs all inputs
            let signatures = (0..num_inputs)
                .map(|_| create_dummy_schnorr_signature())
                .collect();
            signature_sets.push(signatures);
        } else {
            // This validator doesn't sign
            signature_sets.push(Vec::new());
        }
    }

    signature_sets
}

fn calc_checkpoint_size(
    // Number of validators in the multisig committee
    n_validators: usize,
    // Number of input UTXOs to use as inputs
    n_inputs: usize,
    // Number of withdrawals in the checkpoint
    n_withdrawals: usize,
    // Number of unstakes in the checkpoint
    n_unstakes: usize,
    // Number of *total* transfers in the checkpoint
    n_transfers: usize,
    // Number of destination subnets for transfers
    // (transfers will be evenly distributed among the subnets)
    n_destination_subnets: usize,
) -> (Weight, Weight) {
    // Print params in a single line
    println!("validators: {}, inputs: {}, withdrawals: {}, unstakes: {}, transfers: {}, destination_subnets: {}", n_validators, n_inputs, n_withdrawals, n_unstakes, n_transfers, n_destination_subnets);

    //
    // Generate the source subnet
    //

    let subnet = generate_subnet(n_validators);

    //
    // Generate the checkpoint message contents
    //

    let unstakes: Vec<IpcUnstake> = (0..n_unstakes)
        .map(|_| create_rand_ipc_unstake(None))
        .collect();

    let withdrawals: Vec<IpcWithdrawal> = (0..n_withdrawals)
        .map(|_| create_rand_ipc_withdrawal(None))
        .collect();

    let destination_subnets = (0..n_destination_subnets)
        .map(|_| generate_subnet(n_validators))
        .collect::<Vec<_>>();

    let transfers: Vec<IpcCrossSubnetTransfer> = (0..n_transfers)
        .map(|i| {
            let dest_subnet = &destination_subnets[i % n_destination_subnets];
            let mut transfer = create_rand_ipc_cross_subnet_transfer(&dest_subnet, None);
            transfer.subnet_multisig_address = Some(dest_subnet.committee.multisig_address.clone());
            transfer
        })
        .collect();

    //
    // Calculate total output amount and create dummy UTXOs for inputs
    //

    let total_out: Amount = unstakes.iter().map(|u| u.amount).sum::<Amount>()
        + withdrawals.iter().map(|w| w.amount).sum::<Amount>()
        + transfers.iter().map(|t| t.amount).sum::<Amount>();

    // Evenly divide total output amount among the deposit UTXOs

    let mut per_unspent = total_out
        .checked_div(n_inputs as u64)
        .expect("total_out should be divisible by n_deposits");

    // If there's only one input, we can make it big enough
    // since we don't care about coin selection
    if n_inputs == 1 {
        per_unspent *= 100;
    }

    // println!("Total checkpoint output amount: {}", total_out);
    // println!("Each input UTXO amount: {}", per_unspent);

    // Create UTXOs, with 25% more to cover for fees
    // This is picked arbitrarily for testing purposes
    // TODO look into this further

    let unspent: Vec<bitcoincore_rpc::json::ListUnspentResultEntry> = (0..n_inputs)
        .map(|_| {
            let utxo_amount = per_unspent.unchecked_add(per_unspent.checked_div(4).unwrap());
            create_rand_utxo_entry(Some(utxo_amount))
        })
        .collect();

    //
    // Assemble the checkpoint message
    //

    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet.id,
        unstakes,
        withdrawals,
        transfers,
        change_address: Some(subnet.committee.multisig_address.clone()),
        // Static values below, not relevant for size testing
        checkpoint_hash: bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap(),
        checkpoint_height: 50,
        next_committee_configuration_number: 30, // arbitrary test number
        is_kill_checkpoint: false,
    };

    // Sanity check that the message is valid
    assert!(checkpoint_msg.validate().is_ok());

    // Create PSBT

    let unsigned_psbt = checkpoint_msg
        .to_checkpoint_psbt(
            &subnet.committee,
            &subnet.committee,
            DEFAULT_BTC_FEE_RATE,
            &unspent,
        )
        .unwrap();

    // Sign with dummy signatures, fill in the threshold number of signatures
    // (the signatures themselves are not relevant for size testing)
    // TODO double check that this is all correct

    let secp = secp256k1::Secp256k1::new();
    let committee_keys = subnet.committee.validator_weighted_keys();
    let threshold = subnet.committee.threshold;
    let num_inputs = unsigned_psbt.inputs.len();

    let signature_sets = create_threshold_signatures(&committee_keys, threshold, num_inputs);
    // Convert to the format expected by finalize_spend_psbt_from_sigs
    let signature_refs: Vec<&[bitcoin::secp256k1::schnorr::Signature]> =
        signature_sets.iter().map(|sigs| sigs.as_slice()).collect();

    // Make the final checkpoint transaction

    let checkpoint_tx = multisig::finalize_spend_psbt_from_sigs(
        &secp,
        &subnet.id,
        &subnet.committee.validator_weighted_keys(),
        subnet.committee.threshold,
        &unsigned_psbt,
        &signature_refs,
    )
    .unwrap();
    // dbg!(&checkpoint_tx);

    assert_eq!(
        checkpoint_tx.input.len(),
        n_inputs,
        "Input number doesn't match"
    );

    assert!(
        checkpoint_tx.weight() < Transaction::MAX_STANDARD_WEIGHT,
        "Batch transfer transaction is too large to be standard"
    );

    // Get the size of checkpoint transaction

    if n_transfers > 0 {
        let batch_tx = checkpoint_msg
            .make_reveal_batch_transfer_tx(
                checkpoint_tx.compute_txid(),
                DEFAULT_BTC_FEE_RATE,
                &subnet.committee.address_checked(),
            )
            .unwrap();
        // dbg!(&batch_tx);

        assert!(
            batch_tx.is_some(),
            "Should have a batch transfer transaction"
        );

        let batch_tx = batch_tx.unwrap();

        assert!(
            batch_tx.weight() < Transaction::MAX_STANDARD_WEIGHT,
            "Batch transfer transaction is too large to be standard"
        );

        let percentage = batch_tx.weight().to_wu().to_f64().unwrap()
            / Transaction::MAX_STANDARD_WEIGHT.to_wu().to_f64().unwrap();

        if percentage > 0.8 {
            println!(
                "WARNING: Batch transfer transaction is {:.2}% of a standard tx!",
                percentage * 100.0
            );
        }

        (checkpoint_tx.weight(), batch_tx.weight())
    } else {
        (checkpoint_tx.weight(), Weight::ZERO)
    }
}

#[test]
#[ignore]
// cargo test test_checkpoint_size -- --nocapture --ignored
fn test_checkpoint_size() {
    // let n_validators = 4;
    let n_inputs = 1;
    let n_withdrawals = 0;
    let n_unstakes = 0;
    // let n_transfers = 10;
    // let n_destination_subnets = 1;
    for n_validators in [4, 7, 10, 16, 25, 36] {
        for n_destination_subnets in [1, 2, 5, 10] {
            for n_transfers in [1, 2, 3, 4, 5, 10, 20, 50, 100, 150, 200, 250] {
                let (checkpoint_size, transfer_size) = calc_checkpoint_size(
                    n_validators,
                    n_inputs,
                    n_withdrawals,
                    n_unstakes,
                    n_transfers,
                    n_destination_subnets,
                );

                write_to_csv(
                    n_validators as u64,
                    n_destination_subnets as u64,
                    n_transfers as u64,
                    checkpoint_size.to_vbytes_ceil(),
                    transfer_size.to_vbytes_ceil(),
                );

                // println!("{n_validators} validators, {n_inputs} inputs, {n_withdrawals} withdrawals, {n_unstakes} unstakes, {n_transfers} transfers, {n_destination_subnets} destination subnets\nCheckpoint size: {} vbytes\tnBatch transfer: {} vbytes", checkpoint_size.to_vbytes_ceil(), transfer_size.to_vbytes_ceil());
            }
        }
    }
}

fn write_to_csv(
    number_of_validators: u64,
    number_of_subnets: u64,
    total_transfers: u64,
    checkpoint_tx_size: u64,
    transfer_tx_size: u64,
) {
    let file_name = "bench-transfer-sizes.csv";

    let output = format!(
        "{},{},{},{},{}",
        number_of_validators,
        number_of_subnets,
        total_transfers,
        checkpoint_tx_size,
        transfer_tx_size,
    );

    let file = match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(file_name)
    {
        Ok(f) => f,
        Err(e) => {
            panic!("Error opening file: {}", e);
        }
    };

    let mut wtr = csv::Writer::from_writer(file);

    // write header if file is empty
    let metadata = match std::fs::metadata(file_name) {
        Ok(m) => m,
        Err(e) => {
            panic!("Error getting metadata: {}", e);
        }
    };
    if metadata.len() == 0 {
        if let Err(e) = wtr.write_record([
            "Number of validators",
            "Number of subnets",
            "Total transfers",
            "Commit Tx vsize",
            "Reveal Tx vsize",
        ]) {
            panic!("Error writing record: {}", e);
        };
    }

    // write data
    if let Err(e) = wtr.write_record(output.split(',')) {
        panic!("Error writing record: {}", e);
    };

    if let Err(e) = wtr.flush() {
        panic!("Error flushing writer: {}", e);
    }
}
