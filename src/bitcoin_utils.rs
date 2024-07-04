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

pub fn create_unspendable_internal_key(secp: &Secp256k1<All>) -> XOnlyPublicKey {
    let secret_key =
        bitcoin::secp256k1::SecretKey::new(&mut bitcoin::secp256k1::rand::thread_rng());
    let keypair = Keypair::from_secret_key(&secp, &secret_key);
    let (x_only_pubkey, _parity) = XOnlyPublicKey::from_keypair(&keypair);
    x_only_pubkey
}

pub fn init_rpc_client(
    rpc_user: String,
    rpc_pass: String,
    rpc_url: String,
) -> Result<Client, Box<dyn std::error::Error>> {
    let rpc = Client::new(&rpc_url, Auth::UserPass(rpc_user, rpc_pass))?;
    Ok(rpc)
}

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

pub fn get_private_key(seed: usize, network: Network) -> Xpriv {
    Xpriv::new_master(network, &[seed.try_into().unwrap()]).unwrap()
}

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

pub fn sign_transaction_safe(rpc: &Client, unsigned_tx: Transaction) -> Transaction {
    print!("Signing transaction...");
    print!("{:#?}", unsigned_tx);

    let signed_raw_transaction = rpc
        .sign_raw_transaction_with_wallet(&unsigned_tx, None, None)
        .unwrap();
    if !signed_raw_transaction.complete {
        println!("{:#?}", signed_raw_transaction.errors);
        panic!("Transaction couldn't be signed.")
    }
    signed_raw_transaction.transaction().unwrap()
}

pub fn commit_arbitrary_data(
    rpc: &Client,
    input_info: Vec<OutPoint>,
    amount_to_send: Amount,
    change: TxOut,
    data: &[u8],
    secp: &Secp256k1<All>,
    x_only_pubkey: XOnlyPublicKey,
) -> (Transaction, ScriptBuf, TaprootSpendInfo) {
    let push_bytes: &PushBytes;
    unsafe {
        push_bytes = &*(data as *const [u8] as *const PushBytes);
    }
    let script = Builder::new().push_slice(push_bytes).into_script();

    let taproot_spend_info = TaprootBuilder::new()
        .add_leaf(0, script.clone())
        .unwrap()
        .finalize(secp, x_only_pubkey)
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

pub fn get_address_from_private_key(
    secp: &Secp256k1<All>,
    private_key: &Xpriv,
    network: Network,
) -> Address {
    let receiver_pubkey = private_key.to_keypair(&secp).public_key();
    let btc_pubkey = bitcoin::PublicKey::new(receiver_pubkey);

    Address::p2pkh(btc_pubkey, network)
}

// Function to reveal the script in the witness field and create the final transaction
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
