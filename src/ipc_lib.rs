use std::collections::{BTreeMap, BTreeSet, HashMap};

use thiserror::Error;

use bitcoin::{
    address::NetworkUnchecked, secp256k1::PublicKey, Amount, Transaction, TxOut, XOnlyPublicKey,
};

use crate::{
    bitcoin_utils::{
        self, init_rpc_client, init_wallet, test_and_submit, write_arbitrary_data, CommitRevealFee,
    },
    ipc_state::{IPCState, ValidatorData},
    subnet_simulator::{SubnetSimulator, TransferEvent},
    utils,
};

/// Creates a child subnet by attaching arbitrary data to a Bitcoin transaction.
///
/// This function creates a Bitcoin transaction that includes specified arbitrary data and
/// submits it to the Bitcoin network. The transaction involves creating and revealing
/// a script containing the data using the Taproot script-path. This process ensures
/// the data is embedded in the blockchain.
///
/// # Arguments
///
/// * `subnet_address` - A reference to a `bitcoin::Address` that represents the subnet's multisig address.
/// * `subnet_data` - A string slice that holds the data to be embedded in the transaction. This data should contain:
///     - A known tag indicating the creation of a new IPC Subnet.
///     - The subnet name.
///     - Any additional arbitrary data.
///
/// # Returns
///
/// This function returns a `Result`:
/// * `Ok(())` - If the transaction is successfully created and submitted.
/// * `Err(Box<dyn std::error::Error>)` - If an error occurs during the process.
pub fn create_and_submit_create_child_tx(
    subnet_address: &bitcoin::Address,
    subnet_data: &str,
) -> Result<(Transaction, Transaction), IpcLibError> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let amount_to_send = Amount::from_btc(50.0)?;

    let commit_fee = bitcoin_utils::calculate_fee(&rpc, 2, 3, 65);
    let reveal_fee = bitcoin_utils::calculate_fee(&rpc, 1, 1, subnet_data.as_bytes().len());

    let fee = CommitRevealFee::new(commit_fee, reveal_fee);

    let output = TxOut {
        value: amount_to_send,
        script_pubkey: subnet_address.script_pubkey(),
    };

    let (commit_tx, reveal_tx) = write_arbitrary_data(
        &rpc,
        amount_to_send,
        fee,
        subnet_data,
        subnet_address,
        vec![output],
        None,
    )?;

    println!("Commit size: {:?}", commit_tx.vsize());
    println!("Commit fee: {:?}", commit_fee);
    println!("Reveal size: {:?}", reveal_tx.vsize());
    println!("Reveal fee: {:?}", reveal_fee);

    match test_and_submit(
        &rpc,
        vec![commit_tx.clone(), reveal_tx.clone()],
        miner_address,
    ) {
        Ok(_) => Ok((commit_tx, reveal_tx)),
        Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
    }
}

/// Joins an existing subnet by attaching validator data to a Bitcoin transaction.
///
/// This function creates a Bitcoin transaction that includes specified validator data
/// and submits it to the Bitcoin network. The transaction involves creating and revealing
/// a script containing the data using the Taproot script-path. This process ensures
/// the data is embedded in the blockchain.
///
/// # Arguments
///
/// * `subnet_address` - A reference to a `bitcoin::Address` that represents the subnet's multisig address.
/// * `collateral` - An `Amount` representing the collateral to be locked by the subnet's multisig address.
/// * `validator_data` - A string slice that holds the validator data to be embedded in the transaction.
///   This data should contain:
///     - Validator's information, such as their IP, for discovery by other validators.
///
/// # Returns
///
/// This function returns a `Result`:
/// * `Ok(())` - If the transaction is successfully created and submitted.
/// * `Err(JoinChildError)` - If an error occurs during the process.
pub fn create_and_submit_join_child_tx(
    subnet_address: &bitcoin::Address,
    collateral: Amount,
    validator_data: &str,
) -> Result<(), IpcLibError> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let output = TxOut {
        value: collateral,
        script_pubkey: subnet_address.script_pubkey(),
    };

    let commit_fee = bitcoin_utils::calculate_fee(&rpc, 2, 3, 65);
    let reveal_fee = bitcoin_utils::calculate_fee(&rpc, 1, 1, validator_data.as_bytes().len());

    let fee = CommitRevealFee::new(commit_fee, reveal_fee);

    let (commit_tx, reveal_tx) = write_arbitrary_data(
        &rpc,
        collateral,
        fee,
        validator_data,
        subnet_address,
        vec![output],
        None,
    )?;

    match test_and_submit(&rpc, vec![commit_tx, reveal_tx], miner_address) {
        Ok(_) => Ok(()),
        Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
    }
}

/// Submits a checkpoint of a subnet represented by IPCState and SubnetSimulator to the Bitcoin network.
///
/// This function creates a Bitcoin transaction that includes a checkpoint hash in an OP_RETURN output
/// This transaction gets signed by the subnetPK and the signature is added to the witness of the inputs
/// The transaction is then submitted to the Bitcoin network.
///
/// # Arguments
///
/// * `checkpoint_hash` - A string representing the hash of the checkpoint to be submitted.
/// * `ipc_state` - An instance of IPCState representing the state of the subnet.
/// * `simulator` - An instance of SubnetSimulator representing the state of the subnet.
pub fn submit_checkpoint(
    checkpoint_hash: [u8; 32],
    subnet_pk: PublicKey,
    simulator: SubnetSimulator,
) -> Result<(), SubmitCheckpointError> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let fee = bitcoin_utils::calculate_fee(&rpc, 2, 2, 65);

    let checkpoint_tx = bitcoin_utils::create_checkpoint_tx(
        &rpc,
        fee,
        checkpoint_hash,
        XOnlyPublicKey::from(subnet_pk),
    )?;

    let prevouts = bitcoin_utils::find_prevouts_for_tx(&rpc, checkpoint_tx.clone())?;

    // sign transaction with the subnetPK - the keypair of the subnet
    let signed_transaction = simulator.sign_transaction(checkpoint_tx.clone(), prevouts);

    match test_and_submit(&rpc, vec![signed_transaction], miner_address) {
        Ok(_) => Ok(()),
        Err(e) => Err(SubmitCheckpointError::BitcoinUtilsError(e)),
    }
}

/// Creates a deposit transaction for a subnet represented by a Bitcoin address and submits it to the Bitcoin network.
///
/// This function creates a Bitcoin transaction that includes specified deposit target address
/// and submits it to the Bitcoin network. The transaction involves creating an OP_RETURN output
/// that contains the address.
///
/// # Arguments
///
/// * `subnet_address` - A reference to a `bitcoin::Address` that represents the subnet's multisig address.
/// * `amount` - An `Amount` representing the amount to be deposited to the subnet's multisig address.
/// * `deposit_data` - A string slice that holds the deposit data to be embedded in the transaction.
pub fn create_and_submit_deposit_tx(
    subnet_address: &bitcoin::Address,
    amount: Amount,
    deposit_address: &str,
) -> Result<(), IpcLibError> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let fee = bitcoin_utils::calculate_fee(&rpc, 2, 3, 65);

    let deposit_tx =
        bitcoin_utils::create_deposit_tx(&rpc, amount, fee, deposit_address, subnet_address)?;

    match test_and_submit(&rpc, vec![deposit_tx], miner_address) {
        Ok(_) => Ok(()),
        Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
    }
}

pub fn create_and_submit_delete_tx(
    source_subnet_bitcoin_address: bitcoin::Address,
    source_subnet_pk: PublicKey,
    validators: Vec<ValidatorData>,
    collateral: Amount,
    simulator: &SubnetSimulator,
) -> Result<Transaction, IpcLibError> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let mut tx_outs = Vec::new();

    let command = format!("t={}", crate::IPC_DELETE_SUBNET_TAG,);

    for validator in validators.clone() {
        let tx_out = bitcoin::TxOut {
            value: collateral,
            script_pubkey: validator.get_address().assume_checked().script_pubkey(),
        };
        tx_outs.push(tx_out);
    }

    let fee = bitcoin_utils::calculate_fee(&rpc, 2, 2 + tx_outs.len(), 65);

    let delete_tx = bitcoin_utils::create_withdraw_tx(
        &rpc,
        Amount::from_sat(collateral.to_sat() * validators.len() as u64),
        fee,
        command.as_bytes(),
        tx_outs,
        &source_subnet_bitcoin_address,
        XOnlyPublicKey::from(source_subnet_pk),
    )?;

    let prevouts = bitcoin_utils::find_prevouts_for_tx(&rpc, delete_tx.clone())?;

    // sign transaction with the subnetPK - the keypair of the subnet
    let signed_transaction = simulator.sign_transaction(delete_tx, prevouts);

    match test_and_submit(&rpc, vec![signed_transaction.clone()], miner_address) {
        Ok(_) => Ok(signed_transaction),
        Err(e) => Err(IpcLibError::BitcoinUtilsError(e)),
    }
}

/// Creates a transfer transaction for a subnet represented by a Bitcoin address and submits it to the Bitcoin network.
///
/// This function creates a Bitcoin trnassaction that includes batched transfers to multiple subnets
/// and submits it to the Bitcoin network. The transaction involves creating commit and reveal
/// transactions. The commit transaction includes the transfers encoded in a taproot tx and locks
/// each transfer with the key of the subnet which is the target of a particular transfer. The
/// reveal transfer reveals all of the transfers.
///
/// # Arguments
///
/// * `source_subnet_address` - A reference to a `bitcoin::Address` that represents the source subnet's multisig address.
/// * `source_subnet_pk` - A `PublicKey` representing the public key of the source subnet.
/// * `transfer_map` - A reference to a `BTreeSet` of `TransferEvent` representing the transfers to be made.
/// * `subnets` - A vector of `IPCState` representing the state of the subnets.
/// * `simulator` - An instance of `SubnetSimulator` representing the state of the subnet.
pub fn create_and_submit_transfer_tx(
    source_subnet_address: bitcoin::Address,
    source_subnet_pk: PublicKey,
    transfer_map: &BTreeMap<String, BTreeSet<TransferEvent>>,
    subnets: Vec<IPCState>,
    simulator: &SubnetSimulator,
    submit_tx: bool,
) -> Result<(Transaction, Transaction), IpcLibError> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let mut subnet_id_to_address = HashMap::new();
    for subnet in subnets {
        subnet_id_to_address.insert(subnet.get_subnet_id(), subnet.get_bitcoin_address());
    }

    let serialized_transfers = match serde_json::to_string(&transfer_map) {
        Ok(t) => t,
        Err(_) => {
            return Err(IpcLibError::Internal);
        }
    };

    let command = format!(
        "t={}{}transfers={}",
        crate::IPC_TRANSFER_TAG,
        crate::DELIMITER,
        serialized_transfers
    );

    let mut tx_outs = Vec::new();
    let mut total_value_per_subnet = HashMap::new();

    for (target_subnet_id, transfers) in transfer_map {
        let map_result = match subnet_id_to_address.get(&target_subnet_id.clone()) {
            Some(result) => result,
            None => {
                return Err(IpcLibError::SubnetIdNotFound);
            }
        };

        let script_pubkey = match map_result {
            Ok(address) => address.script_pubkey(),
            Err(_) => {
                return Err(IpcLibError::Internal);
            }
        };

        for transfer in transfers {
            total_value_per_subnet
                .entry(target_subnet_id.clone())
                .and_modify(|e| *e += transfer.a.to_sat())
                .or_insert_with(|| transfer.a.to_sat());
        }

        let value = match total_value_per_subnet.get(&target_subnet_id.clone()) {
            Some(result) => result,
            None => {
                return Err(IpcLibError::SubnetIdNotFound);
            }
        };

        let tx_out = bitcoin::TxOut {
            value: Amount::from_sat(*value),
            script_pubkey,
        };
        tx_outs.push(tx_out);
    }

    let commit_fee = bitcoin_utils::calculate_fee(&rpc, 2, 3 + tx_outs.len(), 65);
    let reveal_fee = bitcoin_utils::calculate_fee(&rpc, 1, 1, command.as_bytes().len());

    let fee = CommitRevealFee::new(commit_fee, reveal_fee);

    let (commit_tx, reveal_tx) = match bitcoin_utils::write_arbitrary_data(
        &rpc,
        Amount::from_sat(total_value_per_subnet.values().sum::<u64>()),
        fee,
        command.as_str(),
        &source_subnet_address,
        tx_outs,
        Some(XOnlyPublicKey::from(source_subnet_pk)),
    ) {
        Ok(t) => t,
        Err(e) => {
            return Err(IpcLibError::BitcoinUtilsError(e));
        }
    };

    let prevouts = bitcoin_utils::find_prevouts_for_tx(&rpc, commit_tx.clone())?;

    // sign transaction with the subnetPK - the keypair of the subnet
    let signed_transaction = simulator.sign_transaction(commit_tx.clone(), prevouts);

    println!("Commit size: {:?}", commit_tx.vsize());
    println!("Commit fee: {:?}", commit_fee);
    println!("Reveal size: {:?}", reveal_tx.vsize());
    println!("Reveal fee: {:?}", reveal_fee);
    if !submit_tx {
        return Ok((signed_transaction, reveal_tx));
    }

    if let Err(e) = test_and_submit(
        &rpc,
        vec![signed_transaction.clone(), reveal_tx.clone()],
        miner_address,
    ) {
        return Err(IpcLibError::BitcoinUtilsError(e));
    }

    Ok((signed_transaction, reveal_tx))
}

/// Creates a withdraw transaction for a subnet represented by a Bitcoin address and submits it to the Bitcoin network.
///
/// This function creates a Bitcoin transaction that includes batched withdraws by multiple users from a subnet
/// and submits it to the Bitcoin network. The transaction involves creating commit and reveal
/// transactions. The commit transaction includes the withdraws encoded in a taproot tx and locks
/// each withdraw with the key of the user which is making the withdrawal
/// The reveal withdraw reveals all of the withdraws.
///
/// # Arguments
///
/// * `subnet_bitcoin_address` - A reference to a `bitcoin::Address` that represents the source subnet's multisig address.
/// * `subnet_pk` - A `PublicKey` representing the public key of the source subnet.
/// * `withdraws` - A reference to a `BTreeMap` of `bitcoin::Address` and `Amount` representing the withdraws to be made.`
/// * `simulator` - An instance of `SubnetSimulator` representing the state of the subnet.
pub fn create_and_submit_withdraw_tx(
    subnet_bitcoin_address: bitcoin::Address,
    subnet_pk: PublicKey,
    withdraws: &BTreeMap<bitcoin::Address<NetworkUnchecked>, Amount>,
    simulator: &SubnetSimulator,
    submit_tx: bool,
) -> Result<Transaction, IpcLibError> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = utils::load_env()?;
    let rpc = init_rpc_client(rpc_user, rpc_pass, rpc_url)?;
    let (miner_address, _, _) = init_wallet(&rpc, crate::NETWORK, &wallet_name)?;

    let mut tx_outs = Vec::new();

    let command = format!("t={}", crate::IPC_WITHDRAW_TAG,);

    for (address, amount) in withdraws {
        let tx_out = bitcoin::TxOut {
            value: *amount,
            script_pubkey: address.clone().assume_checked().script_pubkey(),
        };
        tx_outs.push(tx_out);
    }

    let fee = bitcoin_utils::calculate_fee(&rpc, 2, 2 + tx_outs.len(), 65);

    let withdraw_tx = match bitcoin_utils::create_withdraw_tx(
        &rpc,
        Amount::from_sat(withdraws.values().map(|x| x.to_sat()).sum::<u64>()),
        fee,
        command.as_bytes(),
        tx_outs,
        &subnet_bitcoin_address,
        XOnlyPublicKey::from(subnet_pk),
    ) {
        Ok(t) => t,
        Err(e) => {
            return Err(IpcLibError::BitcoinUtilsError(e));
        }
    };

    let prevouts = bitcoin_utils::find_prevouts_for_tx(&rpc, withdraw_tx.clone())?;

    // sign transaction with the subnetPK - the keypair of the subnet
    let signed_transaction = simulator.sign_transaction(withdraw_tx, prevouts);

    if !submit_tx {
        return Ok(signed_transaction);
    }

    println!("Size: {:?}", signed_transaction.vsize());
    println!("Fee: {:?}", fee.to_btc());

    if let Err(e) = test_and_submit(&rpc, vec![signed_transaction.clone()], miner_address) {
        return Err(IpcLibError::BitcoinUtilsError(e));
    }

    Ok(signed_transaction)
}

#[derive(Error, Debug)]
pub enum IpcLibError {
    #[error("error when reading an environment variable")]
    EnvVarError(#[from] std::env::VarError),

    #[error("cannot parse the given amount")]
    AmountError(#[from] bitcoin::amount::ParseAmountError),

    #[error(transparent)]
    BitcoinUtilsError(#[from] crate::bitcoin_utils::BitcoinUtilsError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),

    #[error("Subnet id not found")]
    SubnetIdNotFound,

    #[error("internal error")]
    Internal,
}

#[derive(Error, Debug)]
pub enum SubmitCheckpointError {
    #[error("error when reading an environment variable")]
    EnvVarError(#[from] std::env::VarError),

    #[error("init rpc and wallet error")]
    BitcoinUtilsError(#[from] crate::bitcoin_utils::BitcoinUtilsError),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),

    #[error("internal error")]
    Internal,
}
