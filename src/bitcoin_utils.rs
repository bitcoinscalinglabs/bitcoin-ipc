use bitcoin::{BlockHash, Txid};
use std::cmp::min;
use std::vec;
use thiserror::Error;

use bitcoin::blockdata::script::Builder;
use bitcoin::blockdata::transaction::{OutPoint, Transaction, TxIn, TxOut};
use bitcoin::script::{Instruction, PushBytes};
use bitcoin::secp256k1::{All, Keypair, Secp256k1};
use bitcoin::taproot::TaprootSpendInfo;
use bitcoin::{
    amount::Amount,
    bip32::Xpriv,
    blockdata::{locktime::absolute::LockTime, script, transaction, witness::Witness},
    key::{rand, TapTweak, TweakedPublicKey},
    opcodes::{all::OP_DROP, OP_TRUE},
    secp256k1::{schnorr::Signature, Message},
    sighash::{Prevouts, SighashCache},
    taproot::{LeafVersion, TaprootBuilder},
    Address, Network, ScriptBuf, TapSighashType, XOnlyPublicKey,
};
use bitcoincore_rpc::json::{EstimateMode, EstimateSmartFeeResult, ScanTxOutRequest, Utxo};
use hex::encode;
use tiny_keccak::{Hasher, Keccak};

use bitcoincore_rpc::{Auth, Client, RawTx, RpcApi};

pub struct CommitRevealFee {
    commit_fee: Amount,
    reveal_fee: Amount,
}

impl CommitRevealFee {
    pub fn new(commit_fee: Amount, reveal_fee: Amount) -> Self {
        CommitRevealFee {
            commit_fee,
            reveal_fee,
        }
    }
}

/// This function creates an unspendable internal key. The internal key is created
/// by generating a random secret key and deriving the corresponding public key.
///
/// # Arguments
///
/// * `secp` - A secp256k1 context of type `bitcoin::secp256k1::Secp256k1<All>`
///
/// # Returns
///
/// * `XOnlyPublicKey` - An unspendable internal key
pub fn create_unspendable_internal_key(secp: &Secp256k1<All>) -> XOnlyPublicKey {
    let secret_key =
        bitcoin::secp256k1::SecretKey::new(&mut bitcoin::secp256k1::rand::thread_rng());
    let keypair = Keypair::from_secret_key(secp, &secret_key);
    let (x_only_pubkey, _parity) = XOnlyPublicKey::from_keypair(&keypair);
    x_only_pubkey
}

/// This function initializes a Bitcoin RPC client.
/// The function creates a new RPC client using the provided RPC user, RPC password,
/// and RPC URL. The function returns the initialized RPC client
///
/// # Arguments
///
/// * `rpc_user` - The RPC user of the Bitcoin Core node
/// * `rpc_pass` - The RPC password of the Bitcoin Core node
/// * `rpc_url` - The RPC URL of the Bitcoin Core node
///
/// # Returns
///
/// * `Client` - An initialized RPC client
pub fn init_rpc_client(
    rpc_user: String,
    rpc_pass: String,
    rpc_url: String,
) -> Result<Client, BitcoinUtilsError> {
    let rpc = Client::new(&rpc_url, Auth::UserPass(rpc_user, rpc_pass))?;
    Ok(rpc)
}

/// This function initializes a Bitcoin wallet.
/// If the wallet exists, the function loads the existing wallet.
/// If the wallet doesn't exist, the function creates a new wallet using the provided wallet name and network
/// and generates 101 blocks to fund the wallet.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `network` - The Bitcoin network for which the wallet is created. This is of type `Network`
/// * `wallet` - The name of the wallet to create or load
///
/// # Returns
/// * `(Address, Option<Transaction>, u32)` - A tuple containing the wallet address, the coinbase transaction, and the vout index
pub fn init_wallet(
    rpc: &Client,
    network: Network,
    wallet: &str,
) -> Result<(Address, Option<Transaction>, u32), BitcoinUtilsError> {
    let random_number = rand::random::<usize>().to_string();
    let random_label = random_number.as_str();

    let mut created_wallet = false;

    if rpc.create_wallet(wallet, None, None, None, None).is_ok() {
        created_wallet = true;
    }

    let _ = rpc.load_wallet(wallet);
    let mut coinbase_tx = None;

    let address = rpc
        .get_new_address(Some(random_label), None)?
        .require_network(network)?;

    if created_wallet {
        rpc.generate_to_address(102, &address)?;

        let coinbase_txid = rpc
            .list_transactions(Some(random_label), Some(102), Some(101), None)?
            .first()
            .ok_or(BitcoinUtilsError::Internal)?
            .info
            .txid;

        coinbase_tx = Some(rpc.get_transaction(&coinbase_txid, None)?.transaction()?);
    }

    Ok((address, coinbase_tx, 0))
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
pub fn test_and_submit(
    rpc: &Client,
    txs: Vec<transaction::Transaction>,
    miner_address: Address,
) -> Result<(), BitcoinUtilsError> {
    let result = match rpc
        .test_mempool_accept(&txs.iter().map(|tx| tx.raw_hex()).collect::<Vec<String>>())
    {
        Ok(r) => r,
        Err(e) => {
            return Err(BitcoinUtilsError::MempoolAcceptanceFailed {
                reject_reason: e.to_string(),
            });
        }
    };

    let print_mempool_failure_message = || {
        println!("Mempool acceptance test failed. Try manually testing for mempool acceptance using the bitcoin cli for more information, with the following transactions:");
        for (i, tx) in txs.iter().enumerate() {
            println!("Transaction #{}: {}", i + 1, tx.raw_hex());
        }
    };

    for r in result.iter() {
        if !r.allowed {
            print_mempool_failure_message();
            let reject_reason = match &r.reject_reason {
                Some(r) => r.clone(),
                None => String::new(),
            };
            return Err(BitcoinUtilsError::MempoolAcceptanceFailed { reject_reason });
        }
    }

    for (i, tx) in txs.iter().enumerate() {
        println!(
            "Submitting transaction #{}: {}",
            i + 1,
            rpc.send_raw_transaction(tx.raw_hex())?
        );
    }
    println!(
        "Mined new block: {:#?}",
        rpc.generate_to_address(1, &miner_address)?
    );

    Ok(())
}

/// This function generates a new private key from a seed and a network.
///
/// # Arguments
///
/// * `seed` - The seed value used to generate the private key
/// * `network` - The Bitcoin network for which the private key is generated. This is of type `Network`
///
/// # Returns
///
/// * `Xpriv` - A private key of type `Xpriv`
pub fn generate_private_key(seed: usize, network: Network) -> Result<Xpriv, BitcoinUtilsError> {
    let seed_bytes = seed.to_be_bytes();
    Ok(Xpriv::new_master(network, &seed_bytes)?)
}

/// The function creates a change output for a transaction by calculating the change amount
/// and generating a new address for the change output.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `input_info` - A vector of `OutPoint` objects representing the UTXOs to spend
/// * `amount_to_send` - The amount of Bitcoin to send, of type `Amount`
/// * `fee` - The fee to pay for the transaction, of type `Amount`
///
/// # Returns
///
/// * `TxOut` - A change output of type `TxOut`
pub fn create_change_txout(
    rpc: &Client,
    input_info: &Vec<OutPoint>,
    amount_to_send: Amount,
    fee: Amount,
    address: Option<&Address>,
) -> Result<TxOut, BitcoinUtilsError> {
    let mut input_total_value = 0;

    for input in input_info {
        input_total_value += rpc
            .get_tx_out(&input.txid, input.vout, None)?
            .ok_or(BitcoinUtilsError::UtxoNotFound)?
            .value
            .to_sat();
    }

    if input_total_value < amount_to_send.to_sat() + fee.to_sat() {
        return Err(BitcoinUtilsError::InsufficientAmoutForChangeTx);
    };

    let change_amount = input_total_value - amount_to_send.to_sat() - fee.to_sat();

    let script_pub_key: ScriptBuf = if let Some(address) = address {
        address.script_pubkey()
    } else {
        rpc.get_new_address(None, None)?
            .assume_checked()
            .script_pubkey()
    };

    Ok(TxOut {
        value: Amount::from_sat(change_amount),
        script_pubkey: script_pub_key,
    })
}

/// Signs a transaction using the wallet's private keys.
/// This function takes an unsigned transaction and signs it using the wallet's private keys.
/// The function returns the signed transaction.
/// If the transaction cannot be signed, the function panics.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `unsigned_tx` - An unsigned transaction of type `Transaction`
///
/// # Returns
///
/// * `Transaction` - A signed transaction
pub fn sign_transaction_safe(
    rpc: &Client,
    unsigned_tx: Transaction,
) -> Result<Transaction, BitcoinUtilsError> {
    let signed_raw_transaction = rpc.sign_raw_transaction_with_wallet(&unsigned_tx, None, None)?;
    if !signed_raw_transaction.complete {
        return Err(BitcoinUtilsError::CannotSignTransaction {
            tx: unsigned_tx,
            errors: signed_raw_transaction.errors.unwrap_or(Vec::new()),
        });
    }
    Ok(signed_raw_transaction.transaction()?)
}

/// Commits arbitrary data to the blockchain.
/// This function commits arbitrary data to the blockchain by creating a transaction
/// that sends a specified amount of Bitcoin to a script that contains the data.
/// The function returns the signed transaction, the script containing the data, and
/// the Taproot spend information.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `input_info` - A vector of `OutPoint` objects representing the UTXOs to spend
/// * `amount_to_send` - The amount of Bitcoin to send, of type `Amount`
/// * `change` - The change output of type `TxOut`
/// * `data` - The arbitrary data to commit, as a byte slice
/// * `secp` - A secp256k1 context of type `bitcoin::secp256k1::Secp256k1<All
///
/// # Returns
///
/// * `(Transaction, ScriptBuf, TaprootSpendInfo)` - A tuple containing the transaction, the script containing the data, and the Taproot spend information
pub fn commit_arbitrary_data(
    input_info: Vec<OutPoint>,
    output_amount: Amount,
    change: TxOut,
    data: &[u8],
    secp: &Secp256k1<All>,
) -> Result<(Transaction, ScriptBuf, TaprootSpendInfo), BitcoinUtilsError> {
    // this transaction can only be spent through the script path
    let unspendable_pubkey = create_unspendable_internal_key(secp);

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

    let script = builder.into_script();

    let builder = TaprootBuilder::new().add_leaf(0, script.clone())?;
    let taproot_spend_info = builder
        .finalize(secp, unspendable_pubkey)
        .map_err(|_| BitcoinUtilsError::BuilderNotFinalizable)?;

    let script_pubkey = script::ScriptBuf::new_p2tr(
        secp,
        taproot_spend_info.internal_key(),
        taproot_spend_info.merkle_root(),
    );

    let input_vec: Vec<TxIn> = input_info
        .into_iter()
        .map(|input| TxIn {
            previous_output: input,
            script_sig: ScriptBuf::new(),
            sequence: transaction::Sequence::MAX,
            witness: Witness::default(),
        })
        .collect();

    let unsigned_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: input_vec,
        output: vec![
            TxOut {
                value: output_amount,
                script_pubkey,
            },
            change,
        ],
    };

    Ok((unsigned_tx, script, taproot_spend_info))
}

fn assemble_outpoints_for_target_amount(
    output: Vec<Utxo>,
    target_amount: u64,
) -> Result<Vec<OutPoint>, BitcoinUtilsError> {
    let mut collected_outpoints = Vec::new();
    let mut total_collected: u64 = 0;

    for utxo in output {
        // Break the loop if we've collected enough
        if total_collected >= target_amount {
            break;
        }

        let outpoint = OutPoint {
            txid: utxo.txid,
            vout: utxo.vout,
        };

        if utxo.amount.to_sat() < 600 {
            continue;
        }

        // Add the outpoint to the collection
        collected_outpoints.push(outpoint);

        // Add the amount to the total
        total_collected += utxo.amount.to_sat();
    }

    // Check if we have collected enough funds
    if total_collected >= target_amount {
        Ok(collected_outpoints)
    } else {
        Err(BitcoinUtilsError::InsufficientFunds)
    }
}

pub fn collect_amount_for_wallet(
    rpc: &Client,
    amount: Amount,
    fee: Amount,
) -> Result<Vec<OutPoint>, BitcoinUtilsError> {
    let unspent = rpc.list_unspent(None, None, None, None, None)?;

    let outpoints: Vec<Utxo> = unspent
        .iter()
        .map(|utxo| Utxo {
            txid: utxo.txid,
            vout: utxo.vout,
            amount: utxo.amount,
            height: 1,
            descriptor: "".to_string(),
            script_pub_key: utxo.script_pub_key.clone(),
        })
        .collect();

    assemble_outpoints_for_target_amount(outpoints, amount.to_sat() + fee.to_sat())
}

/// Collects a specified amount of Bitcoin from a specified pubkey
/// This function collects a specified amount of Bitcoin that the specified pubkey can spend
/// by selecting a set of unspent outputs (UTXOs) that sum up to the desired amount. The function
/// returns a vector of `OutPoint` objects that represent the selected UTXOs.
/// If the pubkey does not have enough funds, the function returns an error.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `amount` - The amount of Bitcoin to collect, of type `Amount`
/// * `fee` - The fee to pay for the transaction, of type `Amount`
/// * `pubkey` - The public key that can spend the utxos, of type `PublicKey`
///
/// # Returns
///
/// * `Vec<OutPoint>` - A vector of `OutPoint` objects representing the selected UTXOs
pub fn collect_amount(
    rpc: &Client,
    amount: Amount,
    fee: Amount,
    pubkey: XOnlyPublicKey,
) -> Result<Vec<OutPoint>, BitcoinUtilsError> {
    let desc = format!("tr({})", pubkey);

    let result = rpc.scan_tx_out_set_blocking(&[ScanTxOutRequest::Single(desc)])?;

    assemble_outpoints_for_target_amount(result.unspents, amount.to_sat() + fee.to_sat())
}

/// Collects all UTXOs that can be spent by a specified pubkey.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `pubkey` - The public key that can spend the utxos, of type `PublicKey`
///
/// # Returns
///
/// * `Amount` - The total amount of Bitcoin that the pubkey can spend
pub fn get_balance(rpc: &Client, pubkey: XOnlyPublicKey) -> Result<Amount, BitcoinUtilsError> {
    let desc = format!("tr({})", pubkey);

    let result = rpc.scan_tx_out_set_blocking(&[ScanTxOutRequest::Single(desc)])?;

    let mut total = Amount::ZERO;

    for utxo in result.unspents {
        total += utxo.amount;
    }

    Ok(total)
}

/// Converts a given x_only_pubkey to a taproot address.
///
/// # Arguments
///
/// * `x_only_pubkey` - A public key of type `XOnlyPublicKey`
/// * `network` - The Bitcoin network for which the address is generated. This is of type `Network`.
///
/// # Returns
/// * `Address` - A P2WPKH Bitcoin address for the specified network.
pub fn get_address_from_x_only_public_key(
    x_only_pubkey: XOnlyPublicKey,
    network: Network,
) -> Address {
    let secp = Secp256k1::new();
    Address::p2tr(&secp, x_only_pubkey, None, network)
}

/// This function reveals arbitrary data that was previously committed to the blockchain
/// using the `commit_arbitrary_data` function. The function creates a transaction that
/// reveals the arbitrary data by spending the output that contains the data.
/// The function returns the transaction that reveals the data.
///
/// # Arguments
///
/// * `prev_outpoint` - The OutPoint of the output that contains the arbitrary data
/// * `output` - The TxOut that specifies the amount and the recipient of the revealed data
/// * `script` - The script that contains the arbitrary data, of type `ScriptBuf`
/// * `taproot_spend_info` - The Taproot spend information that was used to commit the data
///
/// # Returns
///
/// * `Transaction` - A transaction that reveals the arbitrary data
pub fn reveal_arbitrary_data(
    prev_outpoint: OutPoint,
    output: TxOut,
    script: ScriptBuf,
    taproot_spend_info: TaprootSpendInfo,
) -> Result<Transaction, BitcoinUtilsError> {
    let mut unsigned_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: prev_outpoint,
            script_sig: script::ScriptBuf::new(),
            sequence: transaction::Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![output],
    };

    let control_block = taproot_spend_info
        .control_block(&(script.clone(), LeafVersion::TapScript))
        .ok_or(BitcoinUtilsError::CannotConstructControlBlock)?;

    for input in &mut unsigned_tx.input {
        input.witness.push(script.to_bytes());
        input.witness.push(control_block.serialize());
    }

    Ok(unsigned_tx)
}

/// This function writes arbitrary data to the blockchain by committing the data
/// and then revealing it. The function creates two transactions: the first transaction
/// commits the data by sending a specified amount of Bitcoin to a script that contains
/// the data, and the second transaction reveals the data by spending the output that
/// contains the data. The function returns the two transactions.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `amount_to_send` - The amount of Bitcoin to send, of type `Amount`
/// * `fee` - The fee to pay for the transaction, of type `Amount`
/// * `data` - The arbitrary data to commit, as a string
/// * `receiver_address` - The Bitcoin address of the recipient of the output associated with the reveal transaction
/// * `additional_outputs` - Additional outputs to include in the commit transaction
/// * `pubkey` - The public key that receives the change utxos, of type `PublicKey`
///
/// # Returns
///
/// * `(Transaction, Transaction)` - A tuple containing the commit transaction and the reveal transaction
pub fn write_arbitrary_data(
    rpc: &Client,
    amount_to_send: Amount,
    fee: CommitRevealFee,
    data: &str,
    receiver_address: &Address,
    additional_outputs: Vec<TxOut>,
    pubkey: Option<XOnlyPublicKey>,
) -> Result<(Transaction, Transaction), BitcoinUtilsError> {
    let input_info = if pubkey.is_none() {
        collect_amount_for_wallet(
            rpc,
            amount_to_send,
            fee.commit_fee + fee.reveal_fee + fee.reveal_fee,
        )?
    } else {
        collect_amount(
            rpc,
            amount_to_send,
            fee.commit_fee + fee.reveal_fee + fee.reveal_fee,
            pubkey.unwrap(),
        )?
    };

    let change = create_change_txout(
        rpc,
        &input_info,
        amount_to_send,
        fee.commit_fee + fee.reveal_fee + fee.reveal_fee,
        if pubkey.is_none() {
            None
        } else {
            Some(receiver_address)
        },
    )?;

    let secp = Secp256k1::new();

    // Commit the arbitrary data
    let (mut commit_tx, script, taproot_spend_info) = commit_arbitrary_data(
        input_info,
        fee.reveal_fee + fee.reveal_fee,
        change,
        data.as_bytes(),
        &secp,
    )?;

    commit_tx.output.extend(additional_outputs);

    if pubkey.is_none() {
        commit_tx = match sign_transaction_safe(rpc, commit_tx) {
            Ok(tx) => tx,
            Err(e) => return Err(e),
        };
    }

    let commit_tx_outpoint = OutPoint {
        txid: commit_tx.compute_txid(),
        vout: 0,
    };

    let script_pubkey = if pubkey.is_none() {
        rpc.get_new_address(None, None)?
            .assume_checked()
            .script_pubkey()
    } else {
        receiver_address.script_pubkey()
    };

    let output = TxOut {
        value: fee.reveal_fee,
        script_pubkey,
    };

    // Reveal Transaction : Reveal the Arbitrary Data
    let reveal_tx = reveal_arbitrary_data(commit_tx_outpoint, output, script, taproot_spend_info)?;

    Ok((commit_tx, reveal_tx))
}

/// This function calculates the expected fee to be paid for a transaction that has specific number
/// of inputs outputs and witness bytes.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `num_inputs` - The number of inputs in the transaction
/// * `num_outputs` - The number of outputs in the transaction
/// * `witness_bytes` - The number of witness bytes in the transaction
///
/// # Returns
///
/// * `Amount` - The expected fee to be paid for the transaction
pub fn calculate_fee(
    rpc: &Client,
    num_inputs: usize,
    num_outputs: usize,
    witness_bytes: usize,
) -> Amount {
    let input_size_vbytes = 75;
    let output_size_vbytes = 34;

    let base_tx_size_vbytes = (num_inputs * input_size_vbytes) + (num_outputs * output_size_vbytes);

    let additional_witness_vbytes = (witness_bytes as f64 * 0.25).ceil() as usize;

    let total_tx_size_vbytes = base_tx_size_vbytes + additional_witness_vbytes;

    let conf_target = 6;
    let default_fee_rate = Amount::from_sat(10000);

    let estimate_fee_result: EstimateSmartFeeResult =
        match rpc.estimate_smart_fee(conf_target, Some(EstimateMode::Economical)) {
            Ok(result) => result,
            Err(_) => EstimateSmartFeeResult {
                fee_rate: Some(default_fee_rate),
                blocks: conf_target as i64,
                errors: None,
            },
        };

    let mut fee_rate = match estimate_fee_result.fee_rate {
        Some(fee_rate) => fee_rate,
        None => default_fee_rate,
    };

    if fee_rate < Amount::from_sat(1000) {
        fee_rate = Amount::from_sat(1000);
    }
    if fee_rate > Amount::from_sat(100000) {
        fee_rate = Amount::from_sat(100000);
    }

    fee_rate * (total_tx_size_vbytes as u64) / 1000
}

pub fn convert_bytes_to_push_bytes(data: &[u8]) -> &PushBytes {
    unsafe { &*(data as *const [u8] as *const PushBytes) }
}

/// This function creates a withdraw transaction that creates multiple outputs each representing a withdraw
/// It also contains an OP_RETRUN output that contains the appropriate IPC tag and a change output.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `amount_to_send` - The amount of Bitcoin to send, of type `Amount`
/// * `fee` - The fee to pay for the transaction, of type `Amount`
/// * `data` - The arbitrary data to encode in the OP_RETURN, as a string
/// * `additional_outputs` - Additional outputs to include in the transaction
/// * `spender_address` - The spender bitcoin address
///
/// # Returns
///
/// * `Transaction` - A transaction that withdraws Bitcoin to multiple accounts
/// * `BitcoinUtilsError` - An error that occurred during the transaction creation
pub fn create_withdraw_tx(
    rpc: &Client,
    amount_to_send: Amount,
    fee: Amount,
    data: &[u8],
    additional_outputs: Vec<TxOut>,
    spender_address: &Address,
    pubkey: XOnlyPublicKey,
) -> Result<Transaction, BitcoinUtilsError> {
    let input_info = collect_amount(rpc, amount_to_send, fee, pubkey)?;

    let input_vec: Vec<TxIn> = input_info
        .clone()
        .into_iter()
        .map(|input| TxIn {
            previous_output: input,
            script_sig: ScriptBuf::new(),
            sequence: transaction::Sequence::MAX,
            witness: Witness::default(),
        })
        .collect();

    let change = create_change_txout(rpc, &input_info, amount_to_send, fee, Some(spender_address))?;

    let push_bytes: &PushBytes = convert_bytes_to_push_bytes(data);

    let op_return_out = transaction::TxOut {
        value: Amount::ZERO,
        script_pubkey: ScriptBuf::new_op_return(push_bytes),
    };

    let mut all_txouts = vec![op_return_out];
    if change.value.to_sat() > 600 {
        all_txouts.push(change);
    }
    all_txouts.extend(additional_outputs);

    let unsigned_tx = transaction::Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: input_vec,
        output: all_txouts,
    };

    // We don't need to sign the transaction, the subnetPK will sign it.
    Ok(unsigned_tx)
}

/// This function creates a checkpoint transaction that commits a checkpoint hash to the blockchain.
/// The function creates a transaction that contains the checkpoint hash in an OP_RETURN output,
/// and returns the unsigned transaction.
///
/// # Arguments
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `fee` - The fee to pay for the transaction, of type `Amount`
/// * `checkpoint_hash` - The checkpoint hash to commit, as a string
/// * `public_key` - The public key of the subnet that is committing the checkpoint hash
///
/// # Returns
///
/// * `Transaction` - A transaction that commits the checkpoint hash to the blockchain
pub fn create_checkpoint_tx(
    rpc: &Client,
    fee: Amount,
    checkpoint_hash: [u8; 32],
    public_key: XOnlyPublicKey,
) -> Result<Transaction, BitcoinUtilsError> {
    let input_info = collect_amount(rpc, Amount::from_sat(0), fee, public_key)?;

    let input_vec: Vec<TxIn> = input_info
        .clone()
        .into_iter()
        .map(|input| TxIn {
            previous_output: input,
            script_sig: ScriptBuf::new(),
            sequence: transaction::Sequence::MAX,
            witness: Witness::default(),
        })
        .collect();

    let subnet_address = get_address_from_x_only_public_key(public_key, crate::NETWORK);

    let change = create_change_txout(
        rpc,
        &input_info,
        Amount::from_sat(0),
        fee,
        Some(&subnet_address),
    )?;

    let data = format!("{}{}", crate::IPC_CHECKPOINT_TAG, crate::DELIMITER);

    let data_bytes = [data.as_bytes(), &checkpoint_hash].concat();

    let push_bytes: &PushBytes = convert_bytes_to_push_bytes(&data_bytes);

    let op_return_out = transaction::TxOut {
        value: Amount::ZERO,
        script_pubkey: ScriptBuf::new_op_return(push_bytes),
    };

    let unsigned_tx = transaction::Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: input_vec,
        output: vec![op_return_out, change],
    };

    // We don't need to sign the transaction, the subnetPK will sign it.
    Ok(unsigned_tx)
}

/// This function creates a deposit transaction that sends a specified amount of Bitcoin to a deposit address.
/// The function specifies the deposit address into an OP_RETURN output. It sends the deposited amount to the target address
/// And returns the the signed transaction.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * 'amount_to_send' - The amount of Bitcoin to send, of type `Amount`'
/// * `fee` - The fee to pay for the transaction, of type `Amount`
/// * `deposit_address` - The address that the wrapped tokens are sent to
/// * `target_address` - The address to which the Bitcoin is sent
///
/// # Returns
///
/// * `Transaction` - A transaction that deposits Bitcoin to the deposit address
/// * `BitcoinUtilsError` - An error that occurred during the transaction creation
pub fn create_deposit_tx(
    rpc: &Client,
    amount_to_send: Amount,
    fee: Amount,
    deposit_address: &str,
    target_address: &Address,
) -> Result<Transaction, BitcoinUtilsError> {
    let input_info = collect_amount_for_wallet(rpc, amount_to_send, fee)?;

    let input_vec: Vec<TxIn> = input_info
        .clone()
        .into_iter()
        .map(|input| TxIn {
            previous_output: input,
            script_sig: ScriptBuf::new(),
            sequence: transaction::Sequence::MAX,
            witness: Witness::default(),
        })
        .collect();

    let change = create_change_txout(rpc, &input_info, amount_to_send, fee, None)?;

    let data = format!("{}{}", crate::IPC_DEPOSIT_TAG, crate::DELIMITER);

    let target_address_bytes = deposit_address.as_bytes();

    let data_bytes = [data.as_bytes(), target_address_bytes].concat();

    let push_bytes: &PushBytes = convert_bytes_to_push_bytes(&data_bytes);

    let op_return_out = TxOut {
        value: Amount::ZERO,
        script_pubkey: ScriptBuf::new_op_return(push_bytes),
    };

    let output = TxOut {
        value: amount_to_send,
        script_pubkey: target_address.script_pubkey(),
    };

    let unsigned_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: input_vec,
        output: vec![op_return_out, output, change],
    };

    sign_transaction_safe(rpc, unsigned_tx)
}

/// This function hashes  an input string using the Keccak256 algorithm.
///
/// # Arguments
///
/// * `input` - The data to hash
///
/// # Returns
///
/// * `[u8; 32]` - The hash of the input as a 32-byte array
pub fn hash(input: String) -> [u8; 32] {
    let mut keccak = Keccak::v256();
    keccak.update(input.as_bytes());
    let mut hash = [0u8; 32];
    keccak.finalize(&mut hash);
    hash
}

/// This function generates a seed from a string input.
///
/// # Arguments
///
/// * `input` - The input string
///
/// # Returns
///
/// * `usize` - The seed generated from the input string
pub fn get_seed(input: String) -> usize {
    let hash = hash(input);
    let encoded = encode(hash);
    usize::from_str_radix(&encoded[..8], 16).unwrap_or(0)
}

/// This function generates a keypair from a string input.
///
/// # Arguments
///
/// * `input` - The input string
///
/// # Returns
///
/// * `Keypair` - A keypair generated from the input string
pub fn generate_keypair(input: String) -> Result<Keypair, BitcoinUtilsError> {
    let secp = &Secp256k1::new();

    let seed = get_seed(input);

    let private_key = generate_private_key(seed, crate::NETWORK)?;
    Ok(private_key.to_keypair(secp))
}

/// This function finds the previous output for a given input.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `input` - The input of type `TxIn`
///
/// # Returns
///
/// * `TxOut` - The previous output for the given input
pub fn find_prevout_for_input(rpc: &Client, input: TxIn) -> Result<TxOut, BitcoinUtilsError> {
    let txid = input.previous_output.txid;
    let vout = input.previous_output.vout;

    let tx_out_option = rpc.get_tx_out(&txid, vout, None).unwrap_or_default();

    let tx_out = match tx_out_option {
        Some(tx_out) => tx_out,
        None => return Err(BitcoinUtilsError::CannotGeneratePrevouts),
    };

    Ok(TxOut {
        value: tx_out.value,
        script_pubkey: ScriptBuf::from(tx_out.script_pub_key.hex),
    })
}

/// This function finds the previous outputs for a given transaction.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `tx` - The transaction of type `Transaction`
///
/// # Returns
///
/// * `Vec<TxOut>` - The previous outputs for the given transaction
pub fn find_prevouts_for_tx(
    rpc: &Client,
    tx: Transaction,
) -> Result<Vec<TxOut>, BitcoinUtilsError> {
    tx.input
        .iter()
        .map(|a: &TxIn| {
            let prevout = find_prevout_for_input(rpc, a.clone())?;
            Ok(prevout)
        })
        .collect::<Result<Vec<TxOut>, BitcoinUtilsError>>()
}

/// This function finds the block hash containing a given transaction ID.
/// The function iterates through all blocks in the blockchain to find the block
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `txid` - The transaction ID of type `Txid`
///
/// # Returns
///
/// * `BlockHash` - The block hash containing the given transaction ID
/// * `BitcoinUtilsError` - An error if the block hash cannot be found
pub fn find_block_hash_containing_txid(
    rpc: &Client,
    txid: &Txid,
) -> Result<BlockHash, BitcoinUtilsError> {
    let latest_block_height = match rpc.get_blockchain_info() {
        Ok(info) => info.blocks,
        Err(_) => {
            return Err(BitcoinUtilsError::Internal);
        }
    };

    for block_height in 0..=latest_block_height {
        let block_hash = match rpc.get_block_hash(block_height) {
            Ok(hash) => hash,
            Err(_) => {
                continue;
            }
        };

        let block = match rpc.get_block(&block_hash) {
            Ok(block) => block,
            Err(_) => {
                continue;
            }
        };

        if block.txdata.iter().any(|tx| &tx.compute_txid() == txid) {
            return Ok(block_hash);
        }
    }

    Err(BitcoinUtilsError::Internal)
}

/// This function finds the previous outputs for an already sent transaction.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `tx` - The transaction of type `Transaction`
///
/// # Returns
///
/// * `Vec<TxOut>` - The previous outputs for the given transaction
/// * `BitcoinUtilsError` - An error if the previous outputs cannot be found
pub fn find_prevouts_for_a_sent_transaction(
    rpc: &Client,
    tx: &Transaction,
) -> Result<Vec<TxOut>, BitcoinUtilsError> {
    tx.input
        .iter()
        .map(|input| {
            let prev_txid = input.previous_output.txid;
            let vout = input.previous_output.vout;

            let block_hash = find_block_hash_containing_txid(rpc, &prev_txid)?;

            let prev_tx: Transaction = rpc.get_raw_transaction(&prev_txid, Some(&block_hash))?;

            Ok(prev_tx.output[vout as usize].clone())
        })
        .collect()
}

/// This function verifies a taproot signature for a given transaction.
///
/// Arguments:
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `tx` - The transaction of type `Transaction`
/// * `public_key` - The public key of type `XOnlyPublicKey`
///
/// Returns:
///
/// * `bool` - A boolean indicating whether the signature is valid
pub fn verify_taproot_signature(
    rpc: &bitcoincore_rpc::Client,
    tx: &Transaction,
    public_key: XOnlyPublicKey,
) -> Result<bool, BitcoinUtilsError> {
    let secp = Secp256k1::new();

    let prevouts = match find_prevouts_for_a_sent_transaction(rpc, tx) {
        Ok(prevouts) => prevouts,
        Err(_) => return Err(BitcoinUtilsError::CannotLoadPrevouts),
    };

    for (i, input) in tx.input.iter().enumerate() {
        if let Some(signature_bytes) = input.witness.last() {
            let signature = match Signature::from_slice(signature_bytes) {
                Ok(sig) => sig,
                Err(_) => return Err(BitcoinUtilsError::InvalidSchnorrSig),
            };

            let mut sighash_cache = SighashCache::new(tx);

            let sighash = match sighash_cache.taproot_key_spend_signature_hash(
                i,
                &Prevouts::All(&prevouts),
                TapSighashType::Default,
            ) {
                Ok(sighash) => sighash,
                Err(_) => return Err(BitcoinUtilsError::ErrorCreatingSigHash),
            };

            let msg = match Message::from_digest_slice(&sighash[..]) {
                Ok(msg) => msg,
                Err(_) => return Err(BitcoinUtilsError::ErrorCreatingMessage),
            };

            let tweaked_pubkey: TweakedPublicKey = public_key.tap_tweak(&secp, None).0;

            if secp
                .verify_schnorr(&signature, &msg, &tweaked_pubkey.to_inner())
                .is_ok()
            {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn concatenate_op_push_data(witness: &[u8]) -> Result<Vec<u8>, BitcoinUtilsError> {
    let mut concatenated_data = Vec::new();

    let script = ScriptBuf::from(witness.to_vec().clone());

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

#[derive(Error, Debug)]
pub enum BitcoinUtilsError {
    #[error("cannot connect to the bitcoin node")]
    CannotConnectToBitcoinNode(#[from] bitcoincore_rpc::Error),

    #[error("tried to create a wallet on an invalid or non-existing network")]
    InvalidNetwork(#[from] bitcoin::address::ParseError),

    #[error("cannot parse a transaction")]
    CannotDeserializeTransaction(#[from] bitcoin::consensus::encode::Error),

    #[error(
        "failed to submit transaction, mempool acceptance test failed. reason: {reject_reason}"
    )]
    MempoolAcceptanceFailed { reject_reason: String },

    #[error("an error related to the BIP32 specification occured")]
    Bip32Error(#[from] bitcoin::bip32::Error),

    #[error("an error occured when building a taproot transaction")]
    TaprootBuilderError(#[from] bitcoin::taproot::TaprootBuilderError),

    #[error("unsupported opcode")]
    UnsuportedOpCode,

    #[error("error parsing witness script")]
    ErrorParsingWitnessScript,

    #[error("tried to finalize a taproot transaction builder that is not ready")]
    BuilderNotFinalizable,

    #[error("cannot construct control block for the given script")]
    CannotConstructControlBlock,

    #[error("the utxo was not found")]
    UtxoNotFound,

    #[error("cannot create change tx for the given arguments")]
    InsufficientAmoutForChangeTx,

    #[error("cannot generate prevouts")]
    CannotGeneratePrevouts,

    #[error("cannot collect utxos")]
    CannotCollectUtxos,

    #[error("insufficient funds")]
    InsufficientFunds,

    #[error("transaction could not be signed. tx: {:?}. errors: {:?}", tx, errors)]
    CannotSignTransaction {
        tx: Transaction,
        errors: Vec<bitcoincore_rpc::json::SignRawTransactionResultError>,
    },

    #[error("cannot generate a keypair")]
    CannotGenerateKeypaair,

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),

    #[error("cannot load prevouts")]
    CannotLoadPrevouts,

    #[error("invalid schnorr signature")]
    InvalidSchnorrSig,

    #[error("error creating signature hash")]
    ErrorCreatingSigHash,

    #[error("error creating message")]
    ErrorCreatingMessage,

    #[error("internal error")]
    Internal,
}
