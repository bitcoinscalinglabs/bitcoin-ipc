use super::*;
use crate::test_utils::{generate_equal_weighted_keys, generate_keypairs, generate_subnet_id};
use crate::NETWORK;
use bitcoin::consensus::encode;
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::Message;
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::taproot::LeafVersion;
use bitcoin::{
    absolute::LockTime, secp256k1::Secp256k1, transaction, AddressType, Amount, Network, OutPoint,
    Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use bitcoincore_rpc::json::ListUnspentResultEntry;
use std::str::FromStr;

/// Verifies that this transaction is able to spend its inputs.
///
/// The `spent` closure should return the [`TxOut`] for the given [`OutPoint`] (the ones we're spending).
/// The `spent` closure should not return the same [`TxOut`] twice!
pub fn verify_transaction<S>(tx: &Transaction, mut spent: S) -> Result<(), bitcoinconsensus::Error>
where
    S: FnMut(&OutPoint) -> Option<TxOut>,
{
    let serialized_tx = encode::serialize(tx);

    // First, collect all UTXOs being spent in this transaction
    let mut all_spent_utxos = Vec::with_capacity(tx.input.len());
    for input in &tx.input {
        if let Some(output) = spent(&input.previous_output) {
            all_spent_utxos.push(bitcoinconsensus::Utxo {
                script_pubkey: output.script_pubkey.as_bytes().as_ptr(),
                script_pubkey_len: output.script_pubkey.len() as u32,
                value: output.value.to_sat() as i64,
            });
        } else {
            println!("Unknown spent output: {:?}", input.previous_output);
            panic!("Unknown spent output");
        }
    }

    for (idx, input) in tx.input.iter().enumerate() {
        // Get the current input's UTXO for the first argument
        if let Some(output) = spent(&input.previous_output) {
            bitcoinconsensus::verify_with_flags(
                output.script_pubkey.as_bytes(),
                output.value.to_sat(),
                serialized_tx.as_slice(),
                Some(&all_spent_utxos),
                idx,
                bitcoinconsensus::VERIFY_ALL_PRE_TAPROOT | bitcoinconsensus::VERIFY_TAPROOT,
            )?;
        } else {
            println!("Unknown spent output: {:?}", input.previous_output);
            panic!("Unknown spent output");
        }
    }
    Ok(())
}

#[test]
fn test_create_multisig_address_single_key() {
    let secp = Secp256k1::new();
    let public_keys = generate_equal_weighted_keys(1);
    let required_sigs = 1;
    let network = Network::Bitcoin;

    let address = create_subnet_multisig_address(
        &secp,
        &generate_subnet_id(),
        &public_keys,
        required_sigs,
        network,
    )
    .expect("Failed to create multisig address");

    assert_eq!(address.address_type(), Some(AddressType::P2tr));
}

#[test]
fn test_create_multisig_address_multiple_keys() {
    let secp = Secp256k1::new();
    let public_keys = generate_equal_weighted_keys(3);
    let required_sigs = 2;
    let network = Network::Bitcoin;

    let address = create_subnet_multisig_address(
        &secp,
        &generate_subnet_id(),
        &public_keys,
        required_sigs,
        network,
    )
    .expect("Failed to create multisig address");

    assert_eq!(address.address_type(), Some(AddressType::P2tr));
}

#[test]
fn test_create_multisig_address_insufficient_keys() {
    let secp = Secp256k1::new();
    let public_keys = generate_equal_weighted_keys(1);
    let threshold = 2; // More signatures required than keys available
    let network = Network::Bitcoin;

    let result = create_subnet_multisig_address(
        &secp,
        &generate_subnet_id(),
        &public_keys,
        threshold,
        network,
    );

    assert!(matches!(result, Err(MultisigError::InsufficientTotalPower)));
}

//
// Test spending
//

#[test]
fn test_spend_multisig_script() {
    let secp = Secp256k1::new();

    //
    // Setup: Create 3-of-5 multisig
    //

    let keypairs = generate_keypairs(5);

    for keypair in keypairs.iter() {
        let (x_only, parity) = keypair.x_only_public_key();
        println!(
            "PK = {}\nP = {:?}\nXPK = {}\nSK = {}\nADDR = {}\n\n",
            keypair.public_key(),
            parity,
            x_only,
            keypair.secret_key().display_secret(),
            alloy_primitives::Address::from_raw_public_key(
                &keypair.public_key().serialize_uncompressed()[1..]
            ),
        );
    }

    let power_per_validator = 2000;

    let committee_keys: Vec<WeightedKey> = keypairs
        .iter()
        .map(|kp| (kp.x_only_public_key().0, power_per_validator))
        .collect();

    let required_sigs = 3;
    let network = Network::Regtest;

    let subnet_id = generate_subnet_id();

    let script = create_multisig_script(&committee_keys, required_sigs)
        .expect("Failed to create multisig script");

    // dbg!(&script);

    let spend_info =
        create_subnet_multisig_spend_info(&secp, &subnet_id, &committee_keys, required_sigs)
            .expect("Failed to create multisig spend info");

    let control_block = spend_info
        .control_block(&(script.clone(), LeafVersion::TapScript))
        .expect("Should create control block");

    // dbg!(&control_block.size());

    let multisig_address =
        create_subnet_multisig_address(&secp, &subnet_id, &committee_keys, required_sigs, network)
            .expect("Failed to create multisig address");

    dbg!(&multisig_address);

    //
    // Create funding transaction
    //

    let funding_amount = Amount::from_sat(100_000);
    let funding_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::all_zeros(),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: funding_amount,
            script_pubkey: multisig_address.script_pubkey(),
        }],
    };
    let funding_txid = funding_tx.compute_txid();

    //
    // Create spending transaction
    //

    let spending_amount = Amount::from_sat(90_000);
    let spending_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: funding_txid,
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: spending_amount,
            script_pubkey: ScriptBuf::new_op_return([1]),
        }],
    };

    //
    //  Create sighash for signing
    //

    let mut sighash_cache = SighashCache::new(&spending_tx);
    let leaf_hash = script.tapscript_leaf_hash();
    let sighash = sighash_cache
        .taproot_script_spend_signature_hash(
            0,
            &Prevouts::All(&[funding_tx.output[0].clone()]),
            leaf_hash,
            TapSighashType::Default,
        )
        .expect("Failed to create sighash");

    //
    // Case 1: Not enough signatures (only 2)
    //

    {
        let mut tx_insufficient = spending_tx.clone();
        let mut witness = Witness::new();

        // Sign with only 2 keys
        for keypair in keypairs.iter().take(2) {
            let msg =
                Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");
            let sig = secp.sign_schnorr(&msg, keypair);
            witness.push(sig.serialize());
            dbg!(sig);
        }

        // Push empty signatures for the remaining keys
        for _ in 2..keypairs.len() {
            witness.push([]); // Empty signature slots for unused keys
        }

        witness.push(script.to_bytes());
        witness.push(control_block.serialize());

        tx_insufficient.input[0].witness = witness;

        let verify_result =
            verify_transaction(&tx_insufficient, |_| Some(funding_tx.output[0].clone()));

        dbg!(&verify_result);

        assert!(
            verify_result.is_err(),
            "Transaction with insufficient signatures should fail"
        );
    }

    //
    // Case 2: Valid spend with required signatures (3 of 5)
    //
    {
        let mut tx_valid = spending_tx.clone();
        let mut witness = Witness::new();

        // Sign with 3 keys
        for (idx, keypair) in keypairs.iter().rev().enumerate() {
            // Skip keys 4 and 5, pushing empty signatures
            if idx > 2 {
                witness.push([]);
                continue;
            }
            let msg =
                Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");
            let sig = secp.sign_schnorr(&msg, keypair);
            witness.push(sig.serialize());
        }

        witness.push(script.to_bytes());
        witness.push(control_block.serialize());

        // Test the spend witness size calculation
        let spend_witness_size = multisig_spend_max_witness_size(&committee_keys, 3);
        dbg!(spend_witness_size);
        dbg!(witness.size());
        assert!(witness.size() < spend_witness_size);

        tx_valid.input[0].witness = witness;

        let verify_result = verify_transaction(&tx_valid, |_| Some(funding_tx.output[0].clone()));

        dbg!(&verify_result);

        assert!(
            verify_result.is_ok(),
            "Transaction with sufficient signatures should pass"
        );
    }

    //
    // Case 3: Valid spend with all signatures
    //
    {
        let mut tx_all = spending_tx.clone();
        let mut witness = Witness::new();

        // Sign with all keys
        for keypair in keypairs.iter().rev() {
            let msg =
                Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");
            let sig = secp.sign_schnorr(&msg, keypair);
            witness.push(sig.serialize());
        }

        witness.push(script.to_bytes());
        witness.push(control_block.serialize());

        tx_all.input[0].witness = witness;

        let verify_result = verify_transaction(&tx_all, |_| Some(funding_tx.output[0].clone()));

        dbg!(&verify_result);

        assert!(
            verify_result.is_ok(),
            "Transaction with all signatures should pass"
        );
    }

    //
    // Case 4: Wrong signature order
    //
    {
        let mut tx_wrong_order = spending_tx.clone();
        let mut witness = Witness::new();

        // Sign with 3 keys but push signatures in wrong order
        for keypair in keypairs.iter().take(3).rev() {
            // Reverse order
            let msg =
                Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");
            let sig = secp.sign_schnorr(&msg, keypair);
            witness.push(sig.serialize());
        }

        // Push empty signatures for the remaining keys
        for _ in 3..keypairs.len() {
            witness.push([]); // Empty signature slots for unused keys
        }

        witness.push(script.to_bytes());
        witness.push(
            spend_info
                .control_block(&(script.clone(), LeafVersion::TapScript))
                .unwrap()
                .serialize(),
        );

        tx_wrong_order.input[0].witness = witness;

        let verify_result =
            verify_transaction(&tx_wrong_order, |_| Some(funding_tx.output[0].clone()));

        dbg!(&verify_result);

        assert!(
            verify_result.is_err(),
            "Transaction with wrong signature order should fail"
        );
    }
}

#[test]
fn test_multisig_addresses_different_subnet_id() {
    let secp = Secp256k1::new();
    let public_keys = generate_equal_weighted_keys(3);
    let required_sigs = 2;
    let network = Network::Bitcoin;

    let subnet_id_1 = generate_subnet_id();
    let subnet_id_2 = generate_subnet_id();

    let address_1 =
        create_subnet_multisig_address(&secp, &subnet_id_1, &public_keys, required_sigs, network)
            .expect("Failed to create first multisig address");
    dbg!(&address_1);

    let address_2 =
        create_subnet_multisig_address(&secp, &subnet_id_2, &public_keys, required_sigs, network)
            .expect("Failed to create second multisig address");
    dbg!(&address_2);

    assert_ne!(
        address_1, address_2,
        "Addresses should be different with different subnet IDs"
    );
}

#[test]
fn test_witness_size_calculation() {
    let committee_configs: [(u32, u32); 12] = [
        (1, 1),
        (2, 2),
        (2, 3),
        (3, 3),
        (3, 4),
        (3, 5),
        (5, 7),
        (7, 10),
        (11, 15),
        (11, 20),
        (14, 20),
        (19, 20),
    ];

    let secp = Secp256k1::new();

    for (threshold, committee_size) in committee_configs {
        let keypairs = generate_keypairs(committee_size as usize);
        let public_keys: Vec<WeightedKey> = keypairs
            .iter()
            .map(|kp| (kp.x_only_public_key().0, 100000))
            .collect();

        let subnet_id = generate_subnet_id();
        let script = create_multisig_script(&public_keys, threshold).unwrap();

        let spend_info =
            create_subnet_multisig_spend_info(&secp, &subnet_id, &public_keys, threshold).unwrap();
        let control_block = spend_info
            .control_block(&(script.clone(), LeafVersion::TapScript))
            .unwrap();

        // Create a witness
        let mut witness = Witness::new();

        // Add required signatures
        let dummy_sig = [1u8; bitcoin::key::constants::SCHNORR_SIGNATURE_SIZE];
        for _ in 0..threshold {
            witness.push(&dummy_sig[..]);
        }
        // Add empty signatures for unused keys
        for _ in threshold..committee_size {
            witness.push([]);
        }
        // Add the script and control block
        witness.push(script.to_bytes());
        witness.push(control_block.serialize());

        // Calculate the actual size
        let actual_size = witness.size();

        // Calculate using our function
        let calculated_size = multisig_spend_max_witness_size(&public_keys, threshold);

        // They should match
        println!(
            "{threshold}-of-{committee_size} Witness Size: Actual={}, Calculated={}",
            actual_size, calculated_size
        );
        assert!(actual_size <= calculated_size);
    }
}

fn sorted_test_keys() -> Vec<XOnlyPublicKey> {
    let secp = Secp256k1::new();
    let mut keys: Vec<_> = (1..=4)
        .map(|n| {
            let mut bytes = [0; 32];
            bytes[31] = n;
            bitcoin::secp256k1::SecretKey::from_slice(&bytes)
                .unwrap()
                .x_only_public_key(&secp)
                .0
        })
        .collect();
    keys.sort_by_key(|k| k.serialize());
    keys
}

#[test]
fn signature_indices_follow_key_bytes_instead_of_insertion_order_or_weight() {
    let k = sorted_test_keys();
    let c = Configuration {
        keys: vec![(k[2], 99), (k[0], 7), (k[3], 1), (k[1], 40)],
        threshold: 99,
    };
    for (i, key) in k.iter().enumerate() {
        assert_eq!(c.key_index(key), Some(i as u16));
    }
    assert_eq!(c.power_at(0), Some(7));
    assert_eq!(c.power_at(1), Some(40));
    assert_eq!(c.power_at(2), Some(99));
    assert_eq!(c.power_at(3), Some(1));
}

#[test]
fn absent_configuration_key_does_not_alias_an_existing_signature_index() {
    let k = sorted_test_keys();
    let c = Configuration {
        keys: vec![(k[2], 2), (k[0], 1)],
        threshold: 3,
    };
    assert_eq!(c.key_index(&k[1]), None);
    assert_eq!(c.key_index(&k[3]), None);
}

#[test]
fn signature_indices_at_the_length_and_u16_maximum_have_no_member() {
    let k = sorted_test_keys();
    let c = Configuration {
        keys: vec![(k[0], 1), (k[1], 2)],
        threshold: 3,
    };
    assert_eq!(c.power_at(2), None);
    assert_eq!(c.power_at(u16::MAX), None);
}

#[test]
fn threshold_is_exact_at_maximum_total_power() {
    assert_eq!(multisig_threshold(2_147_483_645), 1_431_655_764);
    assert_eq!(multisig_threshold(2_147_483_646), 1_431_655_765);
    assert_eq!(multisig_threshold(2_147_483_647), 1_431_655_765);
}

fn create_test_utxo(amount: u64, txid: &str, vout: u32) -> ListUnspentResultEntry {
    ListUnspentResultEntry {
        txid: txid.parse().unwrap(),
        vout,
        address: None,
        label: None,
        redeem_script: None,
        witness_script: None,
        script_pub_key: ScriptBuf::new(),
        amount: Amount::from_sat(amount),
        confirmations: 1,
        spendable: true,
        solvable: true,
        descriptor: None,
        safe: true,
    }
}

#[test]
fn test_basic() {
    let target = Amount::from_sat(1000);
    let utxos = vec![
        create_test_utxo(
            500,
            "7224e1f11ddc838100abd123d23af0d02493001fdd746685dc539fe062b45e3e",
            0,
        ),
        create_test_utxo(
            1000,
            "d7f3553b9631f48a2842a2cb6e0f2b6e344bf82d3ee78295a5361adc17b838b1",
            0,
        ),
    ];
    let fee_rate = FeeRate::from_sat_per_vb(1).expect("works");
    let base_weight = Weight::from_wu(100);
    let input_weight = Weight::from_wu(50);
    let change_address = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080")
        .unwrap()
        .assume_checked();

    let (selected, change) = select_coins(
        target,
        &utxos,
        fee_rate,
        base_weight,
        input_weight,
        &change_address,
        0,
    )
    .unwrap();

    assert_eq!(selected.len(), 2);
    assert!(change.is_some());
    assert_eq!(change.unwrap().value, Amount::from_sat(450));
}

#[test]
fn test_consolidation_sweeps_dust() {
    // One large UTXO that alone covers the target, plus dust UTXOs that greedy
    // largest-first selection would never touch (distinct by vout).
    let target = Amount::from_sat(1000);
    let large_txid = "d7f3553b9631f48a2842a2cb6e0f2b6e344bf82d3ee78295a5361adc17b838b1";
    let dust_txid = "7224e1f11ddc838100abd123d23af0d02493001fdd746685dc539fe062b45e3e";
    let mut utxos = vec![create_test_utxo(100_000, large_txid, 0)];
    for vout in 0..5u32 {
        utxos.push(create_test_utxo(330, dust_txid, vout));
    }

    let fee_rate = FeeRate::from_sat_per_vb(1).expect("works");
    let base_weight = Weight::from_wu(100);
    let input_weight = Weight::from_wu(50);
    let change_address = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080")
        .unwrap()
        .assume_checked();

    // Limit 0: unchanged behaviour — greedy selection takes only the large UTXO.
    let (selected, _) = select_coins(
        target,
        &utxos,
        fee_rate,
        base_weight,
        input_weight,
        &change_address,
        0,
    )
    .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].amount, Amount::from_sat(100_000));

    // Limit 3: additionally folds in the 3 smallest dust UTXOs, value into change.
    let (selected, change) = select_coins(
        target,
        &utxos,
        fee_rate,
        base_weight,
        input_weight,
        &change_address,
        3,
    )
    .unwrap();
    assert_eq!(selected.len(), 4);
    assert_eq!(selected[0].amount, Amount::from_sat(100_000));
    assert!(selected[1..]
        .iter()
        .all(|u| u.amount == Amount::from_sat(330)));
    assert!(change.is_some());

    // Limit exceeding the available dust sweeps them all without overshooting.
    let (selected, _) = select_coins(
        target,
        &utxos,
        fee_rate,
        base_weight,
        input_weight,
        &change_address,
        100,
    )
    .unwrap();
    assert_eq!(selected.len(), 6); // large + all 5 dust
}

#[test]
fn test_change_below_dust() {
    let target = Amount::from_sat(1000);
    let utxos = vec![create_test_utxo(
        1050,
        "d7f3553b9631f48a2842a2cb6e0f2b6e344bf82d3ee78295a5361adc17b838b1",
        0,
    )];
    let fee_rate = FeeRate::from_sat_per_vb(1).expect("works");
    let base_weight = Weight::from_wu(100);
    let input_weight = Weight::from_wu(50);
    let change_address = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080")
        .unwrap()
        .assume_checked();

    let (selected, change) = select_coins(
        target,
        &utxos,
        fee_rate,
        base_weight,
        input_weight,
        &change_address,
        0,
    )
    .unwrap();

    assert_eq!(selected.len(), 1);
    assert!(change.is_none()); // No change expected for exact match
}

#[test]
fn test_insufficient_funds() {
    let target = Amount::from_sat(2000);
    let utxos = vec![
        create_test_utxo(
            500,
            "7224e1f11ddc838100abd123d23af0d02493001fdd746685dc539fe062b45e3e",
            0,
        ),
        create_test_utxo(
            1000,
            "d7f3553b9631f48a2842a2cb6e0f2b6e344bf82d3ee78295a5361adc17b838b1",
            0,
        ),
    ];
    let fee_rate = FeeRate::from_sat_per_vb(1).expect("works");
    let base_weight = Weight::from_wu(100);
    let input_weight = Weight::from_wu(50);
    let change_address = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080")
        .unwrap()
        .assume_checked();

    let result = select_coins(
        target,
        &utxos,
        fee_rate,
        base_weight,
        input_weight,
        &change_address,
        0,
    );

    assert!(result.is_err());
    match result {
        Err(MultisigError::CoinSelectionFailed(_)) => {}
        _ => panic!("Expected CoinSelectionFailed error"),
    }
}

#[test]
fn test_largest_sort() {
    let target = Amount::from_sat(1500);
    let utxos = vec![
        create_test_utxo(
            500,
            "7224e1f11ddc838100abd123d23af0d02493001fdd746685dc539fe062b45e3e",
            0,
        ),
        create_test_utxo(
            2000,
            "d7f3553b9631f48a2842a2cb6e0f2b6e344bf82d3ee78295a5361adc17b838b1",
            0,
        ),
        create_test_utxo(
            1000,
            "f4184fc596403b9d638783cf57adfe4c75c605f6356fbc91338530e9831e9e16",
            0,
        ),
    ];
    let fee_rate = FeeRate::from_sat_per_vb(1).expect("works");
    let base_weight = Weight::from_wu(100);
    let input_weight = Weight::from_wu(50);
    let change_address = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080")
        .unwrap()
        .assume_checked();

    let (selected, _) = select_coins(
        target,
        &utxos,
        fee_rate,
        base_weight,
        input_weight,
        &change_address,
        0,
    )
    .unwrap();

    // Algorithm should select the 2000 sat UTXO first since it's the largest
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].amount, Amount::from_sat(2000));
}

#[test]
fn test_high_fee() {
    let target = Amount::from_sat(1000);
    let utxos = vec![create_test_utxo(
        1500,
        "7224e1f11ddc838100abd123d23af0d02493001fdd746685dc539fe062b45e3e",
        0,
    )];
    // Very high fee rate
    let fee_rate = FeeRate::from_sat_per_vb(100).expect("works");
    let base_weight = Weight::from_wu(1000); // Large tx
    let input_weight = Weight::from_wu(500); // Heavy input
    let change_address = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080")
        .unwrap()
        .assume_checked();

    let result = select_coins(
        target,
        &utxos,
        fee_rate,
        base_weight,
        input_weight,
        &change_address,
        0,
    );

    assert!(result.is_err());
    match result {
        Err(MultisigError::CoinSelectionFailed(_)) => {}
        _ => panic!("Expected CoinSelectionFailed error"),
    }
}

#[test]
fn test_multiple_utxos_needed() {
    let target = Amount::from_sat(3000);
    let utxos = vec![
        create_test_utxo(
            1000,
            "7224e1f11ddc838100abd123d23af0d02493001fdd746685dc539fe062b45e3e",
            0,
        ),
        create_test_utxo(
            1200,
            "d7f3553b9631f48a2842a2cb6e0f2b6e344bf82d3ee78295a5361adc17b838b1",
            0,
        ),
        create_test_utxo(
            1500,
            "f4184fc596403b9d638783cf57adfe4c75c605f6356fbc91338530e9831e9e16",
            0,
        ),
    ];
    let fee_rate = FeeRate::from_sat_per_vb(1).expect("works");
    let base_weight = Weight::from_wu(100);
    let input_weight = Weight::from_wu(50);
    let change_address = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080")
        .unwrap()
        .assume_checked();

    let (selected, change) = select_coins(
        target,
        &utxos,
        fee_rate,
        base_weight,
        input_weight,
        &change_address,
        0,
    )
    .unwrap();

    // We should select at least 2 UTXOs to meet the target
    assert!(selected.len() >= 2);

    // Verify the total amount is sufficient
    let total_selected = selected
        .iter()
        .fold(Amount::ZERO, |acc, utxo| acc + utxo.amount);
    let total_fee = fee_rate
        .fee_wu(base_weight + input_weight * selected.len() as u64)
        .unwrap();
    assert!(total_selected >= target + total_fee);

    // Verify change if present
    let change_out = change.unwrap();
    assert_eq!(change_out.script_pubkey, change_address.script_pubkey());
    assert_eq!(change_out.value, total_selected - target - total_fee);
}

#[test]
fn test_sign_psbt() {
    let secp = Secp256k1::new();

    // Generate keypairs for the committee
    let committee_keypairs = generate_keypairs(3);
    let committee_pubkeys: Vec<WeightedKey> = committee_keypairs
        .iter()
        .map(|kp| (kp.x_only_public_key().0, 1))
        .collect();

    // Generate a subnet ID
    let subnet_id = generate_subnet_id();

    // Create the multisig script and spend info
    let required_sigs = 2;
    let script = create_multisig_script(&committee_pubkeys, required_sigs).unwrap();

    // Create a multisig address
    let multisig_address = create_subnet_multisig_address(
        &secp,
        &subnet_id,
        &committee_pubkeys,
        required_sigs,
        NETWORK,
    )
    .unwrap();

    // Create a "funding UTXO" - a UTXO that sends funds to our multisig address
    let funding_amount = Amount::from_sat(100_000);
    let utxo = bitcoincore_rpc::json::ListUnspentResultEntry {
        txid: bitcoin::Txid::from_str(
            "f61b1742ca13176464adb3cb66050c00787bb3a4eead37e985f2df1e37718126",
        )
        .unwrap(),
        vout: 0,
        address: None,
        label: None,
        redeem_script: None,
        witness_script: None,
        script_pub_key: multisig_address.script_pubkey(),
        amount: funding_amount,
        confirmations: 1,
        spendable: true,
        solvable: true,
        descriptor: None,
        safe: true,
    };

    // Destination address and amount
    let destination = Address::from_str("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080")
        .unwrap()
        .assume_checked();
    let spend_amount = Amount::from_sat(50_000);

    // Construct a PSBT for spending
    let fee_rate = FeeRate::from_sat_per_vb(2).unwrap();
    let psbt = construct_spend_psbt(
        &secp,
        &subnet_id,
        &committee_pubkeys,
        2,                 // required signatures
        &multisig_address, // Use the same address for change
        &vec![utxo],
        false,
        &[bitcoin::TxOut {
            value: spend_amount,
            script_pubkey: destination.script_pubkey(),
        }],
        &fee_rate,
        0,
    )
    .unwrap();

    // Sign the PSBT with the first keypair
    let (signed_psbt, signatures) = sign_spend_psbt(&secp, psbt, committee_keypairs[0]).unwrap();

    // Verify we got exactly one signature (one for each input)
    assert_eq!(signatures.len(), 1);

    // Verify the signature is actually in the PSBT
    let (xonly_pubkey, _parity) = committee_keypairs[0].x_only_public_key();
    let found_sig = signed_psbt.inputs[0]
        .tap_script_sigs
        .iter()
        .find(|((pubkey, _), _)| *pubkey == xonly_pubkey)
        .map(|(_, sig)| sig);

    assert!(found_sig.is_some());
    assert_eq!(found_sig.unwrap().signature, signatures[0]);

    // Verify the signature is valid by checking it against the sighash
    let mut sighash_cache = SighashCache::new(&signed_psbt.unsigned_tx);
    let leaf_hash = script.tapscript_leaf_hash();

    // We need to recreate the TxOut that's being spent
    let prev_txout = TxOut {
        value: funding_amount,
        script_pubkey: multisig_address.script_pubkey(),
    };

    let sighash = sighash_cache
        .taproot_script_spend_signature_hash(
            0,
            &Prevouts::All(&[prev_txout]),
            leaf_hash,
            TapSighashType::Default,
        )
        .unwrap();

    let msg = Message::from_digest_slice(sighash.as_ref()).unwrap();

    assert!(
        secp.verify_schnorr(&signatures[0], &msg, &xonly_pubkey)
            .is_ok(),
        "Schnorr signature verification failed"
    );
}

#[test]
fn test_signing_and_spending() {
    let secp = Secp256k1::new();

    // Generate 3 keypairs for the committee
    let keypairs = generate_keypairs(3);

    for keypair in keypairs.iter() {
        let (x_only, parity) = keypair.x_only_public_key();
        println!(
            "PK = {}\nP = {:?}\nXPK = {}\nSK = {}\nADDR = {}\n\n",
            keypair.public_key(),
            parity,
            x_only,
            keypair.secret_key().display_secret(),
            alloy_primitives::Address::from_raw_public_key(
                &keypair.public_key().serialize_uncompressed()[1..]
            ),
        );
    }

    let committee_pubkeys: Vec<WeightedKey> = keypairs
        .iter()
        .map(|kp| (kp.x_only_public_key().0, 1))
        .collect();

    // Generate a subnet ID
    let subnet_id = generate_subnet_id();

    // Create a 2-of-3 multisig setup
    let required_sigs = 2_u16;
    let script = create_multisig_script(&committee_pubkeys, required_sigs.into()).unwrap();

    // Create multisig address
    let multisig_address = create_subnet_multisig_address(
        &secp,
        &subnet_id,
        &committee_pubkeys,
        required_sigs.into(),
        NETWORK,
    )
    .unwrap();

    println!("Multisig address: {}", multisig_address);

    // Create a funding UTXO that sends to our multisig address
    let funding_amount = Amount::from_sat(100_000);

    // Create the actual funding transaction (for verification later)
    let funding_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::all_zeros(),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: funding_amount,
            script_pubkey: multisig_address.script_pubkey(),
        }],
    };

    let utxo = bitcoincore_rpc::json::ListUnspentResultEntry {
        txid: funding_tx.compute_txid(),
        vout: 0,
        address: None,
        label: None,
        redeem_script: None,
        witness_script: None,
        script_pub_key: multisig_address.script_pubkey(),
        amount: funding_amount,
        confirmations: 1,
        spendable: true,
        solvable: true,
        descriptor: None,
        safe: true,
    };

    // Destination for the spending transaction
    let destination = Address::from_str("bcrt1qrj2fz0jj45y5gx9nawgmpyzegnn828vzvletjm")
        .unwrap()
        .assume_checked();
    let spend_amount = Amount::from_sat(90_000);

    let tx_out = TxOut {
        value: spend_amount,
        script_pubkey: destination.script_pubkey(),
    };

    // Create a PSBT to spend from the multisig
    let fee_rate = FeeRate::from_sat_per_vb(2).unwrap();
    let unsigned_psbt = construct_spend_psbt(
        &secp,
        &subnet_id,
        &committee_pubkeys,
        2,                 // required signatures
        &multisig_address, // Use the same address for change
        &vec![utxo.clone()],
        false,
        &[tx_out],
        &fee_rate,
        0,
    )
    .unwrap();

    // dbg!(&unsigned_psbt);

    // Sign with first keypair
    let (psbt_signed_once, sigs1) =
        sign_spend_psbt(&secp, unsigned_psbt, keypairs[0]).expect("First signature should work");

    println!("Signed with first keypair");
    assert_eq!(sigs1.len(), 1);

    // Sign with second keypair
    let (psbt_signed_twice, sigs2) = sign_spend_psbt(&secp, psbt_signed_once, keypairs[1])
        .expect("Second signature should work");

    println!("Signed with second keypair");
    assert_eq!(sigs2.len(), 1);

    // Verify the signatures are for the correct pubkeys
    let (xonly_pubkey1, _) = keypairs[0].x_only_public_key();
    let (xonly_pubkey2, _) = keypairs[1].x_only_public_key();

    // Check that signatures for both pubkeys exist in the PSBT
    let leaf_hash = script.tapscript_leaf_hash();

    assert!(psbt_signed_twice.inputs[0]
        .tap_script_sigs
        .contains_key(&(xonly_pubkey1, leaf_hash)));
    assert!(psbt_signed_twice.inputs[0]
        .tap_script_sigs
        .contains_key(&(xonly_pubkey2, leaf_hash)));

    //
    // Finalize the PSBT
    //

    let finalized_tx = finalize_spend_psbt(
        &secp,
        &subnet_id,
        &committee_pubkeys,
        required_sigs.into(),
        &psbt_signed_twice,
    )
    .expect("Failed to finalize PSBT");

    dbg!(&finalized_tx);

    // Verify the finalized transaction can spend the UTXO
    let verify_result = verify_transaction(&finalized_tx, |_| Some(funding_tx.output[0].clone()));

    assert!(
        verify_result.is_ok(),
        "Transaction should be valid with 2 of 3 signatures: {:?}",
        verify_result
    );
    println!("Transaction successfully verified with 2-of-3 signatures!");
}

#[test]
#[test_retry::retry(5)]
// For some reason this test is flaky
fn test_parallel_signing_and_spending() {
    let secp = Secp256k1::new();

    // Generate 3 keypairs for the committee
    let keypairs = generate_keypairs(3);
    let committee_pubkeys: Vec<WeightedKey> = keypairs
        .iter()
        .map(|kp| (kp.x_only_public_key().0, 1))
        .collect();

    // Generate a subnet ID
    let subnet_id = generate_subnet_id();

    // Create a 2-of-3 multisig setup
    let threshold = 2_u32;

    // Create multisig address
    let multisig_address =
        create_subnet_multisig_address(&secp, &subnet_id, &committee_pubkeys, threshold, NETWORK)
            .unwrap();

    println!("Multisig address: {}", multisig_address);

    // Create two funding UTXOs that send to our multisig address
    let funding_amount1 = Amount::from_sat(70_000);
    let funding_amount2 = Amount::from_sat(60_000);

    // Create the first funding transaction (for verification later)
    let funding_tx1 = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::all_zeros(),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: funding_amount1,
            script_pubkey: multisig_address.script_pubkey(),
        }],
    };

    // Create the second funding transaction
    let funding_tx2 = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::from_str(
                    "1111111111111111111111111111111111111111111111111111111111111111",
                )
                .unwrap(),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: funding_amount2,
            script_pubkey: multisig_address.script_pubkey(),
        }],
    };

    let utxos = vec![
        bitcoincore_rpc::json::ListUnspentResultEntry {
            txid: funding_tx1.compute_txid(),
            vout: 0,
            address: None,
            label: None,
            redeem_script: None,
            witness_script: None,
            script_pub_key: multisig_address.script_pubkey(),
            amount: funding_amount1,
            confirmations: 1,
            spendable: true,
            solvable: true,
            descriptor: None,
            safe: true,
        },
        bitcoincore_rpc::json::ListUnspentResultEntry {
            txid: funding_tx2.compute_txid(),
            vout: 0,
            address: None,
            label: None,
            redeem_script: None,
            witness_script: None,
            script_pub_key: multisig_address.script_pubkey(),
            amount: funding_amount2,
            confirmations: 1,
            spendable: true,
            solvable: true,
            descriptor: None,
            safe: true,
        },
    ];

    // Destination for the spending transaction
    let destination = Address::from_str("bcrt1qrj2fz0jj45y5gx9nawgmpyzegnn828vzvletjm")
        .unwrap()
        .assume_checked();
    let spend_amount = Amount::from_sat(90_000);
    let tx_out = TxOut {
        value: spend_amount,
        script_pubkey: destination.script_pubkey(),
    };

    // Create a PSBT to spend from the multisig
    let fee_rate = FeeRate::from_sat_per_vb(2).unwrap();
    let unsigned_psbt = construct_spend_psbt(
        &secp,
        &subnet_id,
        &committee_pubkeys,
        2,                 // required signatures
        &multisig_address, // Use the same address for change
        &utxos,
        false,
        &[tx_out],
        &fee_rate,
        0,
    )
    .unwrap();

    // Verify the PSBT has two inputs (one for each UTXO)
    assert_eq!(unsigned_psbt.inputs.len(), 2, "PSBT should have two inputs");

    // Clone the unsigned PSBT for each signer
    let unsigned_psbt_for_signer1 = unsigned_psbt.clone();
    let unsigned_psbt_for_signer2 = unsigned_psbt.clone();

    // Each signer signs their own copy of the PSBT independently
    let (_, sigs1) = sign_spend_psbt(&secp, unsigned_psbt_for_signer1, keypairs[0])
        .expect("First signature should work");

    let (_, sigs2) = sign_spend_psbt(&secp, unsigned_psbt_for_signer2, keypairs[1])
        .expect("Second signature should work");

    println!("Signed independently with two keypairs");

    // Verify we have signatures for both inputs
    assert_eq!(sigs1.len(), 2, "First signer should sign both inputs");
    assert_eq!(sigs2.len(), 2, "Second signer should sign both inputs");

    // Create a fresh PSBT and manually add all signatures
    let finalized_tx = finalize_spend_psbt_from_sigs(
        &secp,
        &subnet_id,
        &committee_pubkeys,
        threshold,
        &unsigned_psbt,
        &[&sigs1, &sigs2, &[]],
    )
    .expect("should finalize");

    // Verify the finalized transaction can spend the UTXO
    let verify_result = verify_transaction(&finalized_tx, |outpoint| {
        dbg!(&outpoint);
        dbg!(funding_tx1.compute_txid());
        dbg!(funding_tx2.compute_txid());

        if outpoint.txid == funding_tx1.compute_txid() && outpoint.vout == 0 {
            Some(funding_tx1.output[0].clone())
        } else if outpoint.txid == funding_tx2.compute_txid() && outpoint.vout == 0 {
            Some(funding_tx2.output[0].clone())
        } else {
            None
        }
    });

    assert!(
        verify_result.is_ok(),
        "Transaction should be valid with signatures collected in parallel but in reverse: {:?}",
        verify_result
    );

    let committe_pubkeys_reverse = committee_pubkeys.iter().rev().cloned().collect::<Vec<_>>();

    // Create a fresh PSBT and manually add all signatures
    let finalized_tx = finalize_spend_psbt_from_sigs(
        &secp,
        &subnet_id,
        &committe_pubkeys_reverse,
        threshold,
        &unsigned_psbt,
        &[&[], &sigs2, &sigs1],
    )
    .expect("should finalize");

    // Verify the finalized transaction can spend the UTXO
    let verify_result = verify_transaction(&finalized_tx, |outpoint| {
        dbg!(&outpoint);
        dbg!(funding_tx1.compute_txid());
        dbg!(funding_tx2.compute_txid());

        if outpoint.txid == funding_tx1.compute_txid() && outpoint.vout == 0 {
            Some(funding_tx1.output[0].clone())
        } else if outpoint.txid == funding_tx2.compute_txid() && outpoint.vout == 0 {
            Some(funding_tx2.output[0].clone())
        } else {
            None
        }
    });

    assert!(
        verify_result.is_ok(),
        "Transaction should be valid with signatures collected in parallel: {:?}",
        verify_result
    );

    println!("Transaction successfully verified with signatures collected in parallel!");
}

#[test]
fn test_weighted_spend() {
    let secp = Secp256k1::new();

    // Generate 4 keypairs with different weights
    let keypairs = crate::test_utils::generate_keypairs(4);

    // Assign different weights to each validator:
    // - Validator 0: 10 power (heavy)
    // - Validator 1: 5 power (medium)
    // - Validator 2: 3 power (light)
    // - Validator 3: 2 power (light)
    // Total power: 20
    let committee_pubkeys: Vec<WeightedKey> = vec![
        (keypairs[0].x_only_public_key().0, 10), // Heavy validator
        (keypairs[1].x_only_public_key().0, 5),  // Medium validator
        (keypairs[2].x_only_public_key().0, 3),  // Light validator
        (keypairs[3].x_only_public_key().0, 2),  // Light validator
    ];

    // Set threshold to 12 (60% of total power)
    let threshold = 12_u32;
    let subnet_id = generate_subnet_id();

    // Create multisig address
    let network = bitcoin::Network::Regtest;
    let multisig_address =
        create_subnet_multisig_address(&secp, &subnet_id, &committee_pubkeys, threshold, network)
            .unwrap();

    println!("Multisig address: {}", multisig_address);

    // Create the multisig script
    let script = create_multisig_script(&committee_pubkeys, threshold).unwrap();
    let leaf_hash = script.tapscript_leaf_hash();

    // Get spend info for the multisig
    let spend_info =
        create_subnet_multisig_spend_info(&secp, &subnet_id, &committee_pubkeys, threshold)
            .unwrap();

    // Create control block
    let control_block = spend_info
        .control_block(&(script.clone(), LeafVersion::TapScript))
        .unwrap();

    // Create a funding transaction that sends to our multisig
    let funding_amount = Amount::from_sat(100_000);
    let funding_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::all_zeros(),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: funding_amount,
            script_pubkey: multisig_address.script_pubkey(),
        }],
    };
    let funding_txid = funding_tx.compute_txid();

    // Create spending transaction
    let spending_amount = Amount::from_sat(90_000);
    let spending_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: funding_txid,
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: spending_amount,
            script_pubkey: ScriptBuf::new_op_return([1]),
        }],
    };

    // Create sighash for signing
    let mut sighash_cache = SighashCache::new(&spending_tx);
    let sighash = sighash_cache
        .taproot_script_spend_signature_hash(
            0,
            &Prevouts::All(&[funding_tx.output[0].clone()]),
            leaf_hash,
            TapSighashType::Default,
        )
        .expect("Failed to create sighash");

    // Test case 1: Only the heavy validator (10 power) - insufficient
    {
        let mut tx_insufficient = spending_tx.clone();
        let mut witness = Witness::new();

        // Sign with the heavy validator only (10 power)
        let msg = Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");
        let sig = secp.sign_schnorr(&msg, &keypairs[0]);

        // Add signature for validator 0 (heavy)
        witness.push(sig.serialize());

        // Empty signatures for the rest
        for _ in 1..keypairs.len() {
            witness.push([]);
        }

        witness.push(script.to_bytes());
        witness.push(control_block.serialize());

        tx_insufficient.input[0].witness = witness;

        let verify_result = super::tests::verify_transaction(&tx_insufficient, |_| {
            Some(funding_tx.output[0].clone())
        });

        // Should fail: 10 power < 12 threshold
        assert!(
            verify_result.is_err(),
            "Transaction with only the heavy validator should fail"
        );
        println!("Test case 1 passed: Heavy validator alone (10 power) cannot meet threshold");
    }

    // Test case 2: Heavy validator (10) + Light validator (2) = 12 power (exactly threshold)
    {
        let mut tx_sufficient = spending_tx.clone();
        let mut witness = Witness::new();

        // Empty signature array in reverse order of committee
        let mut signatures = vec![vec![]; keypairs.len()];

        // Sign with validators 0 (heavy, 10 power) and 3 (light, 2 power)
        let msg = Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");

        // Sign with heavy validator (index 0)
        let sig0 = secp.sign_schnorr(&msg, &keypairs[0]);
        signatures[0] = sig0.serialize().to_vec();

        // Sign with light validator (index 3)
        let sig3 = secp.sign_schnorr(&msg, &keypairs[3]);
        signatures[3] = sig3.serialize().to_vec();

        // Add signatures in reverse order
        for sig_bytes in signatures.iter().rev() {
            if sig_bytes.is_empty() {
                witness.push([]);
            } else {
                witness.push(sig_bytes.as_slice());
            }
        }

        witness.push(script.to_bytes());
        witness.push(control_block.serialize());

        tx_sufficient.input[0].witness = witness;

        let verify_result = super::tests::verify_transaction(&tx_sufficient, |_| {
            Some(funding_tx.output[0].clone())
        });

        // Should succeed: 10 + 2 = 12 power = threshold
        assert!(
            verify_result.is_ok(),
            "Transaction with exactly threshold power should succeed: {:?}",
            verify_result
        );
        println!("Test case 2 passed: Heavy validator (10 power) + Light validator (2 power) = 12 power reaches threshold");
    }

    // Test case 3: Medium validator (5) + two Light validators (3+2=5) = 10 power (insufficient)
    {
        let mut tx_insufficient = spending_tx.clone();
        let mut witness = Witness::new();

        // Empty signature array in reverse order of committee
        let mut signatures = vec![vec![]; keypairs.len()];

        // Sign with validators 1 (medium, 5 power), 2 (light, 3 power), and 3 (light, 2 power)
        let msg = Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");

        // Sign with medium validator (index 1)
        let sig1 = secp.sign_schnorr(&msg, &keypairs[1]);
        signatures[1] = sig1.serialize().to_vec();

        // Sign with light validator (index 2)
        let sig2 = secp.sign_schnorr(&msg, &keypairs[2]);
        signatures[2] = sig2.serialize().to_vec();

        // Sign with light validator (index 3)
        let sig3 = secp.sign_schnorr(&msg, &keypairs[3]);
        signatures[3] = sig3.serialize().to_vec();

        // Add signatures in reverse order
        for sig_bytes in signatures.iter().rev() {
            if sig_bytes.is_empty() {
                witness.push([]);
            } else {
                witness.push(sig_bytes.as_slice());
            }
        }

        witness.push(script.to_bytes());
        witness.push(control_block.serialize());

        tx_insufficient.input[0].witness = witness;

        let verify_result = super::tests::verify_transaction(&tx_insufficient, |_| {
            Some(funding_tx.output[0].clone())
        });

        // Should fail: 5 + 3 + 2 = 10 power < 12 threshold
        assert!(
            verify_result.is_err(),
            "Transaction with 3 validators but insufficient power should fail"
        );
        println!("Test case 3 passed: Medium (5) + two Light validators (3+2) = 10 power cannot meet threshold");
    }

    // Test case 4: Heavy validator (10) + Medium validator (5) = 15 power (above threshold)
    {
        let mut tx_above_threshold = spending_tx.clone();
        let mut witness = Witness::new();

        // Empty signature array in reverse order of committee
        let mut signatures = vec![vec![]; keypairs.len()];

        // Sign with validators 0 (heavy, 10 power) and 1 (medium, 5 power)
        let msg = Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");

        // Sign with heavy validator (index 0)
        let sig0 = secp.sign_schnorr(&msg, &keypairs[0]);
        signatures[0] = sig0.serialize().to_vec();

        // Sign with medium validator (index 1)
        let sig1 = secp.sign_schnorr(&msg, &keypairs[1]);
        signatures[1] = sig1.serialize().to_vec();

        // Add signatures in reverse order
        for sig_bytes in signatures.iter().rev() {
            if sig_bytes.is_empty() {
                witness.push([]);
            } else {
                witness.push(sig_bytes.as_slice());
            }
        }

        witness.push(script.to_bytes());
        witness.push(control_block.serialize());

        tx_above_threshold.input[0].witness = witness;

        let verify_result = super::tests::verify_transaction(&tx_above_threshold, |_| {
            Some(funding_tx.output[0].clone())
        });

        // Should succeed: 10 + 5 = 15 power > 12 threshold
        assert!(
            verify_result.is_ok(),
            "Transaction with above threshold power should succeed"
        );
        println!(
            "Test case 4 passed: Heavy (10) + Medium validator (5) = 15 power exceeds threshold"
        );
    }
}

#[test]
fn test_exhaust_unspent() {
    let secp = Secp256k1::new();

    // Create committee keys
    let keypairs = generate_keypairs(3);
    let committee_keys: Vec<WeightedKey> = keypairs
        .iter()
        .map(|kp| (kp.x_only_public_key().0, 1))
        .collect();

    // Generate subnet ID and address
    let subnet_id = generate_subnet_id();
    let network = bitcoin::Network::Regtest;
    let committee_address = create_subnet_multisig_address(
        &secp,
        &subnet_id,
        &committee_keys,
        2, // threshold
        network,
    )
    .unwrap();

    // Create UTXOs with different amounts
    let utxos = vec![
        create_test_utxo(
            5000,
            "7224e1f11ddc838100abd123d23af0d02493001fdd746685dc539fe062b45e3e",
            0,
        ),
        create_test_utxo(
            3000,
            "d7f3553b9631f48a2842a2cb6e0f2b6e344bf82d3ee78295a5361adc17b838b1",
            0,
        ),
        create_test_utxo(
            9000,
            "f4184fc596403b9d638783cf57adfe4c75c605f6356fbc91338530e9831e9e16",
            0,
        ),
    ];

    // Calculate total input amount
    let total_input_amount = utxos.iter().map(|utxo| utxo.amount).sum::<Amount>();

    // Define output
    let destination = Address::from_str("bcrt1qzswe5l7xyzvgfn4v9s96r3cxtlxhq87x2etpp7")
        .unwrap()
        .assume_checked();

    let output_amount = Amount::from_sat(1000);
    let tx_out = TxOut {
        value: output_amount,
        script_pubkey: destination.script_pubkey(),
    };

    let fee_rate = FeeRate::from_sat_per_vb(2).unwrap();

    // Case 1: Normal spend (don't exhaust UTXOs)
    let tx_normal = construct_spend_unsigned_transaction(
        &committee_keys,
        2, // threshold
        &committee_address,
        &utxos,
        false, // don't exhaust
        &[tx_out.clone()],
        &fee_rate,
        0,
    )
    .unwrap();

    // Should select just enough UTXOs to cover the output
    assert!(
        tx_normal.input.len() < utxos.len(),
        "Normal spend should not use all UTXOs, used {} of {}",
        tx_normal.input.len(),
        utxos.len()
    );

    // Case 2: Exhaust UTXOs
    let tx_exhaust = construct_spend_unsigned_transaction(
        &committee_keys,
        2, // threshold
        &committee_address,
        &utxos,
        true, // exhaust all UTXOs
        &[tx_out.clone()],
        &fee_rate,
        0,
    )
    .unwrap();

    // Should use all UTXOs
    assert_eq!(
        tx_exhaust.input.len(),
        utxos.len(),
        "Exhaust spend should use all UTXOs"
    );

    // Should have exactly one change output plus the specified output
    assert_eq!(
        tx_exhaust.output.len(),
        2,
        "Exhaust spend should have original output plus change"
    );

    // Verify the change goes to the committee address
    assert_eq!(
        tx_exhaust.output[1].script_pubkey,
        committee_address.script_pubkey(),
        "Change output has incorrect script pubkey"
    );

    // Verify change amount is approximately what we expect
    // (total_inputs - output_amount - fees)
    let change_output_amount = tx_exhaust.output[1].value;
    let spent_amount = output_amount;
    let fee_estimate = fee_rate.fee_wu(tx_exhaust.weight()).unwrap();

    assert!(
        change_output_amount < total_input_amount - spent_amount,
        "Change amount should be less than total input minus spent amount"
    );

    // Check that the change plus output plus estimated fee is approximately equal to total input
    let total_accounted_for = change_output_amount + spent_amount + fee_estimate;
    let margin = Amount::from_sat(600); // Allow for a small margin of error in fee calculation

    assert!(
        total_accounted_for <= total_input_amount,
        "Total outputs plus fee ({}) should not exceed total input ({})",
        total_accounted_for,
        total_input_amount
    );

    dbg!(&tx_exhaust);
    dbg!(total_input_amount - total_accounted_for);

    assert!(
        total_input_amount - total_accounted_for <= margin,
        "Difference between total input and (outputs + fee) should be small"
    );

    // Case 3: Exhaust UTXOs with multiple outputs
    let tx_out2 = TxOut {
        value: Amount::from_sat(2000),
        script_pubkey: destination.script_pubkey(),
    };

    let tx_exhaust_multi = construct_spend_unsigned_transaction(
        &committee_keys,
        2, // threshold
        &committee_address,
        &utxos,
        true, // exhaust all UTXOs
        &[tx_out, tx_out2],
        &fee_rate,
        0,
    )
    .unwrap();

    // Should have exactly one change output plus the two specified outputs
    assert_eq!(
        tx_exhaust_multi.output.len(),
        3,
        "Exhaust spend with multiple outputs should have all outputs plus change"
    );

    // Verify change amount reflects all outputs
    let total_outputs = tx_exhaust_multi
        .output
        .iter()
        .filter(|out| out.script_pubkey != committee_address.script_pubkey())
        .map(|out| out.value)
        .sum::<Amount>();

    let multi_change_output_amount = tx_exhaust_multi
        .output
        .iter()
        .find(|out| out.script_pubkey == committee_address.script_pubkey())
        .map(|out| out.value)
        .unwrap();

    assert!(
        multi_change_output_amount < total_input_amount - total_outputs,
        "Multi-output change amount should be less than total input minus all outputs"
    );
}
