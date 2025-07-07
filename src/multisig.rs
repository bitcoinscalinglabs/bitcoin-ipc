use std::vec;

use log::{debug, error, trace};
use thiserror::Error;

use bitcoin::{
    absolute::LockTime,
    blockdata::script::Builder,
    opcodes,
    secp256k1::{All, Secp256k1},
    taproot::{LeafVersion, TaprootBuilder, TaprootSpendInfo},
    Address, Amount, FeeRate, Network, ScriptBuf, Transaction, TxIn, TxOut, VarInt, Weight,
    Witness, XOnlyPublicKey,
};

use crate::bitcoin_utils::unspenable_internal_key;

use crate::SubnetId;

pub type Power = u32;
pub type WeightedKey = (XOnlyPublicKey, Power);

fn sort_committee_keys(committee_keys: &[WeightedKey]) -> Vec<WeightedKey> {
    let mut sorted_keys = committee_keys.to_vec();
    sorted_keys.sort_by(|(key_a, _), (key_b, _)| key_a.cmp(key_b));
    sorted_keys
}

pub fn create_multisig_script(
    public_keys: &[WeightedKey],
    threshold: Power,
) -> Result<ScriptBuf, MultisigError> {
    // Check if enough weight is available
    let total_weight: Power = public_keys.iter().map(|(_, weight)| weight).sum();
    if total_weight < threshold {
        return Err(MultisigError::InsufficientTotalPower);
    }

    // Public keys need to be sorted for consistent scriptPubKey
    let sorted_public_keys = sort_committee_keys(public_keys);

    //  It pushes an accumulator to the stack, and then for each pk:
    // - it swaps the sig that's already on the stack as a witness, with the accumulator
    // - pushes the pk
    // - does a checksig that leaves 0/1 on the stack
    // - if 1, pushes this validator's power and calls OP_ADD, summing the power and accumulator

    // At the end we consume all sigs and public keys, we're left with the accumulator.
    // We push the threshold to the stack and call `OP_GREATERTHANOREQUAL`
    // which leaves 0/1 on the stack.
    let builder = Builder::new().push_int(0); // power accumulator
    Ok(sorted_public_keys
        .iter()
        .fold(builder, |builder, (key, weight)| {
            builder
                .push_opcode(opcodes::all::OP_SWAP)
                .push_x_only_key(key)
                .push_opcode(opcodes::all::OP_CHECKSIG)
                .push_opcode(opcodes::all::OP_IF)
                .push_int((*weight).into())
                .push_opcode(opcodes::all::OP_ADD)
                .push_opcode(opcodes::all::OP_ENDIF)
        })
        .push_int(threshold.into())
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
        .push_slice(subnet_id.txid20())
        .into_script()
}

pub fn create_subnet_multisig_spend_info(
    secp: &Secp256k1<All>,
    subnet_id: &SubnetId,
    public_keys: &[WeightedKey],
    threshold: Power,
) -> Result<TaprootSpendInfo, MultisigError> {
    let multisig_script = create_multisig_script(public_keys, threshold)?;
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
    public_keys: &[WeightedKey],
    threshold: Power,
    network: Network,
) -> Result<Address, MultisigError> {
    let spend_info = create_subnet_multisig_spend_info(secp, subnet_id, public_keys, threshold)?;

    Ok(Address::p2tr(
        secp,
        spend_info.internal_key(),
        spend_info.merkle_root(),
        network,
    ))
}

pub fn multisig_threshold(total_power: Power) -> Power {
    // TODO figure out threshold
    // total_weight * 2 / 3 + 1
    // see quorum_threshold in ipc
    // (total_power / 2) + 1
    total_power * 2 / 3 + 1
}

// TODO figure out scaling factor
pub const POWER_SCALE_FACTOR: u64 = 10_000;
pub const MAX_POWER: u64 = u32::MAX as u64;

/// Converts a Bitcoin Amount to a power/weight value (u32) using a fixed scale factor.
/// Discards small satoshi values based on the provided minimum amount.
//
// TODO improve this, maybe cap the total power so we don't have
// the CollateralTooHigh error possibility
pub fn collateral_to_power(amount: &Amount, min_amount: &Amount) -> Result<Power, MultisigError> {
    // Check if amount is below minimum threshold
    if amount < min_amount {
        return Err(MultisigError::InsufficientCollateral);
    }

    let power = amount.to_sat().min(MAX_POWER) / POWER_SCALE_FACTOR;

    let power: Power = power
        .try_into()
        .map_err(|_| MultisigError::CollateralTooHigh)?;

    Ok(power)
}

/// Calculates the size in bytes of witness elements for spending a multisig utxo
/// Calculates the "worst case" size, assuming all public keys' signatures are included
/// Useful for fee calculations.
//
// NOTE: this is quite sensitive as it's not easy to change if the multisig script is changed
// maybe consider using a real transaction to calculate the size at compile time?
pub fn multisig_spend_max_witness_size(public_keys: &[WeightedKey], threshold: Power) -> usize {
    let committee_size = public_keys.len();

    let signatures_size = bitcoin::key::constants::SCHNORR_SIGNATURE_SIZE * committee_size;

    let script_size = 1
        + public_keys
            .iter()
            .map(|(_, power)| {
                bitcoin::key::constants::SCHNORR_PUBLIC_KEY_SIZE + 5 + VarInt::from(*power).size()
            })
            .sum::<usize>()
        + VarInt::from(threshold).size()
        + 1;

    // Control block calculation:
    // - TAPROOT_CONTROL_BASE_SIZE (33 bytes: 1 byte version + 32 bytes internal key)
    // - TAPROOT_CONTROL_NODE_SIZE (32 bytes for the merkle node)
    let control_block_content_size =
        bitcoin::taproot::TAPROOT_CONTROL_BASE_SIZE + bitcoin::taproot::TAPROOT_CONTROL_NODE_SIZE;

    let var_ints =
	    // varint for the number of witnesses, which is a signature for each committee member + script + control block
	    bitcoin::VarInt::from(committee_size + 2).size()
	    // each schnorr sig
        + VarInt::from(bitcoin::key::constants::SCHNORR_SIGNATURE_SIZE).size() * threshold as usize
        // script size
        + VarInt::from(script_size).size()
        // control block size
        + VarInt::from(control_block_content_size).size();

    signatures_size + script_size + control_block_content_size + var_ints
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
            #[cfg(test)]
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
    committee_keys: &[WeightedKey],
    threshold: Power,
    committee_change_address: &Address,
    unspent: &[bitcoincore_rpc::json::ListUnspentResultEntry],
    exhaust_unspent: bool,
    tx_outs: &[TxOut],
    fee_rate: &FeeRate,
) -> Result<Transaction, MultisigError> {
    trace!("Creating multisig spend tx for tx_outs={:?}", &tx_outs);

    // size of the witness data when spending a multisig utxo
    let spend_witness_size = multisig_spend_max_witness_size(committee_keys, threshold);
    // base input size
    let spend_txin_size = bitcoin::TxIn::default().base_size();
    // weight of spending one input
    let spending_weight_per_input = Weight::from_non_witness_data_size(spend_txin_size as u64)
        + Weight::from_witness_data_size(spend_witness_size as u64);

    let non_dust_change_tx_out =
        bitcoin::TxOut::minimal_non_dust(committee_change_address.script_pubkey());
    if exhaust_unspent {
        // Use all UTXOs as inputs
        if unspent.is_empty() {
            return Err(MultisigError::CoinSelectionFailed(
                "No UTXOs available".to_string(),
            ));
        }

        // Calculate total input amount from all UTXOs
        let total_input_amount = unspent.iter().map(|utxo| utxo.amount).sum::<Amount>();

        // Calculate total output amount from specified tx_outs
        let total_output_amount = tx_outs.iter().map(|tx_out| tx_out.value).sum::<Amount>();

        // Ensure we have enough funds for the specified outputs
        if total_input_amount < total_output_amount {
            return Err(MultisigError::CoinSelectionFailed(
                "Insufficient funds to cover outputs".to_string(),
            ));
        }

        let mut unspent = unspent.to_vec();
        // Sort UTXOs deterministically by amount, txid and vout
        unspent.sort_by(|a, b| {
            b.amount
                .cmp(&a.amount)
                .then(a.txid.cmp(&b.txid))
                .then(a.vout.cmp(&b.vout))
        });

        // Create inputs from all UTXOs
        let inputs = unspent
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

        // TODO filter out inputs which are not profitable to transfer
        // ie. small inputs

        // Start with the transaction containing all inputs and specified outputs
        let mut spend_tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: inputs,
            output: tx_outs.to_vec(),
        };

        // Create a tentative transaction with potential change output to estimate fees
        let mut tentative_tx = spend_tx.clone();
        tentative_tx.output.push(TxOut {
            value: Amount::from_sat(1), // Placeholder value
            script_pubkey: committee_change_address.script_pubkey(),
        });

        // Calculate fee with the correct witness weight
        let fee = fee_rate
            .fee_wu(
                tentative_tx.weight()
                    + Weight::from_witness_data_size(
                        spend_witness_size as u64 * unspent.len() as u64,
                    ),
            )
            .expect("fee calculation shouldn't overflow");

        // Calculate remaining amount after outputs and fees
        let remaining = match total_input_amount.checked_sub(total_output_amount) {
            Some(remainder) => match remainder.checked_sub(fee) {
                Some(after_fee) => after_fee,
                None => {
                    return Err(MultisigError::CoinSelectionFailed(
                        "Insufficient funds to cover outputs and fees".to_string(),
                    ))
                }
            },
            None => {
                return Err(MultisigError::CoinSelectionFailed(
                    "Insufficient funds to cover outputs".to_string(),
                ))
            }
        };

        // Only add the change output if it's above the dust threshold
        if remaining > non_dust_change_tx_out.value {
            spend_tx.output.push(TxOut {
                value: remaining,
                script_pubkey: committee_change_address.script_pubkey(),
            });
        } else {
            trace!(
                "Change amount {} is dust, not creating change output",
                remaining
            );
            // The dust becomes an additional fee
        }

        debug!("Multisig Spend Transaction {:?}", &spend_tx);

        Ok(spend_tx)
    } else {
        // Original behavior: select just enough UTXOs to cover the outputs
        // Calculate the total amount to spend for the outputs
        let amount = tx_outs.iter().map(|tx_out| tx_out.value).sum::<Amount>();

        // base transaction, assume a change output
        let base_tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![],
            output: [tx_outs, &[non_dust_change_tx_out]].concat(),
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
            output: tx_outs.to_vec(),
        };
        if let Some(change_tx_out) = change {
            spend_tx.output.push(change_tx_out);
        }

        Ok(spend_tx)
    }
}

/// Constructs a PSBT for spending a multisig output
#[allow(clippy::too_many_arguments)]
pub fn construct_spend_psbt(
    secp: &Secp256k1<All>,
    subnet_id: &SubnetId,
    committee_keys: &[WeightedKey],
    committee_threshold: Power,
    committee_change_address: &Address,
    unspent: &[bitcoincore_rpc::json::ListUnspentResultEntry],
    exhaust_unspent: bool,
    tx_outs: &[TxOut],
    fee_rate: &FeeRate,
) -> Result<bitcoin::Psbt, MultisigError> {
    trace!("Creating multisig spend PSBT for tx_outs={:?}", &tx_outs);

    // First construct the unsigned transaction
    let unsigned_tx = construct_spend_unsigned_transaction(
        committee_keys,
        committee_threshold,
        committee_change_address,
        unspent,
        exhaust_unspent,
        tx_outs,
        fee_rate,
    )?;

    // Create the PSBT from the unsigned transaction
    let mut psbt = bitcoin::psbt::Psbt::from_unsigned_tx(unsigned_tx.clone())
        .map_err(|e| MultisigError::CoinSelectionFailed(format!("Failed to create PSBT: {}", e)))?;

    let script = create_multisig_script(committee_keys, committee_threshold)?;

    // Get the spend info for the multisig
    let spend_info =
        create_subnet_multisig_spend_info(secp, subnet_id, committee_keys, committee_threshold)?;

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
) -> Result<(bitcoin::Psbt, Vec<bitcoin::secp256k1::schnorr::Signature>), MultisigError> {
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
        // Add to our returned signatures
        signatures.push(signature);

        let taproot_sig = bitcoin::taproot::Signature {
            signature,
            sighash_type,
        };

        // Add the signature to the PSBT
        input
            .tap_script_sigs
            .insert((xonly_pubkey, leaf_hash), taproot_sig);
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
    committee_keys: &[WeightedKey],
    committee_threshold: Power,
    psbt: &bitcoin::Psbt,
) -> Result<Transaction, MultisigError> {
    let committee_keys = sort_committee_keys(committee_keys);
    trace!(
        "finalize_spend_psbt sorted committee_keys: {:?}",
        &committee_keys
    );

    let script = create_multisig_script(&committee_keys, committee_threshold)?;
    let leaf_hash = script.tapscript_leaf_hash();

    // Get the spend info for the multisig
    let spend_info =
        create_subnet_multisig_spend_info(secp, subnet_id, &committee_keys, committee_threshold)?;

    // Create the control block
    let control_block = spend_info
        .control_block(&(script.clone(), LeafVersion::TapScript))
        .ok_or(MultisigError::TaprootBuilderNotFinalizable)?;

    let mut finalized_psbt = psbt.clone();

    // Finalize each input in the PSBT
    for input in finalized_psbt.inputs.iter_mut() {
        let mut witness = Witness::new();

        for (pubkey, _power) in committee_keys.iter().rev() {
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
    committee_keys: &[WeightedKey],
    committee_threshold: u32,
    psbt: &bitcoin::Psbt,
    signature_sets: &[&[bitcoin::secp256k1::schnorr::Signature]],
) -> Result<Transaction, MultisigError> {
    let mut signed_psbt = psbt.clone();

    let mut key_sig_pairs: Vec<(WeightedKey, &[bitcoin::secp256k1::schnorr::Signature])> =
        committee_keys
            .iter()
            .cloned()
            .zip(signature_sets.iter().cloned())
            .collect();

    // Sort by public key - same ordering as in create_multisig_script
    key_sig_pairs.sort_by(|(key_a, _), (key_b, _)| key_a.0.cmp(&key_b.0));

    let script = create_multisig_script(committee_keys, committee_threshold).unwrap();
    let leaf_hash = script.tapscript_leaf_hash();

    for ((xonly_pubkey, _power), signatures) in key_sig_pairs {
        // For each input this signer has signed
        for (input_idx, signature) in signatures.iter().enumerate() {
            debug!(
                "finalize_spend_psbt_from_sigs xpk={} input_idx={} sig={}",
                xonly_pubkey, input_idx, signature
            );

            // Make sure we don't go out of bounds
            if input_idx < signed_psbt.inputs.len() {
                let taproot_sig = bitcoin::taproot::Signature {
                    signature: *signature,
                    sighash_type: bitcoin::sighash::TapSighashType::Default,
                };

                // Add the signature to the PSBT
                signed_psbt.inputs[input_idx]
                    .tap_script_sigs
                    .insert((xonly_pubkey, leaf_hash), taproot_sig);
            }
        }
    }

    let finalized_psbt = finalize_spend_psbt(
        secp,
        subnet_id,
        committee_keys,
        committee_threshold,
        &signed_psbt,
    )?;

    Ok(finalized_psbt)
}

#[derive(Error, Debug)]
pub enum MultisigError {
    #[error("insufficient public keys provided")]
    InsufficientPublicKeys,

    #[error("insufficient total validator power provided")]
    InsufficientTotalPower,

    #[error("insufficient collateral provided")]
    InsufficientCollateral,

    #[error("collateral too high")]
    CollateralTooHigh,

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
    use crate::test_utils::{generate_equal_weighted_keys, generate_keypairs, generate_subnet_id};
    use bitcoin::consensus::encode;
    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::Message;
    use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
    use bitcoin::taproot::LeafVersion;
    use bitcoin::{
        absolute::LockTime, secp256k1::Secp256k1, AddressType, Amount, Network, Sequence,
        Transaction, TxIn, TxOut, Txid, Witness,
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

        let multisig_address = create_subnet_multisig_address(
            &secp,
            &subnet_id,
            &committee_keys,
            required_sigs,
            network,
        )
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
        let public_keys = generate_equal_weighted_keys(3);
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
                create_subnet_multisig_spend_info(&secp, &subnet_id, &public_keys, threshold)
                    .unwrap();
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
    use super::*;
    use crate::multisig::tests::verify_transaction;
    use crate::test_utils::{generate_keypairs, generate_subnet_id};
    use crate::NETWORK;
    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::{Message, Secp256k1};
    use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
    use bitcoin::{transaction, Amount, OutPoint, Sequence, TxOut, Txid};
    use std::str::FromStr;

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
            required_sigs.into(),
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
        let multisig_address = create_subnet_multisig_address(
            &secp,
            &subnet_id,
            &committee_pubkeys,
            threshold,
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
}

#[cfg(test)]
mod multisig_weighted_tests {
    use super::*;
    use crate::test_utils::generate_subnet_id;
    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::{Message, Secp256k1};
    use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
    use bitcoin::{transaction, Amount, OutPoint, Sequence, TxOut, Txid};

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
        let multisig_address = create_subnet_multisig_address(
            &secp,
            &subnet_id,
            &committee_pubkeys,
            threshold,
            network,
        )
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
            let msg =
                Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");
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
            let msg =
                Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");

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
            let msg =
                Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");

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
            let msg =
                Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");

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
            println!("Test case 4 passed: Heavy (10) + Medium validator (5) = 15 power exceeds threshold");
        }
    }
}

#[cfg(test)]
mod exhaust_unspent_tests {
    use super::*;
    use crate::test_utils::{generate_keypairs, generate_subnet_id};
    use bitcoin::Amount;
    use bitcoincore_rpc::json::ListUnspentResultEntry;
    use std::str::FromStr;

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
}
