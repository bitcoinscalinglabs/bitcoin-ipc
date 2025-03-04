use std::vec;

use log::{error, trace};
use thiserror::Error;

use bitcoin::{
    absolute::LockTime,
    blockdata::script::Builder,
    hashes::Hash,
    opcodes,
    secp256k1::{All, Secp256k1},
    taproot::{LeafVersion, TaprootBuilder, TaprootSpendInfo},
    Address, Amount, FeeRate, Network, ScriptBuf, Transaction, TxIn, TxOut, VarInt, Weight,
    Witness, XOnlyPublicKey,
};

use crate::bitcoin_utils::unspenable_internal_key;

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
    let internal_key = unspenable_internal_key();
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

/// Calculates the size in bytes of witness elements for spending a multisig utxo
/// Useful for fee calculations.
pub fn multisig_spend_witness_size(committee_size: u16, committee_threshold: u16) -> usize {
    // Each required signature
    let signatures_size =
        bitcoin::key::constants::SCHNORR_SIGNATURE_SIZE * committee_threshold as usize;

    // For each unused key position, we need an empty signature (represented as empty vector with 1 byte length)
    // In witness format, an empty element still takes 1 byte (the length byte indicating zero-length data)
    let empty_sigs_size = (committee_size - committee_threshold) as usize;

    // Script size: all x-only public keys (32 bytes each) plus opcodes
    // - Each key needs 1 byte push operation (for the 32-byte key) = 33 bytes per key
    // - 1 byte for each OP_CHECKSIG or OP_CHECKSIGADD
    // - 1 byte for integer push (required_sigs)
    // - 1 byte for OP_GREATERTHANOREQUAL
    let script_size =
        (bitcoin::key::constants::SCHNORR_PUBLIC_KEY_SIZE + 2) * committee_size as usize + 2;

    // Control block calculation:
    // - TAPROOT_CONTROL_BASE_SIZE (33 bytes: 1 byte version + 32 bytes internal key)
    // - TAPROOT_CONTROL_NODE_SIZE (32 bytes for the merkle node)
    let control_block_content_size =
        bitcoin::taproot::TAPROOT_CONTROL_BASE_SIZE + bitcoin::taproot::TAPROOT_CONTROL_NODE_SIZE;

    let var_ints =
	    // varint for the number of witnesses, which is a signature for each committee member + script + control block
	    bitcoin::VarInt::from(committee_size + 2).size()
	    // each schnorr sig
        + VarInt::from(bitcoin::key::constants::SCHNORR_SIGNATURE_SIZE).size() * committee_threshold as usize
        // script size
        + VarInt::from(script_size).size()
        // control block size
        + VarInt::from(control_block_content_size).size();

    signatures_size + empty_sigs_size + script_size + control_block_content_size + var_ints
}

/// Selects UTXOs to spend for a transaction
/// Returns the selected UTXOs and the change output if any
///
/// It uses the largest utxos first
// TODO improve coin selection algorithm
pub fn select_coins(
    target: Amount,
    utxos: &[bitcoincore_rpc::json::ListUnspentResultEntry],
    fee_rate: FeeRate,
    base_tx_weight: Weight,
    satisfaction_weight_per_input: Weight,
    change_address: &Address,
) -> Result<
    (
        // The list of selected UTXOs
        Vec<bitcoincore_rpc::json::ListUnspentResultEntry>,
        // Optional change output
        Option<TxOut>,
    ),
    MultisigError,
> {
    let mut utxos = utxos.to_vec();
    // Sort UTXOs deterministically by amount, txid and vout
    utxos.sort_by(|a, b| {
        b.amount
            .cmp(&a.amount)
            .then(a.txid.cmp(&b.txid))
            .then(a.vout.cmp(&b.vout))
    });

    trace!(
        "Selecting coins for target amount {}. Unspent: {:?}",
        target,
        utxos
    );

    let non_dust_change_tx_out = bitcoin::TxOut::minimal_non_dust(change_address.script_pubkey());

    let mut selected = Vec::with_capacity(utxos.len());
    let mut total_amount = Amount::ZERO;
    let mut total_weight = base_tx_weight;

    for utxo in utxos {
        // Append the utxo
        selected.push(utxo.clone());

        total_amount += utxo.amount;
        total_weight += satisfaction_weight_per_input;

        let total_fee = fee_rate
            .fee_wu(total_weight)
            .expect("fee rate shouldn't overflow");

        if total_amount >= target + total_fee {
            let change = total_amount - target - total_fee;
            trace!(
                "Selected coins for target amount {}. Total amount: {}, total fee: {}, change: {}",
                target,
                total_amount,
                total_fee,
                change
            );
            dbg!(change, non_dust_change_tx_out.value);

            // if change is non-dust, return it
            if change > non_dust_change_tx_out.value {
                let change_tx_out = bitcoin::TxOut {
                    value: change,
                    script_pubkey: change_address.script_pubkey(),
                };
                return Ok((selected, Some(change_tx_out)));
            } else {
                return Ok((selected, None));
            }
        }
    }

    Err(MultisigError::CoinSelectionFailed(
        "Insufficient funds".to_string(),
    ))
}

pub fn construct_spend_unsigned_transaction(
    committee_size: u16,
    committee_threshold: u16,
    committee_change_address: &Address,
    unspent: &[bitcoincore_rpc::json::ListUnspentResultEntry],
    to: &Address,
    amount: Amount,
    fee_rate: &FeeRate,
) -> Result<Transaction, MultisigError> {
    trace!(
        "Creating multisig spend tx for address={}, amount={}",
        &to,
        &amount
    );

    // size of the witness data when spending a multisig utxo
    let spend_witness_size = multisig_spend_witness_size(committee_size, committee_threshold);
    // base input size
    let spend_txin_size = bitcoin::TxIn::default().base_size();
    // weight of spending one input
    let spending_weight_per_input = Weight::from_non_witness_data_size(spend_txin_size as u64)
        + Weight::from_witness_data_size(spend_witness_size as u64);

    let tx_out = bitcoin::TxOut {
        value: amount,
        script_pubkey: to.script_pubkey(),
    };
    let non_dust_change_tx_out =
        bitcoin::TxOut::minimal_non_dust(committee_change_address.script_pubkey());

    // base transaction, assume a change output
    let base_tx = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![],
        output: vec![tx_out.clone(), non_dust_change_tx_out],
    };
    let base_tx_weight = base_tx.weight();

    // coin selection from utxos
    let (selected_utxos, change) = select_coins(
        amount,
        unspent,
        *fee_rate,
        base_tx_weight,
        spending_weight_per_input,
        committee_change_address,
    )?;

    let inputs = selected_utxos
        .iter()
        .map(|utxo| TxIn {
            previous_output: bitcoin::OutPoint {
                txid: utxo.txid,
                vout: utxo.vout,
            },
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence::MAX,
            witness: Witness::new(),
        })
        .collect::<Vec<TxIn>>();

    let mut spend_tx = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: inputs,
        output: vec![tx_out],
    };
    if let Some(change_tx_out) = change {
        spend_tx.output.push(change_tx_out);
    }

    Ok(spend_tx)
}

/// Constructs a PSBT for spending a multisig output
#[allow(clippy::too_many_arguments)]
pub fn construct_spend_psbt(
    secp: &Secp256k1<All>,
    subnet_id: &SubnetId,
    committee_keys: &[XOnlyPublicKey],
    committee_threshold: u16,
    committee_change_address: &Address,
    unspent: &[bitcoincore_rpc::json::ListUnspentResultEntry],
    to: &Address,
    amount: Amount,
    fee_rate: &FeeRate,
) -> Result<bitcoin::Psbt, MultisigError> {
    trace!(
        "Creating multisig spend PSBT for address={}, amount={}",
        &to,
        &amount
    );

    let committee_size: u16 = committee_keys
        .len()
        .try_into()
        .map_err(|_| MultisigError::PsbtError("Committee size too large".to_string()))?;

    // First construct the unsigned transaction
    let unsigned_tx = construct_spend_unsigned_transaction(
        committee_size,
        committee_threshold,
        committee_change_address,
        unspent,
        to,
        amount,
        fee_rate,
    )?;

    // Create the PSBT from the unsigned transaction
    let mut psbt = bitcoin::psbt::Psbt::from_unsigned_tx(unsigned_tx.clone())
        .map_err(|e| MultisigError::CoinSelectionFailed(format!("Failed to create PSBT: {}", e)))?;

    let script = create_multisig_script(committee_keys, committee_threshold.into())?;

    // Get the spend info for the multisig
    let spend_info = create_subnet_multisig_spend_info(
        secp,
        subnet_id,
        committee_keys,
        committee_threshold.into(),
    )?;

    // Create the control block
    let control_block = spend_info
        .control_block(&(script.clone(), LeafVersion::TapScript))
        .ok_or(MultisigError::TaprootBuilderNotFinalizable)?;

    for (psbt_input_index, psbt_input) in psbt.inputs.iter_mut().enumerate() {
        // Find the matching UTXO from our list
        let utxo = unspent
            .iter()
            .find(|u| {
                u.txid == unsigned_tx.input[psbt_input_index].previous_output.txid
                    && u.vout == unsigned_tx.input[psbt_input_index].previous_output.vout
            })
            .ok_or_else(|| {
                MultisigError::PsbtError(format!(
                    "Cannot find matching utxo for {psbt_input_index}"
                ))
            })?;

        psbt_input.witness_script = Some(script.clone());

        // 2. Taproot-specific fields
        psbt_input.tap_script_sigs = Default::default();
        let mut tap_scripts = std::collections::BTreeMap::new();
        tap_scripts.insert(
            control_block.clone(),
            (script.clone(), LeafVersion::TapScript),
        );
        psbt_input.tap_scripts = tap_scripts;
        psbt_input.tap_merkle_root = spend_info.merkle_root();
        psbt_input.tap_internal_key = Some(spend_info.internal_key());

        // 3. Previous UTXO information
        psbt_input.witness_utxo = Some(TxOut {
            value: utxo.amount,
            script_pubkey: utxo.script_pub_key.clone(),
        });
    }

    Ok(psbt)
}

pub fn sign_spend_psbt(
    secp: &Secp256k1<All>,
    mut psbt: bitcoin::Psbt,
    keypair: bitcoin::key::Keypair,
) -> Result<(bitcoin::Psbt, Vec<bitcoin::taproot::Signature>), MultisigError> {
    let (xonly_pubkey, _parity) = keypair.x_only_public_key();
    let mut signatures = Vec::new();

    let all_witness_utxos: Vec<TxOut> = psbt
        .iter_funding_utxos()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| MultisigError::SigningError(format!("Failed to collect UTXOs: {}", e)))?
        .into_iter()
        .cloned()
        .collect();

    // Create a sighash cache once for the transaction
    let mut sighash_cache = bitcoin::sighash::SighashCache::new(&psbt.unsigned_tx);
    let sighash_type = bitcoin::sighash::TapSighashType::Default;

    // For each input in the PSBT
    for (input_index, input) in psbt.inputs.iter_mut().enumerate() {
        // dbg!(input_index, &input);

        // We need the script from tap_scripts
        let (_control_block, (script, _leaf_version)) = match input.tap_scripts.iter().next() {
            Some(entry) => entry,
            None => continue,
        };

        let leaf_hash = script.tapscript_leaf_hash();
        let prevouts = bitcoin::sighash::Prevouts::All(&all_witness_utxos);

        // Calculate the sighash
        let sighash = sighash_cache
            .taproot_script_spend_signature_hash(input_index, &prevouts, leaf_hash, sighash_type)
            .map_err(|e| {
                MultisigError::SigningError(format!(
                    "Failed to create sighash for input {}: {}",
                    input_index, e
                ))
            })?;

        // Create message from sighash
        let msg =
            bitcoin::secp256k1::Message::from_digest_slice(sighash.as_ref()).map_err(|e| {
                MultisigError::SigningError(format!("Failed to create message from sighash: {}", e))
            })?;

        // Sign the message
        let signature = secp.sign_schnorr(&msg, &keypair);
        let signature = bitcoin::taproot::Signature {
            signature,
            sighash_type,
        };

        // Add the signature to the PSBT
        input
            .tap_script_sigs
            .insert((xonly_pubkey, leaf_hash), signature);

        // Add to our returned signatures
        signatures.push(signature);
    }

    if signatures.is_empty() {
        return Err(MultisigError::SigningError(
            "No inputs were signed".to_string(),
        ));
    }

    Ok((psbt, signatures))
}

pub fn finalize_spend_psbt(
    secp: &Secp256k1<All>,
    subnet_id: &SubnetId,
    committee_keys: &[XOnlyPublicKey],
    committee_threshold: u16,
    psbt: &bitcoin::Psbt,
) -> Result<Transaction, MultisigError> {
    let script = create_multisig_script(committee_keys, committee_threshold.into())?;
    let leaf_hash = script.tapscript_leaf_hash();

    // Get the spend info for the multisig
    let spend_info = create_subnet_multisig_spend_info(
        secp,
        subnet_id,
        committee_keys,
        committee_threshold.into(),
    )?;

    // Create the control block
    let control_block = spend_info
        .control_block(&(script.clone(), LeafVersion::TapScript))
        .ok_or(MultisigError::TaprootBuilderNotFinalizable)?;

    let mut finalized_psbt = psbt.clone();

    // Finalize each input in the PSBT
    for input in finalized_psbt.inputs.iter_mut() {
        let mut witness = Witness::new();

        for pubkey in committee_keys.iter().rev() {
            if let Some(sig) = input.tap_script_sigs.get(&(*pubkey, leaf_hash)) {
                witness.push(sig.signature.serialize());
            } else {
                witness.push([]);
            }
        }

        // Add script and control block
        witness.push(script.to_bytes());
        witness.push(control_block.serialize());

        // Set the finalized witness
        input.final_script_witness = Some(witness);
        // Clear other fields now that we've finalized
        input.tap_script_sigs.clear();
        input.tap_scripts.clear();
    }

    // Extract the transaction
    let finalized_tx = finalized_psbt
        .extract_tx()
        .map_err(|e| MultisigError::PsbtError(format!("Failed to extract transaction: {}", e)))?;

    Ok(finalized_tx)
}

pub fn finalize_spend_psbt_from_sigs(
    secp: &Secp256k1<All>,
    subnet_id: &SubnetId,
    committee_keys: &[XOnlyPublicKey],
    committee_threshold: u16,
    psbt: &bitcoin::Psbt,
    signature_sets: &[&[bitcoin::taproot::Signature]],
) -> Result<Transaction, MultisigError> {
    let mut signed_psbt = psbt.clone();

    let script = create_multisig_script(&committee_keys, committee_threshold.into()).unwrap();
    let leaf_hash = script.tapscript_leaf_hash();

    for (xonly_pubkey, signatures) in committee_keys.iter().zip(signature_sets.iter()) {
        // For each input this signer has signed
        for (input_idx, signature) in signatures.iter().enumerate() {
            // Make sure we don't go out of bounds
            if input_idx < signed_psbt.inputs.len() {
                // Add the signature to the PSBT
                signed_psbt.inputs[input_idx]
                    .tap_script_sigs
                    .insert((*xonly_pubkey, leaf_hash), *signature);
            }
        }
    }

    let finalized_psbt = finalize_spend_psbt(
        &secp,
        &subnet_id,
        &committee_keys,
        committee_threshold,
        &signed_psbt,
    )?;

    Ok(finalized_psbt)
}

#[derive(Error, Debug)]
pub enum MultisigError {
    #[error("insufficient public keys provided")]
    InsufficientPublicKeys,

    #[error("error during coin selection: {0}")]
    CoinSelectionFailed(String),

    #[error("signing error: {0}")]
    SigningError(String),

    #[error("psbt error: {0}")]
    PsbtError(String),

    #[error("taproot error: {0}")]
    SighashTaprootError(#[from] bitcoin::sighash::TaprootError),

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

    pub fn generate_keypairs(n: usize) -> Vec<Keypair> {
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

    pub fn generate_subnet_id() -> SubnetId {
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

        dbg!(&control_block.size());

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
            let spend_witness_size = multisig_spend_witness_size(5, 3);
            assert_eq!(witness.size(), spend_witness_size);

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

    #[test]
    fn test_witness_size_calculation() {
        let committee_configs: [(u16, u16); 7] =
            [(1, 1), (2, 3), (3, 5), (5, 7), (7, 10), (11, 15), (14, 20)];

        let secp = Secp256k1::new();

        for (required_sigs, committee_size) in committee_configs {
            let keypairs = generate_keypairs(committee_size as usize);
            let public_keys: Vec<XOnlyPublicKey> =
                keypairs.iter().map(|kp| kp.x_only_public_key().0).collect();

            let subnet_id = generate_subnet_id();
            let script = create_multisig_script(&public_keys, required_sigs as i64).unwrap();

            let spend_info = create_subnet_multisig_spend_info(
                &secp,
                &subnet_id,
                &public_keys,
                required_sigs as i64,
            )
            .unwrap();
            let control_block = spend_info
                .control_block(&(script.clone(), LeafVersion::TapScript))
                .unwrap();

            // Create a witness
            let mut witness = Witness::new();

            // Add required signatures
            let dummy_sig = [1u8; bitcoin::key::constants::SCHNORR_SIGNATURE_SIZE];
            for _ in 0..required_sigs {
                witness.push(&dummy_sig[..]);
            }
            // Add empty signatures for unused keys
            for _ in required_sigs..committee_size {
                witness.push([]);
            }
            // Add the script and control block
            witness.push(script.to_bytes());
            witness.push(control_block.serialize());

            // Calculate the actual size
            let actual_size = witness.size();

            // Calculate using our function
            let calculated_size = multisig_spend_witness_size(committee_size, required_sigs);

            // They should match
            assert_eq!(calculated_size, actual_size);
            println!(
                "{required_sigs}-of-{committee_size} Witness Size: Actual={}, Calculated={}",
                actual_size, calculated_size
            );
        }
    }
}

#[cfg(test)]
mod coin_selection_tests {
    use std::str::FromStr;

    use super::*;
    use bitcoin::Amount;
    use bitcoincore_rpc::json::ListUnspentResultEntry;

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
        )
        .unwrap();

        assert_eq!(selected.len(), 2);
        assert!(change.is_some());
        assert_eq!(change.unwrap().value, Amount::from_sat(450));
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
}

#[cfg(test)]
mod psbt_tests {
    use super::tests::{generate_keypairs, generate_subnet_id};
    use super::*;
    use crate::multisig::tests::verify_transaction;
    use crate::NETWORK;
    use bitcoin::secp256k1::{Message, Secp256k1};
    use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
    use bitcoin::{transaction, Amount, OutPoint, Sequence, TxOut, Txid};
    use std::str::FromStr;

    #[test]
    fn test_sign_psbt() {
        let secp = Secp256k1::new();

        // Generate keypairs for the committee
        let committee_keypairs = generate_keypairs(3);
        let committee_pubkeys: Vec<XOnlyPublicKey> = committee_keypairs
            .iter()
            .map(|kp| kp.x_only_public_key().0)
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
            &destination,
            spend_amount,
            &fee_rate,
        )
        .unwrap();

        // Sign the PSBT with the first keypair
        let (signed_psbt, signatures) =
            sign_spend_psbt(&secp, psbt, committee_keypairs[0]).unwrap();

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
        assert_eq!(found_sig.unwrap(), &signatures[0]);

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

        // Verify the signature against the message and public key
        let schnorr_sig = bitcoin::secp256k1::schnorr::Signature::from_slice(
            &signatures[0].to_vec()[..64], // Take only the signature part, not the sighash byte
        )
        .unwrap();

        assert!(
            secp.verify_schnorr(&schnorr_sig, &msg, &xonly_pubkey)
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

        let committee_pubkeys: Vec<XOnlyPublicKey> =
            keypairs.iter().map(|kp| kp.x_only_public_key().0).collect();

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

        // Create a PSBT to spend from the multisig
        let fee_rate = FeeRate::from_sat_per_vb(2).unwrap();
        let unsigned_psbt = construct_spend_psbt(
            &secp,
            &subnet_id,
            &committee_pubkeys,
            2,                 // required signatures
            &multisig_address, // Use the same address for change
            &vec![utxo.clone()],
            &destination,
            spend_amount,
            &fee_rate,
        )
        .unwrap();

        // dbg!(&unsigned_psbt);

        // Sign with first keypair
        let (psbt_signed_once, sigs1) = sign_spend_psbt(&secp, unsigned_psbt, keypairs[0])
            .expect("First signature should work");

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
            required_sigs.try_into().unwrap(),
            &psbt_signed_twice,
        )
        .expect("Failed to finalize PSBT");

        dbg!(&finalized_tx);

        // Verify the finalized transaction can spend the UTXO
        let verify_result =
            verify_transaction(&finalized_tx, |_| Some(funding_tx.output[0].clone()));

        assert!(
            verify_result.is_ok(),
            "Transaction should be valid with 2 of 3 signatures: {:?}",
            verify_result
        );
        println!("Transaction successfully verified with 2-of-3 signatures!");
    }

    #[test]
    fn test_parallel_signing_and_spending() {
        let secp = Secp256k1::new();

        // Generate 3 keypairs for the committee
        let keypairs = generate_keypairs(3);
        let committee_pubkeys: Vec<XOnlyPublicKey> =
            keypairs.iter().map(|kp| kp.x_only_public_key().0).collect();

        // Generate a subnet ID
        let subnet_id = generate_subnet_id();

        // Create a 2-of-3 multisig setup
        let required_sigs = 2_u16;

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

        // Create a PSBT to spend from the multisig
        let fee_rate = FeeRate::from_sat_per_vb(2).unwrap();
        let unsigned_psbt = construct_spend_psbt(
            &secp,
            &subnet_id,
            &committee_pubkeys,
            2,                 // required signatures
            &multisig_address, // Use the same address for change
            &utxos,
            &destination,
            spend_amount,
            &fee_rate,
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
            required_sigs,
            &unsigned_psbt,
            &[&sigs1, &sigs2],
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
}
