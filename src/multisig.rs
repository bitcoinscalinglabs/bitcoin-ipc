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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Configuration {
    pub keys: Vec<WeightedKey>,
    pub threshold: Power,
}

impl Configuration {
    /// The keys in ascending order, the index of a key is the index of its slash pre-signature
    pub fn sorted_keys(&self) -> Vec<WeightedKey> {
        sort_committee_keys(&self.keys)
    }

    pub fn key_index(&self, key: &XOnlyPublicKey) -> Option<u16> {
        self.sorted_keys()
            .iter()
            .position(|(k, _)| k == key)
            .map(|i| i as u16)
    }

    pub fn power_at(&self, index: u16) -> Option<Power> {
        self.sorted_keys().get(usize::from(index)).map(|(_, p)| *p)
    }

    pub fn multisig_script(&self) -> Result<ScriptBuf, MultisigError> {
        create_multisig_script(&self.keys, self.threshold)
    }
}

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

/// The keys with their powers and the threshold of a leaf built by `create_multisig_script`,
/// `None` for any other script.
pub fn parse_multisig_script(script: &bitcoin::Script) -> Option<(Vec<WeightedKey>, Power)> {
    use bitcoin::script::Instruction;
    let ops: Vec<Instruction> = script.instructions().collect::<Result<_, _>>().ok()?;
    let is_op = |i: &Instruction, op: opcodes::Opcode| matches!(i, Instruction::Op(o) if *o == op);
    let num = |i: &Instruction| i.script_num().and_then(|n| Power::try_from(n).ok());

    let (first, rest) = ops.split_first()?;
    let (body, end) = rest.split_at(rest.len().checked_sub(2)?);
    if num(first) != Some(0) || body.len() % 7 != 0 || !is_op(&end[1], opcodes::all::OP_GREATERTHANOREQUAL)
    {
        return None;
    }
    let threshold = num(&end[0])?;
    let keys = body
        .chunks(7)
        .map(|b| {
            let key = XOnlyPublicKey::from_slice(b[1].push_bytes()?.as_bytes()).ok()?;
            let shape = is_op(&b[0], opcodes::all::OP_SWAP)
                && is_op(&b[2], opcodes::all::OP_CHECKSIG)
                && is_op(&b[3], opcodes::all::OP_IF)
                && is_op(&b[5], opcodes::all::OP_ADD)
                && is_op(&b[6], opcodes::all::OP_ENDIF);
            shape.then_some((key, num(&b[4])?))
        })
        .collect::<Option<Vec<_>>>()?;
    // Only the exact script of these keys and threshold, in ascending key order
    (create_multisig_script(&keys, threshold).ok()?.as_script() == script).then_some((keys, threshold))
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

pub const MAX_TOTAL_POWER: Power = i32::MAX as Power;
pub const MIN_CONFIGURATION_SIZE: usize = 4;

/// How many extra (smallest) committee UTXOs a checkpoint folds in beyond what
/// coin-selection needs. Data-carrying checkpoints leak a small commit-reveal
/// "reveal" UTXO each time; greedy largest-first selection never spends them, so
/// they accumulate until a committee rotation must sweep them all at once (which
/// then produces an oversized, unbroadcastable transaction). Sweeping a bounded
/// number per checkpoint keeps the committee UTXO set small and drains any backlog
/// gradually. The cap keeps a single checkpoint from growing too large.
pub const CHECKPOINT_CONSOLIDATION_LIMIT: usize = 50;

/// The power of a collateral: `⌊amount / power_unit⌋`, for an amount of at least `min_amount`
pub fn collateral_to_power(
    amount: &Amount,
    min_amount: &Amount,
    power_unit: &Amount,
) -> Result<Power, MultisigError> {
    if amount < min_amount {
        return Err(MultisigError::InsufficientCollateral);
    }

    let power = amount.to_sat() / power_unit.to_sat();

    Power::try_from(power)
        .ok()
        .filter(|p| *p <= MAX_TOTAL_POWER)
        .ok_or(MultisigError::CollateralTooHigh)
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
        + VarInt::from(bitcoin::key::constants::SCHNORR_SIGNATURE_SIZE).size() * committee_size
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
    // Fold in up to this many of the smallest leftover UTXOs beyond what is needed to
    // cover the target, to drain accumulated dust. Pass 0 for plain coin-selection.
    consolidation_limit: usize,
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
    // Sort UTXOs deterministically by amount, txid and vout — largest first.
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

    // Phase 1: greedily take the largest UTXOs until the target plus fee is covered.
    let mut covered_index = None;
    for (index, utxo) in utxos.iter().enumerate() {
        selected.push(utxo.clone());
        total_amount += utxo.amount;
        total_weight += satisfaction_weight_per_input;

        let total_fee = fee_rate
            .fee_wu(total_weight)
            .expect("fee rate shouldn't overflow");

        if total_amount >= target + total_fee {
            covered_index = Some(index);
            break;
        }
    }

    let covered_index = covered_index.ok_or(MultisigError::CoinSelectionFailed(
        "Insufficient funds".to_string(),
    ))?;

    // Phase 2: consolidation. Fold in up to `consolidation_limit` of the smallest
    // remaining UTXOs (the tail of the largest-first order), so leaked commit-reveal
    // dust does not accumulate. Their value flows into the change output. This is
    // best-effort: an input is only swept while the change stays above dust, so
    // consolidation can never turn a spendable set into an unbuildable transaction.
    // `consolidation_limit == 0` makes this a no-op and leaves behaviour unchanged.
    for utxo in utxos[covered_index + 1..]
        .iter()
        .rev()
        .take(consolidation_limit)
    {
        let next_weight = total_weight + satisfaction_weight_per_input;
        let next_amount = total_amount + utxo.amount;
        let next_fee = fee_rate
            .fee_wu(next_weight)
            .expect("fee rate shouldn't overflow");
        // Only sweep if change would still cover the dust threshold afterwards.
        if next_amount < target + next_fee + non_dust_change_tx_out.value {
            break;
        }
        selected.push(utxo.clone());
        total_amount = next_amount;
        total_weight = next_weight;
    }

    // Phase 3: compute the final fee and change over the full selected set.
    let total_fee = fee_rate
        .fee_wu(total_weight)
        .expect("fee rate shouldn't overflow");
    let change = total_amount
        .checked_sub(target)
        .and_then(|remainder| remainder.checked_sub(total_fee))
        .ok_or(MultisigError::CoinSelectionFailed(
            "Insufficient funds to cover outputs and fees".to_string(),
        ))?;

    trace!(
        "Selected {} coins for target {}. Total: {}, fee: {}, change: {}",
        selected.len(),
        target,
        total_amount,
        total_fee,
        change
    );

    // If change is non-dust, include it; otherwise it becomes extra fee.
    if change > non_dust_change_tx_out.value {
        Ok((
            selected,
            Some(TxOut {
                value: change,
                script_pubkey: change_address.script_pubkey(),
            }),
        ))
    } else {
        Ok((selected, None))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn construct_spend_unsigned_transaction(
    committee_keys: &[WeightedKey],
    threshold: Power,
    committee_change_address: &Address,
    unspent: &[bitcoincore_rpc::json::ListUnspentResultEntry],
    exhaust_unspent: bool,
    tx_outs: &[TxOut],
    fee_rate: &FeeRate,
    // Extra small UTXOs to sweep in the coin-selection path (ignored when exhausting).
    consolidation_limit: usize,
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
            consolidation_limit,
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
    // Extra small UTXOs to sweep in the coin-selection path (ignored when exhausting).
    consolidation_limit: usize,
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
        consolidation_limit,
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

/// Builds the witness of every input from the posted signatures: each signer's key with one
/// signature per input, in input order, `None` for a missing or malformed one. A signature counts
/// for an input if its key is in the input's leaf (`tap_scripts`), no earlier signature of that key
/// counted for the input, and it verifies; every other signature is dropped. Fails with
/// `SignaturesMissing` while the counted powers of an input stay below the leaf's threshold.
pub fn finalize_spend_psbt_from_sigs(
    secp: &Secp256k1<All>,
    psbt: &bitcoin::Psbt,
    signatures: &[(XOnlyPublicKey, Vec<Option<bitcoin::secp256k1::schnorr::Signature>>)],
) -> Result<Transaction, MultisigError> {
    let all_witness_utxos: Vec<TxOut> = psbt
        .iter_funding_utxos()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| MultisigError::PsbtError(format!("Failed to collect UTXOs: {}", e)))?
        .into_iter()
        .cloned()
        .collect();
    let prevouts = bitcoin::sighash::Prevouts::All(&all_witness_utxos);
    let mut sighash_cache = bitcoin::sighash::SighashCache::new(&psbt.unsigned_tx);

    let mut finalized_psbt = psbt.clone();
    let mut missing = Vec::new();
    for (input_index, input) in finalized_psbt.inputs.iter_mut().enumerate() {
        let (control_block, (script, _leaf_version)) =
            input.tap_scripts.iter().next().ok_or_else(|| {
                MultisigError::PsbtError(format!("input {} has no leaf", input_index))
            })?;
        let (keys, threshold) = parse_multisig_script(script).ok_or_else(|| {
            MultisigError::PsbtError(format!("input {} has no multisig leaf", input_index))
        })?;

        let sighash = sighash_cache.taproot_script_spend_signature_hash(
            input_index,
            &prevouts,
            script.tapscript_leaf_hash(),
            bitcoin::sighash::TapSighashType::Default,
        )?;
        let msg = bitcoin::secp256k1::Message::from_digest_slice(sighash.as_ref())
            .map_err(|e| MultisigError::SigningError(e.to_string()))?;

        let mut counted: Vec<Option<bitcoin::secp256k1::schnorr::Signature>> = vec![None; keys.len()];
        for (key, sigs) in signatures {
            let (Some(position), Some(Some(sig))) = (
                keys.iter().position(|(k, _)| k == key),
                sigs.get(input_index),
            ) else {
                continue;
            };
            if counted[position].is_none() && secp.verify_schnorr(sig, &msg, key).is_ok() {
                counted[position] = Some(*sig);
            } else {
                debug!("input {} drops a signature of {}", input_index, key);
            }
        }

        let power: u64 = keys
            .iter()
            .zip(&counted)
            .filter(|(_, sig)| sig.is_some())
            .map(|((_, power), _)| u64::from(*power))
            .sum();
        if power < u64::from(threshold) {
            missing.push((input_index, power, threshold));
            continue;
        }

        let mut witness = Witness::new();
        for sig in counted.iter().rev() {
            match sig {
                Some(sig) => witness.push(sig.serialize()),
                None => witness.push([]),
            }
        }
        witness.push(script.to_bytes());
        witness.push(control_block.serialize());
        input.final_script_witness = Some(witness);
        input.tap_script_sigs.clear();
        input.tap_scripts.clear();
    }
    if !missing.is_empty() {
        return Err(MultisigError::SignaturesMissing(missing));
    }

    finalized_psbt
        .extract_tx()
        .map_err(|e| MultisigError::PsbtError(format!("Failed to extract transaction: {}", e)))
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

    #[error(
        "signatures missing: {}",
        .0.iter()
            .map(|(input, power, threshold)| format!("input {input} power {power} of {threshold}"))
            .collect::<Vec<_>>()
            .join(", ")
    )]
    SignaturesMissing(Vec<(usize, u64, Power)>),

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
mod tests;
