use std::cmp::min;
use std::vec;

use log::{debug, error, trace};
use num_bigint::BigUint;
use num_traits::ops::bytes::ToBytes;
use thiserror::Error;

use bitcoin::{
    blockdata::{
        locktime::absolute::LockTime,
        script::{self, Builder},
        transaction::{self, OutPoint, Transaction, TxIn, TxOut},
        witness::Witness,
    },
    hashes::Hash,
    key::UntweakedPublicKey,
    opcodes::{self, all::OP_DROP, OP_TRUE},
    script::{Instruction, PushBytes},
    secp256k1::{All, Secp256k1},
    taproot::{LeafVersion, TaprootBuilder, TaprootSpendInfo},
    Address, FeeRate, Network, ScriptBuf, Weight, XOnlyPublicKey,
};

use bitcoincore_rpc::json::{EstimateMode, EstimateSmartFeeResult};
use bitcoincore_rpc::{Auth, Client, RawTx, RpcApi};

use crate::{DEFAULT_BTC_FEE_RATE, MAXIMUM_BTC_FEE_RATE, MINIMUM_BTC_FEE_RATE};

/// Returns the number of blocks to wait for before considering a
/// confirmed in a given network.
pub const fn confirmations(network: Network) -> u64 {
    use Network::*;
    match network {
        Bitcoin | Testnet => 6,
        Regtest | Signet | _ => 0,
    }
}

//
// BitcoinCore RPC

pub fn make_rpc_client_from_env() -> Client {
    let rpc_user = std::env::var("RPC_USER").expect("RPC_USER env var not defined");
    let rpc_pass = std::env::var("RPC_PASS").expect("RPC_PASS env var not defined");
    let rpc_url = std::env::var("RPC_URL").expect("RPC_URL env var not defined");
    let wallet_name = std::env::var("WALLET_NAME").expect("WALLET_NAME env var not defined");

    let rpc = match init_rpc_client(rpc_user, rpc_pass, rpc_url) {
        Ok(rpc) => rpc,
        Err(e) => {
            panic!("Error: {}", e);
        }
    };
    let _ = rpc.load_wallet(&wallet_name);
    rpc
}

pub fn init_rpc_client(
    rpc_user: String,
    rpc_pass: String,
    rpc_url: String,
) -> Result<Client, BitcoinUtilsError> {
    let rpc = Client::new(&rpc_url, Auth::UserPass(rpc_user, rpc_pass))?;
    Ok(rpc)
}

/// Returns a provably unspendable internal key
pub fn create_unspendable_internal_key() -> XOnlyPublicKey {
    // the Gx of SECP, incremented till a valid x is found
    // See
    // https://github.com/bitcoin/bips/blob/master/bip-0341.mediawiki#constructing-and-spending-taproot-outputs,
    // bullet 3, for a proper way to choose such a key
    let nothing_up_my_sleeve_key = [
        0x79, 0xBE, 0x66, 0x7E, 0xF9, 0xDC, 0xBB, 0xAC, 0x55, 0xA0, 0x62, 0x95, 0xCE, 0x87, 0x0B,
        0x07, 0x02, 0x9B, 0xFC, 0xDB, 0x2D, 0xCE, 0x28, 0xD9, 0x59, 0xF2, 0x81, 0x5B, 0x16, 0xF8,
        0x17, 0x99,
    ];
    let mut int_key = BigUint::from_bytes_be(&nothing_up_my_sleeve_key);
    while UntweakedPublicKey::from_slice(&int_key.to_be_bytes()).is_err() {
        int_key += 1u32;
    }
    UntweakedPublicKey::from_slice(&int_key.to_be_bytes())
        .expect("Should not error creating an unspendable key")
}

/// This function tests and submits a set of transactions to the Bitcoin network.
/// The function tests the transactions for mempool acceptance and submits them to the network.
/// If the transactions are not accepted by the mempool, the function prints an error message.
/// If the transactions are accepted, the function prints the transaction IDs and the mined block.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `txs` - A vector of transactions of type `Transaction`
/// * `miner_address` - The address to which the block reward is sent, of type `Address`
///
/// # Returns
///
/// * `()` - The function returns a BitcoinUtilsError if the transaction was accepted by the mempool.`
pub fn submit_to_mempool(
    rpc: &Client,
    txs: Vec<transaction::Transaction>,
) -> Result<(), BitcoinUtilsError> {
    let result = match rpc
        .test_mempool_accept(&txs.iter().map(|tx| tx.raw_hex()).collect::<Vec<String>>())
    {
        Ok(r) => r,
        Err(e) => {
            return Err(BitcoinUtilsError::MempoolAcceptanceFailed(e.to_string()));
        }
    };

    for r in result.iter() {
        if !r.allowed {
            let reason = r
                .reject_reason
                .clone()
                .unwrap_or("Unknown reason".to_string());
            error!("Txid={} rejected by mempool: {}", r.txid, reason);
            return Err(BitcoinUtilsError::MempoolAcceptanceFailed(reason));
        }
    }

    for tx in txs {
        debug!(
            "Transaction sent to mempool: {}",
            rpc.send_raw_transaction(tx.raw_hex())?
        );
    }

    Ok(())
}

/// Pushes arbitrary data in chuns, dropping all of them,
/// and then pushes a OP_TRUE at the end to allow spending.
pub fn make_push_data_script(data: &[u8]) -> ScriptBuf {
    let mut builder = Builder::new();
    let mut offset = 0;
    let chunk_size = 520;

    while offset < data.len() {
        let end = min(offset + chunk_size, data.len());
        builder = builder.push_slice(convert_bytes_to_push_bytes(&data[offset..end]));
        offset += chunk_size;
        builder = builder.push_opcode(OP_DROP);
    }

    builder = builder.push_opcode(OP_TRUE);

    builder.into_script()
}

/// Creates and submits two transactions to the Bitcoin network:
/// 1. A commit transaction that sends funds to a taproot script containing the data
/// 2. A reveal transaction that spends the taproot script by revealing its script path data
///
/// The commit transaction contains a taproot output that can only be spent by revealing the
/// data in the script path. This is enforced by using an unspendable internal key.
///
/// The reveal transaction spends this output by providing:
/// - The script containing the data (as a series of push operations)
/// - The control block proving this script was committed to
///
/// The commit transaction is funded and signed using the RPC wallet. It has enough value
/// to cover for commit and reveal tx fees.
/// The reveal transaction spends to the specified final address.
pub fn create_commit_reveal_txs(
    rpc: &Client,
    secp: &Secp256k1<bitcoin::secp256k1::All>,
    final_address: &Address,
    data: &[u8],
) -> Result<(Transaction, Transaction), BitcoinUtilsError> {
    trace!(
        "Creating commit-reveal tx for addres={}, data={:02x?}",
        &final_address,
        &data
    );

    let fee_rate = get_current_fee_rate(rpc, None, None);

    // construct the script that will contain the data
    let commit_script = make_push_data_script(data);

    // this transaction can only be spent through the script path
    let unspendable_pubkey = create_unspendable_internal_key();

    let builder = TaprootBuilder::new().add_leaf(0, commit_script.clone())?;
    let commit_spend_info = builder
        .finalize(secp, unspendable_pubkey)
        .map_err(|_| BitcoinUtilsError::TaprootBuilderNotFinalizable)?;

    let commit_script_pubkey = script::ScriptBuf::new_p2tr(
        secp,
        commit_spend_info.internal_key(),
        commit_spend_info.merkle_root(),
    );

    let mut commit_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        // This will be filled by the wallet afterwards
        input: Vec::with_capacity(0),
        output: vec![TxOut {
            // This will be increased by the value needed for the reveal tx fee
            value: commit_script_pubkey.minimal_non_dust_custom(fee_rate),
            script_pubkey: commit_script_pubkey.clone(),
        }],
    };

    //
    // Building reveal tx
    //

    let control_block = commit_spend_info
        .control_block(&(commit_script.clone(), LeafVersion::TapScript))
        .ok_or(BitcoinUtilsError::CannotConstructControlBlock)?;

    let mut reveal_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                // This will be replaced by the commit txid after
                // signing with wallet
                txid: bitcoin::Txid::all_zeros(),
                vout: 0,
            },
            witness: Witness::from_slice(&[
                // First witness, the commit script
                commit_script.to_bytes(),
                // Second, the taproot control block
                control_block.serialize(),
            ]),
            ..Default::default()
        }],
        output: vec![TxOut {
            // Provide the minimal value for the output
            value: commit_script_pubkey.minimal_non_dust_custom(fee_rate),
            // Send to the final address specified
            script_pubkey: final_address.script_pubkey(),
        }],
    };

    //
    // Adjust for reveal tx fee
    //

    // Get the weight of the reveal transaction
    let reveal_tx_weight = reveal_tx.weight();

    // Get the reveal transaction fee from the current fee rate
    // FeeRate x Weight = Fee
    let reveal_tx_fee =
        fee_rate
            .fee_wu(reveal_tx_weight)
            .ok_or(BitcoinUtilsError::FeeRateOverflow(
                fee_rate,
                reveal_tx_weight,
            ))?;

    trace!("Reveal TX fee: {}", reveal_tx_fee);

    // Increase the value of the commit tx to cover the reveal tx fee
    commit_tx
        .output
        .first_mut()
        .expect("Commit TX must have one output")
        .value += reveal_tx_fee;

    trace!(
        "Commit TX = {} Reveal TX = {}",
        commit_tx.raw_hex(),
        reveal_tx.raw_hex()
    );

    //
    // Fund commit tx
    //

    let commit_tx = crate::wallet::fund_tx(rpc, commit_tx, None)?;

    trace!("Commit TX funded: {}", commit_tx.raw_hex());

    //
    // Sign
    //

    let commit_tx = crate::wallet::sign_tx(rpc, commit_tx)?;

    trace!("Commit TX signed: {}", commit_tx.raw_hex());

    // Update the previous output of the reveal tx with the signed commit txid
    reveal_tx
        .input
        .first_mut()
        .expect("Reveal TX must have one input")
        .previous_output
        .txid = commit_tx.compute_txid();

    trace!(
        "Commit-Reveal IDs {} {}",
        commit_tx.compute_txid(),
        reveal_tx.compute_txid()
    );

    Ok((commit_tx, reveal_tx))
}

pub fn get_current_fee_rate(
    rpc: &Client,
    mode: Option<EstimateMode>,
    target: Option<u16>,
) -> FeeRate {
    let mode = mode.or(Some(EstimateMode::Economical));
    let target = target.unwrap_or(6);

    // Estimate fee rate in BTC/kB.
    let fee_rate = match rpc.estimate_smart_fee(target, mode) {
        // We use the fee rate if returned by the RPC
        Ok(EstimateSmartFeeResult {
            fee_rate: Some(fee_rate),
            ..
        }) => {
            trace!("Got fee rate from rpc (BTC/kVB): {}", fee_rate);
            let fee_rate = fee_rate.to_sat() / 4; // Convert to sats/kWU
            FeeRate::from_sat_per_kwu(fee_rate)
        }
        // In any other case, error or none, we use the default fee rate
        _ => DEFAULT_BTC_FEE_RATE,
    };

    trace!("Current fee rate is {}", fee_rate);

    FeeRate::clamp(fee_rate, MINIMUM_BTC_FEE_RATE, MAXIMUM_BTC_FEE_RATE)
}

pub fn convert_bytes_to_push_bytes(data: &[u8]) -> &PushBytes {
    unsafe { &*(data as *const [u8] as *const PushBytes) }
}

pub fn concatenate_op_push_data(witness: &[u8]) -> Result<Vec<u8>, BitcoinUtilsError> {
    // TODO instantiate with_capacity
    let mut concatenated_data = Vec::new();

    let script = ScriptBuf::from(witness.to_vec());

    for instruction in script.instructions() {
        match instruction {
            Ok(Instruction::PushBytes(bytes)) => {
                concatenated_data.extend_from_slice(bytes.as_bytes());
            }
            Ok(Instruction::Op(op))
                if op == bitcoin::opcodes::all::OP_DROP || op == bitcoin::opcodes::OP_TRUE =>
            {
                // Do nothing, ignore these opcodes
            }
            // Return an error if any other instruction is encountered
            Ok(_) => {
                return Err(BitcoinUtilsError::UnsuportedOpCode);
            }
            Err(_) => {
                return Err(BitcoinUtilsError::ErrorParsingWitnessScript);
            }
        }
    }

    Ok(concatenated_data)
}

pub fn create_multisig_script(
    public_keys: &[XOnlyPublicKey],
    required_sigs: i64,
) -> Result<ScriptBuf, BitcoinUtilsError> {
    // check if enough public keys are provided
    if (public_keys.len() as i64) < required_sigs {
        return Err(BitcoinUtilsError::InsufficientPublicKeys);
    }

    Ok(public_keys
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

pub fn create_multisig_spend_info(
    secp: &Secp256k1<All>,
    public_keys: &[XOnlyPublicKey],
    required_sigs: i64,
) -> Result<TaprootSpendInfo, BitcoinUtilsError> {
    let multisig_script = create_multisig_script(public_keys, required_sigs)?;

    let builder = TaprootBuilder::with_huffman_tree(vec![(1, multisig_script)])?;
    let internal_key = create_unspendable_internal_key();
    let spend_info = builder
        .finalize(secp, internal_key)
        .map_err(|_| BitcoinUtilsError::TaprootBuilderNotFinalizable)?;

    Ok(spend_info)
}

pub fn create_multisig_address(
    secp: &Secp256k1<All>,
    public_keys: &[XOnlyPublicKey],
    required_sigs: i64,
    network: Network,
) -> Result<Address, BitcoinUtilsError> {
    let spend_info = create_multisig_spend_info(secp, public_keys, required_sigs)?;

    Ok(Address::p2tr(
        secp,
        spend_info.internal_key(),
        spend_info.merkle_root(),
        network,
    ))
}

// TODO decouple errors
#[derive(Error, Debug)]
pub enum BitcoinUtilsError {
    #[error(transparent)]
    BitcoinRpcError(#[from] bitcoincore_rpc::Error),

    #[error(transparent)]
    WalletError(#[from] crate::wallet::WalletError),

    #[error("transaction rejected by mempool: {0}")]
    MempoolAcceptanceFailed(String),

    #[error("insufficient public keys provided")]
    InsufficientPublicKeys,

    #[error("fee rate overflow: {0} * {1} > u64::MAX")]
    FeeRateOverflow(FeeRate, Weight),

    #[error("taproot builder is not finalizable")]
    TaprootBuilderNotFinalizable,

    #[error("an error occured when building a taproot transaction")]
    TaprootBuilderError(#[from] bitcoin::taproot::TaprootBuilderError),

    #[error("unsupported opcode")]
    UnsuportedOpCode,

    #[error("error parsing witness script")]
    ErrorParsingWitnessScript,

    #[error("cannot construct control block for the given script")]
    CannotConstructControlBlock,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::consensus::encode;
    use bitcoin::secp256k1::Message;
    use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
    use bitcoin::taproot::LeafVersion;
    use bitcoin::{
        absolute::LockTime,
        key::Keypair,
        secp256k1::{PublicKey, Secp256k1, SecretKey},
        AddressType, Amount, Network, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
    };

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
                    &output.script_pubkey.as_bytes(),
                    output.value.to_sat(),
                    serialized_tx.as_slice(),
                    Some(&vec![spent_utxo]),
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
        (0..n)
            .map(|_| {
                let secret_key = SecretKey::new(&mut rand::thread_rng());
                Keypair::from_secret_key(&secp, &secret_key)
            })
            .collect()
    }

    #[test]
    fn test_create_multisig_address_single_key() {
        let secp = Secp256k1::new();
        let public_keys = generate_xonly_pubkeys(1);
        let required_sigs = 1;
        let network = Network::Bitcoin;

        let address = create_multisig_address(&secp, &public_keys, required_sigs, network)
            .expect("Failed to create multisig address");

        assert_eq!(address.address_type(), Some(AddressType::P2tr));
    }

    #[test]
    fn test_create_multisig_address_multiple_keys() {
        let secp = Secp256k1::new();
        let public_keys = generate_xonly_pubkeys(3);
        let required_sigs = 2;
        let network = Network::Bitcoin;

        let address = create_multisig_address(&secp, &public_keys, required_sigs, network)
            .expect("Failed to create multisig address");

        assert_eq!(address.address_type(), Some(AddressType::P2tr));
    }

    #[test]
    fn test_create_multisig_address_insufficient_keys() {
        let secp = Secp256k1::new();
        let public_keys = generate_xonly_pubkeys(1);
        let required_sigs = 2; // More signatures required than keys available
        let network = Network::Bitcoin;

        let result = create_multisig_address(&secp, &public_keys, required_sigs, network);

        assert!(matches!(
            result,
            Err(BitcoinUtilsError::InsufficientPublicKeys)
        ));
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
        let public_keys: Vec<XOnlyPublicKey> =
            keypairs.iter().map(|kp| kp.x_only_public_key().0).collect();

        let required_sigs = 3;
        let network = Network::Regtest;

        let script = create_multisig_script(&public_keys, required_sigs)
            .expect("Failed to create multisig script");

        let spend_info = create_multisig_spend_info(&secp, &public_keys, required_sigs)
            .expect("Failed to create multisig spend info");

        let control_block = spend_info
            .control_block(&(script.clone(), LeafVersion::TapScript))
            .expect("Should create control block");

        let multisig_address = create_multisig_address(&secp, &public_keys, required_sigs, network)
            .expect("Failed to create multisig address");

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
                script_pubkey: ScriptBuf::new_op_return(&[1]),
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
                witness.push(&[]); // Empty signature slots for unused keys
            }

            witness.push(&script.to_bytes());
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
                    witness.push(&[]);
                    continue;
                }
                let msg =
                    Message::from_digest_slice(sighash.as_ref()).expect("Failed to create message");
                let sig = secp.sign_schnorr(&msg, keypair);
                witness.push(sig.serialize());
            }

            witness.push(&script.to_bytes());
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

            witness.push(&script.to_bytes());
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
                witness.push(&[]); // Empty signature slots for unused keys
            }

            witness.push(&script.to_bytes());
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
}
