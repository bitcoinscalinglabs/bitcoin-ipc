use super::*;
use crate::db::DatabaseCore;
use crate::eth_utils::{
    delegated_fvm_to_eth_address, evm_address_to_delegated_fvm, set_fvm_network,
};
use crate::test_utils::{self, create_test_db};
use crate::{collateral, DEFAULT_BTC_FEE_RATE};
use alloy_primitives::{I256, U256};
use bitcoin::hex::DisplayHex;
use bitcoin::secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use bitcoin::{Amount, XOnlyPublicKey};
use fvm_shared::address::{set_current_network, Network as FvmNetwork};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::str::FromStr;

fn set_test_fvm_network() {
    set_current_network(FvmNetwork::Testnet);
}

#[test]
fn test_ipc_tag_as_str() {
    assert_eq!(IpcTag::CreateSubnet.as_str(), IPC_CREATE_SUBNET_TAG);
}

#[test]
fn test_tagged_sha256_vector() {
    assert_eq!(
        hex::encode(tagged_sha256(JOIN_UE_SIGNATURE_TAG, b"abc")),
        "f8268caa7f0cd000ffa8e1c9ae5d3cb7095b18337a0d5a772828ed25162407c6"
    );
}

#[test]
fn test_ipc_tag_from_str() {
    assert_eq!(
        IpcTag::from_str(IPC_CREATE_SUBNET_TAG).unwrap(),
        IpcTag::CreateSubnet
    );
    assert!(IpcTag::from_str("INVALID_TAG").is_err());
}

//
// Create subnet
//

#[test]
fn test_ipc_create_subnet_msg_serialize() {
    set_test_fvm_network();
    let validator1 = XOnlyPublicKey::from_str(
        "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
    )
    .unwrap();
    let validator2 = XOnlyPublicKey::from_str(
        "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df",
    )
    .unwrap();

    let params = spec_example_create_msg(validator1, validator2);

    let serialized = params.to_bytes().expect("Serialization failed");
    assert_eq!(
        serialized,
        spec_example_create_bytes(validator1, validator2)
    );

    let params = IpcCreateSubnetMsg::from_bytes(&serialized).unwrap();
    assert_spec_example_create_msg(&params, validator1, validator2);
    params.validate().unwrap();
}

#[test]
fn a_second_create_of_an_existing_subnet_id_is_ignored() {
    set_test_fvm_network();
    let db = crate::test_utils::create_test_db();
    let msg = IpcCreateSubnetMsg {
        min_validator_stake: Amount::from_sat(10_000),
        power_unit_exponent: 4,
        min_validators: 2,
        bottomup_check_period: 10,
        active_validators_limit: 10,
        min_cross_msg_fee: Amount::from_sat(1),
        whitelist: crate::test_utils::generate_xonly_pubkeys(2),
        epoch_length: 10,
        min_timeout: 1,
        max_timeout: 1000,
        min_deposit: Amount::from_sat(330),
        kill_vote_period: 36,
        kill_delay_period: 5,
    };
    let txid = crate::test_utils::create_rand_txid();
    let subnet_id = SubnetId::from_txid(&txid);
    let mut txn = db.write_txn().unwrap();
    assert_eq!(
        msg.save_to_db(&db, &mut txn, 100, txid).unwrap().subnet_id,
        subnet_id
    );
    txn.commit().unwrap();

    let mut info = db.get_subnet_genesis_info(subnet_id).unwrap().unwrap();
    info.bootstrapped = true;
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_genesis_info(&mut wtxn, subnet_id, &info)
        .unwrap();
    wtxn.commit().unwrap();

    // A second create with other parameters and the same id is ignored, the state kept
    let mut other = msg.clone();
    other.min_validators = 1;
    let mut txn = db.write_txn().unwrap();
    let err = other.save_to_db(&db, &mut txn, 101, txid).unwrap_err();
    txn.commit().unwrap();
    assert!(matches!(err, IpcLibError::IpcValidateError(_)), "{err}");
    let kept = db.get_subnet_genesis_info(subnet_id).unwrap().unwrap();
    assert!(kept.bootstrapped);
    assert_eq!(kept.create_subnet_msg.min_validators, 2);
    assert_eq!(kept.create_msg_block_height, 100);
}

#[test]
fn a_batch_reveal_of_an_unprocessed_transaction_is_rejected() {
    set_test_fvm_network();
    let db = crate::test_utils::create_test_db();
    let source = crate::test_utils::generate_subnet(3);
    let destination = crate::test_utils::generate_subnet(3);
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_state(&mut wtxn, source.id, &source).unwrap();
    db.save_subnet_state(&mut wtxn, destination.id, &destination)
        .unwrap();
    wtxn.commit().unwrap();
    let msg = IpcCheckpointSubnetMsg {
        subnet_id: source.id,
        checkpoint_hash: <bitcoin::hashes::sha256::Hash as bitcoin::hashes::Hash>::from_byte_array(
            [7u8; 32],
        ),
        checkpoint_height: 1,
        next_committee_configuration_number: 0,
        withdrawals: vec![],
        transfers: vec![IpcCrossSubnetTransfer {
            amount: Amount::from_sat(1_000),
            destination_subnet_id: destination.id,
            subnet_multisig_address: Some(destination.committee.multisig_address.clone()),
            subnet_user_address: crate::test_utils::create_rand_eth_addr(),
        }],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: Some(source.committee.multisig_address.clone()),
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    // The reveal spends the commitment output of a transaction that is not recorded
    let unknown = crate::test_utils::create_rand_txid();
    let reveal = msg
        .make_reveal_batch_transfer_tx(
            unknown,
            crate::DEFAULT_BTC_FEE_RATE,
            &source.committee.address_checked(),
        )
        .unwrap()
        .unwrap();
    let mut found = IpcMessage::from_reveal_inputs(&reveal);
    assert_eq!(found.len(), 1);
    let Some((0, IpcMessage::BatchTransfer(mut batch))) = found.pop() else {
        panic!("the reveal carries a batch transfer at input 0");
    };
    let mut txn = db.write_txn().unwrap();
    let err = batch
        .process(
            &db,
            &mut txn,
            200,
            <bitcoin::BlockHash as bitcoin::hashes::Hash>::all_zeros(),
            reveal.compute_txid(),
        )
        .unwrap_err();
    txn.commit().unwrap();
    assert!(
        matches!(&err, IpcLibError::IpcValidateError(e) if e.to_string().contains("not found in database")),
        "{err}"
    );

    assert_eq!(db.get_next_rootnet_msg_nonce(destination.id).unwrap(), 0);
    assert!(db
        .get_subnet_state(source.id)
        .unwrap()
        .unwrap()
        .last_checkpoint_number
        .is_none());
}

const CHECKPOINT_BLOCK: u64 = 150;
const REVEAL_BLOCK: u64 = 160;

fn block_hash() -> bitcoin::BlockHash {
    <bitcoin::BlockHash as bitcoin::hashes::Hash>::all_zeros()
}

fn save_subnets(db: &db::HeedDb, subnets: &[&db::SubnetState]) {
    let mut wtxn = db.write_txn().unwrap();
    for subnet in subnets {
        db.save_subnet_state(&mut wtxn, subnet.id, subnet).unwrap();
    }
    wtxn.commit().unwrap();
}

/// A transfer of `sats` to a fresh address on `destination`, paid to `pays` (the
/// destination's current multisig by default)
fn transfer_paying(
    destination: &db::SubnetState,
    sats: u64,
    pays: &db::SubnetCommittee,
) -> IpcCrossSubnetTransfer {
    IpcCrossSubnetTransfer {
        amount: Amount::from_sat(sats),
        destination_subnet_id: destination.id,
        subnet_multisig_address: Some(pays.multisig_address.clone()),
        subnet_user_address: test_utils::create_rand_eth_addr(),
    }
}

fn transfer(destination: &db::SubnetState, sats: u64) -> IpcCrossSubnetTransfer {
    transfer_paying(destination, sats, &destination.committee)
}

/// A checkpoint message of `source` at subnet height `height`
fn checkpoint_msg(
    source: &db::SubnetState,
    height: u64,
    withdrawals: Vec<IpcWithdrawal>,
    transfers: Vec<IpcCrossSubnetTransfer>,
) -> IpcCheckpointSubnetMsg {
    IpcCheckpointSubnetMsg {
        subnet_id: source.id,
        checkpoint_hash: <bitcoin::hashes::sha256::Hash as bitcoin::hashes::Hash>::from_byte_array(
            [height as u8; 32],
        ),
        checkpoint_height: height,
        next_committee_configuration_number: 0,
        withdrawals,
        transfers,
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: Some(source.committee.multisig_address.clone()),
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    }
}

/// The checkpoint transaction of `msg`
fn checkpoint_tx(msg: &IpcCheckpointSubnetMsg, source: &db::SubnetState) -> Transaction {
    let mut utxo = test_utils::create_rand_utxo_entry(Some(Amount::from_sat(200_000)));
    utxo.script_pub_key = source.committee.address_checked().script_pubkey();
    msg.to_checkpoint_psbt(
        &source.committee,
        &source.committee,
        DEFAULT_BTC_FEE_RATE,
        &[utxo],
    )
    .unwrap()
    .unsigned_tx
}

/// Records `tx` as the checkpoint of `msg`
fn record_checkpoint(
    db: &db::HeedDb,
    msg: &IpcCheckpointSubnetMsg,
    tx: &Transaction,
) -> db::SubnetCheckpoint {
    let mut txn = db.write_txn().unwrap();
    db.save_transaction(&mut txn, tx).unwrap();
    let checkpoint = msg
        .save_to_db(
            db,
            &mut txn,
            CHECKPOINT_BLOCK,
            block_hash(),
            tx.compute_txid(),
        )
        .unwrap();
    txn.commit().unwrap();
    checkpoint
}

/// The batch transfer reveal of `msg`, spending the commitment output of `tx`
fn reveal_of(msg: &IpcCheckpointSubnetMsg, tx: &Transaction) -> Transaction {
    msg.make_reveal_batch_transfer_tx(
        tx.compute_txid(),
        DEFAULT_BTC_FEE_RATE,
        &test_utils::create_rand_addr(),
    )
    .unwrap()
    .unwrap()
}

/// The processing of the batch transfer in input `index` of `reveal`
fn process_batch(
    db: &db::HeedDb,
    reveal: &Transaction,
    index: usize,
) -> Result<db::SubnetCheckpoint, IpcLibError> {
    let mut found = IpcMessage::from_reveal_inputs(reveal);
    let position = found
        .iter()
        .position(|(i, msg)| *i == index && matches!(msg, IpcMessage::BatchTransfer(_)))
        .expect("a batch transfer in that input");
    let IpcMessage::BatchTransfer(mut batch) = found.swap_remove(position).1 else {
        unreachable!()
    };
    let mut txn = db.write_txn().unwrap();
    let checkpoint = batch.process(
        db,
        &mut txn,
        REVEAL_BLOCK,
        block_hash(),
        reveal.compute_txid(),
    );
    txn.commit().unwrap();
    checkpoint
}

fn rejected(result: Result<db::SubnetCheckpoint, IpcLibError>, reason: &str) {
    match result {
        Err(IpcLibError::IpcValidateError(e)) => {
            assert!(e.to_string().contains(reason), "{e}")
        }
        other => panic!("expected a rejection about {reason:?}, got {other:?}"),
    }
}

/// The sats credited to `subnet` by fund rootnet messages
fn credited(db: &db::HeedDb, subnet: &db::SubnetState) -> Vec<u64> {
    db.get_all_rootnet_msgs(subnet.id)
        .unwrap()
        .iter()
        .map(|m| match m {
            db::RootnetMessage::FundSubnet { msg, .. } => msg.amount.to_sat(),
            other => panic!("not a fund message: {other:?}"),
        })
        .collect()
}

/// A commitment output and reveal input for `data`, as anyone can build them
fn commitment_of(data: &[u8]) -> (bitcoin::TxOut, bitcoin::TxIn) {
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (commit, reveal) = bitcoin_utils::unsigned_commit_reveal(&secp, data, vec![]).unwrap();
    (commit.output[0].clone(), reveal.input[0].clone())
}

#[test]
fn the_batches_the_checkpoint_pays_are_credited_to_its_own_record() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let a = test_utils::generate_subnet(2);
    let b = test_utils::generate_subnet(2);
    save_subnets(&db, &[&source, &a, &b]);

    // Two transfers to a and one to b: one output per destination, in txid20 order
    let msg = checkpoint_msg(
        &source,
        1,
        vec![],
        vec![
            transfer(&a, 30_000),
            transfer(&b, 50_000),
            transfer(&a, 40_000),
        ],
    );
    let tx = checkpoint_tx(&msg, &source);
    let (_, batch_count, _, _) =
        IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&tx.output[0]).unwrap();
    assert_eq!(
        batch_count, 2,
        "one batch per destination, not per transfer"
    );
    assert_eq!(tx.output.len(), 5, "OP_RETURN, commitment, a, b, change");
    let batches = transfer_batches(&msg.transfers);
    assert_eq!(batches.len(), 2);
    for (i, (txid20, total)) in batches.iter().enumerate() {
        let output = &tx.output[msg.checkpoint_outputs().transfer(i)];
        assert_eq!(output.value, *total);
        let destination = if *txid20 == a.id.txid20() { &a } else { &b };
        assert_eq!(
            output.script_pubkey,
            destination.committee.address_checked().script_pubkey()
        );
    }
    let reveal = reveal_of(&msg, &tx);
    assert_eq!(
        reveal.input[0].previous_output.vout as usize,
        msg.checkpoint_outputs().commitment()
    );
    assert!(
        tx.output[1].script_pubkey.is_p2tr(),
        "the commitment output"
    );

    // The next checkpoint is recorded before the reveal is processed
    record_checkpoint(&db, &msg, &tx);
    let next = checkpoint_msg(&source, 2, vec![], vec![]);
    record_checkpoint(&db, &next, &checkpoint_tx(&next, &source));

    let record = process_batch(&db, &reveal, 0).unwrap();
    assert_eq!(record.checkpoint_number, 0);
    assert_eq!(record.batch_transfer_txid, Some(reveal.compute_txid()));
    assert_eq!(record.batch_transfer_block_height, Some(REVEAL_BLOCK));
    let saved = db.get_checkpoint(source.id, 0).unwrap().unwrap();
    assert_eq!(saved.batch_transfer_txid, Some(reveal.compute_txid()));
    assert_eq!(saved.txid, tx.compute_txid());
    assert!(db
        .get_checkpoint(source.id, 1)
        .unwrap()
        .unwrap()
        .batch_transfer_txid
        .is_none());
    assert!(db.get_checkpoint(source.id, 2).unwrap().is_none());
    assert_eq!(credited(&db, &a), vec![30_000, 40_000]);
    assert_eq!(credited(&db, &b), vec![50_000]);

    // The same reveal processed again changes nothing
    process_batch(&db, &reveal, 0).unwrap();
    assert_eq!(credited(&db, &a), vec![30_000, 40_000]);
}

#[test]
fn a_reveal_from_a_withdrawal_output_is_rejected() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let a = test_utils::generate_subnet(2);
    save_subnets(&db, &[&source, &a]);

    // The attacker withdraws to an address that commits to forged batch data: a
    // transfer of 1 BTC to itself on a
    let forged = checkpoint_msg(&source, 7, vec![], vec![transfer(&a, 100_000_000)]);
    let (commitment, mut forged_input) =
        commitment_of(&forged.make_batched_transfer_data().unwrap());
    let withdrawal = IpcWithdrawal {
        amount: Amount::from_sat(5_000),
        address: bitcoin::Address::from_script(&commitment.script_pubkey, NETWORK)
            .unwrap()
            .into_unchecked(),
    };
    let msg = checkpoint_msg(&source, 1, vec![withdrawal], vec![transfer(&a, 1_000)]);
    let tx = checkpoint_tx(&msg, &source);
    record_checkpoint(&db, &msg, &tx);

    // The forged reveal spends the withdrawal output, output 1
    forged_input.previous_output = bitcoin::OutPoint::new(tx.compute_txid(), 1);
    let forged_reveal = Transaction {
        input: vec![forged_input],
        ..reveal_of(&msg, &tx)
    };
    rejected(
        process_batch(&db, &forged_reveal, 0),
        "the commitment output is 2",
    );
    assert!(credited(&db, &a).is_empty());

    process_batch(&db, &reveal_of(&msg, &tx), 0).unwrap();
    assert_eq!(credited(&db, &a), vec![1_000]);
}

#[test]
fn a_reveal_from_an_extra_output_of_the_checkpoint_is_rejected() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let a = test_utils::generate_subnet(2);
    save_subnets(&db, &[&source, &a]);

    // A compromised source signs a checkpoint with an extra output that commits to
    // forged batch data, after the change
    let forged = checkpoint_msg(&source, 7, vec![], vec![transfer(&a, 100_000_000)]);
    let (commitment, mut forged_input) =
        commitment_of(&forged.make_batched_transfer_data().unwrap());
    let msg = checkpoint_msg(&source, 1, vec![], vec![transfer(&a, 1_000)]);
    let mut tx = checkpoint_tx(&msg, &source);
    tx.output.push(commitment);
    record_checkpoint(&db, &msg, &tx);

    forged_input.previous_output =
        bitcoin::OutPoint::new(tx.compute_txid(), tx.output.len() as u32 - 1);
    let forged_reveal = Transaction {
        input: vec![forged_input],
        ..reveal_of(&msg, &tx)
    };
    rejected(
        process_batch(&db, &forged_reveal, 0),
        "the commitment output is 1",
    );
    assert!(credited(&db, &a).is_empty());
}

#[test]
fn a_second_batch_transfer_for_the_same_checkpoint_is_rejected() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let a = test_utils::generate_subnet(2);
    save_subnets(&db, &[&source, &a]);
    let msg = checkpoint_msg(&source, 1, vec![], vec![transfer(&a, 1_000)]);
    let tx = checkpoint_tx(&msg, &source);
    record_checkpoint(&db, &msg, &tx);
    let reveal = reveal_of(&msg, &tx);
    process_batch(&db, &reveal, 0).unwrap();

    // The same data in another reveal of the same commitment output
    let mut again = reveal.clone();
    again.output.push(bitcoin::TxOut {
        value: Amount::from_sat(330),
        script_pubkey: bitcoin::ScriptBuf::new_op_return([1]),
    });
    assert_ne!(again.compute_txid(), reveal.compute_txid());
    rejected(
        process_batch(&db, &again, 0),
        "already has the batch transfer",
    );
    assert_eq!(credited(&db, &a), vec![1_000]);
}

#[test]
fn a_batch_whose_sums_exceed_the_checkpoint_outputs_is_rejected() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let a = test_utils::generate_subnet(2);
    save_subnets(&db, &[&source, &a]);

    // The data says 5_000 sats, the output pays 1_000
    let msg = checkpoint_msg(&source, 1, vec![], vec![transfer(&a, 5_000)]);
    let mut tx = checkpoint_tx(&msg, &source);
    let output = msg.checkpoint_outputs().transfer(0);
    assert_eq!(tx.output[output].value, Amount::from_sat(5_000));
    tx.output[output].value = Amount::from_sat(1_000);
    record_checkpoint(&db, &msg, &tx);
    rejected(
        process_batch(&db, &reveal_of(&msg, &tx), 0),
        "output pays 0.00001000 BTC, the batch transfers 0.00005000 BTC",
    );
    assert!(credited(&db, &a).is_empty());
}

#[test]
fn a_batch_whose_output_pays_another_multisig_is_rejected() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let a = test_utils::generate_subnet(2);
    let other = test_utils::generate_subnet(2);
    save_subnets(&db, &[&source, &a, &other]);

    // The data names a, the output pays the multisig of another subnet
    let msg = checkpoint_msg(
        &source,
        1,
        vec![],
        vec![transfer_paying(&a, 1_000, &other.committee)],
    );
    let tx = checkpoint_tx(&msg, &source);
    record_checkpoint(&db, &msg, &tx);
    rejected(
        process_batch(&db, &reveal_of(&msg, &tx), 0),
        "does not pay its multisig",
    );
    assert!(credited(&db, &a).is_empty());
    assert!(credited(&db, &other).is_empty());

    // An unknown destination has no multisig to pay
    let unknown = test_utils::generate_subnet(2);
    let msg = checkpoint_msg(&source, 2, vec![], vec![transfer(&unknown, 1_000)]);
    let tx = checkpoint_tx(&msg, &source);
    record_checkpoint(&db, &msg, &tx);
    rejected(process_batch(&db, &reveal_of(&msg, &tx), 0), "not found");
}

#[test]
fn a_batch_output_to_the_destinations_previous_multisig_is_accepted() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let mut a = test_utils::generate_subnet(2);
    let previous = test_utils::generate_subnet(2).committee;
    a.committee_number = 2;
    save_subnets(&db, &[&source, &a]);
    {
        let mut wtxn = db.write_txn().unwrap();
        db.save_committee(&mut wtxn, a.id, 1, &previous).unwrap();
        wtxn.commit().unwrap();
    }

    let msg = checkpoint_msg(
        &source,
        1,
        vec![],
        vec![transfer_paying(&a, 1_000, &previous)],
    );
    let tx = checkpoint_tx(&msg, &source);
    record_checkpoint(&db, &msg, &tx);
    process_batch(&db, &reveal_of(&msg, &tx), 0).unwrap();
    assert_eq!(credited(&db, &a), vec![1_000]);
}

#[test]
fn a_destination_count_other_than_the_count_byte_is_rejected() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let a = test_utils::generate_subnet(2);
    let b = test_utils::generate_subnet(2);
    save_subnets(&db, &[&source, &a, &b]);

    // Two destinations in the data, one in the count byte: the parser sees one output
    let msg = checkpoint_msg(
        &source,
        1,
        vec![],
        vec![transfer(&a, 1_000), transfer(&b, 2_000)],
    );
    let mut tx = checkpoint_tx(&msg, &source);
    let count_byte = tx.output[0].script_pubkey.len() - IpcCheckpointSubnetMsg::DATA_LEN
        + IpcCheckpointSubnetMsg::MARKERS_OFFSET
        + 1;
    let mut script = tx.output[0].script_pubkey.to_bytes();
    assert_eq!(script[count_byte], 2);
    script[count_byte] = 1;
    tx.output[0].script_pubkey = bitcoin::ScriptBuf::from_bytes(script);
    record_checkpoint(&db, &msg, &tx);
    rejected(
        process_batch(&db, &reveal_of(&msg, &tx), 0),
        "batch has 2, checkpoint expected 1",
    );
    assert!(credited(&db, &a).is_empty());
    assert!(credited(&db, &b).is_empty());
}

#[test]
fn a_batch_to_a_destination_bootstrapped_by_an_earlier_input_is_not_credited() {
    set_test_fvm_network();
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let keypair = test_utils::generate_keypairs(1).remove(0);
    let (pubkey, _) = keypair.x_only_public_key();
    let destination_id = test_utils::generate_subnet_id();
    let genesis = db::SubnetGenesisInfo {
        subnet_id: destination_id,
        create_subnet_msg: IpcCreateSubnetMsg {
            min_validator_stake: Amount::from_sat(10_000),
            power_unit_exponent: 4,
            min_validators: 1,
            bottomup_check_period: 10,
            active_validators_limit: 10,
            min_cross_msg_fee: Amount::from_sat(1),
            whitelist: vec![pubkey],
            epoch_length: 10,
            min_timeout: 1,
            max_timeout: 1000,
            min_deposit: Amount::from_sat(330),
            kill_vote_period: 36,
            kill_delay_period: 5,
        },
        create_msg_block_height: 100,
        bootstrapped: false,
        genesis_block_height: None,
        genesis_validators: Vec::new(),
        genesis_balance_entries: Vec::new(),
    };
    let save_genesis = |db: &db::HeedDb| {
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_genesis_info(&mut wtxn, destination_id, &genesis)
            .unwrap();
        wtxn.commit().unwrap();
    };
    let join = IpcJoinSubnetMsg {
        subnet_id: destination_id,
        collateral: Amount::ZERO,
        ip: "127.0.0.1:8080".parse().unwrap(),
        backup_address: test_utils::create_rand_addr().into_unchecked(),
        pubkey,
    };
    let eots_sk = bitcoin::secp256k1::SecretKey::from_slice(&[9u8; 32]).unwrap();
    let commitment = crate::eots::EotsNonceCommitment {
        start: 1,
        count: 1,
        root: [2u8; 32],
    };
    let context = {
        let scratch = test_utils::create_test_db();
        save_genesis(&scratch);
        let txn = scratch.read_txn().unwrap();
        let context = collateral::join_context(&scratch, &txn, destination_id).unwrap();
        context
    };
    let join_ue = IpcJoinUeMsg::new_signed(
        &secp,
        join,
        1_000,
        &keypair.secret_key(),
        &eots_sk,
        commitment,
    );
    let (join_commit, join_reveal, _) = collateral::unsigned_join_ue_txs(
        &secp,
        &join_ue,
        Amount::from_sat(50_000),
        &context.named[0].1,
    )
    .unwrap();

    // The multisig the join creates, learnt from a scratch database
    let destination = {
        let scratch = test_utils::create_test_db();
        save_genesis(&scratch);
        let checked = collateral::check_join_ue(
            &secp,
            &join_reveal,
            0,
            &join_commit,
            REVEAL_BLOCK,
            &collateral::join_context(&scratch, &scratch.read_txn().unwrap(), destination_id)
                .unwrap(),
        )
        .unwrap();
        let mut txn = scratch.write_txn().unwrap();
        let state = collateral::process_join_ue(
            &scratch,
            &mut txn,
            &checked,
            join_reveal.compute_txid(),
            REVEAL_BLOCK,
            block_hash(),
        )
        .unwrap()
        .expect("bootstrapped");
        txn.commit().unwrap();
        state
    };

    let db = test_utils::create_test_db();
    save_genesis(&db);
    let source = test_utils::generate_subnet(3);
    save_subnets(&db, &[&source]);
    let msg = checkpoint_msg(&source, 1, vec![], vec![transfer(&destination, 1_000)]);
    let tx = checkpoint_tx(&msg, &source);
    record_checkpoint(&db, &msg, &tx);

    // One reveal: input 0 bootstraps the destination, input 1 carries the batch to it.
    // Each input is processed in order.
    let reveal = crate::easy_tester::reveal::merge_reveals([join_reveal, reveal_of(&msg, &tx)]);
    let found = IpcMessage::from_reveal_inputs(&reveal);
    assert_eq!(found.len(), 2);
    for (index, message) in found {
        match message {
            IpcMessage::JoinUe { input, msg } => {
                assert_eq!((index, input), (0, 0));
                assert_eq!(msg.join.pubkey, pubkey);
                let checked = collateral::check_join_ue(
                    &secp,
                    &reveal,
                    input,
                    &join_commit,
                    REVEAL_BLOCK,
                    &context,
                )
                .unwrap();
                let mut txn = db.write_txn().unwrap();
                collateral::process_join_ue(
                    &db,
                    &mut txn,
                    &checked,
                    reveal.compute_txid(),
                    REVEAL_BLOCK,
                    block_hash(),
                )
                .unwrap()
                .expect("bootstrapped");
                txn.commit().unwrap();
            }
            IpcMessage::BatchTransfer(mut batch) => {
                assert_eq!(index, 1);
                let mut txn = db.write_txn().unwrap();
                batch
                    .process(
                        &db,
                        &mut txn,
                        REVEAL_BLOCK,
                        block_hash(),
                        reveal.compute_txid(),
                    )
                    .unwrap();
                txn.commit().unwrap();
            }
            other => panic!("unexpected message {other:?}"),
        }
    }
    // The destination is in epoch 0 until its first checkpoint: inactive, not credited
    assert!(credited(&db, &destination).is_empty());
}

#[test]
fn a_batch_credits_a_live_destination() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let live = test_utils::generate_subnet(3);
    save_subnets(&db, &[&source, &live]);
    let token = test_utils::create_rand_eth_addr();
    {
        let mut wtxn = db.write_txn().unwrap();
        db.set_token_balance(&mut wtxn, source.id, token, source.id, U256::from(100))
            .unwrap();
        wtxn.commit().unwrap();
    }
    let erc = |destination: &db::SubnetState, amount: u64| IpcCrossSubnetErcTransfer {
        home_subnet_id: source.id,
        home_token_address: token,
        amount: U256::from(amount),
        destination_subnet_id: destination.id,
        recipient: test_utils::create_rand_eth_addr(),
    };
    let mut msg = checkpoint_msg(&source, 1, vec![], vec![transfer(&live, 1_000)]);
    msg.token_transfers = vec![erc(&live, 30)];
    let tx = checkpoint_tx(&msg, &source);
    record_checkpoint(&db, &msg, &tx);
    process_batch(&db, &reveal_of(&msg, &tx), 0).unwrap();

    let messages = |subnet: &db::SubnetState| {
        db.get_all_rootnet_msgs(subnet.id)
            .unwrap()
            .iter()
            .map(|m| match m {
                db::RootnetMessage::FundSubnet { msg, .. } => ("fund", msg.amount.to_sat()),
                db::RootnetMessage::ErcTransfer { msg, .. } => ("erc", msg.amount.to()),
                other => panic!("unexpected message {other:?}"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(messages(&live), vec![("fund", 1_000), ("erc", 30)]);
    let balance =
        |subnet: &db::SubnetState| db.get_token_balance(source.id, token, subnet.id).unwrap();
    assert_eq!(balance(&source), U256::from(70));
    assert_eq!(balance(&live), U256::from(30));
}

#[test]
fn test_ipc_create_subnet_msg_deserialize() {
    set_test_fvm_network();

    let validator1 = XOnlyPublicKey::from_str(
        "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
    )
    .unwrap();
    let validator2 = XOnlyPublicKey::from_str(
        "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df",
    )
    .unwrap();

    // The 86 bytes of the example in docs/specs/transactions/create-subnet.md
    let serialized = spec_example_create_bytes(validator1, validator2);
    assert_eq!(serialized.len(), 86);

    let params = IpcCreateSubnetMsg::from_bytes(&serialized).unwrap();
    assert_spec_example_create_msg(&params, validator1, validator2);
}

/// The create message of the example in docs/specs/transactions/create-subnet.md.
fn spec_example_create_msg(key1: XOnlyPublicKey, key2: XOnlyPublicKey) -> IpcCreateSubnetMsg {
    IpcCreateSubnetMsg {
        min_validator_stake: Amount::from_sat(1000),
        power_unit_exponent: 3,
        min_validators: 2,
        bottomup_check_period: 10,
        active_validators_limit: 20,
        min_cross_msg_fee: Amount::from_sat(50),
        whitelist: vec![key1, key2],
        epoch_length: 144,
        min_timeout: 1,
        max_timeout: 10,
        min_deposit: Amount::from_sat(330),
        kill_vote_period: 36,
        kill_delay_period: 5,
    }
}

fn spec_example_create_bytes(key1: XOnlyPublicKey, key2: XOnlyPublicKey) -> Vec<u8> {
    [
        &[
            0x49, 0x50, 0x43, 0x43, 0x52, 0x54, 0xe8, 0x07, 0x03, 0x02, 0x0a, 0x14, 0x32, 0x02,
        ][..],
        &key1.serialize(),
        &key2.serialize(),
        &[0x90, 0x01, 0x01, 0x0a, 0xca, 0x02, 0x24, 0x05],
    ]
    .concat()
}

fn assert_spec_example_create_msg(
    params: &IpcCreateSubnetMsg,
    key1: XOnlyPublicKey,
    key2: XOnlyPublicKey,
) {
    assert_eq!(params.min_validator_stake, Amount::from_sat(1000));
    assert_eq!(params.power_unit_exponent, 3);
    assert_eq!(params.min_validators, 2);
    assert_eq!(params.bottomup_check_period, 10);
    assert_eq!(params.active_validators_limit, 20);
    assert_eq!(params.min_cross_msg_fee, Amount::from_sat(50));
    assert_eq!(params.whitelist, vec![key1, key2]);
    assert_eq!(params.epoch_length, 144);
    assert_eq!(params.min_timeout, 1);
    assert_eq!(params.max_timeout, 10);
    assert_eq!(params.min_deposit, Amount::from_sat(330));
    assert_eq!(params.kill_vote_period, 36);
    assert_eq!(params.kill_delay_period, 5);
}

#[test]
fn create_msg_validity_conditions() {
    set_test_fvm_network();
    let key1 = XOnlyPublicKey::from_str(
        "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
    )
    .unwrap();
    let key2 = XOnlyPublicKey::from_str(
        "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df",
    )
    .unwrap();
    let valid = spec_example_create_msg(key1, key2);
    valid.validate().unwrap();
    assert_eq!(valid.power_unit(), Amount::from_sat(1000));
    assert_eq!(valid.power_of(&Amount::from_sat(2500)).unwrap(), 2);
    assert_eq!(valid.cutoff_height(1000, 3), 1432);

    let rejected: Vec<(&str, Box<dyn Fn(&mut IpcCreateSubnetMsg)>)> = vec![
        (
            "power_unit_exponent",
            Box::new(|m| m.power_unit_exponent = 16),
        ),
        (
            "min_validator_stake",
            Box::new(|m| m.power_unit_exponent = 4),
        ),
        ("min_validators", Box::new(|m| m.min_validators = 0)),
        ("whitelist", Box::new(|m| m.whitelist = vec![key1, key1])),
        ("whitelist", Box::new(|m| m.whitelist = vec![key1])),
        (
            "bottomup_check_period",
            Box::new(|m| m.bottomup_check_period = 0),
        ),
        (
            "active_validators_limit",
            Box::new(|m| m.active_validators_limit = 1),
        ),
        (
            "active_validators_limit",
            Box::new(|m| m.active_validators_limit = 351),
        ),
        (
            "min_cross_msg_fee",
            Box::new(|m| m.min_cross_msg_fee = Amount::ZERO),
        ),
        ("epoch_length", Box::new(|m| m.epoch_length = 0)),
        ("epoch_length", Box::new(|m| m.epoch_length = 21_846)),
        ("min_timeout", Box::new(|m| m.min_timeout = 0)),
        ("max_timeout", Box::new(|m| m.max_timeout = 0)),
        (
            "min_deposit",
            Box::new(|m| m.min_deposit = Amount::from_sat(329)),
        ),
    ];
    for (name, mutate) in rejected {
        let mut msg = valid.clone();
        mutate(&mut msg);
        match msg.validate() {
            Err(IpcValidateError::InvalidField(f, _)) => assert_eq!(f, name),
            other => panic!("{name}: expected a rejection, got {other:?}"),
        }
    }

    let mut largest = valid.clone();
    largest.epoch_length = 21_845;
    largest.active_validators_limit = 350;
    largest.validate().unwrap();
}

#[test]
fn create_msg_accepts_more_than_90_boot_validators() {
    set_test_fvm_network();
    let secp = Secp256k1::new();
    let keys: Vec<XOnlyPublicKey> = (1..=91u8)
        .map(|seed| {
            SecretKey::from_slice(&[seed; 32])
                .unwrap()
                .x_only_public_key(&secp)
                .0
        })
        .collect();
    let mut msg = spec_example_create_msg(keys[0], keys[1]);
    msg.min_validators = 91;
    msg.active_validators_limit = 91;
    msg.whitelist = keys;
    msg.validate().unwrap();
}

fn spec_example_keys() -> (XOnlyPublicKey, XOnlyPublicKey) {
    (
        XOnlyPublicKey::from_str(
            "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
        )
        .unwrap(),
        XOnlyPublicKey::from_str(
            "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df",
        )
        .unwrap(),
    )
}

fn create_parameters() -> IpcCreateSubnetMsg {
    let (key1, key2) = spec_example_keys();
    spec_example_create_msg(key1, key2)
}

fn create_literal() -> Vec<u8> {
    let (key1, key2) = spec_example_keys();
    spec_example_create_bytes(key1, key2)
}

#[test]
fn create_rejects_every_truncated_prefix() {
    let bytes = create_literal();
    for end in 0..bytes.len() {
        assert!(
            IpcCreateSubnetMsg::from_bytes(&bytes[..end]).is_err(),
            "prefix {end}"
        );
    }
}

#[test]
fn create_rejects_trailing_bytes() {
    for suffix in [vec![0], vec![1], vec![0x80, 1]] {
        let mut bytes = create_literal();
        bytes.extend(suffix);
        assert!(IpcCreateSubnetMsg::from_bytes(&bytes).is_err());
    }
}

#[test]
fn create_rejects_nonminimal_epoch_varints() {
    // Offsets of the last six fields in the 86-byte example
    for (start, end) in [(78, 80), (80, 81), (81, 82), (82, 84), (84, 85), (85, 86)] {
        let mut bytes = create_literal();
        bytes[end - 1] |= 0x80;
        bytes.insert(end, 0);
        assert!(
            IpcCreateSubnetMsg::from_bytes(&bytes).is_err(),
            "field at {start}"
        );
    }
}

#[test]
fn create_wire_preserves_wide_integer_fields() {
    let mut p = create_parameters();
    p.epoch_length = u64::MAX;
    p.min_timeout = 128;
    p.max_timeout = u32::MAX;
    p.min_deposit = Amount::from_sat(u64::MAX);
    p.kill_vote_period = u64::MAX;
    p.kill_delay_period = 128;
    let mut expected = create_literal()[..78].to_vec();
    expected.extend_from_slice(&[
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 0x80, 1, 0xff, 0xff, 0xff, 0xff,
        0x0f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 1, 0x80, 1,
    ]);
    assert_eq!(p.to_bytes().unwrap(), expected);
    let d = IpcCreateSubnetMsg::from_bytes(&expected).unwrap();
    assert_eq!(d.epoch_length, u64::MAX);
    assert_eq!(d.max_timeout, u32::MAX);
    assert_eq!(d.min_deposit.to_sat(), u64::MAX);
    assert_eq!(d.kill_vote_period, u64::MAX);
}

#[test]
fn create_rejects_a_timeout_varint_wider_than_u32() {
    for offset in [80, 81] {
        let mut bytes = create_literal();
        bytes.splice(offset..offset + 1, [0x80, 0x80, 0x80, 0x80, 0x10]);
        assert!(IpcCreateSubnetMsg::from_bytes(&bytes).is_err());
    }
}

#[test]
fn create_rejects_an_epoch_length_varint_wider_than_u64() {
    let mut bytes = create_literal();
    bytes.splice(
        78..80,
        [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 2],
    );
    assert!(IpcCreateSubnetMsg::from_bytes(&bytes).is_err());
}

#[test]
fn power_exponent_upper_edge_is_fifteen() {
    let mut p = create_parameters();
    p.min_validator_stake = Amount::from_sat(1_000_000_000_000_000);
    p.power_unit_exponent = 15;
    assert!(p.validate().is_ok());
    p.power_unit_exponent = 16;
    assert!(p.validate().is_err());
}

#[test]
fn minimum_stake_is_checked_against_the_selected_power_unit() {
    let mut p = create_parameters();
    assert!(p.validate().is_ok());
    p.min_validator_stake = Amount::from_sat(999);
    assert!(p.validate().is_err());
    p.power_unit_exponent = 0;
    p.min_validator_stake = Amount::from_sat(1);
    assert!(p.validate().is_ok());
}

#[test]
fn epoch_length_rejects_large_values_without_wrapping() {
    for k in [u64::MAX, u64::MAX / 3 + 1, 1 << 63] {
        let mut p = create_parameters();
        p.epoch_length = k;
        assert!(p.validate().is_err(), "K={k}");
    }
}

#[test]
fn timeout_range_can_be_a_single_epoch() {
    let mut p = create_parameters();
    p.min_timeout = 7;
    p.max_timeout = 7;
    assert!(p.validate().is_ok());
    p.max_timeout = 6;
    assert!(p.validate().is_err());
}

#[test]
fn minimum_deposit_has_an_inclusive_dust_floor() {
    let mut p = create_parameters();
    assert!(p.validate().is_ok());
    for sats in [0, 329] {
        p.min_deposit = Amount::from_sat(sats);
        assert!(p.validate().is_err());
    }
}

fn distinct_keys(count: u16) -> Vec<XOnlyPublicKey> {
    let secp = Secp256k1::new();
    (1..=count)
        .map(|n| {
            let mut bytes = [0; 32];
            bytes[30..].copy_from_slice(&n.to_be_bytes());
            SecretKey::from_slice(&bytes)
                .unwrap()
                .x_only_public_key(&secp)
                .0
        })
        .collect()
}

#[test]
fn collateral_power_keeps_the_remainder_below_a_power_unit() {
    let mut p = create_parameters();
    p.min_validator_stake = Amount::from_sat(25_000);
    for (sats, expected) in [(25_000, 25), (25_999, 25), (26_000, 26)] {
        assert_eq!(p.power_of(&Amount::from_sat(sats)).unwrap(), expected);
    }
}

#[test]
fn cutoff_heights_keep_the_full_bitcoin_height_width() {
    let mut p = create_parameters();
    p.epoch_length = 10;
    assert_eq!(p.cutoff_height(4_294_967_290, 0), 4_294_967_290);
    assert_eq!(p.cutoff_height(4_294_967_290, 1), 4_294_967_300);
    assert_eq!(p.cutoff_height(4_294_967_290, 3), 4_294_967_320);
}

#[test]
fn whitelist_size_has_an_inclusive_three_hundred_fifty_key_limit() {
    let mut p = create_parameters();
    p.whitelist = distinct_keys(350);
    assert!(p.validate().is_ok());
    p.whitelist = distinct_keys(351);
    assert!(p.validate().is_err());
}

#[test]
fn print_postcard_savings_create_msg() {
    let validator1 = XOnlyPublicKey::from_str(
        "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
    )
    .unwrap();
    let validator2 = XOnlyPublicKey::from_str(
        "6a6538f93a1ae66a2b68aad837dbf3ce97010ecafbed440b79ab798cf28984df",
    )
    .unwrap();

    let old_serialized = format!(
    "{}{}min_validator_stake=1000{}min_validators=2{}bottomup_check_period=10{}active_validators_limit=20{}min_cross_msg_fee=50{}whitelist={},{}",
    IPC_CREATE_SUBNET_TAG,
    IPC_TAG_DELIMITER,
    IPC_TAG_DELIMITER,
    IPC_TAG_DELIMITER,
    IPC_TAG_DELIMITER,
    IPC_TAG_DELIMITER,
    IPC_TAG_DELIMITER,
    validator1,
    validator2,
);

    // Postcard binary format from the above serialization test
    // Works like a snapshot for deserialization
    let serialized = vec![
        73, 80, 67, 67, 82, 84, 232, 7, 2, 10, 20, 50, 2, 24, 132, 87, 129, 246, 49, 196, 143, 28,
        151, 9, 226, 48, 146, 6, 125, 6, 131, 127, 48, 170, 12, 208, 84, 74, 200, 135, 254, 145,
        221, 209, 102, 106, 101, 56, 249, 58, 26, 230, 106, 43, 104, 170, 216, 55, 219, 243, 206,
        151, 1, 14, 202, 251, 237, 68, 11, 121, 171, 121, 140, 242, 137, 132, 223,
    ];

    // Print savings from old serialization
    println!(
    "Old string-based serialization size: {}\nNew postcard serialization size: {}\nSavings: {} bytes ({}% savings)",
    old_serialized.len(),
    serialized.len(),
    old_serialized.len() as isize - serialized.len() as isize,
    100 - (serialized.len() * 100 / old_serialized.len()),
);
}

//
// Join subnet
//

#[test]
fn test_ipc_join_subnet_msg_validate() {
    set_test_fvm_network();

    let pubkey = XOnlyPublicKey::from_str(
        "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
    )
    .unwrap();

    let valid_msg = IpcJoinSubnetMsg {
        subnet_id: SubnetId::from_txid(
            &Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap(),
        ),
        collateral: Amount::from_sat(1000),
        ip: "127.0.0.1:8080".parse().unwrap(),
        backup_address: bitcoin::Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
            .unwrap(),
        pubkey,
    };
    assert!(valid_msg.validate().is_ok());

    // Test invalid subnet_id
    let mut invalid_msg = valid_msg.clone();
    invalid_msg.subnet_id = SubnetId::default();
    assert!(matches!(
        invalid_msg.validate(),
        Err(IpcValidateError::InvalidField("subnet_id", _))
    ));

    // Test wrong network address
    let mut invalid_msg = valid_msg.clone();
    invalid_msg.backup_address =
    	// TODO make this work with any network type
        bitcoin::Address::from_str("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx").unwrap();
    assert!(matches!(
        invalid_msg.validate(),
        Err(IpcValidateError::InvalidField("backup_address", _))
    ));
}

#[test]
fn test_ipc_join_subnet_msg_serialize_deserialize() {
    set_test_fvm_network();

    let pubkey = XOnlyPublicKey::from_str(
        "18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166",
    )
    .unwrap();

    let create_txid =
        &Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
            .unwrap();

    let msg = IpcJoinSubnetMsg {
        subnet_id: SubnetId::from_txid(create_txid),
        collateral: Amount::from_sat(1000),
        ip: "127.0.0.1:8080".parse().unwrap(),
        backup_address: bitcoin::Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
            .unwrap(),
        pubkey,
    };

    let serialized = msg.to_bytes().expect("Serialization failed");

    println!("{:?}", serialized);

    // Test deserialization
    let deserialized = IpcJoinSubnetMsg::from_bytes(&serialized).unwrap();

    // Skipped fields should be default values
    assert_eq!(deserialized.collateral, Amount::from_sat(0));
    // Other fields should match
    assert_eq!(deserialized.subnet_id, SubnetId::from_txid(create_txid));
    assert_eq!(deserialized.ip, msg.ip);
    assert_eq!(deserialized.backup_address, msg.backup_address);
    assert_eq!(deserialized.pubkey, msg.pubkey);

    // The collateral is not a payload field: the message checks pass without it
    assert!(deserialized.validate().is_ok());
}

fn leb128(mut value: u64, bytes: &mut Vec<u8>) {
    loop {
        let next = (value & 127) as u8;
        value >>= 7;
        bytes.push(next | if value == 0 { 0 } else { 128 });
        if value == 0 {
            break;
        }
    }
}

fn reference_digest(tag: &[u8], bytes: &[u8]) -> Message {
    let tag_hash = Sha256::digest(tag);
    let mut digest = Sha256::new();
    digest.update(tag_hash);
    digest.update(tag_hash);
    digest.update(bytes);
    Message::from_digest(digest.finalize().into())
}

fn secret(n: u8) -> SecretKey {
    let mut bytes = [0; 32];
    bytes[31] = n;
    SecretKey::from_slice(&bytes).unwrap()
}

struct JoinTranscript {
    join: IpcJoinSubnetMsg,
    eots: SecretKey,
    validator: SecretKey,
    timeout: u32,
    commitment: EotsNonceCommitment,
}

impl JoinTranscript {
    fn new() -> Self {
        // The signed transcript holds SubnetId's string form, which depends on this process-global.
        crate::eth_utils::set_fvm_network();
        let secp = Secp256k1::new();
        let validator = secret(3);
        let eots = secret(6);
        let pubkey = validator.x_only_public_key(&secp).0;
        Self {
            join: IpcJoinSubnetMsg {
                subnet_id: SubnetId::from_txid20(&std::array::from_fn(|i| i as u8)),
                collateral: Amount::from_sat(200_000),
                ip: "192.0.2.7:16384".parse().unwrap(),
                backup_address: bitcoin::Address::p2tr(&secp, pubkey, None, crate::NETWORK)
                    .into_unchecked(),
                pubkey,
            },
            validator,
            eots,
            timeout: 16384,
            commitment: EotsNonceCommitment {
                start: 129,
                count: 16384,
                root: std::array::from_fn(|i| (31 - i) as u8),
            },
        }
    }

    fn prefix(&self) -> Vec<u8> {
        let mut bytes = b"IPCJUE".to_vec();
        let subnet = self.join.subnet_id.to_string();
        leb128(subnet.len() as u64, &mut bytes);
        bytes.extend_from_slice(subnet.as_bytes());
        match self.join.ip.ip() {
            std::net::IpAddr::V4(ip) => {
                bytes.push(0);
                bytes.extend(ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                bytes.push(1);
                bytes.extend(ip.octets());
            }
        }
        leb128(self.join.ip.port() as u64, &mut bytes);
        let backup = self
            .join
            .backup_address
            .clone()
            .assume_checked()
            .to_string();
        leb128(backup.len() as u64, &mut bytes);
        bytes.extend_from_slice(backup.as_bytes());
        bytes.extend(self.join.pubkey.serialize());
        leb128(self.timeout as u64, &mut bytes);
        bytes.extend(self.eots.x_only_public_key(&Secp256k1::new()).0.serialize());
        leb128(self.commitment.start, &mut bytes);
        leb128(self.commitment.count as u64, &mut bytes);
        bytes.extend(self.commitment.root);
        bytes
    }

    fn nonce_signature(&self, tag: &[u8], prefix: &[u8], signer: &SecretKey) -> [u8; 64] {
        let secp = Secp256k1::new();
        secp.sign_schnorr_no_aux_rand(
            &reference_digest(tag, prefix),
            &Keypair::from_secret_key(&secp, signer),
        )
        .serialize()
    }

    fn finish(&self, nonce_sig: [u8; 64], include_nonce_in_digest: bool) -> Vec<u8> {
        let secp = Secp256k1::new();
        let mut bytes = self.prefix();
        let mut signed = bytes.clone();
        if include_nonce_in_digest {
            signed.extend(nonce_sig);
        }
        bytes.extend(nonce_sig);
        bytes.extend(
            secp.sign_schnorr_no_aux_rand(
                &reference_digest(b"IPCJUE/signature", &signed),
                &Keypair::from_secret_key(&secp, &self.validator),
            )
            .serialize(),
        );
        bytes
    }

    fn encoded(&self) -> Vec<u8> {
        let sig = self.nonce_signature(b"IPCJUE/nonce_commitment", &self.prefix(), &self.eots);
        self.finish(sig, true)
    }

    fn production_message(&self) -> IpcJoinUeMsg {
        IpcJoinUeMsg::new_signed(
            &Secp256k1::new(),
            self.join.clone(),
            self.timeout,
            &self.validator,
            &self.eots,
            self.commitment,
        )
    }
}

fn accepted(bytes: &[u8]) -> bool {
    IpcJoinUeMsg::from_bytes(bytes)
        .map(|m| m.validate().is_ok())
        .unwrap_or(false)
}

#[test]
fn join_signer_uses_the_independently_encoded_nonce_transcript() {
    let f = JoinTranscript::new();
    let msg = f.production_message();
    let bytes = msg.to_bytes().unwrap();
    let prefix = f.prefix();
    assert_eq!(&bytes[..prefix.len()], prefix);
    let sig =
        bitcoin::secp256k1::schnorr::Signature::from_slice(&bytes[prefix.len()..prefix.len() + 64])
            .unwrap();
    Secp256k1::new()
        .verify_schnorr(
            &sig,
            &reference_digest(b"IPCJUE/nonce_commitment", &prefix),
            &msg.pk_eots,
        )
        .unwrap();
}

#[test]
fn join_signer_binds_the_nonce_signature_into_the_validator_transcript() {
    let f = JoinTranscript::new();
    let msg = f.production_message();
    let bytes = msg.to_bytes().unwrap();
    let signed = &bytes[..bytes.len() - 64];
    Secp256k1::new()
        .verify_schnorr(
            &msg.signature,
            &reference_digest(b"IPCJUE/signature", signed),
            &f.join.pubkey,
        )
        .unwrap();
}

#[test]
fn join_ipv6_signing_transcript_uses_sixteen_network_order_octets() {
    let mut f = JoinTranscript::new();
    f.join.ip = "[2001:db8:102:304:506:708:90a:b0c]:65535".parse().unwrap();
    let bytes = f.production_message().to_bytes().unwrap();
    let prefix = f.prefix();
    assert_eq!(&bytes[..prefix.len()], prefix);
    assert!(accepted(&f.encoded()));
}

#[test]
fn independently_signed_join_commitment_is_accepted() {
    assert!(accepted(&JoinTranscript::new().encoded()));
}

#[test]
fn validator_cannot_vouch_for_a_nonce_proof_signed_by_a_different_key() {
    let f = JoinTranscript::new();
    let sig = f.nonce_signature(b"IPCJUE/nonce_commitment", &f.prefix(), &secret(7));
    assert!(!accepted(&f.finish(sig, true)));
}

#[test]
fn nonce_proof_cannot_use_the_validator_signature_domain() {
    let f = JoinTranscript::new();
    let sig = f.nonce_signature(b"IPCJUE/signature", &f.prefix(), &f.eots);
    assert!(!accepted(&f.finish(sig, true)));
}

#[test]
fn nonce_proof_cannot_omit_the_six_byte_message_tag() {
    let f = JoinTranscript::new();
    let sig = f.nonce_signature(b"IPCJUE/nonce_commitment", &f.prefix()[6..], &f.eots);
    assert!(!accepted(&f.finish(sig, true)));
}

#[test]
fn validator_signature_cannot_omit_the_nonce_signature() {
    let f = JoinTranscript::new();
    let sig = f.nonce_signature(b"IPCJUE/nonce_commitment", &f.prefix(), &f.eots);
    assert!(!accepted(&f.finish(sig, false)));
}

#[test]
fn correct_signatures_do_not_allow_one_key_for_both_roles() {
    let mut f = JoinTranscript::new();
    f.eots = f.validator;
    assert!(!accepted(&f.encoded()));
}

#[test]
fn signed_nonce_commitment_cannot_start_at_genesis_height_zero() {
    let mut f = JoinTranscript::new();
    f.commitment.start = 0;
    assert!(!accepted(&f.encoded()));
}

#[test]
fn signed_nonce_commitment_cannot_have_zero_count() {
    let mut f = JoinTranscript::new();
    f.commitment.count = 0;
    assert!(!accepted(&f.encoded()));
}

#[test]
fn join_accepts_a_non_genesis_singleton_nonce_range() {
    let mut f = JoinTranscript::new();
    f.commitment.start = 1_u64 << 32;
    f.commitment.count = 1;
    assert!(accepted(&f.encoded()));
}

#[test]
fn collateral_value_does_not_change_the_signed_join_payload() {
    let mut msg = JoinTranscript::new().production_message();
    let bytes = msg.to_bytes().unwrap();
    msg.join.collateral = Amount::from_sat(9_876_543);
    assert_eq!(msg.to_bytes().unwrap(), bytes);
}

#[test]
fn print_postcard_savings_join_msg() {
    let old_serialized = "IPCJOI#backup_address=bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n#ip=127.0.0.1:8080#pubkey=18845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166#subnet_id=/b4/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei
    test ipc_lib::tests::test_ipc_join_subnet_msg_serialize_deserialize";

    // Postcard binary format from the above serialization test
    // Works like a snapshot for deserialization
    let serialized = vec![
        73, 80, 67, 74, 79, 73, 48, 47, 98, 52, 47, 116, 52, 49, 48, 102, 104, 111, 114, 54, 51,
        55, 108, 50, 112, 109, 106, 108, 101, 54, 119, 104, 102, 113, 55, 103, 111, 53, 117, 112,
        109, 102, 55, 52, 113, 103, 54, 100, 98, 114, 52, 117, 122, 101, 105, 0, 127, 0, 0, 1, 144,
        63, 44, 98, 99, 114, 116, 49, 113, 51, 102, 122, 110, 115, 112, 114, 51, 101, 48, 50, 97,
        114, 116, 109, 57, 100, 102, 55, 116, 107, 56, 50, 55, 97, 50, 120, 104, 110, 121, 50, 109,
        52, 122, 122, 114, 54, 110, 24, 132, 87, 129, 246, 49, 196, 143, 28, 151, 9, 226, 48, 146,
        6, 125, 6, 131, 127, 48, 170, 12, 208, 84, 74, 200, 135, 254, 145, 221, 209, 102,
    ];

    // Print savings from old serialization
    println!(
    "Old string-based serialization size: {}\nNew postcard serialization size: {}\nSavings: {} bytes ({}% savings)",
    old_serialized.len(),
    serialized.len(),
    old_serialized.len() as isize - serialized.len() as isize,
    100 - (serialized.len() * 100 / old_serialized.len()),
);
}

//
// SubnetId
//

#[test]
fn test_subnet_id_creation() {
    set_test_fvm_network();

    let txid =
        Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b").unwrap();
    let subnet_id = SubnetId::from_txid(&txid);
    let addr = subnet_id.addr();

    println!("{subnet_id} {addr}");

    let mut expected_txid20 = [0u8; 20];
    expected_txid20.copy_from_slice(&txid.as_byte_array()[0..20]);

    assert_eq!(subnet_id.txid20(), expected_txid20);
}

#[test]
fn test_subnet_id_from_str() {
    set_test_fvm_network();
    let subnet_id_str = format!(
        "{}/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei",
        crate::L1_NAME
    );
    let subnet_id = SubnetId::from_str(&subnet_id_str).unwrap();

    assert_eq!(
        subnet_id.txid20().to_hex(),
        "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3"
    );
}

#[test]
fn test_subnet_id_display() {
    set_test_fvm_network();
    let txid =
        Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b").unwrap();
    let subnet_id = SubnetId::from_txid(&txid);

    assert_eq!(
        subnet_id.to_string(),
        format!(
            "{}/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei",
            crate::L1_NAME
        )
    );
}

#[test]
fn test_invalid_subnet_id() {
    set_test_fvm_network();

    // Test invalid txid
    let result = SubnetId::from_str(&format!("{}/invalid-txid", crate::L1_NAME));
    assert!(matches!(result, Err(SubnetIdError::InvalidFvmAddress(_))));

    // Test missing prefix
    let result =
        SubnetId::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b");
    assert!(matches!(result, Err(SubnetIdError::InvalidFormat(_, _))));

    // Test missing prefix
    let result =
        SubnetId::from_str("t420fhor637l2pmjle6whfq7go5upmf74qg6drcffcmr2t64kusy6lzfagfyi6m");
    assert!(matches!(result, Err(SubnetIdError::InvalidFormat(_, _))));

    // Test wrong prefix
    let result = SubnetId::from_str(
        "wrongchain/t420fhor637l2pmjle6whfq7go5upmf74qg6drcffcmr2t64kusy6lzfagfyi6m",
    );
    assert!(matches!(result, Err(SubnetIdError::InvalidFormat(_, _))));
}

#[test]
fn test_subnet_id_serde() {
    set_test_fvm_network();

    let txid =
        Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b").unwrap();
    let subnet_id = SubnetId::from_txid(&txid);

    // Test JSON serialization
    let serialized = serde_json::to_string(&subnet_id).unwrap();
    let expected = format!(
        "\"{}/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei\"",
        crate::L1_NAME
    );
    assert_eq!(serialized, expected);

    // Test JSON deserialization
    let deserialized: SubnetId = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized, subnet_id);
}

#[test]
fn test_txid_to_subnet_id_to_eth_addr_conversion() {
    // Set FVM network for address formatting
    set_fvm_network();

    // 1. Start with a valid txid
    let txid =
        Txid::from_str("06426ef74c42f874ce7824c640074d0e0bd8a676b49e91ea567a5eb596cfb8cb").unwrap();

    // 2. Create a SubnetId from the txid
    let subnet_id = SubnetId::from_txid(&txid);

    // Get the txid20 directly from Txid
    let txid20 = Txid20::from_txid(&txid);

    // Also get txid20 from subnet_id for comparison
    let subnet_txid20 = subnet_id.txid20();

    // These should match
    assert_eq!(
        txid20, subnet_txid20,
        "txid20 from Txid and from SubnetId should match"
    );

    // 3. Convert the txid20 to an Ethereum address
    let eth_addr = alloy_primitives::Address::from_slice(&txid20);

    // Verify the Ethereum address bytes match the txid20
    assert_eq!(
        eth_addr.as_slice(),
        &txid20,
        "Ethereum address bytes should match txid20"
    );

    // 4. Convert the Ethereum address to an FVM delegated address
    let eth_fvm_addr = evm_address_to_delegated_fvm(&eth_addr, L1_DELEGATED_NAMESPACE);

    // 5. Extract the Ethereum address back from the FVM address
    let recovered_eth_addr = delegated_fvm_to_eth_address(&eth_fvm_addr)
        .expect("Should be able to convert delegated address back to Ethereum address");

    // The recovered ETH address should match the original
    assert_eq!(
        recovered_eth_addr, eth_addr,
        "Recovered Ethereum address should match original"
    );

    // 6. Create a Subnet ID from the txid20
    let subnet_id_from_txid20 = SubnetId::from_txid20(&txid20);

    // 7. Create a Subnet ID directly from the Ethereum address
    let subnet_id_from_eth = {
        let txid20: Txid20 = eth_addr.into_array();
        SubnetId::from_txid20(&txid20)
    };

    // Both subnet IDs should match the original
    assert_eq!(
        subnet_id_from_txid20, subnet_id,
        "SubnetId from txid20 should match original"
    );
    assert_eq!(
        subnet_id_from_eth, subnet_id,
        "SubnetId from Ethereum address should match original"
    );

    // Verify the string representation
    println!("TXID: {}", txid);
    println!("Subnet ID: {}", subnet_id);
    println!("ETH Address: 0x{}", eth_addr.as_hex());
    println!("ETH FVM Address: {}", eth_fvm_addr);
    println!("Txid20 hex: {}", txid20.to_hex());
}

#[test]
fn batch_wire_decoder_keeps_a_multi_byte_slash_signature_count_in_its_field() {
    let mut bytes = b"IPCTFR".to_vec();
    bytes.extend([0, 0, 0, 0, 1]); // four empty collections; one slash entry
    bytes.extend([0x12; 32]); // internal-order txid
    bytes.extend([0, 0x80, 0x01]); // vout zero; 128 signatures
    for index in 0..128_u8 {
        bytes.push(index);
        bytes.extend([index; 32]);
        bytes.extend([255 - index; 32]);
    }
    let parsed = IpcBatchTransferMsg::witness_to_wire_data(&bytes).unwrap();
    assert!(parsed.transfers.is_empty());
    assert!(parsed.token_registrations.is_empty());
    assert!(parsed.token_supply_adjustments.is_empty());
    assert!(parsed.token_transfers.is_empty());
    assert_eq!(parsed.slash_signatures.len(), 1);
    let entry = &parsed.slash_signatures[0];
    assert_eq!(entry.txid, [0x12; 32]);
    assert_eq!(entry.vout, 0);
    assert_eq!(entry.signatures.len(), 128);
    for (index, (actual_index, signature)) in entry.signatures.iter().enumerate() {
        assert_eq!(*actual_index, index as u16);
        assert_eq!(*signature, [[index as u8; 32], [255 - index as u8; 32]]);
    }
}

/// The join-ue message with `committed` as its committed bytes, both signatures valid
fn signed_join_ue(committed: &[u8], sk: &SecretKey, eots_sk: &SecretKey) -> Vec<u8> {
    let secp = Secp256k1::new();
    let nonce_signature = secp
        .sign_schnorr(
            &IpcJoinUeMsg::digest_of(JOIN_UE_NONCE_COMMITMENT_TAG, committed),
            &eots_sk.keypair(&secp),
        )
        .serialize();
    let unsigned = [committed, &nonce_signature].concat();
    let signature = secp
        .sign_schnorr(
            &IpcJoinUeMsg::digest_of(JOIN_UE_SIGNATURE_TAG, &unsigned),
            &sk.keypair(&secp),
        )
        .serialize();
    [unsigned.as_slice(), &signature].concat()
}

#[test]
fn join_ue_rejects_a_message_that_is_not_the_exact_encoding_of_its_fields() {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&[7u8; 32]).unwrap();
    let eots_sk = SecretKey::from_slice(&[8u8; 32]).unwrap();
    let join = IpcJoinSubnetMsg {
        subnet_id: test_utils::generate_subnet_id(),
        collateral: Amount::ZERO,
        ip: "127.0.0.1:8080".parse().unwrap(),
        backup_address: test_utils::create_rand_addr().into_unchecked(),
        pubkey: sk.x_only_public_key(&secp).0,
    };
    let commitment = EotsNonceCommitment {
        start: 1,
        count: 1,
        root: [2u8; 32],
    };
    let timeout = 400u32;
    let pk_eots = eots_sk.x_only_public_key(&secp).0;
    let exact = IpcJoinUeMsg::committed_bytes(&join, timeout, &pk_eots, &commitment);
    let parsed = IpcJoinUeMsg::from_bytes(&signed_join_ue(&exact, &sk, &eots_sk)).unwrap();
    assert_eq!(parsed.timeout, timeout);

    // The timeout as a three-byte varint: the same value, not the same bytes
    let timeout_at = exact.len() - 32 - 34 - 2;
    assert_eq!(&exact[timeout_at..timeout_at + 2], &[0x90, 0x03]);
    let mut padded = exact[..timeout_at].to_vec();
    padded.extend_from_slice(&[0x90, 0x83, 0x00]);
    padded.extend_from_slice(&exact[timeout_at + 2..]);
    let err = IpcJoinUeMsg::from_bytes(&signed_join_ue(&padded, &sk, &eots_sk)).unwrap_err();
    assert!(
        err.to_string().contains("exact encoding"),
        "unexpected error: {err}"
    );
}

#[test]
fn batch_transfer_rejects_data_that_is_not_the_exact_encoding_of_its_fields() {
    let entry = SlashSignatureEntry {
        txid: [1u8; 32],
        vout: 1,
        signatures: vec![],
    };
    let wire = BatchTransferWireData {
        transfers: BTreeMap::new(),
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: BTreeMap::new(),
        slash_signatures: vec![entry],
    };
    let mut exact = IPC_TRANSFER_TAG.as_bytes().to_vec();
    exact.extend(postcard::to_stdvec(&wire).unwrap());
    assert_eq!(
        IpcBatchTransferMsg::witness_to_wire_data(&exact)
            .unwrap()
            .slash_signatures
            .len(),
        1
    );
    let rejected = |data: &[u8]| {
        let err = IpcBatchTransferMsg::witness_to_wire_data(data)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("exact encoding") || err.contains("bytes left"),
            "unexpected error: {err}"
        );
    };
    // A byte left over
    let mut longer = exact.clone();
    longer.push(0);
    rejected(&longer);
    // The entry's vout as a two-byte varint
    let vout_at = exact.len() - 2;
    assert_eq!(&exact[vout_at..], &[0x01, 0x00]);
    let mut padded = exact[..vout_at].to_vec();
    padded.extend_from_slice(&[0x81, 0x00, 0x00]);
    rejected(&padded);
}
/// A reveal with one input whose witness is `elements`, and one output
fn reveal_with_witness(elements: &[Vec<u8>]) -> Transaction {
    Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint::new(Txid::all_zeros(), 0),
            witness: bitcoin::Witness::from_slice(elements),
            ..Default::default()
        }],
        output: vec![bitcoin::TxOut {
            value: Amount::from_sat(500),
            script_pubkey: bitcoin::ScriptBuf::new_op_return([2]),
        }],
    }
}

fn data_reveal(data: &[u8]) -> Transaction {
    reveal_with_witness(&[
        bitcoin_utils::make_push_data_script(data).to_bytes(),
        vec![0xc0u8; 33],
    ])
}

/// `data` with the tag `tag_index` in front: one of the message tags, or none
fn tagged(tag_index: usize, data: &[u8]) -> Vec<u8> {
    let tags = [
        IpcTag::CreateSubnet,
        IpcTag::JoinSubnet,
        IpcTag::JoinUe,
        IpcTag::BatchTransfer,
        IpcTag::CheckpointSubnet,
        IpcTag::PrefundSubnet,
        IpcTag::FundSubnet,
        IpcTag::StakeCollateral,
        IpcTag::UnstakeCollateral,
        IpcTag::KillSubnet,
    ];
    match tags.get(tag_index) {
        Some(tag) => [tag.as_str().as_bytes(), data].concat(),
        None => data.to_vec(),
    }
}

fn op_return_tx(payload: &[u8], extra_outputs: Vec<bitcoin::TxOut>) -> Transaction {
    let push: &bitcoin::script::PushBytes = payload.try_into().unwrap();
    let mut output = vec![bitcoin::TxOut {
        value: Amount::ZERO,
        script_pubkey: bitcoin_utils::make_op_return_script(push),
    }];
    output.extend(extra_outputs);
    Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint::new(Txid::all_zeros(), 1),
            ..Default::default()
        }],
        output,
    }
}

fn arb_create_msg() -> impl Strategy<Value = IpcCreateSubnetMsg> {
    (
        test_utils::arb_amount(),
        any::<u16>(),
        any::<u64>(),
        any::<u16>(),
        test_utils::arb_amount(),
        prop::collection::vec(test_utils::arb_xonly(), 0..4),
    )
        .prop_map(
            |(min_validator_stake, min_validators, period, limit, fee, whitelist)| {
                IpcCreateSubnetMsg {
                    min_validator_stake,
                    power_unit_exponent: 0,
                    min_validators,
                    bottomup_check_period: period,
                    active_validators_limit: limit,
                    min_cross_msg_fee: fee,
                    whitelist,
                    epoch_length: 10,
                    min_timeout: 1,
                    max_timeout: 1000,
                    min_deposit: Amount::from_sat(330),
                    kill_vote_period: 36,
                    kill_delay_period: 5,
                }
            },
        )
}

/// Slash pre-signature entries in the order the encoder requires: strictly ascending
/// outpoints, and strictly ascending signer indexes within an entry
fn arb_slash_signatures() -> impl Strategy<Value = Vec<SlashSignatureEntry>> {
    prop::collection::btree_map(
        (any::<[u8; 32]>(), any::<u32>()),
        prop::collection::btree_map(any::<u16>(), any::<[[u8; 32]; 2]>(), 1..4),
        0..4,
    )
    .prop_map(|entries| {
        entries
            .into_iter()
            .map(|((txid, vout), signatures)| {
                SlashSignatureEntry::new(
                    bitcoin::OutPoint::new(Txid::from_byte_array(txid), vout),
                    signatures.into_iter().collect(),
                )
            })
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn from_reveal_input_never_panics_and_is_idempotent(
        data in prop::collection::vec(any::<u8>(), 0..300),
        tag_index in 0..11usize,
    ) {
        set_fvm_network();
        let data = tagged(tag_index, &data);
        let tx = data_reveal(&data);
        match IpcMessage::from_reveal_input(&tx, 0, &data) {
            Ok(IpcMessage::JoinUe { .. }) => {
                prop_assert!(false, "random bytes carry no valid signature")
            }
            Ok(IpcMessage::CreateSubnet(msg)) => {
                let again = IpcCreateSubnetMsg::from_bytes(&msg.to_bytes().unwrap()).unwrap();
                prop_assert_eq!(format!("{again:?}"), format!("{msg:?}"));
            }
            Ok(IpcMessage::JoinSubnet(msg)) => {
                let again = IpcJoinSubnetMsg::from_bytes(&msg.to_bytes().unwrap()).unwrap();
                prop_assert_eq!(format!("{again:?}"), format!("{msg:?}"));
            }
            Ok(_) | Err(_) => {}
        }
        let _ = IpcCreateSubnetMsg::from_bytes(&data);
        let _ = IpcJoinSubnetMsg::from_bytes(&data);
        let _ = IpcJoinUeMsg::from_bytes(&data);
        let _ = IpcBatchTransferMsg::witness_to_wire_data(&data);
    }

    #[test]
    fn create_round_trips_and_needs_one_input_and_one_output(msg in arb_create_msg()) {
        set_fvm_network();
        let data = msg.to_bytes().unwrap();
        let parsed = IpcCreateSubnetMsg::from_bytes(&data).unwrap();
        prop_assert_eq!(format!("{parsed:?}"), format!("{msg:?}"));
        let tx = data_reveal(&data);
        prop_assert!(matches!(
            IpcMessage::from_reveal_input(&tx, 0, &data),
            Ok(IpcMessage::CreateSubnet(_))
        ));
        let mut two_inputs = tx.clone();
        two_inputs.input.push(tx.input[0].clone());
        prop_assert!(IpcMessage::from_reveal_input(&two_inputs, 0, &data).is_err());
        let mut two_outputs = tx.clone();
        two_outputs.output.push(tx.output[0].clone());
        prop_assert!(IpcMessage::from_reveal_input(&two_outputs, 0, &data).is_err());
    }

    #[test]
    fn op_return_parsers_never_panic(
        payload in prop::collection::vec(any::<u8>(), 0..100),
        tag_index in 0..11usize,
        extra in prop::collection::vec((1u64..100_000, 0..3usize), 0..3),
    ) {
        set_fvm_network();
        let db = test_utils::create_test_db();
        let payload = tagged(tag_index, &payload);
        let outputs = extra
            .into_iter()
            .map(|(sats, a)| bitcoin::TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: test_utils::address_at(a).assume_checked().script_pubkey(),
            })
            .collect();
        let tx = op_return_tx(&payload, outputs);
        let _ = IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &db.read_txn().unwrap(), &tx);
        let _ = IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&tx.output[0]);
        let _ = IpcMessage::from_reveal_inputs(&tx);
    }

    #[test]
    fn checkpoint_metadata_round_trips(
        hash in any::<[u8; 32]>(),
        height in any::<u64>(),
        configuration in any::<u64>(),
        rotation in any::<bool>(),
        slash_signatures in arb_slash_signatures(),
    ) {
        set_fvm_network();
        let db = test_utils::create_test_db();
        let subnet = test_utils::generate_subnet(2);
        {
            let mut wtxn = db.write_txn().unwrap();
            db.save_subnet_state(&mut wtxn, subnet.id, &subnet).unwrap();
            wtxn.commit().unwrap();
        }
        let msg = IpcCheckpointSubnetMsg {
            subnet_id: subnet.id,
            checkpoint_hash: bitcoin::hashes::sha256::Hash::from_byte_array(hash),
            checkpoint_height: height,
            next_committee_configuration_number: configuration,
            withdrawals: vec![],
            transfers: vec![],
            token_registrations: vec![],
            token_supply_adjustments: vec![],
            token_transfers: vec![],
            slash_signatures: slash_signatures.clone(),
            unstakes: vec![],
            change_address: Some(subnet.committee.multisig_address.clone()),
            is_kill_checkpoint: false,
            is_rotation_checkpoint: rotation,
        };
        let fee_rate = bitcoin::FeeRate::from_sat_per_vb(10).unwrap();
        let metadata = msg.make_metadata_tx_out(fee_rate);
        prop_assert_eq!(
            IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&metadata).unwrap(),
            (0, 0, 0, false)
        );

        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![],
            output: vec![metadata],
        };
        let parsed = IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &db.read_txn().unwrap(), &tx).unwrap();
        prop_assert_eq!(parsed.subnet_id, subnet.id);
        prop_assert_eq!(parsed.checkpoint_hash, msg.checkpoint_hash);
        prop_assert_eq!(parsed.checkpoint_height, height);
        prop_assert_eq!(parsed.next_committee_configuration_number, configuration);
        prop_assert!(!parsed.is_kill_checkpoint);
        prop_assert_eq!(parsed.is_rotation_checkpoint, rotation);

        prop_assert_eq!(msg.has_batch_data(), !slash_signatures.is_empty());
        if !slash_signatures.is_empty() {
            let data = msg.make_batched_transfer_data().unwrap();
            let wire = IpcBatchTransferMsg::witness_to_wire_data(&data).unwrap();
            prop_assert_eq!(&wire.slash_signatures, &slash_signatures);
            prop_assert_eq!(postcard::to_stdvec(&wire).unwrap(), data[IPC_TAG_LENGTH..].to_vec());
        }
    }
}

fn create_test_msg() -> IpcPrefundSubnetMsg {
    let txid =
        Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b").unwrap();
    let subnet_id = SubnetId::from_txid(&txid);
    let eth_addr =
        alloy_primitives::Address::from_str("742d35Cc6634C0532925a3b844Bc454e4438f44e").unwrap();

    IpcPrefundSubnetMsg {
        subnet_id,
        amount: Amount::from_sat(1000),
        address: eth_addr,
    }
}

fn get_addresses() -> (bitcoin::Address, bitcoin::Address) {
    let multisig_address = bitcoin::Address::from_str(
        "bc1pzc5j0fyekrc9p63avup65y8h8rhp7m5ql5tg7590wuhhqdtlkfusng6er8",
    )
    .unwrap()
    .assume_checked();
    let release_address =
        bitcoin::Address::from_str("bcrt1qvr3jycfxtrkk8u6hp5caxc25tueek5f90mpnsv")
            .unwrap()
            .assume_checked();

    (multisig_address, release_address)
}

#[test]
fn test_to_tx_structure() {
    let msg = create_test_msg();
    let (multisig_address, release_address) = get_addresses();

    // Generate transaction
    let tx = msg
        .to_tx(DEFAULT_BTC_FEE_RATE, &multisig_address, &release_address)
        .unwrap();

    // Check basic structure
    assert_eq!(
        tx.output.len(),
        2,
        "Transaction should have exactly 2 outputs"
    );

    // First output should be OP_RETURN
    assert!(
        tx.output[0].script_pubkey.is_op_return(),
        "First output should be OP_RETURN"
    );
    assert_eq!(
        tx.output[0].value,
        Amount::ZERO,
        "OP_RETURN output should have zero value"
    );

    // Second output should have the correct value and script
    assert!(
        tx.output[1].script_pubkey.is_p2tr(),
        "Second output should be p2tr"
    );
    assert_eq!(
        tx.output[1].value,
        Amount::from_sat(1000),
        "Second output should have correct value"
    );
}

#[test]
fn test_from_tx_valid() {
    let original_msg = create_test_msg();
    let (multisig_address, release_address) = get_addresses();

    // Create transaction using to_tx
    let tx = original_msg
        .to_tx(DEFAULT_BTC_FEE_RATE, &multisig_address, &release_address)
        .unwrap();

    // Parse it back using from_tx
    let parsed_msg = IpcPrefundSubnetMsg::from_tx(&tx).unwrap();

    // Verify all fields match
    assert_eq!(parsed_msg.subnet_id, original_msg.subnet_id);
    assert_eq!(parsed_msg.amount, original_msg.amount);
    assert_eq!(parsed_msg.address, original_msg.address);
}

#[test]
fn test_from_tx_invalid_cases() {
    let (multisig_address, release_address) = get_addresses();

    // Test case 1: Empty transaction
    let empty_tx = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![],
        output: vec![],
    };
    assert!(matches!(
        IpcPrefundSubnetMsg::from_tx(&empty_tx),
        Err(IpcLibError::MsgParseError(IPC_PREFUND_SUBNET_TAG, _))
    ));

    // Test case 2: Transaction with only one output
    let tx = create_test_msg()
        .to_tx(DEFAULT_BTC_FEE_RATE, &multisig_address, &release_address)
        .unwrap();
    let single_output_tx = Transaction {
        version: tx.version,
        lock_time: tx.lock_time,
        input: tx.input.clone(),
        output: vec![tx.output[0].clone()], // Only the OP_RETURN output
    };
    assert!(matches!(
        IpcPrefundSubnetMsg::from_tx(&single_output_tx),
        Err(IpcLibError::MsgParseError(IPC_PREFUND_SUBNET_TAG, _))
    ));

    // Test case 3: Wrong tag in OP_RETURN
    let mut wrong_tag_tx = tx.clone();
    let mut wrong_data = Vec::new();
    // same length as "IPCPFD"
    let invalid_tag = "IPC123";
    // sanity check if we change tag length
    assert_eq!(invalid_tag.len(), IPC_TAG_LENGTH);
    wrong_data.extend_from_slice(invalid_tag.as_bytes()); // Different tag
    wrong_data.extend_from_slice(&[0u8; SubnetId::INNER_LEN]); // txid
    wrong_data.extend_from_slice(&[0u8; 20]); // address
    let wrong_data: [u8; IpcPrefundSubnetMsg::DATA_LEN] = wrong_data.try_into().unwrap();
    wrong_tag_tx.output[0].script_pubkey = bitcoin_utils::make_op_return_script(wrong_data);
    assert!(matches!(
        IpcPrefundSubnetMsg::from_tx(&wrong_tag_tx),
        Err(IpcLibError::MsgParseError(IPC_PREFUND_SUBNET_TAG, _))
    ));
}

#[test]
#[test_retry::retry(3)]
fn test_checkpoint_with_unstakes() {
    // Set up test database
    let db = create_test_db();

    // Create a test subnet
    let subnet = test_utils::generate_subnet(3);
    let committee = &subnet.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());

    // dbg!(&subnet);

    // Save subnets to database
    {
        let mut wtxn = db.write_txn().expect("Should create transaction");
        db.save_subnet_state(&mut wtxn, subnet.id, &subnet)
            .expect("Should save subnet");
        wtxn.commit().unwrap();
    }

    // Create checkpoint message with unstakes
    let mut checkpoint_msg = create_test_checkpoint_msg();
    // Update subnet i
    checkpoint_msg.subnet_id = subnet.id;
    // Remove transfers for this test
    checkpoint_msg.transfers.clear();
    // Update unstake address
    checkpoint_msg.unstakes.first_mut().unwrap().address = subnet
        .committee
        .validators
        .first()
        .unwrap()
        .backup_address
        .clone();

    // Verify the test checkpoint message has unstakes
    assert!(!checkpoint_msg.unstakes.is_empty());
    let original_unstakes = checkpoint_msg.unstakes.clone();

    // Generate the transaction
    let checkpoint_psbt = checkpoint_msg
        .to_checkpoint_psbt(committee, committee, DEFAULT_BTC_FEE_RATE, &utxos)
        .expect("Should create checkpoint PSBT");

    let checkpoint_tx = checkpoint_psbt.extract_tx().expect("must extract tx");

    dbg!(&checkpoint_tx);

    // Parse the transaction back to a checkpoint message
    let parsed_msg =
        IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &db.read_txn().unwrap(), &checkpoint_tx)
            .expect("Should parse checkpoint message");
    dbg!(&parsed_msg);

    // Verify unstakes were correctly serialized and deserialized
    assert_eq!(parsed_msg.unstakes.len(), original_unstakes.len());
    for (i, unstake) in parsed_msg.unstakes.iter().enumerate() {
        assert_eq!(unstake.amount, original_unstakes[i].amount);
        assert_eq!(unstake.address, original_unstakes[i].address);
        // Note: pubkey is not preserved in the checkpoint transaction
    }

    // Verify metadata and markers
    let (withdrawal_count, transfer_count, unstake_count, killed) =
        IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&checkpoint_tx.output[0])
            .expect("Should extract markers");

    assert_eq!(withdrawal_count as usize, checkpoint_msg.withdrawals.len());
    assert_eq!(transfer_count as usize, checkpoint_msg.transfers.len());
    assert_eq!(unstake_count as usize, original_unstakes.len());
    assert_eq!(killed, checkpoint_msg.is_kill_checkpoint);
}

fn create_test_checkpoint_msg() -> IpcCheckpointSubnetMsg {
    // Generate a subnet with 3 validators
    let subnet_state = test_utils::generate_subnet(3);

    // Create a second subnet for cross-subnet transfers
    let destination_subnet = test_utils::generate_subnet(3);

    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    )
    .unwrap();

    // Create an unstake
    let unstake = IpcUnstake {
        amount: Amount::from_sat(40000),
        address: bitcoin::Address::from_str("bcrt1qgtzpfqlfkz4nhkpvz2enqtucm2pzhdvlssxnss")
            .unwrap(),
        pubkey: bitcoin::XOnlyPublicKey::from_str(
            "b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0",
        )
        .unwrap(),
    };

    // Create a withdrawal
    let withdrawal = IpcWithdrawal {
        amount: Amount::from_sat(50000),
        address: bitcoin::Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
            .unwrap(),
    };

    // Create a cross-subnet transfer
    let transfer = IpcCrossSubnetTransfer {
        amount: Amount::from_sat(30000),
        destination_subnet_id: destination_subnet.id,
        subnet_multisig_address: Some(destination_subnet.committee.multisig_address.clone()),
        subnet_user_address: alloy_primitives::Address::from_str(
            "742d35Cc6634C0532925a3b844Bc454e4438f44e",
        )
        .unwrap(),
    };

    IpcCheckpointSubnetMsg {
        subnet_id: subnet_state.id,
        checkpoint_hash,
        checkpoint_height: 50,
        slash_signatures: vec![],
        unstakes: vec![unstake],
        withdrawals: vec![withdrawal],
        transfers: vec![transfer],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        change_address: Some(subnet_state.committee.multisig_address.clone()),
        next_committee_configuration_number: 30, // arbitrary test number
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    }
}

fn create_test_checkpoint_msg_multiple_transfers(n_transfers: usize) -> IpcCheckpointSubnetMsg {
    let subnet_state = test_utils::generate_subnet(4);
    let destination_subnet = test_utils::generate_subnet(4);

    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    )
    .unwrap();

    let mut transfers = Vec::with_capacity(n_transfers);

    for i in 0..n_transfers {
        let transfer = IpcCrossSubnetTransfer {
            amount: Amount::from_sat(10000 + (i as u64 * 1000)), // Varying amounts
            destination_subnet_id: destination_subnet.id,
            subnet_multisig_address: Some(destination_subnet.committee.multisig_address.clone()),
            subnet_user_address: alloy_primitives::Address::from_str(
                // Varying addresses
                &format!("742d35Cc6634C0532925a3b844Bc454e4438f4{:02x}", i),
            )
            .unwrap(),
        };
        transfers.push(transfer);
    }

    IpcCheckpointSubnetMsg {
        subnet_id: subnet_state.id,
        checkpoint_hash,
        checkpoint_height: 50,
        slash_signatures: vec![],
        unstakes: vec![],
        withdrawals: vec![],
        transfers,
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        change_address: Some(subnet_state.committee.multisig_address.clone()),
        next_committee_configuration_number: 30, // arbitrary test number
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    }
}

fn create_test_utxos(
    script_pub_key: bitcoin::ScriptBuf,
) -> Vec<bitcoincore_rpc::json::ListUnspentResultEntry> {
    vec![bitcoincore_rpc::json::ListUnspentResultEntry {
        txid: bitcoin::Txid::from_str(
            "f61b1742ca13176464adb3cb66050c00787bb3a4eead37e985f2df1e37718126",
        )
        .unwrap(),
        vout: 0,
        address: None,
        label: None,
        redeem_script: None,
        witness_script: None,
        script_pub_key,
        amount: bitcoin::Amount::from_sat(200000),
        confirmations: 10,
        spendable: true,
        solvable: true,
        descriptor: None,
        safe: true,
    }]
}

/// Building the same checkpoint twice produces byte-identical transactions, with
/// one transfer output per destination in ascending `txid20` order
#[test]
fn checkpoint_and_batch_reveal_are_deterministic() {
    let subnet = test_utils::generate_subnet(3);
    let committee = &subnet.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());
    let destinations = [
        test_utils::generate_subnet(2),
        test_utils::generate_subnet(2),
        test_utils::generate_subnet(2),
    ];
    let transfer = |dest: &db::SubnetState, sats: u64| IpcCrossSubnetTransfer {
        amount: Amount::from_sat(sats),
        destination_subnet_id: dest.id,
        subnet_multisig_address: Some(dest.committee.multisig_address.clone()),
        subnet_user_address: test_utils::create_rand_eth_addr(),
    };
    let msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet.id,
        checkpoint_hash: bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap(),
        checkpoint_height: 7,
        next_committee_configuration_number: 0,
        withdrawals: vec![IpcWithdrawal {
            amount: Amount::from_sat(20_000),
            address: bitcoin::Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
                .unwrap(),
        }],
        transfers: vec![
            transfer(&destinations[0], 10_000),
            transfer(&destinations[1], 11_000),
            transfer(&destinations[2], 12_000),
            transfer(&destinations[0], 13_000),
        ],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: Some(committee.multisig_address.clone()),
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };
    let fee_rate = DEFAULT_BTC_FEE_RATE;
    let build = |msg: &IpcCheckpointSubnetMsg| {
        let psbt = msg
            .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
            .unwrap();
        let txid = psbt.unsigned_tx.compute_txid();
        let reveal = msg
            .make_reveal_batch_transfer_tx(txid, fee_rate, &committee.address_checked())
            .unwrap()
            .unwrap();
        (
            psbt.serialize(),
            bitcoin::consensus::serialize(&reveal),
            psbt.unsigned_tx,
        )
    };
    let (psbt_a, reveal_a, tx) = build(&msg);
    let (psbt_b, reveal_b, _) = build(&msg.clone());
    assert_eq!(psbt_a, psbt_b);
    assert_eq!(reveal_a, reveal_b);

    // Metadata, the withdrawal, the commitment, then one output per destination ascending
    let mut expected: Vec<_> = destinations
        .iter()
        .map(|d| (d.id.txid20(), d.committee.address_checked().script_pubkey()))
        .collect();
    expected.sort();
    let scripts: Vec<_> = tx.output[3..6]
        .iter()
        .map(|o| o.script_pubkey.clone())
        .collect();
    assert_eq!(
        scripts,
        expected.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>()
    );
    assert_eq!(
        tx.output[3 + expected
            .iter()
            .position(|(t, _)| *t == destinations[0].id.txid20())
            .unwrap()]
        .value,
        Amount::from_sat(23_000)
    );
}

#[test]
fn test_checkpoint_to_txs_basic() {
    let checkpoint_msg = create_test_checkpoint_msg();
    let subnet = test_utils::generate_subnet(3);
    let committee = &subnet.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());

    let fee_rate = DEFAULT_BTC_FEE_RATE;

    // Generate transactions
    let checkpoint_psbt = checkpoint_msg
        .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
        .unwrap();
    let checkpoint_tx = checkpoint_psbt.unsigned_tx.clone();

    let batch_tx = checkpoint_msg
        .make_reveal_batch_transfer_tx(
            checkpoint_tx.compute_txid(),
            fee_rate,
            &committee.address_checked(),
        )
        .unwrap();

    // First output should be OP_RETURN with metadata
    assert!(
        checkpoint_tx.output[0].script_pubkey.is_op_return(),
        "First output should be OP_RETURN"
    );

    // Extract OP_RETURN data from metadata output
    let op_return_data = checkpoint_tx.output[0]
        .script_pubkey
        .instructions_minimal()
        .find_map(|ins| match ins {
            Ok(bitcoin::blockdata::script::Instruction::PushBytes(data)) => Some(data.as_bytes()),
            _ => None,
        })
        .unwrap();

    // Check the withdrawal count marker
    assert_eq!(
        op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET],
        1,
        "Withdrawal count marker should be 1"
    );

    // Check the transfer count marker
    assert_eq!(
        op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 1],
        1,
        "Transfer count marker should be 1"
    );

    // Check the unstake count marker
    assert_eq!(
        op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 2],
        1,
        "Unstake count marker should be 1"
    );

    assert_eq!(checkpoint_tx.output.len(), 6, "Should have 6 outputs");

    // Since we have transfers, we should have a batch transaction
    assert!(
        batch_tx.is_some(),
        "Should have a batch transfer transaction"
    );

    if let Some(reveal_tx) = batch_tx {
        // The reveal transaction should have one input
        assert_eq!(reveal_tx.input.len(), 1, "Reveal tx should have 1 input");

        // Input should point to checkpoint transaction
        assert_eq!(
            reveal_tx.input[0].previous_output.txid,
            checkpoint_tx.compute_txid(),
            "Reveal tx input should point to checkpoint tx"
        );

        // Should have witness data
        assert!(
            !reveal_tx.input[0].witness.is_empty(),
            "Reveal tx input should have witness data"
        );

        // Should have one output
        assert_eq!(reveal_tx.output.len(), 1, "Reveal tx should have 1 output");
    }
}

#[test]
fn test_checkpoint_without_transfers() {
    // Create a subnet with 3 validators
    let subnet = test_utils::generate_subnet(3);
    let committee = &subnet.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());

    // Create a simple checkpoint with only withdrawals
    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    )
    .unwrap();

    let withdrawal = IpcWithdrawal {
        amount: Amount::from_sat(50000),
        address: bitcoin::Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
            .unwrap(),
    };
    let withdrawal2 = IpcWithdrawal {
        amount: Amount::from_sat(50000),
        address: bitcoin::Address::from_str("bcrt1qvr3jycfxtrkk8u6hp5caxc25tueek5f90mpnsv")
            .unwrap(),
    };

    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet.id,
        checkpoint_hash,
        checkpoint_height: 50,
        withdrawals: vec![withdrawal, withdrawal2],
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: Some(committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    let fee_rate = DEFAULT_BTC_FEE_RATE;

    // Generate transactions
    let checkpoint_psbt = checkpoint_msg
        .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
        .unwrap();
    let checkpoint_tx = checkpoint_psbt.unsigned_tx.clone();

    let batch_tx = checkpoint_msg
        .make_reveal_batch_transfer_tx(
            checkpoint_tx.compute_txid(),
            fee_rate,
            &committee.address_checked(),
        )
        .unwrap();

    assert!(
        batch_tx.is_none(),
        "No batch transaction should be created when there are no transfers"
    );

    // Verify structure
    assert_eq!(checkpoint_tx.output.len(), 4, "Should have 4 outputs");
    assert!(
        checkpoint_tx.output[0].script_pubkey.is_op_return(),
        "First output should be OP_RETURN"
    );

    // Extract OP_RETURN data
    let op_return_data = checkpoint_tx.output[0]
        .script_pubkey
        .instructions_minimal()
        .find_map(|ins| match ins {
            Ok(bitcoin::blockdata::script::Instruction::PushBytes(data)) => Some(data.as_bytes()),
            _ => None,
        })
        .unwrap();

    // Check the withdrawal count marker
    assert_eq!(
        op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET],
        2,
        "Withdrawal count marker should be 1"
    );

    // Check the transfer count marker
    assert_eq!(
        op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 1],
        0,
        "Transfer count marker should be 0"
    );
}

#[test]
fn test_checkpoint_empty() {
    // Create a subnet with 3 validators
    let subnet = test_utils::generate_subnet(3);
    let committee = &subnet.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());

    // Create a simple checkpoint with no withdrawals and no transfers
    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    )
    .unwrap();

    // Create empty checkpoint message
    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet.id,
        checkpoint_hash,
        checkpoint_height: 50,
        withdrawals: vec![],
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: Some(committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    let fee_rate = DEFAULT_BTC_FEE_RATE;

    // Generate transactions
    let checkpoint_psbt = checkpoint_msg
        .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
        .unwrap();
    let checkpoint_tx = checkpoint_psbt.unsigned_tx.clone();

    let batch_tx = checkpoint_msg
        .make_reveal_batch_transfer_tx(
            checkpoint_tx.compute_txid(),
            fee_rate,
            &committee.address_checked(),
        )
        .unwrap();

    assert!(
        batch_tx.is_none(),
        "No batch transaction should be created when there are no transfers"
    );

    // Verify structure - should only have OP_RETURN metadata and change output
    assert_eq!(checkpoint_tx.output.len(), 2, "Should have 2 outputs");
    assert!(
        checkpoint_tx.output[0].script_pubkey.is_op_return(),
        "First output should be OP_RETURN"
    );

    // Extract OP_RETURN data
    let op_return_data = checkpoint_tx.output[0]
        .script_pubkey
        .instructions_minimal()
        .find_map(|ins| match ins {
            Ok(bitcoin::blockdata::script::Instruction::PushBytes(data)) => Some(data.as_bytes()),
            _ => None,
        })
        .unwrap();

    // Check the withdrawal count marker
    assert_eq!(
        op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET],
        0,
        "Withdrawal count marker should be 0"
    );

    // Check the transfer count marker
    assert_eq!(
        op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 1],
        0,
        "Transfer count marker should be 0"
    );

    // Verify no batch transaction is returned when there are no transfers
    assert!(
        batch_tx.is_none(),
        "No batch transaction should be created when there are no transfers"
    );
}

/// An input of another transaction, spent by key path
#[test]
fn a_batch_reveal_whose_input_is_not_as_specified_is_rejected() {
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let reveal = create_test_checkpoint_msg()
        .make_reveal_batch_transfer_tx(
            test_utils::create_rand_txid(),
            DEFAULT_BTC_FEE_RATE,
            &test_utils::create_rand_addr(),
        )
        .unwrap()
        .unwrap();
    let data = bitcoin_utils::reveal_input(&reveal.input[0]).unwrap().data;
    let batches = |tx: &Transaction| {
        IpcMessage::from_reveal_inputs(tx)
            .into_iter()
            .filter(|(_, msg)| matches!(msg, IpcMessage::BatchTransfer(_)))
            .count()
    };
    assert_eq!(batches(&reveal), 1);

    // The witness of a spend through `executed`, one of the commitment's `leaves`
    let witness = |leaves: &[bitcoin::ScriptBuf], executed: &bitcoin::ScriptBuf| {
        let depth = u8::from(leaves.len() > 1);
        let mut builder = bitcoin::taproot::TaprootBuilder::new();
        for leaf in leaves {
            builder = builder.add_leaf(depth, leaf.clone()).unwrap();
        }
        let info = builder
            .finalize(&secp, bitcoin_utils::unspenable_internal_key())
            .unwrap();
        let control_block = info
            .control_block(&(executed.clone(), bitcoin::taproot::LeafVersion::TapScript))
            .unwrap();
        bitcoin::Witness::from_slice(&[executed.to_bytes(), control_block.serialize()])
    };

    // An extra empty push in the leaf: it decodes to the same data
    let mut extra = bitcoin_utils::make_push_data_script(&data).to_bytes();
    extra.pop();
    extra.extend([
        bitcoin::opcodes::all::OP_PUSHBYTES_0.to_u8(),
        bitcoin::opcodes::all::OP_DROP.to_u8(),
        bitcoin::opcodes::OP_TRUE.to_u8(),
    ]);
    let extra = bitcoin::ScriptBuf::from_bytes(extra);
    let mut tampered = reveal.clone();
    tampered.input[0].witness = witness(&[extra.clone()], &extra);
    assert_eq!(
        bitcoin_utils::reveal_input(&tampered.input[0])
            .unwrap()
            .data,
        data
    );
    assert_eq!(batches(&tampered), 0);

    // A commitment with a second leaf
    let leaf = bitcoin_utils::make_push_data_script(&data);
    let other = bitcoin::script::Builder::new()
        .push_opcode(bitcoin::opcodes::OP_TRUE)
        .into_script();
    let mut two_leaves = reveal.clone();
    two_leaves.input[0].witness = witness(&[leaf.clone(), other], &leaf);
    assert_eq!(two_leaves.input[0].witness.nth(1).unwrap().len(), 65);
    assert_eq!(batches(&two_leaves), 0);
}

fn foreign_input(vout: u32) -> bitcoin::TxIn {
    bitcoin::TxIn {
        previous_output: bitcoin::OutPoint::new(test_utils::create_rand_txid(), vout),
        witness: bitcoin::Witness::from_slice(&[vec![0u8; 64]]),
        ..Default::default()
    }
}

#[test]
fn batch_transfer_is_read_from_the_input_that_spends_the_commitment() {
    let db = test_utils::create_test_db();
    let destination = test_utils::generate_subnet(3);
    {
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_state(&mut wtxn, destination.id, &destination)
            .unwrap();
        wtxn.commit().unwrap();
    }
    let source = test_utils::generate_subnet(3);
    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: source.id,
        checkpoint_hash: bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap(),
        checkpoint_height: 50,
        slash_signatures: vec![],
        unstakes: vec![],
        withdrawals: vec![],
        transfers: vec![IpcCrossSubnetTransfer {
            amount: Amount::from_sat(30_000),
            destination_subnet_id: destination.id,
            subnet_multisig_address: Some(destination.committee.multisig_address.clone()),
            subnet_user_address: alloy_primitives::Address::from_str(
                "742d35Cc6634C0532925a3b844Bc454e4438f44e",
            )
            .unwrap(),
        }],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        change_address: Some(source.committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };
    let checkpoint_txid = test_utils::create_rand_txid();
    let fee_rate = bitcoin::FeeRate::from_sat_per_vb(10).unwrap();
    let reveal = checkpoint_msg
        .make_reveal_batch_transfer_tx(checkpoint_txid, fee_rate, &test_utils::create_rand_addr())
        .unwrap()
        .unwrap();
    let commitment = reveal.input[0].previous_output;
    assert_eq!(commitment.txid, checkpoint_txid);

    // (input index, checkpoint txid, commitment vout, transfers) of every batch found
    let batches = |tx: &Transaction| {
        IpcMessage::from_reveal_inputs(tx)
            .into_iter()
            .filter_map(|(index, msg)| match msg {
                IpcMessage::BatchTransfer(b) => Some((
                    index,
                    b.checkpoint_txid,
                    b.checkpoint_vout,
                    b.transfers.len(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    // The batch transfer of the checkpoint
    assert_eq!(
        batches(&reveal),
        vec![(0, checkpoint_txid, commitment.vout, 1)]
    );

    // Rebuilt with an input before and after, and another output
    let mut rebuilt = reveal.clone();
    rebuilt.input.insert(0, foreign_input(1));
    rebuilt.input.push(foreign_input(2));
    rebuilt.output.push(bitcoin::TxOut {
        value: Amount::from_sat(1_000),
        script_pubkey: bitcoin::ScriptBuf::new_op_return([1]),
    });
    assert_eq!(
        batches(&rebuilt),
        vec![(1, checkpoint_txid, commitment.vout, 1)]
    );

    // Reordered: the data input last
    let mut reordered = rebuilt.clone();
    reordered.input.swap(1, 2);
    assert_eq!(
        batches(&reordered),
        vec![(2, checkpoint_txid, commitment.vout, 1)]
    );

    // Two checkpoints' batches in one reveal: one message per input, in input order
    let other_txid = test_utils::create_rand_txid();
    let other = checkpoint_msg
        .make_reveal_batch_transfer_tx(other_txid, fee_rate, &test_utils::create_rand_addr())
        .unwrap()
        .unwrap();
    let both = crate::easy_tester::reveal::merge_reveals([reveal.clone(), other]);
    assert_eq!(
        batches(&both),
        vec![
            (0, checkpoint_txid, commitment.vout, 1),
            (1, other_txid, commitment.vout, 1)
        ]
    );
}

#[test]
fn create_is_read_only_from_a_reveal_with_one_input_and_one_output() {
    let msg = IpcCreateSubnetMsg {
        min_validator_stake: Amount::from_sat(10_000),
        power_unit_exponent: 4,
        min_validators: 2,
        bottomup_check_period: 10,
        active_validators_limit: 10,
        min_cross_msg_fee: Amount::from_sat(1),
        whitelist: test_utils::generate_xonly_pubkeys(2),
        epoch_length: 10,
        min_timeout: 1,
        max_timeout: 1000,
        min_deposit: Amount::from_sat(330),
        kill_vote_period: 36,
        kill_delay_period: 5,
    };
    let leaf = bitcoin_utils::make_push_data_script(&msg.to_bytes().unwrap()).to_bytes();
    let output = bitcoin::TxOut {
        value: Amount::from_sat(500),
        script_pubkey: bitcoin::ScriptBuf::new_op_return([2]),
    };
    let reveal = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint::new(test_utils::create_rand_txid(), 0),
            witness: bitcoin::Witness::from_slice(&[leaf, vec![0xc0u8; 33]]),
            ..Default::default()
        }],
        output: vec![output.clone()],
    };
    let creates = |tx: &Transaction| {
        IpcMessage::from_reveal_inputs(tx)
            .iter()
            .filter(|(_, msg)| matches!(msg, IpcMessage::CreateSubnet(_)))
            .count()
    };
    assert_eq!(creates(&reveal), 1);

    // A second input, before or after the data input, or a second output: not a create
    let mut two_inputs = reveal.clone();
    two_inputs.input.push(foreign_input(3));
    assert_eq!(creates(&two_inputs), 0);
    let mut data_second = reveal.clone();
    data_second.input.insert(0, foreign_input(3));
    assert_eq!(creates(&data_second), 0);
    let mut two_outputs = reveal.clone();
    two_outputs.output.push(output);
    assert_eq!(creates(&two_outputs), 0);
}

#[test]
fn test_batch_transfer_reveal_tx_tag() {
    // Create a test checkpoint message with transfers
    let checkpoint_msg = create_test_checkpoint_msg();

    // Get the batched transfer data
    let transfer_data = checkpoint_msg.make_batched_transfer_data().unwrap();

    println!("{:?}", transfer_data);

    // Check that the transfer data starts with the correct tag
    let tag_bytes = IPC_TRANSFER_TAG.as_bytes();
    let prefix = &transfer_data[0..tag_bytes.len()];

    assert_eq!(
        prefix, tag_bytes,
        "Transfer data should start with IPC:TFR tag"
    );

    let tag_str = std::str::from_utf8(prefix).unwrap();
    assert_eq!(
        tag_str, IPC_TRANSFER_TAG,
        "Transfer tag should be valid UTF-8"
    );

    // Verify we have additional data after the tag
    assert!(
        transfer_data.len() > tag_bytes.len(),
        "Transfer data should contain more than just the tag"
    );
}

#[test]
fn print_postcard_savings_batch_transfer_msg() {
    let checkpoint_1_transfers = create_test_checkpoint_msg_multiple_transfers(1)
        .make_batched_transfer_data()
        .unwrap();
    // println!("{:?}", checkpoint_1_transfers);
    let old_checkpoint_1_transfer = [
        73, 80, 67, 84, 70, 82, 1, 19, 194, 87, 48, 180, 147, 217, 61, 218, 58, 159, 102, 135, 19,
        235, 46, 174, 111, 27, 185, 1, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163, 184,
        68, 188, 69, 78, 68, 56, 244, 0, 144, 78,
    ];

    let old_len = old_checkpoint_1_transfer.len() as isize;
    let new_len = checkpoint_1_transfers.len() as isize;
    println!(
        "Bincode size 1 transfer: {}\nPostcard size: {}\nDiff: {} bytes",
        old_len,
        new_len,
        old_len - new_len,
    );

    let checkpoint_5_transfers = create_test_checkpoint_msg_multiple_transfers(5)
        .make_batched_transfer_data()
        .unwrap();
    // println!("{:?}", checkpoint_5_transfers);

    let old_checkpoint_5_batch_transfers = vec![
        73, 80, 67, 84, 70, 82, 1, 122, 221, 179, 48, 179, 107, 209, 110, 158, 205, 125, 18, 233,
        196, 43, 196, 26, 42, 107, 251, 5, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163,
        184, 68, 188, 69, 78, 68, 56, 244, 0, 251, 16, 39, 20, 116, 45, 53, 204, 102, 52, 192, 83,
        41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 1, 251, 248, 42, 20, 116, 45, 53, 204, 102,
        52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 2, 251, 224, 46, 20, 116, 45,
        53, 204, 102, 52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 3, 251, 200, 50,
        20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 4,
        251, 176, 54,
    ];

    let old_len = old_checkpoint_5_batch_transfers.len() as isize;
    let new_len = checkpoint_5_transfers.len() as isize;
    println!(
        "Bincode size 5 transfers: {}\nPostcard size: {}\nDiff: {} bytes",
        old_len,
        new_len,
        old_len - new_len,
    );

    let checkpoint_15_transfers = create_test_checkpoint_msg_multiple_transfers(15)
        .make_batched_transfer_data()
        .unwrap();
    // println!("{:?}", checkpoint_15_transfers);

    let old_checkpoint_15_batch_transfers = vec![
        73, 80, 67, 84, 70, 82, 1, 14, 163, 184, 103, 216, 174, 124, 241, 177, 96, 47, 229, 47, 93,
        137, 70, 188, 206, 89, 55, 15, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163, 184,
        68, 188, 69, 78, 68, 56, 244, 0, 251, 16, 39, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41,
        37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 1, 251, 248, 42, 20, 116, 45, 53, 204, 102, 52,
        192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 2, 251, 224, 46, 20, 116, 45, 53,
        204, 102, 52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 3, 251, 200, 50, 20,
        116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 4, 251,
        176, 54, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56,
        244, 5, 251, 152, 58, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163, 184, 68, 188,
        69, 78, 68, 56, 244, 6, 251, 128, 62, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163,
        184, 68, 188, 69, 78, 68, 56, 244, 7, 251, 104, 66, 20, 116, 45, 53, 204, 102, 52, 192, 83,
        41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 8, 251, 80, 70, 20, 116, 45, 53, 204, 102,
        52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 9, 251, 56, 74, 20, 116, 45,
        53, 204, 102, 52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 10, 251, 32, 78,
        20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68, 56, 244, 11,
        251, 8, 82, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163, 184, 68, 188, 69, 78, 68,
        56, 244, 12, 251, 240, 85, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37, 163, 184, 68,
        188, 69, 78, 68, 56, 244, 13, 251, 216, 89, 20, 116, 45, 53, 204, 102, 52, 192, 83, 41, 37,
        163, 184, 68, 188, 69, 78, 68, 56, 244, 14, 251, 192, 93,
    ];

    let old_len = old_checkpoint_15_batch_transfers.len() as isize;
    let new_len = checkpoint_15_transfers.len() as isize;
    println!(
        "Bincode size 15 transfers: {}\nPostcard size: {}\nDiff: {} bytes",
        old_len,
        new_len,
        old_len - new_len,
    );
}

#[test]
fn test_batch_transfer_serialization_roundtrip() {
    // Create a checkpoint message with 5 transfers using existing helper
    let checkpoint_msg = create_test_checkpoint_msg_multiple_transfers(5);

    // 1. Serialize the transfer data using make_batched_transfer_data
    let serialized = checkpoint_msg.make_batched_transfer_data().unwrap();

    let wire_data = IpcBatchTransferMsg::witness_to_wire_data(&serialized).unwrap();

    // One subnet for native transfers
    assert_eq!(1, wire_data.transfers.len());

    let transfers = wire_data
        .transfers
        .get(
            // All transfers go to the same subnet in this test
            &checkpoint_msg
                .transfers
                .first()
                .unwrap()
                .destination_subnet_id
                .txid20(),
        )
        .unwrap();

    assert_eq!(checkpoint_msg.transfers.len(), transfers.len());
    assert!(wire_data.token_registrations.is_empty());
    assert!(wire_data.token_transfers.is_empty());
}

#[test]
fn test_checkpoint_batch_transfers_to_same_subnet() {
    // Create a subnet with 3 validators - this will be our source subnet
    let source_subnet = test_utils::generate_subnet(3);
    let committee = &source_subnet.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());

    // Create a destination subnet for transfers
    let destination_subnet = test_utils::generate_subnet(3);

    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    )
    .unwrap();

    // Create two transfers to the same destination subnet but different addresses
    let transfer1 = IpcCrossSubnetTransfer {
        amount: Amount::from_sat(30000),
        destination_subnet_id: destination_subnet.id,
        subnet_multisig_address: Some(destination_subnet.committee.multisig_address.clone()),
        subnet_user_address: alloy_primitives::Address::from_str(
            "742d35Cc6634C0532925a3b844Bc454e4438f44e",
        )
        .unwrap(),
    };

    let transfer2 = IpcCrossSubnetTransfer {
        amount: Amount::from_sat(40000),
        destination_subnet_id: destination_subnet.id,
        subnet_multisig_address: Some(destination_subnet.committee.multisig_address.clone()),
        subnet_user_address: alloy_primitives::Address::from_str(
            "1111111111111111111111111111111111111111", // Different address
        )
        .unwrap(),
    };

    // Create a second destination subnet for a third transfer
    let destination_subnet2 = test_utils::generate_subnet(3);

    let transfer3 = IpcCrossSubnetTransfer {
        amount: Amount::from_sat(50000),
        destination_subnet_id: destination_subnet2.id,
        subnet_multisig_address: Some(destination_subnet2.committee.multisig_address.clone()),
        subnet_user_address: alloy_primitives::Address::from_str(
            "2222222222222222222222222222222222222222",
        )
        .unwrap(),
    };

    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: source_subnet.id,
        checkpoint_hash,
        checkpoint_height: 50,
        transfers: vec![transfer1.clone(), transfer2.clone(), transfer3.clone()],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        withdrawals: vec![],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: Some(committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    let fee_rate = DEFAULT_BTC_FEE_RATE;

    // Generate transactions
    let checkpoint_psbt = checkpoint_msg
        .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
        .unwrap();
    let checkpoint_tx = checkpoint_psbt.unsigned_tx.clone();

    // Verify transaction output structure:
    // 1. OP_RETURN metadata
    // 2. Batch transfer commit output
    // 3. First destination subnet output (combined from transfer1 + transfer2)
    // 4. Second destination subnet output (from transfer3)
    // 5. Change output
    assert_eq!(checkpoint_tx.output.len(), 5, "Should have 5 outputs");

    // First output should be OP_RETURN with metadata
    assert!(
        checkpoint_tx.output[0].script_pubkey.is_op_return(),
        "First output should be OP_RETURN"
    );

    // Extract OP_RETURN data from metadata output
    let op_return_data = checkpoint_tx.output[0]
        .script_pubkey
        .instructions_minimal()
        .find_map(|ins| match ins {
            Ok(bitcoin::blockdata::script::Instruction::PushBytes(data)) => Some(data.as_bytes()),
            _ => None,
        })
        .unwrap();

    // The count byte is the number of destination subnets, not of transfers
    assert_eq!(
        op_return_data[IpcCheckpointSubnetMsg::MARKERS_OFFSET + 1],
        2,
        "Transfer batch count should be 2"
    );

    // Check that there's a batch transfer commit output
    let batch_tx_out = &checkpoint_tx.output[1]; // Second output should be batch transfer commit
    assert!(
        batch_tx_out.script_pubkey.is_p2tr(),
        "Batch transfer commit output should be P2TR"
    );

    // Outputs 2 and 3 pay the destinations in ascending txid20 order: the first
    // destination's output combines transfer1 and transfer2, the second's is transfer3
    let mut expected = vec![
        (
            destination_subnet.id.txid20(),
            destination_subnet
                .committee
                .multisig_address
                .assume_checked()
                .script_pubkey(),
            transfer1.amount + transfer2.amount,
        ),
        (
            destination_subnet2.id.txid20(),
            destination_subnet2
                .committee
                .multisig_address
                .assume_checked()
                .script_pubkey(),
            transfer3.amount,
        ),
    ];
    expected.sort_by_key(|(txid20, _, _)| *txid20);
    for (output, (_, script_pubkey, value)) in checkpoint_tx.output[2..4].iter().zip(&expected) {
        assert_eq!(
            output.script_pubkey, *script_pubkey,
            "Transfer outputs pay the destinations in ascending txid20 order"
        );
        assert_eq!(
            output.value, *value,
            "A destination's output carries the sum of its transfers"
        );
    }

    // Make the batch reveal transaction
    let batch_tx = checkpoint_msg
        .make_reveal_batch_transfer_tx(
            checkpoint_tx.compute_txid(),
            fee_rate,
            &committee.address_checked(),
        )
        .unwrap();

    // Verify we have a batch transaction
    assert!(
        batch_tx.is_some(),
        "Should have a batch transfer transaction"
    );
}

#[test]
fn test_stake_collateral_from_tx() {
    let db = crate::test_utils::create_test_db();
    let subnet_id = crate::test_utils::generate_subnet_id();
    let subnet_state = crate::db::SubnetState {
        id: subnet_id,
        committee_number: 1,
        committee: crate::db::SubnetCommittee {
            configuration_number: 0,
            threshold: 1,
            validators: vec![],
            multisig_address: bitcoin::Address::from_str(
                "bcrt1p2wqu7w8n8mnw37sl40u6y9tlpyy9hy6d2k4wt6r8ut7897ejl8fsgdhxzg",
            )
            .unwrap(),
        },
        waiting_committee: None,
        last_checkpoint_number: None,
        killed: db::SubnetKillState::NotKilled,
        marked_for_kill_checkpoint_number: None,
        epoch: 1,
        h_0: Some(0),
        last_rotation_epoch: 1,
        pending_multisig_changes: Vec::new(),
    };

    {
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_state(&mut wtxn, subnet_id, &subnet_state)
            .expect("saves subnet");
        wtxn.commit().expect("commits transaction");
    }

    let txbytes = hex::decode("020000000001019c40ba653f54403e8bfda357ccd6fff74746dea98d22316cd194544c27fe81ac0000000000fdffffff030000000000000000286a2649504353544b851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf460ec5300000000002251205381cf38f33ee6e8fa1fabf9a2157f09085b934d55aae5e867e2fc72fb32f9d31e03b229010000002251209acb5b5b3729a8f9ca2c2df76a6e600974f858352dbbca140e7e7d732e9da145024730440220316c97ebc27947c85e5ddec43d35075dc029b244188161265c4f540ceabd40d302200a7940cb366e46f1ba744ea84f1c34b6ade3fd3425f424099f19289fb64321ff012103fe6bbd509c039530b43c6c93524ec4d3df82da5de278096c095694166ccb828200000000").expect("Failed to parse transaction hex");

    let tx: bitcoin::Transaction =
        bitcoin::consensus::deserialize(&txbytes).expect("Failed to deserialize transaction");

    let msg = IpcStakeCollateralMsg::from_tx(&db, &db.read_txn().unwrap(), &tx)
        .expect("Should parse tx as msg");

    assert_eq!(msg.amount, Amount::from_sat(5500000));
    assert_eq!(
        msg.pubkey,
        XOnlyPublicKey::from_str(
            "851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4"
        )
        .unwrap()
    );

    dbg!(&msg);
}

#[test]
fn test_unstake_collateral_from_tx() {
    use crate::db::SubnetValidators;
    use crate::DEFAULT_BTC_FEE_RATE;

    let db = crate::test_utils::create_test_db();

    // Create test data
    let mut subnet_state = crate::test_utils::generate_subnet(4);
    let subnet_id = subnet_state.id;

    let keypairs = crate::test_utils::generate_keypairs(1);
    let keypair = keypairs.first().expect("should generate");
    let (xpk, _) = keypair.x_only_public_key();

    let prev_txid = bitcoin::Txid::all_zeros();

    // Update the subnet to use our validator
    subnet_state
        .committee
        .validators
        .first_mut()
        .unwrap()
        .pubkey = xpk;

    subnet_state.committee.multisig_address = subnet_state
        .committee
        .validators
        .multisig_address(&subnet_id);

    {
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_state(&mut wtxn, subnet_id, &subnet_state)
            .expect("saves subnet");
        wtxn.commit().expect("commits transaction");
    }

    let amount = bitcoin::Amount::from_sat(5500000);

    // Create the unstake message
    let unstake_msg = IpcUnstakeCollateralMsg {
        subnet_id,
        amount,
        pubkey: Some(xpk),
    };

    let msg = IpcUnstakeCollateralMsg::make_signature_msg(&prev_txid, &xpk, &amount, &subnet_id);

    dbg!(&subnet_state.committee);
    dbg!(prev_txid);
    dbg!(xpk);
    dbg!(amount);
    dbg!(subnet_id);
    dbg!(msg);

    let signature = unstake_msg.make_signature(&prev_txid, keypair.secret_key());

    // Generate a transaction from the message
    let mut tx = unstake_msg
        .to_tx(
            DEFAULT_BTC_FEE_RATE,
            &subnet_state.committee.multisig_address.assume_checked(),
            signature,
        )
        .expect("Should create transaction");

    tx.input = vec![bitcoin::TxIn {
        previous_output: bitcoin::OutPoint {
            txid: prev_txid,
            vout: 0,
        },
        script_sig: bitcoin::ScriptBuf::new(),
        sequence: bitcoin::Sequence::MAX,
        witness: bitcoin::Witness::new(),
    }];

    // Test parsing the transaction back into a message
    let parsed_msg = IpcUnstakeCollateralMsg::from_tx(&db, &db.read_txn().unwrap(), &tx)
        .expect("Should parse tx as msg");

    // Verify the parsed message matches the original
    assert_eq!(parsed_msg.subnet_id, subnet_id);
    assert_eq!(parsed_msg.amount, amount);
    assert_eq!(parsed_msg.pubkey, Some(xpk));

    // Print debug info
    println!("Original subnet_id: {}", subnet_id);
    println!("Parsed subnet_id  : {}", parsed_msg.subnet_id);

    // Test with hexadecimal transaction (easier to debug if needed)
    let serialized_tx = bitcoin::consensus::serialize(&tx);
    let hex_tx = hex::encode(&serialized_tx);
    println!("Transaction hex: {}", hex_tx);

    let tx_from_hex: bitcoin::Transaction =
        bitcoin::consensus::deserialize(&hex::decode(hex_tx).unwrap()).unwrap();

    let parsed_msg_from_hex =
        IpcUnstakeCollateralMsg::from_tx(&db, &db.read_txn().unwrap(), &tx_from_hex)
            .expect("Should parse tx from hex as msg");

    assert_eq!(parsed_msg_from_hex.subnet_id, subnet_id);
    assert_eq!(parsed_msg_from_hex.amount, amount);
    assert_eq!(parsed_msg_from_hex.pubkey, Some(xpk));
}

#[test]
fn test_unstake_collateral_unknown_signature() {
    use crate::DEFAULT_BTC_FEE_RATE;

    let db = crate::test_utils::create_test_db();

    // Create test data
    let subnet_state = crate::test_utils::generate_subnet(4);
    let subnet_id = subnet_state.id;

    // Intentionaly don't add this validator to the subnet committee
    let keypairs = crate::test_utils::generate_keypairs(1);
    let keypair = keypairs.first().expect("should generate");
    let (xpk, _) = keypair.x_only_public_key();

    let prev_txid = bitcoin::Txid::all_zeros();

    {
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_state(&mut wtxn, subnet_id, &subnet_state)
            .expect("saves subnet");
        wtxn.commit().expect("commits transaction");
    }

    let amount = bitcoin::Amount::from_sat(5500000);

    // Create the unstake message
    let unstake_msg = IpcUnstakeCollateralMsg {
        subnet_id,
        amount,
        pubkey: Some(xpk),
    };

    let msg = IpcUnstakeCollateralMsg::make_signature_msg(&prev_txid, &xpk, &amount, &subnet_id);

    dbg!(&subnet_state.committee);
    dbg!(prev_txid);
    dbg!(xpk);
    dbg!(amount);
    dbg!(subnet_id);
    dbg!(msg);

    let signature = unstake_msg.make_signature(&prev_txid, keypair.secret_key());

    // Generate a transaction from the message
    let mut tx = unstake_msg
        .to_tx(
            DEFAULT_BTC_FEE_RATE,
            &subnet_state.committee.multisig_address.assume_checked(),
            signature,
        )
        .expect("Should create transaction");

    tx.input = vec![bitcoin::TxIn {
        previous_output: bitcoin::OutPoint {
            txid: prev_txid,
            vout: 0,
        },
        script_sig: bitcoin::ScriptBuf::new(),
        sequence: bitcoin::Sequence::MAX,
        witness: bitcoin::Witness::new(),
    }];

    // Test parsing the transaction back into a message
    let parsed_msg = IpcUnstakeCollateralMsg::from_tx(&db, &db.read_txn().unwrap(), &tx);
    assert!(parsed_msg.is_err());

    let expected_error_msg = format!(
        "Signature doesn't match any of the validators in the committee for subnet {}",
        subnet_id
    );

    if let Err(IpcLibError::IpcValidateError(crate::ipc_lib::IpcValidateError::InvalidMsg(msg))) =
        parsed_msg
    {
        assert_eq!(msg, expected_error_msg);
    } else {
        panic!(
            "Expected IpcValidateError::InvalidMsg but got: {:?}",
            parsed_msg
        );
    }
}

#[test]
fn test_kill_checkpoint_change_distribution() {
    let subnet = test_utils::generate_subnet(3);

    // Clone the multisig address early to avoid borrow issues
    let committee_address = subnet.committee.multisig_address.clone();

    // Create unstakes with different amounts
    let unstake1 = IpcUnstake {
        amount: Amount::from_sat(300_000), // 30% of total
        address: subnet.committee.validators[0].backup_address.clone(),
        pubkey: subnet.committee.validators[0].pubkey,
    };
    let unstake2 = IpcUnstake {
        amount: Amount::from_sat(500_000), // 50% of total
        address: subnet.committee.validators[1].backup_address.clone(),
        pubkey: subnet.committee.validators[1].pubkey,
    };
    let unstake3 = IpcUnstake {
        amount: Amount::from_sat(200_000), // 20% of total
        address: subnet.committee.validators[2].backup_address.clone(),
        pubkey: subnet.committee.validators[2].pubkey,
    };

    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_byte_array([42u8; 32]);

    // Create a kill checkpoint with unstakes
    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet.id,
        checkpoint_hash,
        checkpoint_height: 50,
        withdrawals: vec![],
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
        unstakes: vec![unstake1.clone(), unstake2.clone(), unstake3.clone()],
        change_address: None, // Kill checkpoints shouldn't have change address
        next_committee_configuration_number: 1,
        is_kill_checkpoint: true,
        is_rotation_checkpoint: false,
    };

    // Create test UTXOs with extra funds that will become change
    let extra_change = Amount::from_sat(100_000); // This should be distributed among unstakes
    let script_pub_key = committee_address
        .clone()
        .require_network(NETWORK)
        .unwrap()
        .script_pubkey();
    let unspent = vec![bitcoincore_rpc::json::ListUnspentResultEntry {
        txid: bitcoin::Txid::from_str(
            "f61b1742ca13176464adb3cb66050c00787bb3a4eead37e985f2df1e37718126",
        )
        .unwrap(),
        vout: 0,
        address: None,
        label: None,
        redeem_script: None,
        witness_script: None,
        script_pub_key: script_pub_key.clone(),
        amount: checkpoint_msg
            .unstakes
            .iter()
            .map(|u| u.amount)
            .sum::<Amount>()
            + extra_change
            + Amount::from_sat(4_000), // Add some buffer for fees
        descriptor: None,
        spendable: true,
        solvable: true,
        safe: true,
        confirmations: 6,
    }];

    // Generate the checkpoint PSBT
    let psbt_result = checkpoint_msg.to_checkpoint_psbt(
        &subnet.committee,
        &subnet.committee, // Same committee (no rotation)
        DEFAULT_BTC_FEE_RATE,
        &unspent,
    );

    assert!(psbt_result.is_ok(), "Failed to create checkpoint PSBT");
    let psbt = psbt_result.unwrap();

    // Check that there's no change output in the transaction
    let committee_script = committee_address
        .require_network(NETWORK)
        .unwrap()
        .script_pubkey();
    let change_outputs: Vec<_> = psbt
        .unsigned_tx
        .output
        .iter()
        .filter(|out| out.script_pubkey == committee_script)
        .collect();

    dbg!(&psbt.unsigned_tx);
    dbg!(&change_outputs);

    assert_eq!(
        change_outputs.len(),
        0,
        "Kill checkpoint should not have change output"
    );

    // Verify that unstake outputs have been increased
    // The unstake outputs should be outputs 1, 2, 3 (after metadata at index 0)
    let total_original_unstakes: Amount = checkpoint_msg.unstakes.iter().map(|u| u.amount).sum();
    let total_final_unstakes: Amount = psbt.unsigned_tx.output[1..4]
        .iter()
        .map(|out| out.value)
        .sum();

    // The final unstakes should be larger than original due to distributed change
    assert!(
        total_final_unstakes > total_original_unstakes,
        "Unstake outputs should have increased due to change distribution"
    );

    // Verify proportional distribution
    let expected_increase_1 = Amount::from_sat((extra_change.to_sat() as f64 * 0.3) as u64); // 30%
    let expected_increase_2 = Amount::from_sat((extra_change.to_sat() as f64 * 0.5) as u64); // 50%
    let expected_increase_3 = Amount::from_sat((extra_change.to_sat() as f64 * 0.2) as u64); // 20%

    let actual_1 = psbt.unsigned_tx.output[1].value;
    let actual_2 = psbt.unsigned_tx.output[2].value;
    let actual_3 = psbt.unsigned_tx.output[3].value;

    // Allow some tolerance for rounding

    dbg!(actual_1 - (unstake1.amount + expected_increase_1));
    dbg!(actual_2 - (unstake2.amount + expected_increase_2));
    dbg!(actual_3 - (unstake3.amount + expected_increase_3));

    assert!((actual_1 - (unstake1.amount + expected_increase_1)) < Amount::from_sat(200));
    assert!((actual_2 - (unstake2.amount + expected_increase_2)) < Amount::from_sat(200));
    assert!((actual_3 - (unstake3.amount + expected_increase_3)) < Amount::from_sat(200));

    // Verify total is conserved (minus fees)
    let total_input: Amount = unspent.iter().map(|u| u.amount).sum();
    let total_output: Amount = psbt.unsigned_tx.output.iter().map(|out| out.value).sum();
    assert!(
        total_input >= total_output,
        "Total output should not exceed total input"
    );
}

#[test]
fn test_unstake_collateral_replay_attack() {
    use crate::db::SubnetValidators;
    use crate::DEFAULT_BTC_FEE_RATE;

    let db = crate::test_utils::create_test_db();

    // Create test data
    let mut subnet_state = crate::test_utils::generate_subnet(4);
    let subnet_id = subnet_state.id;

    let keypairs = crate::test_utils::generate_keypairs(1);
    let keypair = keypairs.first().expect("should generate");
    let (xpk, _) = keypair.x_only_public_key();

    let prev_txid = bitcoin::Txid::all_zeros();

    // Update the subnet to use our validator
    subnet_state
        .committee
        .validators
        .first_mut()
        .unwrap()
        .pubkey = xpk;

    subnet_state.committee.multisig_address = subnet_state
        .committee
        .validators
        .multisig_address(&subnet_id);

    {
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_state(&mut wtxn, subnet_id, &subnet_state)
            .expect("saves subnet");
        wtxn.commit().expect("commits transaction");
    }

    let amount = bitcoin::Amount::from_sat(5500000);

    // Create the unstake message
    let unstake_msg = IpcUnstakeCollateralMsg {
        subnet_id,
        amount,
        pubkey: Some(xpk),
    };

    let msg = IpcUnstakeCollateralMsg::make_signature_msg(&prev_txid, &xpk, &amount, &subnet_id);

    dbg!(&subnet_state.committee);
    dbg!(prev_txid);
    dbg!(xpk);
    dbg!(amount);
    dbg!(subnet_id);
    dbg!(msg);

    let signature = unstake_msg.make_signature(&prev_txid, keypair.secret_key());

    // Generate a transaction from the message
    let mut tx = unstake_msg
        .to_tx(
            DEFAULT_BTC_FEE_RATE,
            &subnet_state.committee.multisig_address.assume_checked(),
            signature,
        )
        .expect("Should create transaction");

    tx.input = vec![bitcoin::TxIn {
        previous_output: bitcoin::OutPoint {
            // Intentionally set to a different txid to simulate a replay attack
            txid: Txid::from_str(
                "5e66f5f4e961aa5aa35d226c100e1931d5f4ee99e149a3a9549edad279f77236",
            )
            .expect("valid txid"),
            vout: 0,
        },
        script_sig: bitcoin::ScriptBuf::new(),
        sequence: bitcoin::Sequence::MAX,
        witness: bitcoin::Witness::new(),
    }];

    // Test parsing the transaction back into a message
    let parsed_msg = IpcUnstakeCollateralMsg::from_tx(&db, &db.read_txn().unwrap(), &tx);
    assert!(parsed_msg.is_err());

    // Even though our validator is in the committee, the prev_txid is different
    // it will not match the signature

    let expected_error_msg = format!(
        "Signature doesn't match any of the validators in the committee for subnet {}",
        subnet_id
    );

    if let Err(IpcLibError::IpcValidateError(crate::ipc_lib::IpcValidateError::InvalidMsg(msg))) =
        parsed_msg
    {
        assert_eq!(msg, expected_error_msg);
    } else {
        panic!(
            "Expected IpcValidateError::InvalidMsg but got: {:?}",
            parsed_msg
        );
    }
}

// ---- ERC token registration (ETR) and ERC transfer (ETX) tests ----

fn create_test_checkpoint_msg_with_etr_etx() -> IpcCheckpointSubnetMsg {
    let subnet_state = test_utils::generate_subnet(4);
    let destination_subnet = test_utils::generate_subnet(4);

    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    )
    .unwrap();

    let transfer = test_utils::create_rand_ipc_cross_subnet_transfer(&destination_subnet, None);
    let etr = test_utils::create_rand_erc_token_registration();
    let etx = test_utils::create_rand_erc_transfer(subnet_state.id, destination_subnet.id);

    IpcCheckpointSubnetMsg {
        subnet_id: subnet_state.id,
        checkpoint_hash,
        checkpoint_height: 50,
        slash_signatures: vec![],
        unstakes: vec![],
        withdrawals: vec![],
        transfers: vec![transfer],
        token_registrations: vec![etr],
        token_supply_adjustments: vec![],
        token_transfers: vec![etx],
        change_address: Some(subnet_state.committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    }
}

#[test]
fn test_etr_etx_serialization_roundtrip() {
    let checkpoint_msg = create_test_checkpoint_msg_with_etr_etx();

    // Serialize
    let serialized = checkpoint_msg.make_batched_transfer_data().unwrap();

    // Deserialize
    let wire_data = IpcBatchTransferMsg::witness_to_wire_data(&serialized).unwrap();

    // Verify native transfers
    assert_eq!(wire_data.transfers.len(), 1);

    // Verify token registrations roundtrip
    assert_eq!(wire_data.token_registrations.len(), 1);
    let etr = &wire_data.token_registrations[0];
    assert_eq!(
        etr.home_token_address,
        checkpoint_msg.token_registrations[0].home_token_address
    );
    assert_eq!(etr.name, "TestToken");
    assert_eq!(etr.symbol, "TT");
    assert_eq!(etr.decimals, 18);

    // Verify ERC transfers roundtrip
    assert_eq!(wire_data.token_transfers.len(), 1);
    let dst_key = checkpoint_msg.token_transfers[0]
        .destination_subnet_id
        .txid20();
    let etx_list = wire_data.token_transfers.get(&dst_key).unwrap();
    assert_eq!(etx_list.len(), 1);
    assert_eq!(
        etx_list[0].home_subnet_id,
        checkpoint_msg.token_transfers[0].home_subnet_id
    );
    assert_eq!(
        etx_list[0].home_token_address,
        checkpoint_msg.token_transfers[0].home_token_address
    );
    assert_eq!(etx_list[0].amount, checkpoint_msg.token_transfers[0].amount);
    assert_eq!(
        etx_list[0].recipient,
        checkpoint_msg.token_transfers[0].recipient
    );
}

#[test]
fn test_etr_only_checkpoint_has_batch_transfer() {
    let subnet_state = test_utils::generate_subnet(4);
    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    )
    .unwrap();

    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet_state.id,
        checkpoint_hash,
        checkpoint_height: 50,
        slash_signatures: vec![],
        unstakes: vec![],
        withdrawals: vec![],
        transfers: vec![],
        token_registrations: vec![test_utils::create_rand_erc_token_registration()],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        change_address: Some(subnet_state.committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    checkpoint_msg.validate().unwrap();

    // ETR-only should still produce a batch transfer reveal tx
    let data = checkpoint_msg.make_batched_transfer_data().unwrap();
    assert!(
        data.len() > IPC_TAG_LENGTH,
        "ETR-only should produce non-empty witness data"
    );

    let wire_data = IpcBatchTransferMsg::witness_to_wire_data(&data).unwrap();
    assert!(wire_data.transfers.is_empty());
    assert_eq!(wire_data.token_registrations.len(), 1);
    assert!(wire_data.token_transfers.is_empty());
}

#[test]
fn test_etx_only_checkpoint_has_batch_transfer() {
    let subnet_state = test_utils::generate_subnet(4);
    let destination_subnet = test_utils::generate_subnet(4);
    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    )
    .unwrap();

    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet_state.id,
        checkpoint_hash,
        checkpoint_height: 50,
        slash_signatures: vec![],
        unstakes: vec![],
        withdrawals: vec![],
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![test_utils::create_rand_erc_transfer(
            subnet_state.id,
            destination_subnet.id,
        )],
        change_address: Some(subnet_state.committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    checkpoint_msg.validate().unwrap();

    let data = checkpoint_msg.make_batched_transfer_data().unwrap();
    let wire_data = IpcBatchTransferMsg::witness_to_wire_data(&data).unwrap();
    assert!(wire_data.transfers.is_empty());
    assert!(wire_data.token_registrations.is_empty());
    assert_eq!(wire_data.token_transfers.len(), 1);
}

#[test]
fn test_mixed_checkpoint_markers() {
    let checkpoint_msg = create_test_checkpoint_msg_with_etr_etx();
    let fee_rate = bitcoin::FeeRate::from_sat_per_vb(10).unwrap();

    let metadata_tx_out = checkpoint_msg.make_metadata_tx_out(fee_rate);
    let (withdrawal_count, transfer_count, unstake_count, killed) =
        IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&metadata_tx_out)
            .expect("Should extract markers");

    assert_eq!(withdrawal_count, 0);
    assert_eq!(transfer_count, 1);
    assert_eq!(unstake_count, 0);
    assert!(!killed);
}

#[test]
fn test_batch_transfer_validate_rejects_empty_token_name() {
    let mut etr = test_utils::create_rand_erc_token_registration();
    etr.name = "".to_string();

    let batch_msg = IpcBatchTransferMsg {
        subnet_id: SubnetId::default(),
        checkpoint_txid: test_utils::create_rand_txid(),
        checkpoint_vout: 0,
        transfers: vec![],
        token_registrations: vec![etr],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
    };

    let result = batch_msg.validate();
    assert!(result.is_err());
    assert!(format!("{:?}", result.unwrap_err()).contains("name must not be empty"));
}

#[test]
fn test_batch_transfer_validate_rejects_empty_token_symbol() {
    let mut etr = test_utils::create_rand_erc_token_registration();
    etr.symbol = "".to_string();

    let batch_msg = IpcBatchTransferMsg {
        subnet_id: SubnetId::default(),
        checkpoint_txid: test_utils::create_rand_txid(),
        checkpoint_vout: 0,
        transfers: vec![],
        token_registrations: vec![etr],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
    };

    let result = batch_msg.validate();
    assert!(result.is_err());
    assert!(format!("{:?}", result.unwrap_err()).contains("symbol must not be empty"));
}

#[test]
fn test_batch_transfer_validate_rejects_zero_token_address_etr() {
    let mut etr = test_utils::create_rand_erc_token_registration();
    etr.home_token_address = alloy_primitives::Address::ZERO;

    let batch_msg = IpcBatchTransferMsg {
        subnet_id: SubnetId::default(),
        checkpoint_txid: test_utils::create_rand_txid(),
        checkpoint_vout: 0,
        transfers: vec![],
        token_registrations: vec![etr],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
    };

    let result = batch_msg.validate();
    assert!(result.is_err());
    assert!(format!("{:?}", result.unwrap_err()).contains("Token address must not be zero"));
}

#[test]
fn test_batch_transfer_validate_rejects_zero_token_address_etx() {
    let subnet_state = test_utils::generate_subnet(4);
    let destination_subnet = test_utils::generate_subnet(4);
    let mut etx = test_utils::create_rand_erc_transfer(subnet_state.id, destination_subnet.id);
    etx.home_token_address = alloy_primitives::Address::ZERO;

    let batch_msg = IpcBatchTransferMsg {
        subnet_id: SubnetId::default(),
        checkpoint_txid: test_utils::create_rand_txid(),
        checkpoint_vout: 0,
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![etx],
        slash_signatures: vec![],
    };

    let result = batch_msg.validate();
    assert!(result.is_err());
    assert!(format!("{:?}", result.unwrap_err()).contains("token address must not be zero"));
}

#[test]
fn test_batch_transfer_validate_rejects_zero_recipient_etx() {
    let subnet_state = test_utils::generate_subnet(4);
    let destination_subnet = test_utils::generate_subnet(4);
    let mut etx = test_utils::create_rand_erc_transfer(subnet_state.id, destination_subnet.id);
    etx.recipient = alloy_primitives::Address::ZERO;

    let batch_msg = IpcBatchTransferMsg {
        subnet_id: SubnetId::default(),
        checkpoint_txid: test_utils::create_rand_txid(),
        checkpoint_vout: 0,
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![etx],
        slash_signatures: vec![],
    };

    let result = batch_msg.validate();
    assert!(result.is_err());
    assert!(format!("{:?}", result.unwrap_err()).contains("recipient must not be zero"));
}

#[test]
fn test_batch_transfer_validate_rejects_zero_amount_etx() {
    let subnet_state = test_utils::generate_subnet(4);
    let destination_subnet = test_utils::generate_subnet(4);
    let mut etx = test_utils::create_rand_erc_transfer(subnet_state.id, destination_subnet.id);
    etx.amount = U256::ZERO;

    let batch_msg = IpcBatchTransferMsg {
        subnet_id: SubnetId::default(),
        checkpoint_txid: test_utils::create_rand_txid(),
        checkpoint_vout: 0,
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![etx],
        slash_signatures: vec![],
    };

    let result = batch_msg.validate();
    assert!(result.is_err());
    assert!(format!("{:?}", result.unwrap_err()).contains("amount must be greater than zero"));
}

#[test]
fn test_multiple_erc_transfers_to_different_subnets_roundtrip() {
    let subnet_state = test_utils::generate_subnet(4);
    let dest1 = test_utils::generate_subnet(4);
    let dest2 = test_utils::generate_subnet(4);
    let checkpoint_hash = bitcoin::hashes::sha256::Hash::from_str(
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    )
    .unwrap();

    let etx1 = test_utils::create_rand_erc_transfer(subnet_state.id, dest1.id);
    let etx2 = test_utils::create_rand_erc_transfer(subnet_state.id, dest2.id);
    let etx3 = test_utils::create_rand_erc_transfer(subnet_state.id, dest1.id);

    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet_state.id,
        checkpoint_hash,
        checkpoint_height: 50,
        slash_signatures: vec![],
        unstakes: vec![],
        withdrawals: vec![],
        transfers: vec![],
        token_registrations: vec![
            test_utils::create_rand_erc_token_registration(),
            test_utils::create_rand_erc_token_registration(),
        ],
        token_supply_adjustments: vec![],
        token_transfers: vec![etx1, etx2, etx3],
        change_address: Some(subnet_state.committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    checkpoint_msg.validate().unwrap();

    // Verify markers
    let fee_rate = bitcoin::FeeRate::from_sat_per_vb(10).unwrap();
    let metadata_tx_out = checkpoint_msg.make_metadata_tx_out(fee_rate);
    IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&metadata_tx_out).unwrap();

    // Verify roundtrip
    let data = checkpoint_msg.make_batched_transfer_data().unwrap();
    let wire_data = IpcBatchTransferMsg::witness_to_wire_data(&data).unwrap();

    assert_eq!(wire_data.token_registrations.len(), 2);
    // 3 ETX across 2 destination subnets
    let total_etx: usize = wire_data.token_transfers.values().map(|v| v.len()).sum();
    assert_eq!(total_etx, 3);
    assert_eq!(
        wire_data.token_transfers.len(),
        2,
        "Should have 2 destination subnets"
    );
}

// ── U256 arithmetic tests ──

// ── U256/I256 arithmetic tests (using alloy_primitives) ──

#[test]
fn test_u256_arithmetic() {
    let a = U256::from(1000u64);
    let b = U256::from(2000u64);
    assert_eq!(a + b, U256::from(3000u64));
    assert_eq!(b - a, U256::from(1000u64));
    assert!(a < b);
    assert!(a.checked_sub(b).is_none());
}

#[test]
fn test_i256_supply_adjustment() {
    let balance = U256::from(1000u64);
    let mint_delta = I256::try_from(500i64).unwrap();
    let burn_delta = I256::try_from(-300i64).unwrap();

    // Mint: balance + positive delta
    let balance_i = I256::try_from(balance).unwrap();
    let after_mint: U256 = (balance_i + mint_delta).into_raw().into();
    assert_eq!(after_mint, U256::from(1500u64));

    // Burn: balance + negative delta
    let after_burn_i = I256::try_from(after_mint).unwrap() + burn_delta;
    assert!(after_burn_i >= I256::ZERO);
    let after_burn: U256 = after_burn_i.into_raw().into();
    assert_eq!(after_burn, U256::from(1200u64));
}

#[test]
fn test_i256_underflow_detection() {
    let balance = U256::from(100u64);
    let burn_delta = I256::try_from(-200i64).unwrap();
    let result = I256::try_from(balance).unwrap() + burn_delta;
    assert!(result < I256::ZERO, "Should detect underflow");
}

// ── Checkpoint output structure tests for ETR/ETX-only checkpoints ──

#[test]
fn test_etr_only_checkpoint_output_structure() {
    let subnet = test_utils::generate_subnet(3);
    let committee = &subnet.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());

    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet.id,
        checkpoint_hash: bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap(),
        checkpoint_height: 50,
        withdrawals: vec![],
        transfers: vec![],
        token_registrations: vec![test_utils::create_rand_erc_token_registration()],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: Some(committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    let fee_rate = DEFAULT_BTC_FEE_RATE;
    let checkpoint_psbt = checkpoint_msg
        .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
        .unwrap();
    let checkpoint_tx = checkpoint_psbt.unsigned_tx;

    // ETR-only: should have exactly 3 outputs:
    // 1. OP_RETURN metadata
    // 2. Batch transfer commit output (for the witness blob)
    // 3. Change output
    // NO value-transfer outputs to destination subnets
    assert_eq!(
        checkpoint_tx.output.len(),
        3,
        "ETR-only checkpoint should have 3 outputs (OP_RETURN + commit + change)"
    );
    assert!(
        checkpoint_tx.output[0].script_pubkey.is_op_return(),
        "First output should be OP_RETURN"
    );
    // Second output is the commit (P2TR)
    assert!(
        !checkpoint_tx.output[1].script_pubkey.is_op_return(),
        "Second output should be the commit output, not OP_RETURN"
    );

    // Reveal tx should exist (the witness blob needs to be published)
    let reveal_tx = checkpoint_msg
        .make_reveal_batch_transfer_tx(
            checkpoint_tx.compute_txid(),
            fee_rate,
            &committee.address_checked(),
        )
        .unwrap();
    assert!(
        reveal_tx.is_some(),
        "ETR-only checkpoint should produce a reveal transaction"
    );
}

#[test]
fn test_etx_only_checkpoint_output_structure() {
    let subnet = test_utils::generate_subnet(3);
    let destination = test_utils::generate_subnet(3);
    let committee = &subnet.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());

    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet.id,
        checkpoint_hash: bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap(),
        checkpoint_height: 50,
        withdrawals: vec![],
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![test_utils::create_rand_erc_transfer(
            subnet.id,
            destination.id,
        )],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: Some(committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    let fee_rate = DEFAULT_BTC_FEE_RATE;
    let checkpoint_psbt = checkpoint_msg
        .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
        .unwrap();
    let checkpoint_tx = checkpoint_psbt.unsigned_tx;

    // ETX-only (no native transfers): should have exactly 3 outputs:
    // 1. OP_RETURN metadata
    // 2. Batch transfer commit output (for the witness blob)
    // 3. Change output
    // NO BTC value-transfer outputs — ETX doesn't move BTC between multisigs
    assert_eq!(
        checkpoint_tx.output.len(),
        3,
        "ETX-only checkpoint should have 3 outputs (OP_RETURN + commit + change)"
    );
    assert!(
        checkpoint_tx.output[0].script_pubkey.is_op_return(),
        "First output should be OP_RETURN"
    );
}

#[test]
fn test_mixed_native_and_erc_checkpoint_output_structure() {
    let subnet = test_utils::generate_subnet(3);
    let destination = test_utils::generate_subnet(3);
    let committee = &subnet.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());

    let native_transfer = IpcCrossSubnetTransfer {
        amount: Amount::from_sat(30000),
        destination_subnet_id: destination.id,
        subnet_multisig_address: Some(destination.committee.multisig_address.clone()),
        subnet_user_address: test_utils::create_rand_eth_addr(),
    };

    let checkpoint_msg = IpcCheckpointSubnetMsg {
        subnet_id: subnet.id,
        checkpoint_hash: bitcoin::hashes::sha256::Hash::from_str(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        )
        .unwrap(),
        checkpoint_height: 50,
        withdrawals: vec![],
        transfers: vec![native_transfer],
        token_registrations: vec![test_utils::create_rand_erc_token_registration()],
        token_supply_adjustments: vec![],
        token_transfers: vec![test_utils::create_rand_erc_transfer(
            subnet.id,
            destination.id,
        )],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: Some(committee.multisig_address.clone()),
        next_committee_configuration_number: 1,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };

    let fee_rate = DEFAULT_BTC_FEE_RATE;
    let checkpoint_psbt = checkpoint_msg
        .to_checkpoint_psbt(committee, committee, fee_rate, &utxos)
        .unwrap();
    let checkpoint_tx = checkpoint_psbt.unsigned_tx;

    // Mixed native + ERC: should have 4 outputs:
    // 1. OP_RETURN metadata
    // 2. Batch transfer commit output
    // 3. Native transfer value output (to destination multisig)
    // 4. Change output
    // The ETR and ETX are in the witness blob — no extra outputs for them
    assert_eq!(
        checkpoint_tx.output.len(),
        4,
        "Mixed checkpoint should have 4 outputs (OP_RETURN + commit + 1 native transfer + change)"
    );
    assert!(
        checkpoint_tx.output[0].script_pubkey.is_op_return(),
        "First output should be OP_RETURN"
    );
}

/// Seed a fresh DB with a single token balance entry.
fn db_with_balance(
    home_subnet: SubnetId,
    token: alloy_primitives::Address,
    holder: SubnetId,
    balance: U256,
) -> crate::db::HeedDb {
    let db = test_utils::create_test_db();
    let mut wtxn = db.write_txn().unwrap();
    db.set_token_balance(&mut wtxn, home_subnet, token, holder, balance)
        .unwrap();
    wtxn.commit().unwrap();
    db
}

fn etr(token: alloy_primitives::Address, initial_supply: u64) -> IpcErcTokenRegistration {
    IpcErcTokenRegistration {
        home_token_address: token,
        name: "Token".to_string(),
        symbol: "TK".to_string(),
        decimals: 18,
        initial_supply: U256::from(initial_supply),
    }
}

fn ets_mint(token: alloy_primitives::Address, amount: i64) -> IpcErcSupplyAdjustment {
    IpcErcSupplyAdjustment {
        home_token_address: token,
        delta: I256::try_from(amount).unwrap(),
    }
}

fn ets_burn(token: alloy_primitives::Address, amount: i64) -> IpcErcSupplyAdjustment {
    IpcErcSupplyAdjustment {
        home_token_address: token,
        delta: I256::try_from(-amount).unwrap(),
    }
}

fn etx(
    home: SubnetId,
    token: alloy_primitives::Address,
    amount: u64,
    dest: SubnetId,
) -> IpcCrossSubnetErcTransfer {
    IpcCrossSubnetErcTransfer {
        home_subnet_id: home,
        home_token_address: token,
        amount: U256::from(amount),
        destination_subnet_id: dest,
        recipient: test_utils::create_rand_eth_addr(),
    }
}

// ── happy paths ─────────────────────────────────────────────────────────

#[test]
fn test_empty_batch_ok() {
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet_id();
    assert!(validate_erc_batch_data(source, &[], &[], &[], &db, &db.read_txn().unwrap()).is_ok());
}

#[test]
fn test_new_registration_ok() {
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    assert!(validate_erc_batch_data(
        source,
        &[etr(token, 1_000_000)],
        &[],
        &[],
        &db,
        &db.read_txn().unwrap()
    )
    .is_ok());
}

#[test]
fn test_mint_ok() {
    // Minting always succeeds (increases balance); DB balance starts at 0.
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    assert!(validate_erc_batch_data(
        source,
        &[],
        &[ets_mint(token, 500)],
        &[],
        &db,
        &db.read_txn().unwrap()
    )
    .is_ok());
}

#[test]
fn test_burn_within_balance_ok() {
    let source = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    let db = db_with_balance(source, token, source, U256::from(1000u64));
    assert!(validate_erc_batch_data(
        source,
        &[],
        &[ets_burn(token, 500)],
        &[],
        &db,
        &db.read_txn().unwrap()
    )
    .is_ok());
}

#[test]
fn test_transfer_within_balance_ok() {
    let source = test_utils::generate_subnet_id();
    let dest = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    let db = db_with_balance(source, token, source, U256::from(1000u64));
    assert!(validate_erc_batch_data(
        source,
        &[],
        &[],
        &[etx(source, token, 500, dest)],
        &db,
        &db.read_txn().unwrap()
    )
    .is_ok());
}

#[test]
fn test_register_then_transfer_in_same_batch_ok() {
    // Token registered with initial_supply=1000 and transferred 500 in the same checkpoint.
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet_id();
    let dest = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    assert!(validate_erc_batch_data(
        source,
        &[etr(token, 1000)],
        &[],
        &[etx(source, token, 500, dest)],
        &db,
        &db.read_txn().unwrap(),
    )
    .is_ok());
}

#[test]
fn test_mint_before_burn_order_in_batch_ok() {
    // Even when the burn appears before the mint in the slice, mints are processed first.
    // DB balance = 100, batch has burn 250 then mint 300.
    // Expected: mint first → 400, then burn → 150. Should pass.
    let source = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    let db = db_with_balance(source, token, source, U256::from(100u64));
    assert!(validate_erc_batch_data(
        source,
        &[],
        &[ets_burn(token, 250), ets_mint(token, 300)],
        &[],
        &db,
        &db.read_txn().unwrap(),
    )
    .is_ok());
}

#[test]
fn test_foreign_token_transfer_ok() {
    // Token's home subnet differs from the checkpoint submitter.
    let home = test_utils::generate_subnet_id();
    let source = test_utils::generate_subnet_id();
    let dest = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    // source_subnet holds 1000 of the foreign token
    let db = db_with_balance(home, token, source, U256::from(1000u64));
    assert!(validate_erc_batch_data(
        source,
        &[],
        &[],
        &[etx(home, token, 500, dest)],
        &db,
        &db.read_txn().unwrap()
    )
    .is_ok());
}

// ── error paths ─────────────────────────────────────────────────────────

#[test]
fn test_duplicate_registration_in_batch_err() {
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    let result = validate_erc_batch_data(
        source,
        &[etr(token, 1000), etr(token, 2000)],
        &[],
        &[],
        &db,
        &db.read_txn().unwrap(),
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("registered more than once"),
        "wrong error message"
    );
}

#[test]
fn test_re_registration_of_existing_token_err() {
    let source = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    let db = db_with_balance(source, token, source, U256::from(500u64));
    let result = validate_erc_batch_data(
        source,
        &[etr(token, 1_000_000)],
        &[],
        &[],
        &db,
        &db.read_txn().unwrap(),
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("already registered"),
        "wrong error message"
    );
}

#[test]
fn test_burn_exceeds_balance_err() {
    let source = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    let db = db_with_balance(source, token, source, U256::from(100u64));
    let result = validate_erc_batch_data(
        source,
        &[],
        &[ets_burn(token, 200)],
        &[],
        &db,
        &db.read_txn().unwrap(),
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Burn exceeds balance"),
        "wrong error message"
    );
}

#[test]
fn test_transfer_exceeds_balance_err() {
    let source = test_utils::generate_subnet_id();
    let dest = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    let db = db_with_balance(source, token, source, U256::from(100u64));
    let result = validate_erc_batch_data(
        source,
        &[],
        &[],
        &[etx(source, token, 200, dest)],
        &db,
        &db.read_txn().unwrap(),
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Insufficient balance"),
        "wrong error message"
    );
}

#[test]
fn test_register_then_over_transfer_err() {
    // Register with initial_supply=100, transfer 200 — should fail.
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet_id();
    let dest = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    let result = validate_erc_batch_data(
        source,
        &[etr(token, 100)],
        &[],
        &[etx(source, token, 200, dest)],
        &db,
        &db.read_txn().unwrap(),
    );
    assert!(result.is_err());
}

#[test]
fn test_two_transfers_same_token_cumulative_check_err() {
    // Balance = 300; two transfers of 200 each — second exceeds remaining balance.
    let source = test_utils::generate_subnet_id();
    let dest1 = test_utils::generate_subnet_id();
    let dest2 = test_utils::generate_subnet_id();
    let token = test_utils::create_rand_eth_addr();
    let db = db_with_balance(source, token, source, U256::from(300u64));
    let result = validate_erc_batch_data(
        source,
        &[],
        &[],
        &[
            etx(source, token, 200, dest1),
            etx(source, token, 200, dest2),
        ],
        &db,
        &db.read_txn().unwrap(),
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Insufficient balance"),
        "wrong error message"
    );
}

#[test]
fn test_etx_zero_amount_rejected() {
    let source = test_utils::generate_subnet(4).id;
    let dest = test_utils::generate_subnet(4).id;
    let token = test_utils::create_rand_eth_addr();
    let db = db_with_balance(source, token, source, U256::from(1000));

    let result = validate_erc_batch_data(
        source,
        &[],
        &[],
        &[etx(source, token, 0, dest)],
        &db,
        &db.read_txn().unwrap(),
    );
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("Zero-amount ETX"),
        "wrong error message"
    );
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Uw(#[serde(with = "compact_amount::u256")] U256);

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Iw(#[serde(with = "compact_amount::i256")] I256);

fn rt_u256(v: U256) -> U256 {
    let enc = postcard::to_stdvec(&Uw(v)).unwrap();
    postcard::from_bytes::<Uw>(&enc).unwrap().0
}

fn rt_i256(v: I256) -> I256 {
    let enc = postcard::to_stdvec(&Iw(v)).unwrap();
    postcard::from_bytes::<Iw>(&enc).unwrap().0
}

fn wire_len_u256(v: U256) -> usize {
    postcard::to_stdvec(&Uw(v)).unwrap().len()
}

#[test]
fn u256_roundtrips() {
    assert_eq!(rt_u256(U256::ZERO), U256::ZERO);
    assert_eq!(rt_u256(U256::from(1u64)), U256::from(1u64));
    assert_eq!(rt_u256(U256::from(255u64)), U256::from(255u64));
    assert_eq!(rt_u256(U256::from(256u64)), U256::from(256u64));
    assert_eq!(rt_u256(U256::from(1_000_000u64)), U256::from(1_000_000u64));
    assert_eq!(rt_u256(U256::from(u64::MAX)), U256::from(u64::MAX));
    assert_eq!(rt_u256(U256::MAX), U256::MAX);
}

#[test]
fn u256_wire_sizes() {
    // postcard wraps serialize_bytes as [varint(len)][bytes], adding 1 byte overhead.
    // Total: 1 (outer varint) + 1 (inner length N) + N (payload) = 2 + N bytes.
    assert_eq!(wire_len_u256(U256::ZERO), 2); // N=0
    assert_eq!(wire_len_u256(U256::from(255u64)), 3); // N=1
    assert_eq!(wire_len_u256(U256::from(1_000_000u64)), 5); // 0x0F4240 → N=3
    assert_eq!(wire_len_u256(U256::MAX), 34); // N=32; 1 byte more than raw U256
}

#[test]
fn i256_roundtrips() {
    assert_eq!(rt_i256(I256::ZERO), I256::ZERO);
    assert_eq!(
        rt_i256(I256::try_from(1i64).unwrap()),
        I256::try_from(1i64).unwrap()
    );
    assert_eq!(
        rt_i256(I256::try_from(-1i64).unwrap()),
        I256::try_from(-1i64).unwrap()
    );
    assert_eq!(
        rt_i256(I256::try_from(300_000i64).unwrap()),
        I256::try_from(300_000i64).unwrap()
    );
    assert_eq!(
        rt_i256(I256::try_from(-200_000i64).unwrap()),
        I256::try_from(-200_000i64).unwrap()
    );
    assert_eq!(rt_i256(I256::MAX), I256::MAX);
    assert_eq!(rt_i256(I256::MIN), I256::MIN);
}

#[test]
fn i256_sign_preserved() {
    let pos = I256::try_from(1_000_000i64).unwrap();
    let neg = I256::try_from(-1_000_000i64).unwrap();
    assert!(rt_i256(pos) > I256::ZERO);
    assert!(rt_i256(neg) < I256::ZERO);
}

fn withdrawal_of(sats: u64) -> IpcWithdrawal {
    IpcWithdrawal {
        amount: Amount::from_sat(sats),
        address: bitcoin::Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
            .unwrap(),
    }
}

fn unstake_of(source: &db::SubnetState, validator: usize, sats: u64) -> IpcUnstake {
    let validator = &source.committee.validators[validator];
    IpcUnstake {
        amount: Amount::from_sat(sats),
        address: validator.backup_address.clone(),
        pubkey: validator.pubkey,
    }
}

/// Builds the checkpoint that `build` returns for a source and a destination subnet, parses it
/// back and runs the reveal check of its batch transfer
fn assert_checkpoint_parses_back(
    build: impl FnOnce(&db::SubnetState, &db::SubnetState) -> IpcCheckpointSubnetMsg,
) {
    set_test_fvm_network();
    let db = create_test_db();
    let source = test_utils::generate_subnet(3);
    let destination = test_utils::generate_subnet(3);
    save_subnets(&db, &[&source, &destination]);
    let msg = build(&source, &destination);
    let tx = checkpoint_tx(&msg, &source);

    let parsed =
        IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &db.read_txn().unwrap(), &tx).unwrap();
    let withdrawals = |m: &IpcCheckpointSubnetMsg| {
        m.withdrawals
            .iter()
            .map(|w| (w.amount, w.address.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(withdrawals(&parsed), withdrawals(&msg));
    let unstakes = |m: &IpcCheckpointSubnetMsg| {
        m.unstakes
            .iter()
            .map(|u| (u.address.clone(), u.pubkey))
            .collect::<Vec<_>>()
    };
    assert_eq!(unstakes(&parsed), unstakes(&msg));
    // A kill checkpoint adds its remainder to the unstake outputs
    if !msg.is_kill_checkpoint {
        let amounts =
            |m: &IpcCheckpointSubnetMsg| m.unstakes.iter().map(|u| u.amount).collect::<Vec<_>>();
        assert_eq!(amounts(&parsed), amounts(&msg));
    }
    assert_eq!(parsed.change_address, msg.change_address);

    if msg.has_batch_data() {
        let reveal = reveal_of(&msg, &tx);
        let batch = IpcMessage::from_reveal_inputs(&reveal)
            .into_iter()
            .find_map(|(_, m)| match m {
                IpcMessage::BatchTransfer(batch) => Some(batch),
                _ => None,
            })
            .expect("a batch transfer in the reveal");
        batch
            .validate_for_checkpoint(&db, &db.read_txn().unwrap(), &tx)
            .unwrap();
    }
}

#[test]
fn a_checkpoint_with_withdrawals_transfers_and_unstakes_parses_back() {
    assert_checkpoint_parses_back(|source, destination| {
        let mut msg = checkpoint_msg(
            source,
            10,
            vec![withdrawal_of(10_000)],
            vec![transfer(destination, 10_000)],
        );
        // The validators of a generated subnet share one backup address
        msg.unstakes = vec![unstake_of(source, 0, 10_000), unstake_of(source, 0, 20_000)];
        msg
    });
}

#[test]
fn a_checkpoint_with_withdrawals_and_transfers_parses_back() {
    assert_checkpoint_parses_back(|source, destination| {
        checkpoint_msg(
            source,
            10,
            vec![withdrawal_of(10_000)],
            vec![transfer(destination, 10_000)],
        )
    });
}

#[test]
fn a_checkpoint_with_erc20_records_and_unstakes_parses_back() {
    assert_checkpoint_parses_back(|source, _| {
        let mut msg = checkpoint_msg(source, 10, vec![withdrawal_of(10_000)], vec![]);
        msg.token_registrations = vec![test_utils::create_rand_erc_token_registration()];
        msg.unstakes = vec![unstake_of(source, 0, 10_000)];
        msg
    });
}

#[test]
fn a_checkpoint_with_erc20_records_parses_back() {
    assert_checkpoint_parses_back(|source, _| {
        let mut msg = checkpoint_msg(source, 10, vec![withdrawal_of(10_000)], vec![]);
        msg.token_registrations = vec![test_utils::create_rand_erc_token_registration()];
        msg
    });
}

#[test]
fn a_checkpoint_with_slash_signatures_and_unstakes_parses_back() {
    assert_checkpoint_parses_back(|source, _| {
        let mut msg = checkpoint_msg(source, 10, vec![withdrawal_of(10_000)], vec![]);
        msg.slash_signatures = vec![SlashSignatureEntry {
            txid: [1u8; 32],
            vout: 1,
            signatures: vec![],
        }];
        msg.unstakes = vec![unstake_of(source, 0, 10_000)];
        msg
    });
}

#[test]
fn a_checkpoint_with_slash_signatures_parses_back() {
    assert_checkpoint_parses_back(|source, _| {
        let mut msg = checkpoint_msg(source, 10, vec![withdrawal_of(10_000)], vec![]);
        msg.slash_signatures = vec![SlashSignatureEntry {
            txid: [1u8; 32],
            vout: 1,
            signatures: vec![],
        }];
        msg
    });
}

#[test]
fn a_kill_checkpoint_with_erc20_records_and_unstakes_parses_back() {
    assert_checkpoint_parses_back(|source, _| {
        let mut msg = checkpoint_msg(source, 10, vec![], vec![]);
        msg.token_registrations = vec![test_utils::create_rand_erc_token_registration()];
        msg.unstakes = vec![unstake_of(source, 0, 10_000)];
        msg.is_kill_checkpoint = true;
        msg.change_address = None;
        msg
    });
}

/// The batch transfer in the reveal transaction, as the monitor reads it
fn batch_of(reveal: &Transaction) -> IpcBatchTransferMsg {
    IpcMessage::from_reveal_inputs(reveal)
        .into_iter()
        .find_map(|(_, m)| match m {
            IpcMessage::BatchTransfer(batch) => Some(batch),
            _ => None,
        })
        .expect("a batch transfer in the reveal")
}

/// Transfers of 400 sat to `count` distinct addresses of the destination
fn transfers_to(destination: &db::SubnetState, count: u16) -> Vec<IpcCrossSubnetTransfer> {
    (1..=count)
        .map(|i| IpcCrossSubnetTransfer {
            subnet_user_address: alloy_primitives::Address::left_padding_from(&i.to_be_bytes()),
            ..transfer(destination, 400)
        })
        .collect()
}

#[test]
fn one_batch_with_more_than_255_transfers_is_valid() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let destination = test_utils::generate_subnet(3);
    save_subnets(&db, &[&source, &destination]);

    let msg = checkpoint_msg(&source, 1, vec![], transfers_to(&destination, 300));
    msg.validate().expect("One batch must be valid");
    let tx = checkpoint_tx(&msg, &source);
    let (_, batch_count, _, _) =
        IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&tx.output[0]).unwrap();
    assert_eq!(batch_count, 1);
    assert_eq!(
        tx.output.len(),
        4,
        "OP_RETURN, commitment, one transfer output, change"
    );
    assert_eq!(
        tx.output[msg.checkpoint_outputs().transfer(0)].value,
        Amount::from_sat(400 * 300)
    );

    let parsed =
        IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &db.read_txn().unwrap(), &tx).unwrap();
    assert!(parsed.change_address.is_some());
    let batch = batch_of(&reveal_of(&msg, &tx));
    batch
        .validate_for_checkpoint(&db, &db.read_txn().unwrap(), &tx)
        .unwrap();
    assert_eq!(batch.transfers.len(), 300);
}

#[test]
fn a_checkpoint_has_at_most_255_destination_subnets() {
    let source = test_utils::generate_subnet(3);
    let template = transfer(&source, 1_000);
    let to_destinations = |count: u16| -> Vec<IpcCrossSubnetTransfer> {
        (0..count)
            .map(|i| {
                let mut txid20 = [0u8; 20];
                txid20[..2].copy_from_slice(&i.to_be_bytes());
                IpcCrossSubnetTransfer {
                    destination_subnet_id: SubnetId::from_txid20(
                        &Txid20::from_slice(&txid20).unwrap(),
                    ),
                    ..template.clone()
                }
            })
            .collect()
    };

    let mut msg = checkpoint_msg(&source, 1, vec![], to_destinations(255));
    msg.validate()
        .expect("255 destination subnets must be valid");
    let (_, batch_count, _, _) = IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(
        &msg.make_metadata_tx_out(DEFAULT_BTC_FEE_RATE),
    )
    .unwrap();
    assert_eq!(batch_count, 255);

    msg.transfers = to_destinations(256);
    assert!(msg.validate().is_err());
}

#[test]
fn a_kill_checkpoint_with_one_batch_of_three_transfers_gives_its_change_to_the_unstakes() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let destination = test_utils::generate_subnet(3);
    save_subnets(&db, &[&source, &destination]);

    let mut msg = checkpoint_msg(&source, 1, vec![], transfers_to(&destination, 3));
    msg.is_kill_checkpoint = true;
    msg.change_address = None;
    msg.unstakes = vec![unstake_of(&source, 0, 40_000)];

    // The unstake output takes the change
    let tx = checkpoint_tx(&msg, &source);
    assert_eq!(
        tx.output.len(),
        4,
        "OP_RETURN, commitment, one transfer output, unstake"
    );
    let parsed =
        IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &db.read_txn().unwrap(), &tx).unwrap();
    assert!(parsed.is_kill_checkpoint);
    assert!(parsed.change_address.is_none());

    // Without unstakes the change output stays, and the kill checkpoint is invalid
    msg.unstakes.clear();
    let tx = checkpoint_tx(&msg, &source);
    assert_eq!(
        tx.output.len(),
        4,
        "OP_RETURN, commitment, one transfer output, change"
    );
    let parsed =
        IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &db.read_txn().unwrap(), &tx).unwrap();
    assert_eq!(
        parsed.change_address,
        Some(source.committee.multisig_address.clone())
    );
    assert!(parsed.validate().is_err());
}

#[test]
fn the_reveal_spends_the_commitment_and_is_the_same_with_several_destinations() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let destinations: Vec<_> = (0..5).map(|_| test_utils::generate_subnet(3)).collect();
    save_subnets(&db, &[&source]);
    save_subnets(&db, &destinations.iter().collect::<Vec<_>>());

    let transfers = destinations.iter().map(|d| transfer(d, 10_000)).collect();
    let msg = checkpoint_msg(&source, 1, vec![], transfers);

    // Every construction builds the same checkpoint and reveal transactions
    let committee = &source.committee;
    let utxos = create_test_utxos(committee.address_checked().script_pubkey());
    let build = || {
        msg.to_checkpoint_psbt(committee, committee, DEFAULT_BTC_FEE_RATE, &utxos)
            .unwrap()
            .unsigned_tx
    };
    let build_reveal = |tx: &Transaction| {
        msg.make_reveal_batch_transfer_tx(
            tx.compute_txid(),
            DEFAULT_BTC_FEE_RATE,
            &committee.address_checked(),
        )
        .unwrap()
        .unwrap()
    };
    let tx = build();
    let reveal = build_reveal(&tx);
    for _ in 0..10 {
        assert_eq!(build(), tx);
        assert_eq!(build_reveal(&tx), reveal);
    }

    // The reveal transaction spends the commitment output with its data leaf
    let input = &reveal.input[0];
    assert_eq!(input.previous_output.txid, tx.compute_txid());
    let spent = &tx.output[input.previous_output.vout as usize].script_pubkey;
    assert!(spent.is_p2tr());
    let output_key = XOnlyPublicKey::from_slice(&spent.as_bytes()[2..34]).unwrap();
    let control_block =
        bitcoin::taproot::ControlBlock::decode(input.witness.last().unwrap()).unwrap();
    assert!(control_block.verify_taproot_commitment(
        &Secp256k1::verification_only(),
        output_key,
        input.witness.tapscript().expect("a script path spend"),
    ));

    // It is accepted as the batch of its checkpoint
    IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &db.read_txn().unwrap(), &tx).unwrap();
    let batch = batch_of(&reveal);
    batch
        .validate_for_checkpoint(&db, &db.read_txn().unwrap(), &tx)
        .unwrap();
    assert_eq!(batch.transfers.len(), msg.transfers.len());
}

/// Records the checkpoint of `source` at subnet height `height` with one transfer to
/// `destination`, returning its transaction and reveal transaction
fn record_checkpoint_with_transfer(
    db: &db::HeedDb,
    source: &db::SubnetState,
    destination: &db::SubnetState,
    height: u64,
    block_height: u64,
) -> (Transaction, Transaction) {
    let msg = checkpoint_msg(source, height, vec![], vec![transfer(destination, 30_000)]);
    let tx = checkpoint_tx(&msg, source);
    let mut txn = db.write_txn().unwrap();
    db.save_transaction(&mut txn, &tx).unwrap();
    msg.save_to_db(db, &mut txn, block_height, block_hash(), tx.compute_txid())
        .unwrap();
    txn.commit().unwrap();
    let reveal = reveal_of(&msg, &tx);
    (tx, reveal)
}

/// Applies the reveal transaction to its checkpoint record
fn apply_reveal(db: &db::HeedDb, reveal: &Transaction, block_height: u64) {
    let mut batch = batch_of(reveal);
    let mut txn = db.write_txn().unwrap();
    batch
        .process(
            db,
            &mut txn,
            block_height,
            block_hash(),
            reveal.compute_txid(),
        )
        .unwrap();
    txn.commit().unwrap();
}

/// Asserts that the checkpoint records are stored under their own numbers, each with its reveal
/// transaction and the height of the Bitcoin block of that reveal transaction, and that no other
/// record follows them
fn assert_checkpoint_records(
    db: &db::HeedDb,
    subnet_id: SubnetId,
    expected: &[(&Transaction, &Transaction, u64)],
) {
    for (number, (tx, reveal, reveal_height)) in expected.iter().enumerate() {
        let number = number as u64;
        let record = db.get_checkpoint(subnet_id, number).unwrap().unwrap();
        assert_eq!(record.checkpoint_number, number);
        assert_eq!(record.txid, tx.compute_txid());
        assert_eq!(record.batch_transfer_txid, Some(reveal.compute_txid()));
        assert_eq!(record.batch_transfer_block_height, Some(*reveal_height));
    }
    let next_number = expected.len() as u64;
    assert!(db.get_checkpoint(subnet_id, next_number).unwrap().is_none());
    let subnet_state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(subnet_state.last_checkpoint_number, Some(next_number - 1));
}

#[test]
fn a_reveal_updates_its_checkpoint_record() {
    set_test_fvm_network();
    // The reveal transaction in the Bitcoin block of its checkpoint, and in a later one
    for reveal_height in [100, 101] {
        let db = test_utils::create_test_db();
        let source = test_utils::generate_subnet(3);
        let destination = test_utils::generate_subnet(3);
        save_subnets(&db, &[&source, &destination]);

        let (c0, r0) = record_checkpoint_with_transfer(&db, &source, &destination, 50, 100);
        apply_reveal(&db, &r0, reveal_height);
        assert_checkpoint_records(&db, source.id, &[(&c0, &r0, reveal_height)]);
    }
}

#[test]
fn reveals_after_a_later_checkpoint_update_their_own_checkpoint_records() {
    set_test_fvm_network();
    let db = test_utils::create_test_db();
    let source = test_utils::generate_subnet(3);
    let destination = test_utils::generate_subnet(3);
    save_subnets(&db, &[&source, &destination]);

    let (c0, r0) = record_checkpoint_with_transfer(&db, &source, &destination, 50, 100);
    let (c1, r1) = record_checkpoint_with_transfer(&db, &source, &destination, 60, 101);
    apply_reveal(&db, &r0, 102);
    apply_reveal(&db, &r1, 102);
    assert_checkpoint_records(&db, source.id, &[(&c0, &r0, 102), (&c1, &r1, 102)]);
}
