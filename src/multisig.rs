use std::vec;

use log::error;
use thiserror::Error;

use bitcoin::{
    blockdata::script::Builder,
    hashes::Hash,
    opcodes,
    secp256k1::{All, Secp256k1},
    taproot::{TaprootBuilder, TaprootSpendInfo},
    Address, Network, ScriptBuf, XOnlyPublicKey,
};

use crate::bitcoin_utils::create_unspendable_internal_key;

use crate::SubnetId;

pub fn create_multisig_script(
    public_keys: &[XOnlyPublicKey],
    required_sigs: i64,
) -> Result<ScriptBuf, MultisigError> {
    // check if enough public keys are provided
    if (public_keys.len() as i64) < required_sigs {
        return Err(MultisigError::InsufficientPublicKeys);
    }

    // Public keys need to be sorted for consistent scriptPubKey
    let mut sorted_public_keys = public_keys.to_vec();
    sorted_public_keys.sort();

    Ok(sorted_public_keys
        .iter()
        .enumerate()
        .fold(Builder::new(), |builder, (index, key)| {
            let builder = builder.push_x_only_key(key);
            if index == 0 {
                builder.push_opcode(opcodes::all::OP_CHECKSIG)
            } else {
                builder.push_opcode(opcodes::all::OP_CHECKSIGADD)
            }
        })
        .push_int(required_sigs)
        .push_opcode(opcodes::all::OP_GREATERTHANOREQUAL)
        .into_script())
}

/// Creates an unspendable script that includes the subnet id
/// This is to ensure that the multisig is unique per subnet regardless
/// of the validator public keys.
///
/// Different subnets will have different multisig addresses, even if the
/// public keys are the same.
fn create_unspendable_subnet_id_script(subnet_id: &SubnetId) -> ScriptBuf {
    Builder::new()
        .push_opcode(opcodes::all::OP_RETURN)
        .push_slice(subnet_id.txid().as_byte_array())
        .into_script()
}

pub fn create_subnet_multisig_spend_info(
    secp: &Secp256k1<All>,
    subnet_id: &SubnetId,
    public_keys: &[XOnlyPublicKey],
    required_sigs: i64,
) -> Result<TaprootSpendInfo, MultisigError> {
    let multisig_script = create_multisig_script(public_keys, required_sigs)?;
    let subnet_id_script = create_unspendable_subnet_id_script(subnet_id);

    let builder =
        TaprootBuilder::with_huffman_tree(vec![(1, multisig_script), (0, subnet_id_script)])?;
    let internal_key = create_unspendable_internal_key();
    let spend_info = builder
        .finalize(secp, internal_key)
        .map_err(|_| MultisigError::TaprootBuilderNotFinalizable)?;

    Ok(spend_info)
}

pub fn create_subnet_multisig_address(
    secp: &Secp256k1<All>,
    subnet_id: &SubnetId,
    public_keys: &[XOnlyPublicKey],
    required_sigs: i64,
    network: Network,
) -> Result<Address, MultisigError> {
    let spend_info =
        create_subnet_multisig_spend_info(secp, subnet_id, public_keys, required_sigs)?;

    Ok(Address::p2tr(
        secp,
        spend_info.internal_key(),
        spend_info.merkle_root(),
        network,
    ))
}

pub fn multisig_threshold(participants: u16) -> u16 {
    // TODO figure out threshold
    (participants / 2) + 1
}

#[derive(Error, Debug)]
pub enum MultisigError {
    #[error("insufficient public keys provided")]
    InsufficientPublicKeys,

    #[error("taproot builder is not finalizable")]
    TaprootBuilderNotFinalizable,

    #[error("an error occured when building a taproot transaction")]
    TaprootBuilderError(#[from] bitcoin::taproot::TaprootBuilderError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::consensus::encode;
    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::Message;
    use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
    use bitcoin::taproot::LeafVersion;
    use bitcoin::{
        absolute::LockTime,
        key::Keypair,
        secp256k1::{PublicKey, Secp256k1, SecretKey},
        AddressType, Amount, Network, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
    };
    use bitcoin::{transaction, OutPoint};

    /// Verifies that this transaction is able to spend its inputs.
    ///
    /// The `spent` closure should return the [`TxOut`] for the given [`OutPoint`] (the ones we're spending).
    /// The `spent` closure should not return the same [`TxOut`] twice!
    pub fn verify_transaction<S>(
        tx: &Transaction,
        mut spent: S,
    ) -> Result<(), bitcoinconsensus::Error>
    where
        S: FnMut(&OutPoint) -> Option<TxOut>,
    {
        let serialized_tx = encode::serialize(tx);
        for (idx, input) in tx.input.iter().enumerate() {
            if let Some(output) = spent(&input.previous_output) {
                // duplicating the same output because bitcoinconsensus is weird
                // this is needed for taproot verification
                let spent_utxo = bitcoinconsensus::Utxo {
                    script_pubkey: output.script_pubkey.as_bytes().as_ptr(),
                    script_pubkey_len: output.script_pubkey.len() as u32,
                    value: output.value.to_sat() as i64,
                };

                bitcoinconsensus::verify_with_flags(
                    output.script_pubkey.as_bytes(),
                    output.value.to_sat(),
                    serialized_tx.as_slice(),
                    Some(&[spent_utxo]),
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

    fn generate_xonly_pubkeys(n: usize) -> Vec<XOnlyPublicKey> {
        let secp = Secp256k1::new();
        (0..n)
            .map(|_| {
                let secret_key = SecretKey::new(&mut rand::thread_rng());
                let public_key = PublicKey::from_secret_key(&secp, &secret_key);
                XOnlyPublicKey::from(public_key)
            })
            .collect()
    }

    fn generate_keypairs(n: usize) -> Vec<Keypair> {
        let secp = Secp256k1::new();
        let mut keypairs: Vec<Keypair> = (0..n)
            .map(|_| {
                let mut secret_key = SecretKey::new(&mut rand::thread_rng());
                if secret_key.x_only_public_key(&secp).1 == bitcoin::key::Parity::Odd {
                    secret_key = secret_key.negate();
                }
                Keypair::from_secret_key(&secp, &secret_key)
            })
            .collect();
        // sort keypairs by x-only public key
        keypairs.sort_by_key(|k| k.x_only_public_key());
        keypairs
    }

    fn generate_subnet_id() -> SubnetId {
        SubnetId::from_txid(&Txid::from_slice(&rand::random::<[u8; 32]>()).unwrap())
    }

    #[test]
    fn test_create_multisig_address_single_key() {
        let secp = Secp256k1::new();
        let public_keys = generate_xonly_pubkeys(1);
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
        let public_keys = generate_xonly_pubkeys(3);
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
        let public_keys = generate_xonly_pubkeys(1);
        let required_sigs = 2; // More signatures required than keys available
        let network = Network::Bitcoin;

        let result = create_subnet_multisig_address(
            &secp,
            &generate_subnet_id(),
            &public_keys,
            required_sigs,
            network,
        );

        assert!(matches!(result, Err(MultisigError::InsufficientPublicKeys)));
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

        let public_keys: Vec<XOnlyPublicKey> =
            keypairs.iter().map(|kp| kp.x_only_public_key().0).collect();

        let required_sigs = 3;
        let network = Network::Regtest;

        let subnet_id = generate_subnet_id();

        let script = create_multisig_script(&public_keys, required_sigs)
            .expect("Failed to create multisig script");

        dbg!(&script);

        let spend_info =
            create_subnet_multisig_spend_info(&secp, &subnet_id, &public_keys, required_sigs)
                .expect("Failed to create multisig spend info");

        let control_block = spend_info
            .control_block(&(script.clone(), LeafVersion::TapScript))
            .expect("Should create control block");

        let multisig_address =
            create_subnet_multisig_address(&secp, &subnet_id, &public_keys, required_sigs, network)
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
        // Case 2: Valid spend with required signatures (3 of 3)
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

            tx_valid.input[0].witness = witness;

            let verify_result =
                verify_transaction(&tx_valid, |_| Some(funding_tx.output[0].clone()));

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
        let public_keys = generate_xonly_pubkeys(3);
        let required_sigs = 2;
        let network = Network::Bitcoin;

        let subnet_id_1 = generate_subnet_id();
        let subnet_id_2 = generate_subnet_id();

        let address_1 = create_subnet_multisig_address(
            &secp,
            &subnet_id_1,
            &public_keys,
            required_sigs,
            network,
        )
        .expect("Failed to create first multisig address");
        dbg!(&address_1);

        let address_2 = create_subnet_multisig_address(
            &secp,
            &subnet_id_2,
            &public_keys,
            required_sigs,
            network,
        )
        .expect("Failed to create second multisig address");
        dbg!(&address_2);

        assert_ne!(
            address_1, address_2,
            "Addresses should be different with different subnet IDs"
        );
    }
}
