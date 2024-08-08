use thiserror::Error;

use bitcoin::blockdata::script::Builder;
use bitcoin::blockdata::transaction::{OutPoint, Transaction, TxIn, TxOut};
use bitcoin::script::PushBytes;
use bitcoin::secp256k1::{All, Keypair, Secp256k1};
use bitcoin::taproot::TaprootSpendInfo;
use bitcoin::{
    amount::Amount,
    bip32::Xpriv,
    blockdata::{locktime::absolute::LockTime, script, transaction, witness::Witness},
    key::rand,
    taproot::{LeafVersion, TaprootBuilder},
    Address, Network, ScriptBuf, XOnlyPublicKey,
};

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
) -> Result<Client, bitcoincore_rpc::Error> {
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
) -> Result<(Address, Option<Transaction>, u32), InitWalletError> {
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
        .get_new_address(Some(random_label), None)?
        .require_network(network)?;

    if created_wallet {
        rpc.generate_to_address(101, &address)?;

        let coinbase_txid = rpc
            .list_transactions(Some(random_label), Some(101), Some(100), None)?
            .get(0)
            .ok_or(InitWalletError::Internal)?
            .info
            .txid;

        coinbase_tx = Some(rpc.get_transaction(&coinbase_txid, None)?.transaction()?);
    }

    Ok((address, coinbase_tx, 0))
}

#[derive(Error, Debug)]
pub enum InitWalletError {
    #[error("cannot connect to the bitcoin node")]
    CannotConnectToBitcoinNode(#[from] bitcoincore_rpc::Error),

    #[error("tried to create a wallet on an invalid or non-existing network")]
    InvalidNetwork(#[from] bitcoin::address::ParseError),

    #[error("cannot parse a transactin")]
    CannotDecodeTransaction(#[from] bitcoin::consensus::encode::Error),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),

    #[error("internal error")]
    Internal,
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
    Xpriv::new_master(network, &[seed.try_into().unwrap()]).unwrap()
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

    let change_address = rpc.get_new_address(None, None)?;
    let change_script_pubkey = change_address.assume_checked().script_pubkey();
    Ok(TxOut {
        value: Amount::from_sat(change_amount),
        script_pubkey: change_script_pubkey,
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
pub fn sign_transaction_safe(rpc: &Client, unsigned_tx: Transaction) -> Transaction {
    let signed_raw_transaction = rpc
        .sign_raw_transaction_with_wallet(&unsigned_tx, None, None)
        .unwrap();
    if !signed_raw_transaction.complete {
        println!("{:#?}", signed_raw_transaction.errors);
        panic!("Transaction couldn't be signed.")
    }
    signed_raw_transaction.transaction().unwrap()
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
    let push_bytes: &PushBytes;
    unsafe {
        push_bytes = &*(data as *const [u8] as *const PushBytes);
    }
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
        sign_transaction_safe(rpc, unsigned_tx),
        script,
        taproot_spend_info,
    )
}

/// Collects a specified amount of Bitcoin from the wallet.
/// This function collects a specified amount of Bitcoin from the wallet by selecting
/// a set of unspent outputs (UTXOs) that sum up to the desired amount. The function
/// returns a vector of `OutPoint` objects that represent the selected UTXOs.
/// If the wallet does not have enough funds, the function returns an error.
///
/// # Arguments
///
/// * `rpc` - A Bitcoin RPC client of type `bitcoincore_rpc::Client`
/// * `amount` - The amount of Bitcoin to collect, of type `Amount`
/// * `fee` - The fee to pay for the transaction, of type `Amount`
///
/// # Returns
///
/// * `Vec<OutPoint>` - A vector of `OutPoint` objects representing the selected UTXOs
pub fn collect_amount(
    rpc: &Client,
    amount: Amount,
    fee: Amount,
) -> Result<Vec<OutPoint>, Box<dyn std::error::Error>> {
    // Fetch the list of unspent outputs
    let unspent = rpc.list_unspent(None, None, None, None, None)?;

    // Target amount is the sum of the desired amount and the fee
    let target_amount = amount.to_sat() + fee.to_sat();
    let mut collected_outpoints = Vec::new();
    let mut total_collected: u64 = 0;

    for utxo in unspent {
        // Break the loop if we've collected enough
        if total_collected >= target_amount {
            break;
        }

        // Create an OutPoint from the UTXO data
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

/// Converts a given secp256k1 private key to a Bitcoin address for a specified network.
///
/// This function takes a secp256k1 private key and a network identifier, and returns
/// the corresponding Pay-to-PubKey-Hash (P2PKH) Bitcoin address for that network.
///
/// # Arguments
///
/// * `secp` - A secp256k1 context of type `bitcoin::secp256k1::Secp256k1<All>`
/// * `private_key` - A secp256k1 private key of type `bitcoin::bip32::Xpriv`
/// * `network` - The Bitcoin network for which the address is generated. This is of type `Network`.
///
/// # Returns
/// * `Address` - A P2PKH Bitcoin address for the specified network.
pub fn get_address_from_private_key(
    secp: &Secp256k1<All>,
    private_key: &Xpriv,
    network: Network,
) -> Address {
    let receiver_pubkey = private_key.to_keypair(&secp).public_key();
    get_address_from_public_key(receiver_pubkey, network)
}

/// Converts a given secp256k1 public key to a Bitcoin address for a specified network.
///
/// This function takes a secp256k1 public key and a network identifier, and returns
/// the corresponding Pay-to-PubKey-Hash (P2PKH) Bitcoin address for that network.
///
/// # Arguments
///
/// * `pk` - A secp256k1 public key of type `bitcoin::secp256k1::PublicKey`
/// * `network` - The Bitcoin network for which the address is generated. This is of type `Network`.
///
/// # Returns
/// * `Address` - A P2PKH Bitcoin address for the specified network.
pub fn get_address_from_public_key(pk: bitcoin::secp256k1::PublicKey, network: Network) -> Address {
    let btc_pubkey = bitcoin::PublicKey::new(pk);
    Address::p2pkh(btc_pubkey, network)
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
    let input_info = collect_amount(&rpc, amount_to_send, fee).unwrap();

    let change = create_change_txout(&rpc, &input_info, amount_to_send, fee).unwrap();

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
