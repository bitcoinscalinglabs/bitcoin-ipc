use bitcoin::blockdata::script::Builder;
use bitcoin::blockdata::transaction::{OutPoint, Transaction, TxIn, TxOut};
use bitcoin::script::PushBytes;
use bitcoin::secp256k1::{All, Keypair, Secp256k1, SecretKey};
use bitcoin::taproot::TaprootSpendInfo;
use bitcoin::{
    amount::Amount,
    bip32::Xpriv,
    blockdata::{locktime::absolute::LockTime, script, transaction, witness::Witness},
    key::rand,
    taproot::{LeafVersion, TaprootBuilder},
    Address, Network, ScriptBuf, XOnlyPublicKey,
};
use bitcoin::{CompressedPublicKey, PrivateKey, PublicKey};
use bitcoincore_rpc::json::{ScanTxOutRequest, Utxo};
use hex::encode;
use tiny_keccak::{Hasher, Keccak};

use bitcoincore_rpc::{Auth, Client, RawTx, RpcApi};

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
    let keypair = Keypair::from_secret_key(&secp, &secret_key);
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
) -> Result<Client, Box<dyn std::error::Error>> {
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
    wallet: &String,
) -> Result<(Address, Option<Transaction>, u32), Box<dyn std::error::Error>> {
    let random_number = rand::random::<usize>().to_string();
    let random_label = random_number.as_str();

    let mut created_wallet = false;

    match rpc.create_wallet(wallet, None, None, None, None) {
        Ok(_) => created_wallet = true,
        Err(_) => {}
    }

    let _ = rpc.load_wallet(wallet);
    let mut coinbase_tx = None;

    let address = rpc
        .get_new_address(Some(random_label), None)
        .unwrap()
        .require_network(network)?;

    if created_wallet {
        rpc.generate_to_address(101, &address)?;

        let coinbase_txid = rpc
            .list_transactions(Some(random_label), Some(101), Some(100), None)
            .unwrap()[0]
            .info
            .txid;

        coinbase_tx = Some(
            rpc.get_transaction(&coinbase_txid, None)
                .unwrap()
                .transaction()?,
        );
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
/// * `()` - The function returns nothing
pub fn test_and_submit(
    rpc: &Client,
    txs: Vec<transaction::Transaction>,
    miner_address: Address,
) -> () {
    let result =
        rpc.test_mempool_accept(&txs.iter().map(|tx| tx.raw_hex()).collect::<Vec<String>>());

    let mempool_failure = || {
        println!("Mempool acceptance test failed. Try manually testing for mempool acceptance using the bitcoin cli for more information, with the following transactions:");
        for (i, tx) in txs.iter().enumerate() {
            println!("Transaction #{}: {}", i + 1, tx.raw_hex());
        }
    };

    match result {
        Err(error) => {
            println!("{:#?}", error);
            mempool_failure();
        }
        Ok(response) => {
            for r in response.iter() {
                if !r.allowed {
                    mempool_failure();
                    return;
                }
            }

            for (i, tx) in txs.iter().enumerate() {
                println!(
                    "Transaction #{}: {}",
                    i + 1,
                    rpc.send_raw_transaction(tx.raw_hex()).unwrap()
                );

                println!("Transaction #{}: {:#?}", i + 1, tx)
            }
            println!(
                "Mined new block: {:#?}",
                rpc.generate_to_address(1, &miner_address).unwrap()
            );
        }
    }
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
pub fn get_private_key(seed: usize, network: Network) -> Xpriv {
    Xpriv::new_master(network, &seed.to_le_bytes()).unwrap()
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
    pubkey: Option<PublicKey>,
) -> Result<TxOut, Box<dyn std::error::Error>> {
    let mut input_total_value = 0;

    for input in input_info {
        input_total_value += rpc
            .get_tx_out(&input.txid, input.vout, None)?
            .unwrap()
            .value
            .to_sat();
    }

    let change_amount = input_total_value - amount_to_send.to_sat() - fee.to_sat();

    let script_pub_key: ScriptBuf;

    if !pubkey.is_some() {
        script_pub_key = rpc
            .get_new_address(None, None)?
            .assume_checked()
            .script_pubkey();
    } else {
        let wpkh = pubkey.unwrap().wpubkey_hash().expect("key is compressed");

        script_pub_key = ScriptBuf::new_p2wpkh(&wpkh);
    }
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
) -> Result<Transaction, Box<dyn std::error::Error>> {
    let signed_raw_transaction = rpc
        .sign_raw_transaction_with_wallet(&unsigned_tx, None, None)
        .unwrap();
    if !signed_raw_transaction.complete {
        println!("{:#?}", signed_raw_transaction.errors);
        Err("Transaction could not be signed")?
    }
    Ok(signed_raw_transaction.transaction().unwrap())
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
/// * `(Transaction, ScriptBuf, TaprootSpendInfo)` - A tuple containing the signed transaction, the script containing the data, and the Taproot spend information
pub fn commit_arbitrary_data(
    rpc: &Client,
    input_info: Vec<OutPoint>,
    amount_to_send: Amount,
    change: TxOut,
    data: &[u8],
    secp: &Secp256k1<All>,
) -> (Transaction, ScriptBuf, TaprootSpendInfo) {
    let push_bytes: &PushBytes = convert_bytes_to_push_bytes(data);
    let script = Builder::new().push_slice(push_bytes).into_script();

    // this transaction can only be spent through the script path
    let unspendable_pubkey = create_unspendable_internal_key(&secp);

    let taproot_spend_info = TaprootBuilder::new()
        .add_leaf(0, script.clone())
        .unwrap()
        .finalize(secp, unspendable_pubkey)
        .unwrap();

    let script_pubkey = script::ScriptBuf::new_p2tr(
        &secp,
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
                value: amount_to_send,
                script_pubkey: script_pubkey,
            },
            change,
        ],
    };

    (
        sign_transaction_safe(rpc, unsigned_tx).unwrap(),
        script,
        taproot_spend_info,
    )
}

fn assemble_outpoints_for_target_amount(
    output: Vec<Utxo>,
    target_amount: u64,
) -> Result<Vec<OutPoint>, Box<dyn std::error::Error>> {
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

        // Add the outpoint to the collection
        collected_outpoints.push(outpoint);

        // Add the amount to the total
        total_collected += utxo.amount.to_sat();
    }

    // Check if we have collected enough funds
    if total_collected >= target_amount {
        Ok(collected_outpoints)
    } else {
        Err("Not enough funds".into())
    }
}

pub fn collect_amount_for_wallet(
    rpc: &Client,
    amount: Amount,
    fee: Amount,
) -> Result<Vec<OutPoint>, Box<dyn std::error::Error>> {
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
    pubkey: PublicKey,
) -> Result<Vec<OutPoint>, Box<dyn std::error::Error>> {
    let desc = format!("wpkh({})", pubkey);

    let result = rpc.scan_tx_out_set_blocking(&[ScanTxOutRequest::Single(desc)])?;

    println!("{:?}", result);

    assemble_outpoints_for_target_amount(result.unspents, amount.to_sat() + fee.to_sat())
}

/// Converts a given secp256k1 private key to a Bitcoin address for a specified network.
///
/// This function takes a secp256k1 private key and a network identifier, and returns
/// the corresponding Pay-to-PubKey-Hash (P2PKH) Bitcoin address for that network.
///
/// # Arguments
///
/// * `private_key` - A secp256k1 private key of type `bitcoin::bip32::Xpriv`
/// * `network` - The Bitcoin network for which the address is generated. This is of type `Network`.
///
/// # Returns
/// * `Address` - A P2WPKH Bitcoin address for the specified network.
pub fn get_address_from_private_key(sk: SecretKey, network: Network) -> Address {
    Address::p2wpkh(
        &CompressedPublicKey::from_private_key(
            &Secp256k1::new(),
            &PrivateKey::new(sk, crate::NETWORK),
        )
        .unwrap(),
        network,
    )
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
) -> Transaction {
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
        .unwrap();

    for input in &mut unsigned_tx.input {
        input.witness.push(script.to_bytes());
        input.witness.push(control_block.serialize());
    }

    unsigned_tx
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
///
/// # Returns
///
/// * `(Transaction, Transaction)` - A tuple containing the commit transaction and the reveal transaction
pub fn write_arbitrary_data(
    rpc: &Client,
    amount_to_send: Amount,
    fee: Amount,
    data: &str,
    receiver_address: &Address,
) -> (Transaction, Transaction) {
    let input_info = collect_amount_for_wallet(&rpc, amount_to_send, fee).unwrap();

    println!("Input info: {:?}", input_info);

    let change = create_change_txout(&rpc, &input_info, amount_to_send, fee, None).unwrap();

    let secp = Secp256k1::new();

    // Commit the arbitrary data
    let (commit_tx, script, taproot_spend_info) = commit_arbitrary_data(
        &rpc,
        input_info,
        amount_to_send,
        change,
        data.as_bytes(),
        &secp,
    );

    let commit_tx_outpoint = OutPoint {
        txid: commit_tx.compute_txid(),
        vout: 0,
    };

    let output = TxOut {
        value: amount_to_send - fee,
        script_pubkey: receiver_address.script_pubkey(),
    };

    // Reveal Transaction : Reveal the Arbitrary Data
    let reveal_tx = reveal_arbitrary_data(commit_tx_outpoint, output, script, taproot_spend_info);

    (commit_tx, reveal_tx)
}

pub fn convert_bytes_to_push_bytes(data: &[u8]) -> &PushBytes {
    unsafe { &*(data as *const [u8] as *const PushBytes) }
}

/// This function creates a checkpoint transaction that commits a checkpoint hash to the blockchain.
/// The function creates a transaction that sends the checkpoint hash to an OP_RETURN output
/// and returns the signed transaction.
///
/// # Arguments
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `fee` - The fee to pay for the transaction, of type `Amount`
/// * `checkpoint_hash` - The checkpoint hash to commit, as a string
///
/// # Returns
///
/// * `Transaction` - A transaction that commits the checkpoint hash to the blockchain
pub fn create_checkpoint_tx(
    rpc: &Client,
    fee: Amount,
    checkpoint_hash: String,
    pubkey: PublicKey,
) -> Transaction {
    let input_info = collect_amount(&rpc, Amount::from_sat(0), fee, pubkey).unwrap();

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

    let change =
        create_change_txout(&rpc, &input_info, Amount::from_sat(0), fee, Some(pubkey)).unwrap();

    let push_bytes: &PushBytes = convert_bytes_to_push_bytes(checkpoint_hash.as_bytes());

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
    unsigned_tx
}

/// This function hashes a string using the Keccak256 algorithm.
///
/// # Arguments
///
/// * `input` - The string to hash
///
/// # Returns
///
/// * `String` - The hash of the input string
pub fn hash(input: String) -> String {
    let mut keccak = Keccak::v256();
    keccak.update(input.as_bytes());
    let mut hash = [0u8; 32];
    keccak.finalize(&mut hash);
    encode(hash)
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
    usize::from_str_radix(&hash[..8], 16).unwrap()
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
pub fn generate_keypair(input: String) -> Keypair {
    let secp = &Secp256k1::new();

    let seed = get_seed(input);

    let private_key = get_private_key(seed, crate::NETWORK);
    private_key.to_keypair(secp)
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
pub fn find_prevout_for_input(rpc: &Client, input: TxIn) -> TxOut {
    let txid = input.previous_output.txid;
    let vout = input.previous_output.vout;

    let tx_out = rpc.get_tx_out(&txid, vout, None).unwrap().unwrap();

    TxOut {
        value: tx_out.value,
        script_pubkey: ScriptBuf::from(tx_out.script_pub_key.hex),
    }
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
pub fn find_prevouts_for_tx(rpc: &Client, tx: Transaction) -> Vec<TxOut> {
    tx.input
        .iter()
        .map(|a: &TxIn| find_prevout_for_input(rpc, a.clone()))
        .collect()
}
