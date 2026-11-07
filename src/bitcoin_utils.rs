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
    opcodes::{
        all::{OP_CSV, OP_DROP, OP_PUSHNUM_1, OP_PUSHNUM_NEG1},
        Class, ClassifyContext, Opcode, OP_TRUE,
    },
    script::{Instruction, PushBytes},
    secp256k1::Secp256k1,
    taproot::{LeafVersion, TaprootBuilder},
    Address, Amount, Block, BlockHash, FeeRate, Network, ScriptBuf, Weight, XOnlyPublicKey,
};

use bitcoincore_rpc::json::{EstimateMode, EstimateSmartFeeResult};
use bitcoincore_rpc::{Auth, Client, RawTx, RpcApi};

use crate::{
    BTC_CONFIRMATIONS, DEFAULT_BTC_FEE_RATE, MAXIMUM_BTC_FEE_RATE, MINIMUM_BTC_FEE_RATE,
    WATCHONLY_WALLET_SUFFIX,
};

/// How long to wait between retries when bitcoind reports the wallet is mid-load.
const WALLET_LOAD_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
/// Maximum number of `load_wallet` attempts before giving up. 20 × 500 ms = 10 s,
const WALLET_LOAD_MAX_RETRIES: u32 = 20;

/// Returns the number of blocks to wait for before considering a
/// confirmed in a given network.
pub const fn confirmations(network: Network) -> u64 {
    use Network::*;
    match network {
        Bitcoin | Testnet => 6,
        Regtest | Signet | _ => 0,
    }
}

pub fn get_confirmed_from_height(height: u64) -> Option<u64> {
    // Since BTC_CONFIRMATIONS is 0 in regtest and sigtest
    // Clippy will complain about absurd comparisons
    #[allow(clippy::absurd_extreme_comparisons)]
    if height < BTC_CONFIRMATIONS {
        return None;
    }
    Some(height - BTC_CONFIRMATIONS)
}

//
// BitcoinCore RPC
//

pub fn make_rpc_client_from_env() -> Client {
    let rpc_user = std::env::var("RPC_USER").expect("RPC_USER env var not defined");
    let rpc_pass = std::env::var("RPC_PASS").expect("RPC_PASS env var not defined");
    let rpc_url = std::env::var("RPC_URL").expect("RPC_URL env var not defined");
    let wallet_name = std::env::var("WALLET_NAME").expect("WALLET_NAME env var not defined");

    let rpc = match init_rpc_client(rpc_user, rpc_pass, rpc_url, wallet_name.clone()) {
        Ok(rpc) => rpc,
        Err(e) => {
            panic!("Error making bitcoincore rpc client: {}", e);
        }
    };
    // The wallet named by WALLET_NAME is provisioned externally before the
    // monitor/provider start (by the deployment's bitcoin setup); we only load
    // it here. A wallet that won't load breaks every later RPC, so fail loud.
    load_wallet(&rpc, &wallet_name)
        .unwrap_or_else(|e| panic!("Error loading wallet {}: {}", wallet_name, e));
    rpc
}

pub fn make_watchonly_rpc_client_from_env() -> Client {
    let rpc_user = std::env::var("RPC_USER").expect("RPC_USER env var not defined");
    let rpc_pass = std::env::var("RPC_PASS").expect("RPC_PASS env var not defined");
    let rpc_url = std::env::var("RPC_URL").expect("RPC_URL env var not defined");
    let user_wallet_name = std::env::var("WALLET_NAME").expect("WALLET_NAME env var not defined");
    let wallet_name = format!("{}{}", user_wallet_name, WATCHONLY_WALLET_SUFFIX);

    let rpc = match init_rpc_client(rpc_user, rpc_pass, rpc_url, wallet_name.clone()) {
        Ok(rpc) => rpc,
        Err(e) => {
            panic!("Error making bitcoincore rpc client: {}", e);
        }
    };

    create_or_load_wallet(&rpc, &wallet_name, true)
        .unwrap_or_else(|_| panic!("Error creating {}", wallet_name));

    rpc
}

pub fn init_rpc_client(
    rpc_user: String,
    rpc_pass: String,
    rpc_url: String,
    wallet_name: String,
) -> Result<Client, BitcoinUtilsError> {
    // Append the wallet name into the URL.
    // Ensure there is no trailing slash, then add "/wallet/<wallet_name>".
    let full_url = format!("{}/wallet/{}", rpc_url.trim_end_matches('/'), wallet_name);
    let rpc = Client::new(&full_url, Auth::UserPass(rpc_user, rpc_pass))?;
    Ok(rpc)
}

/// Fetches blocks for the inclusive height range `[start, end]` in two batched
/// round-trips (hashes, then verbosity-0 blocks) rather than two per block,
/// returned in ascending-height order with their hash. `client` is a node-level
/// JSON-RPC client, e.g. `bitcoincore_rpc::Client::get_jsonrpc_client`.
pub fn fetch_blocks_batched(
    client: &bitcoincore_rpc::jsonrpc::Client,
    start: u64,
    end: u64,
) -> Result<Vec<(BlockHash, Block)>, bitcoincore_rpc::Error> {
    use bitcoincore_rpc::jsonrpc;
    use bitcoincore_rpc::Error;

    // 1) Resolve every height in the window to a hash in a single batched request.
    let hash_params: Vec<_> = (start..=end).map(|h| jsonrpc::arg([h])).collect();
    let hash_reqs: Vec<_> = hash_params
        .iter()
        .map(|p| client.build_request("getblockhash", Some(&**p)))
        .collect();
    let hashes: Vec<BlockHash> = client
        .send_batch(&hash_reqs)?
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            r.ok_or_else(|| {
                Error::ReturnedError(format!(
                    "missing getblockhash response for height {}",
                    start + i as u64
                ))
            })?
            .result::<BlockHash>()
            .map_err(Error::from)
        })
        .collect::<Result<_, _>>()?;

    // 2) Fetch each block as raw hex (verbosity 0) in a single batched request.
    let block_params: Vec<_> = hashes.iter().map(|h| jsonrpc::arg((h, 0))).collect();
    let block_reqs: Vec<_> = block_params
        .iter()
        .map(|p| client.build_request("getblock", Some(&**p)))
        .collect();
    let block_resps = client.send_batch(&block_reqs)?;

    let mut blocks = Vec::with_capacity(hashes.len());
    for (i, r) in block_resps.into_iter().enumerate() {
        let hex = r
            .ok_or_else(|| {
                Error::ReturnedError(format!("missing getblock response for {}", hashes[i]))
            })?
            .result::<String>()?;
        let block: Block = bitcoin::consensus::encode::deserialize_hex(&hex)?;
        blocks.push((hashes[i], block));
    }

    Ok(blocks)
}

/// Returns a provably unspendable internal key
pub fn unspenable_internal_key() -> XOnlyPublicKey {
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

/// One entry of the `testmempoolaccept` result. In a package result Bitcoin Core omits
/// `allowed` for a transaction it did not validate because another one failed first.
#[derive(Debug, serde::Deserialize)]
struct MempoolAcceptEntry {
    txid: String,
    allowed: Option<bool>,
    #[serde(rename = "reject-reason")]
    reject_reason: Option<String>,
    #[serde(rename = "package-error")]
    package_error: Option<String>,
}

/// Why the transactions of `entries` are not accepted, `None` if all of them are.
fn mempool_rejection(entries: &[MempoolAcceptEntry]) -> Option<String> {
    if !entries.is_empty() && entries.iter().all(|e| e.allowed == Some(true)) {
        return None;
    }
    let mut reasons: Vec<String> = entries
        .iter()
        .find_map(|e| e.package_error.as_ref())
        .map(|pe| format!("package error {pe}"))
        .into_iter()
        .collect();
    for e in entries {
        match e.allowed {
            Some(true) => {}
            Some(false) => reasons.push(format!(
                "txid={} {}",
                e.txid,
                e.reject_reason
                    .as_deref()
                    .unwrap_or("rejected without a reason")
            )),
            None => reasons.push(format!("txid={} not validated", e.txid)),
        }
    }
    if reasons.is_empty() {
        reasons.push("empty testmempoolaccept result".to_string());
    }
    Some(reasons.join(", "))
}

/// Tests `txs` for mempool acceptance, as one package (parents first) when there are several,
/// and if all are accepted sends them to the mempool in order. The error names Bitcoin Core's
/// reject reason.
pub fn submit_to_mempool(
    rpc: &Client,
    txs: Vec<transaction::Transaction>,
) -> Result<(), BitcoinUtilsError> {
    let hexes: Vec<String> = txs.iter().map(|tx| tx.raw_hex()).collect();
    // A raw call: bitcoincore-rpc 0.19 requires `allowed` in every entry
    let entries: Vec<MempoolAcceptEntry> = rpc
        .call("testmempoolaccept", &[serde_json::json!(hexes)])
        .map_err(|e| {
            error!("Error testing mempool acceptance: {}", e);
            for (i, hex) in hexes.iter().enumerate() {
                error!("TX_{}: {}", i, hex);
            }
            BitcoinUtilsError::MempoolAcceptanceFailed(e.to_string())
        })?;

    if let Some(reason) = mempool_rejection(&entries) {
        error!("Rejected by mempool: {}", reason);
        return Err(BitcoinUtilsError::MempoolAcceptanceFailed(reason));
    }

    for tx in txs {
        let txid = rpc.send_raw_transaction(tx.raw_hex())?;
        debug!("Transaction sent to mempool: {}", txid);
    }

    Ok(())
}

/// The data leaf script of `data`: each 520-byte chunk as a minimal push followed by
/// `OP_DROP`, then `OP_TRUE`.
pub fn make_push_data_script(data: &[u8]) -> ScriptBuf {
    let mut builder = Builder::new();
    for chunk in data.chunks(DATA_CHUNK_SIZE) {
        builder = match chunk {
            [b @ 1..=16] => builder.push_opcode(Opcode::from(OP_PUSHNUM_1.to_u8() + b - 1)),
            [0x81] => builder.push_opcode(OP_PUSHNUM_NEG1),
            _ => builder.push_slice(convert_bytes_to_push_bytes(chunk)),
        };
        builder = builder.push_opcode(OP_DROP);
    }
    builder.push_opcode(OP_TRUE).into_script()
}

/// The largest push Bitcoin script allows
const DATA_CHUNK_SIZE: usize = 520;

/// Makes a simple OP_RETURN script
pub fn make_op_return_script<T: AsRef<PushBytes>>(data: T) -> ScriptBuf {
    Builder::new()
        .push_opcode(bitcoin::opcodes::all::OP_RETURN)
        .push_slice(data)
        .into_script()
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
/// `extra_commit_output`, if given, becomes output 1 of the commit transaction, before
/// the wallet's change.
pub fn create_commit_reveal_txs(
    rpc: &Client,
    secp: &Secp256k1<bitcoin::secp256k1::All>,
    final_address: &Address,
    data: &[u8],
    amount_to_send: Option<Amount>,
    extra_commit_output: Option<TxOut>,
) -> Result<(Transaction, Transaction), BitcoinUtilsError> {
    trace!(
        "Creating commit-reveal tx for address={}, data={:02x?}",
        &final_address,
        &data
    );

    let fee_rate = get_fee_rate(rpc, None, None);

    // construct the script that will contain the data
    let commit_script = make_push_data_script(data);

    // this transaction can only be spent through the script path
    let unspendable_pubkey = unspenable_internal_key();

    let amount_to_send = amount_to_send.unwrap_or(Amount::ZERO);

    let builder = TaprootBuilder::new().add_leaf(0, commit_script.clone())?;
    let commit_spend_info = builder
        .finalize(secp, unspendable_pubkey)
        .map_err(|_| BitcoinUtilsError::TaprootBuilderNotFinalizable)?;

    let commit_script_pubkey = script::ScriptBuf::new_p2tr(
        secp,
        commit_spend_info.internal_key(),
        commit_spend_info.merkle_root(),
    );

    // The reveal forwards this value to final_address, so both outputs must clear Bitcoin Core's
    // dust limit at its default dust relay fee, whatever the current fee rate
    let commit_output_value = commit_script_pubkey
        .minimal_non_dust_custom(fee_rate)
        .max(commit_script_pubkey.minimal_non_dust())
        .max(final_address.script_pubkey().minimal_non_dust())
        .max(amount_to_send);

    let mut commit_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        // This will be filled by the wallet afterwards
        input: Vec::with_capacity(0),
        output: vec![TxOut {
            // This will be increased by the value needed for the reveal tx fee
            value: commit_output_value,
            script_pubkey: commit_script_pubkey.clone(),
        }],
    };
    if let Some(extra) = extra_commit_output {
        commit_tx.output.push(extra);
    }

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
            // Forward the value sent by the commit tx
            value: commit_output_value,
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

pub fn get_fee_rate(rpc: &Client, mode: Option<EstimateMode>, target: Option<u16>) -> FeeRate {
    let mode = mode.or(Some(EstimateMode::Economical));
    let target = target.unwrap_or(6);

    // Estimate fee rate in BTC/kvB.
    let fee_rate = match rpc.estimate_smart_fee(target, mode) {
        // We use the fee rate if returned by the RPC
        Ok(EstimateSmartFeeResult {
            fee_rate: Some(fee_rate),
            ..
        }) => {
            trace!("Got fee rate from rpc (BTC/kVB): {}", fee_rate);
            match FeeRate::from_sat_per_vb(fee_rate.to_sat() / 1000) {
                Some(fee_rate) => fee_rate,
                None => DEFAULT_BTC_FEE_RATE,
            }
        }
        // In any other case, error or none, we use the default fee rate
        _ => DEFAULT_BTC_FEE_RATE,
    };

    trace!(
        "Fee rate is {} with mode={:?} and target={}",
        fee_rate,
        mode,
        target
    );

    FeeRate::clamp(fee_rate, MINIMUM_BTC_FEE_RATE, MAXIMUM_BTC_FEE_RATE)
}

pub fn create_or_load_wallet(
    rpc: &Client,
    wallet_name: &str,
    disable_private_keys: bool,
) -> Result<(), BitcoinUtilsError> {
    // Positional createwallet args up to load_on_startup (pos 7); null keeps the defaults
    // for blank/passphrase/avoid_reuse/descriptors. load_on_startup=true so a freshly
    // created wallet also survives bitcoind restarts
    let wallet = rpc.call::<bitcoincore_rpc::json::LoadWalletResult>(
        "createwallet",
        &[
            wallet_name.into(),
            disable_private_keys.into(),
            serde_json::Value::Null,
            serde_json::Value::Null,
            serde_json::Value::Null,
            serde_json::Value::Null,
            true.into(),
        ],
    );
    if wallet.is_ok() {
        let wallet_info = wallet.unwrap();
        trace!("Created wallet '{}': {:?}", wallet_name, wallet_info);
        return Ok(());
    }

    let create_err = wallet.unwrap_err();
    if !create_err.to_string().contains("already exists") {
        error!("Error creating wallet '{}': {}", wallet_name, create_err);
        return Err(BitcoinUtilsError::BitcoinRpcError(create_err));
    }

    trace!(
        "Wallet '{}' already exists. Attempting to load it.",
        wallet_name
    );
    load_wallet(rpc, wallet_name)
}

/// Load a wallet that already exists, retrying if bitcoind reports it is mid-loading.
/// bitcoind serialises wallet loads; if it is already mid-loading this wallet
/// (e.g. it auto-reloaded on startup and our call raced it), it returns -4
/// "Wallet already loading". Retry briefly before propagating — the load
/// finishes within a couple of seconds in practice.
pub fn load_wallet(rpc: &Client, wallet_name: &str) -> Result<(), BitcoinUtilsError> {
    for attempt in 1..=WALLET_LOAD_MAX_RETRIES {
        match rpc.call::<bitcoincore_rpc::json::LoadWalletResult>(
            "loadwallet",
            &[wallet_name.into(), true.into()],
        ) {
            Ok(_) => {
                break;
            }
            Err(e) => {
                let msg = e.to_string();

                if msg.contains("already loaded") {
                    break;
                }

                if attempt == WALLET_LOAD_MAX_RETRIES {
                    error!("Error loading wallet '{}': {}", wallet_name, e);
                    return Err(BitcoinUtilsError::BitcoinRpcError(e));
                }

                if msg.contains("already loading") {
                    debug!(
                        "bitcoind is mid-loading wallet '{}', retry {}/{}",
                        wallet_name, attempt, WALLET_LOAD_MAX_RETRIES,
                    );
                }
                std::thread::sleep(WALLET_LOAD_RETRY_DELAY);
            }
        }
    }
    trace!("Loaded wallet '{}'", wallet_name);
    Ok(())
}

pub fn convert_bytes_to_push_bytes(data: &[u8]) -> &PushBytes {
    unsafe { &*(data as *const [u8] as *const PushBytes) }
}

/// A reveal input: the data leaf it executes, its control block and the data the leaf pushes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevealInput<'a> {
    pub leaf: &'a [u8],
    pub control_block: &'a [u8],
    pub data: Vec<u8>,
}

/// The leaf script and control block of a script-path spend (BIP341: the last two witness
/// elements, after an optional annex). `None` for a key-path spend or an empty witness.
pub fn script_path_leaf(input: &TxIn) -> Option<(&[u8], &[u8])> {
    let witness: Vec<&[u8]> = input.witness.iter().collect();
    let annex = witness.len() >= 2 && witness[witness.len() - 1].first() == Some(&0x50);
    match &witness[..witness.len() - usize::from(annex)] {
        [.., leaf, control_block] => Some((leaf, control_block)),
        _ => None,
    }
}

impl RevealInput<'_> {
    /// Checks the input is exactly as specified: a 33-byte control block (a single-leaf
    /// commitment) and a leaf that is the data leaf script of its data.
    pub fn check_exact(&self) -> Result<(), BitcoinUtilsError> {
        if self.control_block.len() != 33 {
            return Err(BitcoinUtilsError::NotASingleLeaf);
        }
        if self.leaf != make_push_data_script(&self.data).as_bytes() {
            return Err(BitcoinUtilsError::NotTheDataScript);
        }
        Ok(())
    }
}

/// The message data of a reveal input: the pushes of the leaf it executes. `None` when the
/// input is not a script-path spend or its leaf is not a data script.
pub fn reveal_input(input: &TxIn) -> Option<RevealInput<'_>> {
    let (leaf, control_block) = script_path_leaf(input)?;
    let data = concatenate_op_push_data(leaf).ok()?;
    Some(RevealInput {
        leaf,
        control_block,
        data,
    })
}

/// An unsigned commit and reveal pair for `data`: the commit spends a dummy outpoint and
/// carries `extra_outputs` after the commitment output. For tests and the db tester, which
/// have no wallet.
pub fn unsigned_commit_reveal(
    secp: &Secp256k1<bitcoin::secp256k1::All>,
    data: &[u8],
    extra_outputs: Vec<TxOut>,
) -> Result<(Transaction, Transaction), BitcoinUtilsError> {
    let commit_script = make_push_data_script(data);
    let info = TaprootBuilder::new()
        .add_leaf(0, commit_script.clone())?
        .finalize(secp, unspenable_internal_key())
        .map_err(|_| BitcoinUtilsError::TaprootBuilderNotFinalizable)?;
    let mut output = vec![TxOut {
        value: Amount::from_sat(1_000),
        script_pubkey: ScriptBuf::new_p2tr(secp, info.internal_key(), info.merkle_root()),
    }];
    output.extend(extra_outputs);
    let commit = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(
                bitcoin::Txid::from_byte_array(rand::random::<[u8; 32]>()),
                0,
            ),
            ..Default::default()
        }],
        output,
    };
    let control_block = info
        .control_block(&(commit_script.clone(), LeafVersion::TapScript))
        .ok_or(BitcoinUtilsError::CannotConstructControlBlock)?;
    let reveal = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(commit.compute_txid(), 0),
            witness: Witness::from_slice(&[commit_script.to_bytes(), control_block.serialize()]),
            ..Default::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(500),
            script_pubkey: ScriptBuf::new_op_return([2]),
        }],
    };
    Ok((commit, reveal))
}

/// One reveal with the inputs and the outputs of `reveals`, in order.
pub fn merge_reveals(reveals: impl IntoIterator<Item = Transaction>) -> Transaction {
    let mut merged = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: Vec::new(),
        output: Vec::new(),
    };
    for reveal in reveals {
        merged.input.extend(reveal.input);
        merged.output.extend(reveal.output);
    }
    merged
}

/// A rebuilding of a reveal: other inputs, another input order or other outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealShape {
    AsBuilt,
    /// A reveal that carries no message merged before it
    InputBefore,
    /// A reveal that carries no message merged after it
    InputAfter,
    /// One before and one after
    InputsAround,
    /// The inputs in reverse order
    Reversed,
    /// Every output paying another script
    OtherOutput,
    /// Output 0 split in two, the second paying another script
    ExtraOutput,
}

impl RevealShape {
    pub const NAMES: [&'static str; 7] = [
        "as-built",
        "input-before",
        "input-after",
        "inputs-around",
        "reversed",
        "other-output",
        "extra-output",
    ];
}

impl std::str::FromStr for RevealShape {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "as-built" => Self::AsBuilt,
            "input-before" => Self::InputBefore,
            "input-after" => Self::InputAfter,
            "inputs-around" => Self::InputsAround,
            "reversed" => Self::Reversed,
            "other-output" => Self::OtherOutput,
            "extra-output" => Self::ExtraOutput,
            _ => {
                return Err(format!(
                    "unknown reveal shape '{s}', expected one of {}",
                    Self::NAMES.join(", ")
                ))
            }
        })
    }
}

impl std::fmt::Display for RevealShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(Self::NAMES[*self as usize])
    }
}

/// `reveal` rebuilt in `shape`. `filler()` gives a reveal that carries no message (an
/// empty-data commit-reveal pair), `other_output` the script every output pays instead.
pub fn reshape_reveal<E>(
    reveal: Transaction,
    shape: RevealShape,
    mut filler: impl FnMut() -> Result<Transaction, E>,
    other_output: &ScriptBuf,
) -> Result<Transaction, E> {
    Ok(match shape {
        RevealShape::AsBuilt => reveal,
        RevealShape::InputBefore => merge_reveals([filler()?, reveal]),
        RevealShape::InputAfter => merge_reveals([reveal, filler()?]),
        RevealShape::InputsAround => {
            let before = filler()?;
            let after = filler()?;
            merge_reveals([before, reveal, after])
        }
        RevealShape::Reversed => {
            let mut reveal = reveal;
            reveal.input.reverse();
            reveal
        }
        RevealShape::OtherOutput => {
            let mut reveal = reveal;
            for output in &mut reveal.output {
                output.script_pubkey = other_output.clone();
            }
            reveal
        }
        RevealShape::ExtraOutput => {
            let mut reveal = reveal;
            let half = reveal.output[0].value / 2;
            reveal.output[0].value -= half;
            reveal.output.push(TxOut {
                value: half,
                script_pubkey: other_output.clone(),
            });
            reveal
        }
    })
}

/// The data of a data leaf script: the concatenation of the chunks that the pushes followed
/// by `OP_DROP` push, `OP_1`…`OP_16` and `OP_1NEGATE` included. The final `OP_TRUE` is not
/// data. Any non-minimal push or other instruction makes the script not a data leaf.
pub fn concatenate_op_push_data(leaf: &[u8]) -> Result<Vec<u8>, BitcoinUtilsError> {
    let script = ScriptBuf::from(leaf.to_vec());
    let mut data = Vec::with_capacity(leaf.len());
    let mut pending: Option<Vec<u8>> = None;
    for instruction in script.instructions_minimal() {
        let push = match instruction {
            Ok(Instruction::PushBytes(bytes)) => bytes.as_bytes().to_vec(),
            Ok(Instruction::Op(op)) => match op.classify(ClassifyContext::TapScript) {
                Class::PushNum(n) => vec![if n < 0 { 0x81 } else { n as u8 }],
                _ if op == OP_DROP => {
                    data.extend(pending.take().ok_or(BitcoinUtilsError::UnsuportedOpCode)?);
                    continue;
                }
                _ => return Err(BitcoinUtilsError::UnsuportedOpCode),
            },
            Err(_) => return Err(BitcoinUtilsError::ErrorParsingWitnessScript),
        };
        // Two pushes in a row: the first is not followed by OP_DROP
        if pending.replace(push).is_some() {
            return Err(BitcoinUtilsError::UnsuportedOpCode);
        }
    }
    Ok(data)
}

pub fn create_send_with_timelock_release_tx_script(
    secp: &Secp256k1<bitcoin::secp256k1::All>,
    receive_address: &Address,
    release_address: &Address,
    release_blocks: u32,
) -> Result<ScriptBuf, BitcoinUtilsError> {
    // Script 1. Send to the receive address
    let send_script = receive_address.script_pubkey();

    // Script 2. Release after n blocks
    let release_timelock_script = Builder::new()
        .push_int(release_blocks.into()) // Push the relative timelock value
        .push_opcode(OP_CSV) // Enforce relative timelock
        .push_opcode(OP_DROP) // Drop the timelock value
        .into_script();

    // Merge the release address with a timelock
    let mut release_script_bytes = release_timelock_script.into_bytes();
    release_script_bytes.extend(release_address.script_pubkey().to_bytes());
    let release_script = ScriptBuf::from_bytes(release_script_bytes);

    // Create the taproot output

    let taproot_builder = TaprootBuilder::new()
        .add_leaf(1, send_script)?
        .add_leaf(1, release_script)?;

    let unspendable_pubkey = unspenable_internal_key();

    let spend_info = taproot_builder
        .finalize(secp, unspendable_pubkey)
        .map_err(|_| BitcoinUtilsError::TaprootBuilderNotFinalizable)?;

    let script_pubkey =
        ScriptBuf::new_p2tr(secp, spend_info.internal_key(), spend_info.merkle_root());

    // TODO return both scripts and their control blocks
    Ok(script_pubkey)
}

pub fn create_tx_from_txouts(txouts: Vec<TxOut>) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: Vec::with_capacity(0),
        output: txouts,
    }
}

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

    #[error("commitment output is not a single leaf")]
    NotASingleLeaf,

    #[error("witness leaf is not the data script of the message")]
    NotTheDataScript,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mempool_rejection_reads_a_package_result_without_allowed() {
        let parse = |json: &str| -> Vec<MempoolAcceptEntry> { serde_json::from_str(json).unwrap() };
        // Bitcoin Core 27.2: the parent fails, the child has no `allowed`
        let failed = parse(
            r#"[{"txid":"15b6228e5da2c3971d87e21109e74ab871026d42a13aebdb1a2b88e169ff13bf","wtxid":"b24692f2c43a6dd756d51483f3085dc6c42db214d846265299dbe3747e1def14","allowed":false,"reject-reason":"missing-inputs"},{"txid":"bf9a64f0436ab2e313c9797cf3b464d70c65b623bb66f31ac20c410625de6567","wtxid":"f81fde140b190f7e2d4b2c14160e7eb83d0b72d0e06ee4e20f1a36c984205134"}]"#,
        );
        assert_eq!(
            mempool_rejection(&failed).unwrap(),
            "txid=15b6228e5da2c3971d87e21109e74ab871026d42a13aebdb1a2b88e169ff13bf missing-inputs, \
             txid=bf9a64f0436ab2e313c9797cf3b464d70c65b623bb66f31ac20c410625de6567 not validated"
        );

        let package_error = parse(
            r#"[{"txid":"aa","wtxid":"ab","package-error":"package-not-sorted"},{"txid":"ba","wtxid":"bb","package-error":"package-not-sorted"}]"#,
        );
        assert_eq!(
            mempool_rejection(&package_error).unwrap(),
            "package error package-not-sorted, txid=aa not validated, txid=ba not validated"
        );

        let accepted = parse(
            r#"[{"txid":"aa","wtxid":"ab","allowed":true,"vsize":200,"fees":{"base":0.00001}},{"txid":"ba","wtxid":"bb","allowed":true,"vsize":110,"fees":{"base":0.00001}}]"#,
        );
        assert_eq!(mempool_rejection(&accepted), None);
        assert!(mempool_rejection(&[]).is_some());
    }

    #[test]
    fn test_unspendable_internal_key_does_not_change() {
        let key = unspenable_internal_key();
        assert_eq!(key, unspenable_internal_key());
        assert_eq!(
            format!("{key}"),
            "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81799"
        );
    }

    fn input_with_witness(elements: &[Vec<u8>]) -> TxIn {
        TxIn {
            witness: Witness::from_slice(elements),
            ..Default::default()
        }
    }

    #[test]
    fn reveal_input_reads_the_data_of_the_executed_leaf() {
        let data = b"IPCXYZ some message bytes".to_vec();
        let leaf = make_push_data_script(&data).to_bytes();
        let control_block = vec![0xc0u8; 33];

        let input = input_with_witness(&[leaf.clone(), control_block.clone()]);
        let found = reveal_input(&input).unwrap();
        assert_eq!(found.data, data);
        assert_eq!(found.leaf, leaf.as_slice());
        assert_eq!(found.control_block, control_block.as_slice());

        // An annex after the control block, and stack elements below the leaf, are skipped
        let annexed = input_with_witness(&[leaf.clone(), control_block.clone(), vec![0x50, 1, 2]]);
        assert_eq!(reveal_input(&annexed).unwrap().data, data);
        let with_stack = input_with_witness(&[vec![7u8; 3], leaf.clone(), control_block.clone()]);
        assert_eq!(reveal_input(&with_stack).unwrap().data, data);

        // Data of more than one chunk
        let long = vec![0x42u8; 1_300];
        let input = input_with_witness(&[make_push_data_script(&long).to_bytes(), control_block]);
        assert_eq!(reveal_input(&input).unwrap().data, long);
    }

    #[test]
    fn data_leaf_pushes_are_minimal_at_the_chunk_boundary() {
        // 521 bytes: a full chunk, then a one-byte chunk whose minimal push depends on its value
        let full = vec![0x42u8; 520];
        let mut full_push = vec![0x4d, 0x08, 0x02];
        full_push.extend(&full);
        for (last, push) in [
            (0x01u8, vec![0x51]),
            (0x10, vec![0x60]),
            (0x81, vec![0x4f]),
            (0x11, vec![0x01, 0x11]),
            (0x00, vec![0x01, 0x00]),
        ] {
            let mut data = full.clone();
            data.push(last);
            let mut expected = full_push.clone();
            expected.push(0x75);
            expected.extend(&push);
            expected.extend([0x75, 0x51]);
            let leaf = make_push_data_script(&data).to_bytes();
            assert_eq!(leaf, expected, "last byte {last:#04x}");
            assert!(
                ScriptBuf::from(leaf.clone())
                    .instructions_minimal()
                    .all(|i| i.is_ok()),
                "last byte {last:#04x}: a non-minimal push"
            );
            assert_eq!(concatenate_op_push_data(&leaf).unwrap(), data);
            let input = input_with_witness(&[leaf, vec![0xc0u8; 33]]);
            assert_eq!(reveal_input(&input).unwrap().data, data);
        }

        // A single byte of data
        assert_eq!(
            make_push_data_script(&[0x05]).to_bytes(),
            vec![0x55, 0x75, 0x51]
        );
        assert_eq!(
            concatenate_op_push_data(&[0x55, 0x75, 0x51]).unwrap(),
            vec![0x05]
        );
        assert_eq!(make_push_data_script(&[]).to_bytes(), vec![0x51]);
        assert_eq!(concatenate_op_push_data(&[0x51]).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn data_leaf_decoder_rejects_other_scripts() {
        // A non-minimal push of a one-byte chunk, as rust-bitcoin's push_slice writes it
        assert!(concatenate_op_push_data(&[0x01, 0x05, 0x75, 0x51]).is_err());
        // A push without OP_DROP before another push, and OP_DROP without a push
        assert!(concatenate_op_push_data(&[0x55, 0x56, 0x75, 0x51]).is_err());
        assert!(concatenate_op_push_data(&[0x75, 0x51]).is_err());
        // Another opcode
        assert!(concatenate_op_push_data(&[0x55, 0x75, 0xb2, 0x51]).is_err());
        // A truncated push
        assert!(concatenate_op_push_data(&[0x02, 0x05]).is_err());
    }

    #[test]
    fn reveal_input_ignores_inputs_without_a_data_leaf() {
        let data = b"IPCXYZ some message bytes".to_vec();
        let leaf = make_push_data_script(&data).to_bytes();

        // A key-path spend, with and without an annex, and an empty witness
        assert!(reveal_input(&input_with_witness(&[vec![0u8; 64]])).is_none());
        assert!(reveal_input(&input_with_witness(&[vec![0u8; 64], vec![0x50]])).is_none());
        assert!(reveal_input(&input_with_witness(&[])).is_none());

        // The data script as a stack element under another leaf
        let other_leaf = Builder::new()
            .push_opcode(OP_CSV)
            .push_opcode(OP_DROP)
            .into_script()
            .to_bytes();
        assert!(reveal_input(&input_with_witness(&[leaf, other_leaf, vec![0xc0u8; 33]])).is_none());
    }

    #[test]
    fn check_exact_requires_the_data_script_and_a_single_leaf() {
        let data = b"IPCXYZ some message bytes".to_vec();
        let leaf = make_push_data_script(&data).to_bytes();
        let checked = |elements: &[Vec<u8>]| {
            reveal_input(&input_with_witness(elements))
                .unwrap()
                .check_exact()
        };
        assert!(checked(&[leaf.clone(), vec![0xc0u8; 33]]).is_ok());

        // A two-leaf control block
        assert!(matches!(
            checked(&[leaf.clone(), vec![0xc0u8; 65]]),
            Err(BitcoinUtilsError::NotASingleLeaf)
        ));

        // An extra empty push: the leaf decodes to the same data but is not its data script
        let mut extra = leaf.clone();
        extra.pop();
        extra.extend([
            bitcoin::opcodes::all::OP_PUSHBYTES_0.to_u8(),
            OP_DROP.to_u8(),
            OP_TRUE.to_u8(),
        ]);
        assert_eq!(concatenate_op_push_data(&extra).unwrap(), data);
        assert!(matches!(
            checked(&[extra, vec![0xc0u8; 33]]),
            Err(BitcoinUtilsError::NotTheDataScript)
        ));
    }
}
