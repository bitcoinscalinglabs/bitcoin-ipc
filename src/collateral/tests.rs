use super::*;
use crate::bitcoin_utils::make_push_data_script;
use crate::db::{DatabaseCore, HeedDb, StakingChange};
use crate::ipc_lib::{
    IpcCreateSubnetMsg, IpcJoinSubnetMsg, IpcUnstakeCollateralMsg, IPC_JOIN_UE_TAG,
};
use crate::test_utils::generate_keypairs;
use crate::test_utils::{address_at, arb_amount, arb_keypair, arb_xonly};
use crate::test_utils::{create_test_db, generate_subnet_id};
use crate::{SubnetId, NETWORK};
use bitcoin::consensus::encode;
use bitcoin::secp256k1::SecretKey;
use bitcoin::taproot::TapLeafHash;
use bitcoin::{absolute, Txid};
use fvm_shared::address::{set_current_network, Network as FvmNetwork};
use proptest::prelude::*;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::str::FromStr;

/// The subnet id string form depends on this process-global; every test sets it
fn setup() -> Secp256k1<secp256k1::All> {
    set_current_network(FvmNetwork::Testnet);
    Secp256k1::new()
}

/// Verifies with libbitcoinconsensus that input `idx` of `tx` spends `spent[idx]`.
fn verify_input(
    tx: &Transaction,
    spent: &[TxOut],
    idx: usize,
) -> Result<(), bitcoinconsensus::Error> {
    let serialized_tx = encode::serialize(tx);
    let utxos: Vec<_> = spent
        .iter()
        .map(|o| bitcoinconsensus::Utxo {
            script_pubkey: o.script_pubkey.as_bytes().as_ptr(),
            script_pubkey_len: o.script_pubkey.len() as u32,
            value: o.value.to_sat() as i64,
        })
        .collect();
    bitcoinconsensus::verify_with_flags(
        spent[idx].script_pubkey.as_bytes(),
        spent[idx].value.to_sat(),
        &serialized_tx,
        Some(&utxos),
        idx,
        bitcoinconsensus::VERIFY_ALL_PRE_TAPROOT | bitcoinconsensus::VERIFY_TAPROOT,
    )
}

/// Verifies with libbitcoinconsensus that `tx` spends `spent`, one per input.
fn verify_spend(tx: &Transaction, spent: &[TxOut]) -> Result<(), bitcoinconsensus::Error> {
    (0..spent.len()).try_for_each(|idx| verify_input(tx, spent, idx))
}

fn test_keypair() -> Keypair {
    let secp = Secp256k1::new();
    let sk = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    Keypair::from_secret_key(&secp, &sk)
}

fn test_join_msg(pubkey: XOnlyPublicKey) -> IpcJoinSubnetMsg {
    let create_txid =
        Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b").unwrap();
    IpcJoinSubnetMsg {
        subnet_id: SubnetId::from_txid(&create_txid),
        collateral: Amount::ZERO,
        ip: "127.0.0.1:8080".parse().unwrap(),
        backup_address: Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n").unwrap(),
        pubkey,
    }
}

fn signed_msg(keypair: &Keypair, timeout: u32) -> IpcJoinUeMsg {
    let secp = Secp256k1::new();
    let (pubkey, _) = keypair.x_only_public_key();
    IpcJoinUeMsg::new_signed(
        &secp,
        test_join_msg(pubkey),
        timeout,
        &keypair.secret_key(),
        &eots_key_of(&pubkey),
        test_commitment(),
    )
}

/// A validator's EOTS secret key, derived from its key so that every validator has its own
fn eots_key_of(pubkey: &XOnlyPublicKey) -> secp256k1::SecretKey {
    secp256k1::SecretKey::from_slice(&crate::bitcoin_utils::tagged_sha256(
        b"test/eots",
        &pubkey.serialize(),
    ))
    .unwrap()
}

fn test_commitment() -> crate::eots::EotsNonceCommitment {
    crate::eots::EotsNonceCommitment {
        start: 1,
        count: 10,
        root: [1u8; 32],
    }
}

/// Three fixed keys with power 1 and threshold 2
fn test_configuration() -> Configuration {
    let secp = Secp256k1::new();
    Configuration {
        keys: (11u8..14)
            .map(|seed| {
                let sk = secp256k1::SecretKey::from_slice(&[seed; 32]).unwrap();
                (sk.x_only_public_key(&secp).0, 1)
            })
            .collect(),
        threshold: 2,
    }
}

fn named() -> Vec<(u64, Configuration)> {
    vec![(WHITELIST_COMMITTEE_NUMBER, test_configuration())]
}

/// The join context of a subnet before activation, naming the test configuration: epochs
/// of ten blocks, timeouts of 1 to 1000 epochs
fn context() -> JoinContext {
    JoinContext {
        named: named(),
        epoch: 0,
        h_0: None,
        epoch_length: 10,
        min_timeout: 1,
        max_timeout: 1000,
    }
}

fn test_script(pubkey: &XOnlyPublicKey, timeout: u32) -> CollateralScript {
    let secp = Secp256k1::new();
    CollateralScript::new(
        *pubkey,
        eots_key_of(pubkey).x_only_public_key(&secp).0,
        timeout,
        test_configuration(),
    )
}

const TIMEOUT: u32 = 1_000;
const BLOCK: u64 = 100;
const COLLATERAL: Amount = Amount::from_sat(100_000);
/// One unit of power of the test subnets, the value of a `C_gen` member
const UNIT: Amount = Amount::from_sat(10_000);

/// The slash pre-signature entry of `record` by `signers`
fn presign_entry(
    db: &impl DatabaseCore,
    record: &UeCollateral,
    signers: &[&Keypair],
) -> crate::slash::SlashSignatureEntry {
    let secp = Secp256k1::new();
    let script = collateral_script_of(db, record).unwrap();
    let mut signatures = signers
        .iter()
        .map(|k| {
            crate::slash::sign_slash(&secp, &script, record.outpoint, record.value, k).unwrap()
        })
        .collect::<Vec<_>>();
    signatures.sort_by_key(|(i, _)| *i);
    crate::slash::SlashSignatureEntry::new(record.outpoint, signatures)
}

fn batch_with(
    subnet_id: SubnetId,
    entries: Vec<crate::slash::SlashSignatureEntry>,
) -> crate::ipc_lib::IpcBatchTransferMsg {
    crate::ipc_lib::IpcBatchTransferMsg {
        subnet_id,
        checkpoint_txid: Txid::from_slice(&[5u8; 32]).unwrap(),
        checkpoint_vout: 0,
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: entries,
    }
}

#[test]
fn a_join_after_activation_is_seated_at_the_next_cutoff_once_slashable() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(4);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1], pks[2]], 2);
    let block_hash = BlockHash::all_zeros();
    for key in &keys[..2] {
        let (checked, join_txid) = checked_join_in_db(&db, &secp, key, subnet_id, TIMEOUT);
        assert_eq!(checked.committee_number, WHITELIST_COMMITTEE_NUMBER);
        process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
    }
    let state = db
        .get_subnet_state(subnet_id)
        .unwrap()
        .expect("bootstrapped");
    assert_eq!(state.committee.validators.len(), 2);
    // The commitments of the joins are recorded
    let record0 = db.get_ue_collaterals(subnet_id).unwrap()[0].clone();
    let commitments = db
        .get_eots_commitments(subnet_id, &record0.pk_eots)
        .unwrap();
    assert_eq!(commitments.len(), 1);
    assert_eq!(commitments[0].commitment, test_commitment());
    activate(&db, subnet_id, BLOCK);

    // A third key joins after activation: recorded, naming committee 1, not seated
    let (checked, join_txid) = checked_join_in_db(&db, &secp, &keys[2], subnet_id, TIMEOUT);
    assert!(
        process_join_ue(&db, &checked, join_txid, BLOCK + 1, block_hash)
            .unwrap()
            .is_none()
    );
    let record = db
        .get_ue_collateral(subnet_id, join_txid, &pks[2])
        .unwrap()
        .unwrap();
    assert_eq!(record.committee_number, 1);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert!(state.waiting_committee.is_none());
    assert!(db.get_all_stake_changes(subnet_id).unwrap().is_empty());

    // One member of committee 1 does not reach its threshold: valid, not slashable
    let one = presign_entry(&db, &record, &[&keys[0]]);
    assert!(batch_with(subnet_id, vec![one])
        .newly_slashable(&db)
        .unwrap()
        .is_empty());
    // A signature by a key outside the configuration cannot be built
    let script = collateral_script_of(&db, &record).unwrap();
    assert!(
        crate::slash::sign_slash(&secp, &script, record.outpoint, record.value, &keys[3]).is_err()
    );
    // A corrupted signature fails the batch, as does an unknown output
    let mut bad = presign_entry(&db, &record, &[&keys[0], &keys[1]]);
    bad.signatures[0].1[1][0] ^= 1;
    assert!(batch_with(subnet_id, vec![bad])
        .newly_slashable(&db)
        .is_err());
    let mut unknown = presign_entry(&db, &record, &[&keys[0], &keys[1]]);
    unknown.vout = 7;
    assert!(batch_with(subnet_id, vec![unknown])
        .newly_slashable(&db)
        .is_err());
    // Disorder is rejected by validation
    let entry = presign_entry(&db, &record, &[&keys[0], &keys[1]]);
    assert!(batch_with(subnet_id, vec![entry.clone(), entry.clone()])
        .validate()
        .is_err());
    batch_with(subnet_id, vec![entry.clone()])
        .validate()
        .unwrap();

    // Both members reach the threshold: slashable
    let newly = batch_with(subnet_id, vec![entry.clone()])
        .newly_slashable(&db)
        .unwrap();
    assert_eq!(newly.len(), 1);
    assert_eq!(newly[0].0.outpoint, record.outpoint);
    make_slashable(&db, subnet_id, join_txid, &pks[2], BLOCK + 2);
    // A later entry for a slashable output changes nothing
    assert!(batch_with(subnet_id, vec![entry])
        .newly_slashable(&db)
        .unwrap()
        .is_empty());
    // A genesis member's collateral names the whitelist: two whitelisted keys suffice
    let genesis_entry = presign_entry(&db, &record0, &[&keys[1], &keys[2]]);
    assert_eq!(
        batch_with(subnet_id, vec![genesis_entry])
            .newly_slashable(&db)
            .unwrap()
            .len(),
        1
    );
    make_slashable(
        &db,
        subnet_id,
        record0.join_txid,
        &record0.pubkey,
        BLOCK + 2,
    );

    // The next cutoff seats the slashable collaterals with their values: the third key and
    // the genesis member whose collateral is slashable; the other genesis member leaves
    let outcomes = cutoff(&db, BLOCK + 10);
    assert_eq!(outcomes.len(), 1);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(state.epoch, 2);
    let next = state
        .waiting_committee
        .as_ref()
        .expect("a new configuration");
    assert!(next.is_validator(&pks[2]) && next.is_validator(&record0.pubkey));
    assert_eq!(next.validators.len(), 2);
    assert!(next.validators.iter().all(|v| v.collateral == COLLATERAL));
    // A deposit and a withdrawal of the genesis members, a join and a deposit of the third
    assert_eq!(db.get_all_stake_changes(subnet_id).unwrap().len(), 4);
}

/// A commit and reveal pair as the provider builds them, unsigned, and the data.
fn join_ue_txs(
    secp: &Secp256k1<secp256k1::All>,
    msg: &IpcJoinUeMsg,
) -> (Transaction, Transaction, Vec<u8>) {
    join_ue_txs_with(secp, msg, COLLATERAL)
}

fn join_ue_txs_with(
    secp: &Secp256k1<secp256k1::All>,
    msg: &IpcJoinUeMsg,
    collateral: Amount,
) -> (Transaction, Transaction, Vec<u8>) {
    unsigned_join_ue_txs(secp, msg, collateral, &test_configuration()).unwrap()
}

#[test]
fn leaf_script_and_script_pubkey_are_stable() {
    let secp = setup();
    let (pubkey, _) = test_keypair().x_only_public_key();
    let script = test_script(&pubkey, TIMEOUT);
    let leaf = script.exit_leaf();
    assert_eq!(
        leaf.to_string(),
        format!(
            "OP_PUSHBYTES_2 e803 OP_CLTV OP_DROP OP_PUSHBYTES_32 {} OP_CHECKSIG",
            pubkey
        )
    );
    // The slash leaf is `<pk_eots> OP_CHECKSIGVERIFY` followed by the multisig of the
    // named configuration, and the cooperative-exit leaf the same with the validator key
    let multisig = test_configuration().multisig_script().unwrap();
    assert_eq!(
        script.slash_leaf().unwrap().to_bytes(),
        [
            vec![0x20],
            script.pk_eots.serialize().to_vec(),
            vec![0xad],
            multisig.to_bytes()
        ]
        .concat()
    );
    assert_eq!(
        script.cooperative_exit_leaf().unwrap().to_bytes(),
        [
            vec![0x20],
            pubkey.serialize().to_vec(),
            vec![0xad],
            multisig.to_bytes()
        ]
        .concat()
    );
    let spk = script.script_pubkey(&secp).unwrap();
    assert!(spk.is_p2tr());
    // The exit leaf sits at depth 2 beside the slash leaf, under the cooperative-exit leaf:
    // its control block carries `TapLeaf(L) ‖ TapLeaf(X)`
    let control_block = script.control_block(&secp, &leaf).unwrap().serialize();
    assert_eq!(control_block.len(), 97);
    let slash_hash = script.slash_leaf().unwrap().tapscript_leaf_hash();
    let coop_hash = script
        .cooperative_exit_leaf()
        .unwrap()
        .tapscript_leaf_hash();
    assert_eq!(&control_block[33..65], slash_hash.as_byte_array());
    assert_eq!(&control_block[65..], coop_hash.as_byte_array());
    let coop_block = script
        .control_block(&secp, &script.cooperative_exit_leaf().unwrap())
        .unwrap()
        .serialize();
    assert_eq!(coop_block.len(), 65);
    // Fixed vectors for these keys and timeout, so that any change of the output is noticed
    assert_eq!(
        (
            spk.to_hex_string(),
            Address::from_script(&spk, NETWORK).unwrap().to_string(),
            hex::encode(&control_block[..33])
        ),
        (
            "512032d4ec1a2dd9fd389519c7f9791dfe85d1a3741cf6418dc273be25982c43afb2".to_string(),
            "bcrt1pxt2wcx3dm87n39gecluhj807shg6xaqu7eqcmsnnhcjestzr47eq89xfrl".to_string(),
            // Leaf version 0xc0 with even parity, then the unspendable internal key
            "c079be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81799".to_string()
        )
    );
    // Another timeout, key, EOTS key or configuration gives another output
    assert_ne!(
        spk,
        test_script(&pubkey, TIMEOUT + 1)
            .script_pubkey(&secp)
            .unwrap()
    );
    let (other, _) = generate_keypairs(1)[0].x_only_public_key();
    assert_ne!(
        spk,
        test_script(&other, TIMEOUT).script_pubkey(&secp).unwrap()
    );
    let mut other_eots = script.clone();
    other_eots.pk_eots = other;
    assert_ne!(spk, other_eots.script_pubkey(&secp).unwrap());
    let mut other_configuration = script.clone();
    other_configuration.configuration.threshold = 3;
    assert_ne!(spk, other_configuration.script_pubkey(&secp).unwrap());
    // Address round trip
    let address = Address::from_script(&spk, NETWORK).unwrap();
    assert_eq!(address.script_pubkey(), spk);
}

#[test]
fn check_join_ue_accepts_a_correct_join() {
    let secp = setup();
    let keypair = test_keypair();
    let msg = signed_msg(&keypair, TIMEOUT);
    let (commit, reveal, _) = join_ue_txs(&secp, &msg);
    let accepted = check_join_ue(&secp, &reveal, 0, &commit, BLOCK, &context()).unwrap();
    assert_eq!(accepted.outpoint, OutPoint::new(commit.compute_txid(), 1));
    assert_eq!(accepted.msg.join.collateral, COLLATERAL);
    assert_eq!(accepted.msg.join.pubkey, msg.join.pubkey);
    assert_eq!(accepted.msg.timeout, TIMEOUT);

    // A commit with only the two required outputs is accepted too
    let mut commit2 = commit.clone();
    commit2.output.pop();
    let mut reveal2 = reveal.clone();
    reveal2.input[0].previous_output = OutPoint::new(commit2.compute_txid(), 0);
    assert!(check_join_ue(&secp, &reveal2, 0, &commit2, BLOCK, &context()).is_ok());

    // A replaced reveal with other outputs and extra inputs is accepted
    let mut replaced = reveal.clone();
    replaced.output = vec![TxOut {
        value: Amount::from_sat(1),
        script_pubkey: ScriptBuf::new_op_return([9]),
    }];
    replaced.input.push(TxIn {
        previous_output: OutPoint::new(Txid::all_zeros(), 9),
        ..Default::default()
    });
    assert!(check_join_ue(&secp, &replaced, 0, &commit, BLOCK, &context()).is_ok());

    // The data input may be at any index: with an input prepended it is input 1
    let mut shifted = replaced.clone();
    shifted.input.insert(
        0,
        TxIn {
            previous_output: OutPoint::new(Txid::all_zeros(), 8),
            witness: Witness::from_slice(&[vec![0u8; 64]]),
            ..Default::default()
        },
    );
    assert!(check_join_ue(&secp, &shifted, 0, &commit, BLOCK, &context()).is_err());
    let accepted = check_join_ue(&secp, &shifted, 1, &commit, BLOCK, &context()).unwrap();
    assert_eq!(accepted.outpoint, OutPoint::new(commit.compute_txid(), 1));
    assert_eq!(accepted.msg.join.pubkey, msg.join.pubkey);
}

#[test]
fn check_join_ue_rejects_wrong_input_or_script() {
    let secp = setup();
    let keypair = test_keypair();
    let msg = signed_msg(&keypair, TIMEOUT);
    let (commit, reveal, data) = join_ue_txs(&secp, &msg);

    // Input 0 spends another output of the commit
    let mut bad = reveal.clone();
    bad.input[0].previous_output.vout = 1;
    assert!(check_join_ue(&secp, &bad, 0, &commit, BLOCK, &context()).is_err());

    // The data sits in input 1: input 0 carries no join, input 1 does, there is no input 2
    let mut shifted = reveal.clone();
    shifted.input.insert(
        0,
        TxIn {
            previous_output: OutPoint::new(Txid::all_zeros(), 9),
            ..Default::default()
        },
    );
    assert!(check_join_ue(&secp, &shifted, 0, &commit, BLOCK, &context()).is_err());
    assert!(check_join_ue(&secp, &shifted, 1, &commit, BLOCK, &context()).is_ok());
    assert!(check_join_ue(&secp, &shifted, 2, &commit, BLOCK, &context()).is_err());

    // The witness script carries the data but is not our data script
    let mut bad = reveal.clone();
    let other_script = Builder::new()
        .push_slice(crate::bitcoin_utils::convert_bytes_to_push_bytes(&data))
        .push_opcode(OP_DROP)
        .push_opcode(bitcoin::opcodes::OP_TRUE)
        .push_opcode(OP_DROP)
        .push_opcode(bitcoin::opcodes::OP_TRUE)
        .into_script();
    bad.input[0].witness = Witness::from_slice(&[other_script.to_bytes(), vec![0u8; 33]]);
    assert!(check_join_ue(&secp, &bad, 0, &commit, BLOCK, &context()).is_err());

    // Trailing bytes after the message in the leaf
    let mut longer = data.clone();
    longer.push(0);
    let mut bad = reveal.clone();
    bad.input[0].witness =
        Witness::from_slice(&[make_push_data_script(&longer).to_bytes(), vec![0u8; 33]]);
    assert!(check_join_ue(&secp, &bad, 0, &commit, BLOCK, &context()).is_err());

    // The data script is a stack element, but the executed leaf is another script
    let control_block = reveal.input[0].witness.nth(1).unwrap().to_vec();
    let other_leaf = Builder::new()
        .push_opcode(OP_DROP)
        .push_opcode(bitcoin::opcodes::OP_TRUE)
        .into_script();
    let mut bad = reveal.clone();
    bad.input[0].witness = Witness::from_slice(&[
        make_push_data_script(&data).to_bytes(),
        other_leaf.to_bytes(),
        control_block.clone(),
    ]);
    assert!(check_join_ue(&secp, &bad, 0, &commit, BLOCK, &context()).is_err());

    // A key-path spend or a two-leaf control block
    let mut bad = reveal.clone();
    bad.input[0].witness = Witness::from_slice(&[vec![0u8; 64]]);
    assert!(check_join_ue(&secp, &bad, 0, &commit, BLOCK, &context()).is_err());
    let mut bad = reveal.clone();
    bad.input[0].witness =
        Witness::from_slice(&[make_push_data_script(&data).to_bytes(), vec![0u8; 65]]);
    assert!(check_join_ue(&secp, &bad, 0, &commit, BLOCK, &context()).is_err());

    // An annex after the control block is allowed
    let mut annexed = reveal.clone();
    annexed.input[0].witness = Witness::from_slice(&[
        make_push_data_script(&data).to_bytes(),
        control_block,
        vec![0x50, 1, 2],
    ]);
    assert!(check_join_ue(&secp, &annexed, 0, &commit, BLOCK, &context()).is_ok());

    // Commit output 0 is not P2TR
    let mut bad_commit = commit.clone();
    bad_commit.output[0].script_pubkey = ScriptBuf::new_op_return([3]);
    let mut bad = reveal.clone();
    bad.input[0].previous_output = OutPoint::new(bad_commit.compute_txid(), 0);
    assert!(check_join_ue(&secp, &bad, 0, &bad_commit, BLOCK, &context()).is_err());
}

#[test]
fn check_join_ue_rejects_bad_signature() {
    let secp = setup();
    let keypair = test_keypair();
    let (pubkey, _) = keypair.x_only_public_key();

    // Signed by another key
    let other = generate_keypairs(1)[0];
    let msg = IpcJoinUeMsg::new_signed(
        &secp,
        test_join_msg(pubkey),
        TIMEOUT,
        &other.secret_key(),
        &eots_key_of(&other.x_only_public_key().0),
        test_commitment(),
    );
    let (commit, reveal, _) = join_ue_txs(&secp, &msg);
    assert!(check_join_ue(&secp, &reveal, 0, &commit, BLOCK, &context()).is_err());

    // Signed on the same fields with another timeout
    let mut msg = signed_msg(&keypair, TIMEOUT);
    msg.timeout = TIMEOUT + 1;
    let (commit, reveal, _) = join_ue_txs(&secp, &msg);
    assert!(check_join_ue(&secp, &reveal, 0, &commit, BLOCK, &context()).is_err());
}

#[test]
fn check_join_ue_rejects_bad_commit_outputs() {
    let secp = setup();
    let keypair = test_keypair();
    let (pubkey, _) = keypair.x_only_public_key();
    let msg = signed_msg(&keypair, TIMEOUT);
    let (commit, reveal, _) = join_ue_txs(&secp, &msg);

    let with_commit = |commit: Transaction| {
        let mut reveal = reveal.clone();
        reveal.input[0].previous_output = OutPoint::new(commit.compute_txid(), 0);
        check_join_ue(&secp, &reveal, 0, &commit, BLOCK, &context())
    };
    let p2tr = |info: TaprootSpendInfo| {
        ScriptBuf::new_p2tr(&secp, info.internal_key(), info.merkle_root())
    };

    // One output
    let mut bad = commit.clone();
    bad.output.truncate(1);
    assert!(with_commit(bad).is_err());

    // Four outputs
    let mut bad = commit.clone();
    bad.output.push(commit.output[2].clone());
    assert!(with_commit(bad).is_err());

    // Output 1 under another timeout
    let mut bad = commit.clone();
    bad.output[1].script_pubkey = test_script(&pubkey, TIMEOUT + 1)
        .script_pubkey(&secp)
        .unwrap();
    assert!(with_commit(bad).is_err());

    // Output 1 under another key
    let (other, _) = generate_keypairs(1)[0].x_only_public_key();
    let mut bad = commit.clone();
    bad.output[1].script_pubkey = test_script(&other, TIMEOUT).script_pubkey(&secp).unwrap();
    assert!(with_commit(bad).is_err());

    // Output 1 with a real internal key (a key path)
    let mut bad = commit.clone();
    bad.output[1].script_pubkey = p2tr(
        TaprootBuilder::new()
            .add_leaf(0, ue_leaf_script(&pubkey, TIMEOUT))
            .unwrap()
            .finalize(&secp, pubkey)
            .unwrap(),
    );
    assert!(with_commit(bad).is_err());

    // Output 1 with a second leaf
    let mut bad = commit.clone();
    bad.output[1].script_pubkey = p2tr(
        TaprootBuilder::new()
            .add_leaf(1, ue_leaf_script(&pubkey, TIMEOUT))
            .unwrap()
            .add_leaf(1, ue_leaf_script(&other, TIMEOUT))
            .unwrap()
            .finalize(&secp, unspenable_internal_key())
            .unwrap(),
    );
    assert!(with_commit(bad).is_err());

    // Output 1 with a non-minimal encoding of the timeout in the leaf
    let mut bad = commit.clone();
    let leaf = Builder::new()
        .push_slice([0xe8, 0x03, 0x00])
        .push_opcode(OP_CLTV)
        .push_opcode(OP_DROP)
        .push_x_only_key(&pubkey)
        .push_opcode(OP_CHECKSIG)
        .into_script();
    bad.output[1].script_pubkey = p2tr(
        TaprootBuilder::new()
            .add_leaf(0, leaf)
            .unwrap()
            .finalize(&secp, unspenable_internal_key())
            .unwrap(),
    );
    assert!(with_commit(bad).is_err());
}

#[test]
fn check_join_ue_rejects_bad_timeout() {
    let secp = setup();
    let keypair = test_keypair();
    let check = |timeout: u32| {
        let msg = signed_msg(&keypair, timeout);
        let (commit, reveal, _) = join_ue_txs(&secp, &msg);
        check_join_ue(&secp, &reveal, 0, &commit, BLOCK, &context())
    };

    // Rule 2.6 before activation at block 100, epochs of ten blocks and timeouts of 1 to
    // 1000 epochs: T is above h + 1 and in epochs 4 to 1003 from the block, 130 < T <= 10130
    assert!(check(BLOCK as u32 + 1).is_err());
    assert!(check(130).is_err());
    assert!(check(131).is_ok());
    assert!(check(10_130).is_ok());
    assert!(check(10_131).is_err());

    // T at the locktime threshold is a timestamp
    assert!(check(LOCKTIME_THRESHOLD).is_err());
}

#[test]
fn message_round_trip_and_tag() {
    let _secp = setup();
    let keypair = test_keypair();
    let msg = signed_msg(&keypair, TIMEOUT);
    let bytes = msg.to_bytes().unwrap();
    assert_eq!(&bytes[..6], IPC_JOIN_UE_TAG.as_bytes());
    let parsed = IpcJoinUeMsg::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.join.subnet_id, msg.join.subnet_id);
    assert_eq!(parsed.join.ip, msg.join.ip);
    assert_eq!(parsed.join.backup_address, msg.join.backup_address);
    assert_eq!(parsed.join.pubkey, msg.join.pubkey);
    assert_eq!(parsed.timeout, msg.timeout);
    assert_eq!(parsed.signature, msg.signature);
    assert_eq!(parsed.to_bytes().unwrap(), bytes);
    // The signature is the last 64 bytes; anything after or before it changes the message
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(IpcJoinUeMsg::from_bytes(&longer).is_err());
    let mut shorter = bytes.clone();
    shorter.pop();
    assert!(IpcJoinUeMsg::from_bytes(&shorter).is_err());
    let mut tampered = bytes.clone();
    tampered[bytes.len() - 65] ^= 1;
    assert!(IpcJoinUeMsg::from_bytes(&tampered).is_err());
    // A join message does not parse as a join-ue
    let join = test_join_msg(msg.join.pubkey).to_bytes().unwrap();
    assert!(IpcJoinUeMsg::from_bytes(&join).is_err());
}

fn exit_inputs(
    secp: &Secp256k1<secp256k1::All>,
    pubkey: &XOnlyPublicKey,
) -> (Vec<UeExitInput>, Vec<TxOut>) {
    let inputs = vec![
        UeExitInput {
            outpoint: OutPoint::new(Txid::all_zeros(), 1),
            value: COLLATERAL,
            script: test_script(pubkey, TIMEOUT),
        },
        UeExitInput {
            outpoint: OutPoint::new(Txid::from_slice(&[1u8; 32]).unwrap(), 1),
            value: COLLATERAL,
            script: test_script(pubkey, TIMEOUT - 10),
        },
    ];
    let prevouts = inputs
        .iter()
        .map(|i| TxOut {
            value: i.value,
            script_pubkey: i.script.script_pubkey(secp).unwrap(),
        })
        .collect();
    (inputs, prevouts)
}

#[test]
fn exit_tx_spends_matured_collaterals() {
    let secp = setup();
    let keypair = test_keypair();
    let (pubkey, _) = keypair.x_only_public_key();
    let (inputs, prevouts) = exit_inputs(&secp, &pubkey);
    let to = Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
        .unwrap()
        .require_network(NETWORK)
        .unwrap();
    let fee_rate = FeeRate::from_sat_per_vb(10).unwrap();

    let tx = build_ue_exit_tx(&secp, &inputs, &keypair, &to, fee_rate).unwrap();
    assert_eq!(tx.lock_time, LockTime::from_height(TIMEOUT).unwrap());
    assert!(tx
        .input
        .iter()
        .all(|i| i.sequence == Sequence::ENABLE_RBF_NO_LOCKTIME));
    assert_eq!(tx.output.len(), 1);
    assert_eq!(tx.output[0].script_pubkey, to.script_pubkey());
    let fee = COLLATERAL * 2 - tx.output[0].value;
    assert_eq!(fee, fee_rate.fee_wu(tx.weight()).unwrap());
    assert!(tx
        .input
        .iter()
        .all(|i| i.witness.len() == 3 && i.witness.nth(0).unwrap().len() == 64));
    verify_spend(&tx, &prevouts).expect("valid spend");

    // Re-signs every input of a modified transaction, so that only the script can fail
    let resign = |tx: &Transaction, keypair: &Keypair| {
        let mut tx = tx.clone();
        for index in 0..tx.input.len() {
            let leaf = ScriptBuf::from_bytes(tx.input[index].witness.nth(1).unwrap().to_vec());
            let sighash = SighashCache::new(&tx)
                .taproot_script_spend_signature_hash(
                    index,
                    &Prevouts::All(&prevouts),
                    leaf.tapscript_leaf_hash(),
                    TapSighashType::Default,
                )
                .unwrap();
            let sig = secp.sign_schnorr(&Message::from_digest(sighash.to_byte_array()), keypair);
            let mut witness = tx.input[index].witness.to_vec();
            witness[0] = sig.serialize().to_vec();
            tx.input[index].witness = Witness::from_slice(&witness);
        }
        tx
    };
    verify_spend(&resign(&tx, &keypair), &prevouts).expect("re-signed spend");

    // nLockTime below a timeout: CLTV fails
    let mut early = tx.clone();
    early.lock_time = LockTime::from_height(TIMEOUT - 1).unwrap();
    assert!(verify_spend(&resign(&early, &keypair), &prevouts).is_err());

    // nSequence final: CLTV fails
    let mut no_locktime = tx.clone();
    no_locktime.input[0].sequence = Sequence::MAX;
    assert!(verify_spend(&resign(&no_locktime, &keypair), &prevouts).is_err());

    // Another key's signature fails
    let other = generate_keypairs(1)[0];
    assert!(verify_spend(&resign(&tx, &other), &prevouts).is_err());

    // A fee above the collateral is refused
    let huge = FeeRate::from_sat_per_vb(10_000).unwrap();
    assert!(build_ue_exit_tx(&secp, &inputs, &keypair, &to, huge).is_err());
    assert!(build_ue_exit_tx(&secp, &[], &keypair, &to, fee_rate).is_err());
}

#[test]
fn exit_tx_refuses_a_payout_below_the_dust_limit() {
    let secp = setup();
    let keypair = test_keypair();
    let (inputs, _) = exit_inputs(&secp, &keypair.x_only_public_key().0);
    let to = Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
        .unwrap()
        .require_network(NETWORK)
        .unwrap();
    let weight = build_ue_exit_tx(&secp, &inputs, &keypair, &to, FeeRate::BROADCAST_MIN)
        .unwrap()
        .weight();
    let dust = to.script_pubkey().minimal_non_dust();
    // A fee rate whose fee leaves a payout below the dust limit
    let rate = FeeRate::from_sat_per_kwu(
        (COLLATERAL * 2 - dust + Amount::ONE_SAT).to_sat() * 1000 / weight.to_wu() + 1,
    );

    let err = build_ue_exit_tx(&secp, &inputs, &keypair, &to, rate).unwrap_err();
    assert!(err.to_string().contains("below the dust limit"), "{err}");
}

/// One reveal carrying the join-ue of every `(key, subnet, timeout)`, and the commits
fn merged_join_ue(
    secp: &Secp256k1<secp256k1::All>,
    joins: &[(&Keypair, SubnetId, u32)],
) -> (Vec<Transaction>, Transaction) {
    let with_eots: Vec<_> = joins
        .iter()
        .map(|(keypair, subnet_id, timeout)| {
            (
                *keypair,
                eots_key_of(&keypair.x_only_public_key().0),
                *subnet_id,
                *timeout,
            )
        })
        .collect();
    merged_join_ue_with(secp, &with_eots)
}

/// `merged_join_ue` with an EOTS key per join
fn merged_join_ue_with(
    secp: &Secp256k1<secp256k1::All>,
    joins: &[(&Keypair, secp256k1::SecretKey, SubnetId, u32)],
) -> (Vec<Transaction>, Transaction) {
    let mut commits = Vec::new();
    let mut reveals = Vec::new();
    for (keypair, eots_key, subnet_id, timeout) in joins {
        let (pubkey, _) = keypair.x_only_public_key();
        let mut join = test_join_msg(pubkey);
        join.subnet_id = *subnet_id;
        let msg = IpcJoinUeMsg::new_signed(
            secp,
            join,
            *timeout,
            &keypair.secret_key(),
            eots_key,
            test_commitment(),
        );
        let (commit, reveal, _) = join_ue_txs(secp, &msg);
        commits.push(commit);
        reveals.push(reveal);
    }
    (commits, crate::easy_tester::reveal::merge_reveals(reveals))
}

#[test]
fn two_join_ue_in_one_reveal_of_the_same_subnet() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(2);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();
    let (commits, reveal) = merged_join_ue(
        &secp,
        &[
            (&keys[0], subnet_id, TIMEOUT),
            (&keys[1], subnet_id, TIMEOUT + 10),
        ],
    );
    let join_txid = reveal.compute_txid();

    // Each input is one join, checked against the commit it spends
    let a = check_join_ue(&secp, &reveal, 0, &commits[0], BLOCK, &context()).unwrap();
    let b = check_join_ue(&secp, &reveal, 1, &commits[1], BLOCK, &context()).unwrap();
    assert!(check_join_ue(&secp, &reveal, 0, &commits[1], BLOCK, &context()).is_err());
    assert_ne!(a.outpoint, b.outpoint);
    assert!(process_join_ue(&db, &a, join_txid, BLOCK, block_hash)
        .unwrap()
        .is_none());
    let state = process_join_ue(&db, &b, join_txid, BLOCK, block_hash)
        .unwrap()
        .expect("the second join bootstraps");

    // Two records under the reveal txid, one per key, and both keys in the committee
    let mut records = db.get_ue_collaterals(subnet_id).unwrap();
    records.sort_by_key(|r| r.timeout);
    let summary: Vec<_> = records
        .iter()
        .map(|r| (r.pubkey, r.join_txid, r.outpoint, r.timeout))
        .collect();
    assert_eq!(
        summary,
        vec![
            (pks[0], join_txid, a.outpoint, TIMEOUT),
            (pks[1], join_txid, b.outpoint, TIMEOUT + 10)
        ]
    );
    let committee = state.latest_committee();
    assert_eq!(committee.validators.len(), 2);
    assert!(committee
        .validators
        .iter()
        .all(|v| v.join_txid == join_txid));
    assert!(is_ue_validator(&db, subnet_id, join_txid, &pks[0]).unwrap());
    assert!(is_ue_validator(&db, subnet_id, join_txid, &pks[1]).unwrap());
    let other = generate_keypairs(1)[0].x_only_public_key().0;
    assert!(!is_ue_validator(&db, subnet_id, join_txid, &other).unwrap());

    // Once slashable, key 0 leaves at the cutoff whose boundary reaches T, with the value
    // `C_gen` gave it; key 1 stays, and stays past its own timeout as the only member
    activate(&db, subnet_id, BLOCK);
    for pk in &pks {
        make_slashable(&db, subnet_id, join_txid, pk, BLOCK + 1);
    }
    let h_87 = BLOCK + 87 * 10;
    assert_eq!(cutoff(&db, h_87)[0].epoch, 88);
    assert_eq!(withdrawals(&db, subnet_id), vec![(pks[0], UNIT)]);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(state.latest_committee().pubkeys(), vec![pks[1]]);
    assert!(cutoff(&db, h_87 + 10)[0].new_committee.is_none());
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(state.latest_committee().pubkeys(), vec![pks[1]]);
    assert_eq!(withdrawals(&db, subnet_id), vec![(pks[0], UNIT)]);
}

#[test]
fn two_join_ue_in_one_reveal_of_different_subnets() {
    let secp = setup();
    let db = create_test_db();
    let keypair = test_keypair();
    let (pk, _) = keypair.x_only_public_key();
    let subnet_a = subnet_with_whitelist(&db, vec![pk], 1);
    let subnet_b = subnet_with_whitelist(&db, vec![pk], 1);
    let block_hash = BlockHash::all_zeros();

    // The same key joins two subnets in one reveal, with an EOTS key per subnet
    let eots_b = secp256k1::SecretKey::from_slice(&[9u8; 32]).unwrap();
    let (commits, reveal) = merged_join_ue_with(
        &secp,
        &[
            (&keypair, eots_key_of(&pk), subnet_a, TIMEOUT),
            (&keypair, eots_b, subnet_b, TIMEOUT),
        ],
    );
    let join_txid = reveal.compute_txid();
    let a = check_join_ue(&secp, &reveal, 0, &commits[0], BLOCK, &context()).unwrap();
    let b = check_join_ue(&secp, &reveal, 1, &commits[1], BLOCK, &context()).unwrap();
    assert_ne!(a.outpoint, b.outpoint);
    assert!(process_join_ue(&db, &a, join_txid, BLOCK, block_hash)
        .unwrap()
        .is_some());
    assert!(process_join_ue(&db, &b, join_txid, BLOCK, block_hash)
        .unwrap()
        .is_some());

    for (subnet_id, checked) in [(subnet_a, &a), (subnet_b, &b)] {
        let records = db.get_ue_collaterals(subnet_id).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            (records[0].pubkey, records[0].join_txid, records[0].outpoint),
            (pk, join_txid, checked.outpoint)
        );
        assert!(is_ue_validator(&db, subnet_id, join_txid, &pk).unwrap());
        let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        assert_eq!(state.latest_committee().validators.len(), 1);
    }

    // Past its timeout the key stays in each subnet, each handled by the same cutoff: a
    // configuration needs a key
    for subnet_id in [subnet_a, subnet_b] {
        activate(&db, subnet_id, BLOCK);
        make_slashable(&db, subnet_id, join_txid, &pk, BLOCK + 1);
    }
    let mut outcomes = cutoff(&db, BLOCK + 87 * 10);
    outcomes.sort_by_key(|o| o.subnet_id);
    let mut expected = vec![subnet_a, subnet_b];
    expected.sort();
    assert_eq!(
        outcomes.iter().map(|o| o.subnet_id).collect::<Vec<_>>(),
        expected
    );
    for subnet_id in [subnet_a, subnet_b] {
        let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        assert_eq!(state.latest_committee().pubkeys(), vec![pk]);
        assert!(withdrawals(&db, subnet_id).is_empty());
    }
}

#[test]
fn replayed_block_with_two_join_ue_in_one_reveal_gives_the_same_state() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(2);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();
    let (commits, reveal) = merged_join_ue(
        &secp,
        &[
            (&keys[0], subnet_id, TIMEOUT),
            (&keys[1], subnet_id, TIMEOUT),
        ],
    );
    let join_txid = reveal.compute_txid();
    let joins: Vec<_> = (0..2)
        .map(|i| check_join_ue(&secp, &reveal, i, &commits[i], BLOCK, &context()).unwrap())
        .collect();
    let apply = || {
        joins
            .iter()
            .map(|j| {
                process_join_ue(&db, j, join_txid, BLOCK, block_hash)
                    .unwrap()
                    .is_some()
            })
            .collect::<Vec<_>>()
    };
    let snapshot = || {
        let mut records = db.get_ue_collaterals(subnet_id).unwrap();
        records.sort_by_key(|r| r.pubkey.serialize());
        (
            format!("{:?}", db.get_subnet_state(subnet_id).unwrap()),
            format!("{:?}", db.get_subnet_genesis_info(subnet_id).unwrap()),
            records,
        )
    };

    assert_eq!(apply(), vec![false, true]);
    let after_first_run = snapshot();
    assert_eq!(after_first_run.2.len(), 2);

    // The block is processed again after a crash: both joins are no-ops
    assert_eq!(apply(), vec![false, false]);
    assert_eq!(snapshot(), after_first_run);
}

/// A subnet whose genesis info is in the DB, not bootstrapped
fn subnet_with_whitelist(
    db: &HeedDb,
    whitelist: Vec<XOnlyPublicKey>,
    min_validators: u16,
) -> SubnetId {
    let subnet_id = generate_subnet_id();
    let genesis = db::SubnetGenesisInfo {
        subnet_id,
        create_subnet_msg: IpcCreateSubnetMsg {
            min_validator_stake: Amount::from_sat(10_000),
            power_unit_exponent: 4,
            min_validators,
            bottomup_check_period: 10,
            active_validators_limit: 10,
            min_cross_msg_fee: Amount::from_sat(1),
            whitelist,
            epoch_length: 10,
            min_timeout: 1,
            max_timeout: 1000,
            min_deposit: Amount::from_sat(330),
            kill_vote_period: 36,
            kill_delay_period: 5,
        },
        create_msg_block_height: 1,
        bootstrapped: false,
        genesis_block_height: None,
        genesis_validators: Vec::new(),
        genesis_balance_entries: Vec::new(),
    };
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_genesis_info(&mut wtxn, subnet_id, &genesis)
        .unwrap();
    wtxn.commit().unwrap();
    subnet_id
}

/// A checked join-ue of `keypair` in `subnet_id`, and its reveal txid
fn checked_join(
    secp: &Secp256k1<secp256k1::All>,
    keypair: &Keypair,
    subnet_id: SubnetId,
    timeout: u32,
) -> (CheckedJoinUe, Txid) {
    checked_join_with(secp, keypair, subnet_id, timeout, COLLATERAL)
}

fn checked_join_with(
    secp: &Secp256k1<secp256k1::All>,
    keypair: &Keypair,
    subnet_id: SubnetId,
    timeout: u32,
    collateral: Amount,
) -> (CheckedJoinUe, Txid) {
    let (pubkey, _) = keypair.x_only_public_key();
    let mut join = test_join_msg(pubkey);
    join.subnet_id = subnet_id;
    let msg = IpcJoinUeMsg::new_signed(
        secp,
        join,
        timeout,
        &keypair.secret_key(),
        &eots_key_of(&keypair.x_only_public_key().0),
        test_commitment(),
    );
    let (commit, reveal, _) = join_ue_txs_with(secp, &msg, collateral);
    let checked = check_join_ue(secp, &reveal, 0, &commit, BLOCK, &context()).unwrap();
    (checked, reveal.compute_txid())
}

/// Records the slash pre-signatures of the join's collateral, as the batch transfer that
/// carries them does: the collateral is slashable from then on
fn make_slashable(
    db: &HeedDb,
    subnet_id: SubnetId,
    join_txid: Txid,
    pubkey: &XOnlyPublicKey,
    height: u64,
) {
    let record = db
        .get_ue_collateral(subnet_id, join_txid, pubkey)
        .unwrap()
        .unwrap();
    let mut wtxn = db.write_txn().unwrap();
    db.add_slash_presigs(
        &mut wtxn,
        &db::SlashPresigRecord {
            subnet_id,
            outpoint: record.outpoint,
            signatures: vec![],
            block_height: height,
            batch_transfer_txid: Txid::all_zeros(),
        },
    )
    .unwrap();
    wtxn.commit().unwrap();
}

/// Activates a bootstrapped subnet at `h_0`, with `C_gen` as the configuration of epoch 1
fn activate(db: &HeedDb, subnet_id: SubnetId, h_0: u64) {
    let mut state = db
        .get_subnet_state(subnet_id)
        .unwrap()
        .expect("bootstrapped");
    state.h_0 = Some(h_0);
    state.epoch = 1;
    state.last_rotation_epoch = 1;
    let mut wtxn = db.write_txn().unwrap();
    db.save_committee(
        &mut wtxn,
        subnet_id,
        state.committee_number,
        &state.committee,
    )
    .unwrap();
    db.save_epoch_committee_number(&mut wtxn, subnet_id, 1, state.committee_number)
        .unwrap();
    db.save_subnet_state(&mut wtxn, subnet_id, &state).unwrap();
    wtxn.commit().unwrap();
}

/// The cutoff pass of block `height`
fn cutoff(db: &HeedDb, height: u64) -> Vec<crate::epochs::CutoffOutcome> {
    crate::epochs::process_cutoffs(db, height, BlockHash::all_zeros()).unwrap()
}

/// A join checked against the configurations the subnet's state names, as the monitor does
fn checked_join_in_db(
    db: &HeedDb,
    secp: &Secp256k1<secp256k1::All>,
    keypair: &Keypair,
    subnet_id: SubnetId,
    timeout: u32,
) -> (CheckedJoinUe, Txid) {
    let (pubkey, _) = keypair.x_only_public_key();
    let mut join = test_join_msg(pubkey);
    join.subnet_id = subnet_id;
    let msg = IpcJoinUeMsg::new_signed(
        secp,
        join,
        timeout,
        &keypair.secret_key(),
        &eots_key_of(&pubkey),
        test_commitment(),
    );
    let context = join_context(db, subnet_id).unwrap();
    let (commit, reveal, _) =
        unsigned_join_ue_txs(secp, &msg, COLLATERAL, &context.named[0].1).unwrap();
    let checked = check_join_ue(secp, &reveal, 0, &commit, BLOCK, &context).unwrap();
    (checked, reveal.compute_txid())
}

/// Adopts the waiting committee, as the rotation checkpoint of the epoch does
fn rotate(db: &HeedDb, subnet_id: SubnetId) {
    let mut state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    let next = state.waiting_committee.take().expect("a waiting committee");
    let epoch = state.epoch;
    let number = db
        .get_epoch_committee_number(subnet_id, epoch)
        .unwrap()
        .expect("a cutoff");
    state.adopt_committee(epoch, number, next);
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_state(&mut wtxn, subnet_id, &state).unwrap();
    wtxn.commit().unwrap();
}

fn withdrawals(db: &HeedDb, subnet_id: SubnetId) -> Vec<(XOnlyPublicKey, Amount)> {
    db.get_unconfirmed_stake_changes(subnet_id)
        .unwrap()
        .iter()
        .filter_map(|sc| match sc.change {
            StakingChange::Withdraw { amount } => Some((sc.validator_xpk, amount)),
            _ => None,
        })
        .collect()
}

#[test]
fn multisig_unstake_rejects_a_ue_member() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(1);
    let pk = keys[0].x_only_public_key().0;
    let subnet_id = subnet_with_whitelist(&db, vec![pk], 1);
    let block_hash = BlockHash::all_zeros();
    let (checked, join_txid) = checked_join(&secp, &keys[0], subnet_id, TIMEOUT);
    process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
    let before = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(before.latest_committee().pubkeys(), vec![pk]);

    // A multisig unstake of a member that joined with join-ue is refused
    let unstake = IpcUnstakeCollateralMsg {
        subnet_id,
        amount: COLLATERAL,
        pubkey: Some(pk),
    };
    let err = unstake
        .save_to_db(&db, BLOCK + 1, block_hash, join_txid)
        .unwrap_err();
    assert!(matches!(err, IpcLibError::IpcValidateError(_)), "{err}");
    let after = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(after.latest_committee().pubkeys(), vec![pk]);
    assert!(withdrawals(&db, subnet_id).is_empty());
}

#[test]
fn multisig_unstake_of_the_only_member_withdraws_its_whole_collateral() {
    let db = create_test_db();
    let pk = generate_keypairs(1)[0].x_only_public_key().0;
    let subnet_id = subnet_with_whitelist(&db, vec![pk], 1);
    let block_hash = BlockHash::all_zeros();
    let mut join = test_join_msg(pk);
    join.subnet_id = subnet_id;
    join.collateral = COLLATERAL;
    join.save_to_db(&db, BLOCK, block_hash, Txid::from_byte_array([0x61; 32]))
        .unwrap();
    // A configuration after the first rotation, holding the join collateral
    let mut state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    let mut member = state.committee.validators[0].clone();
    member.collateral = COLLATERAL;
    member.power = 10;
    state.committee = crate::db::SubnetValidators::to_committee(&vec![member], &subnet_id, 1);
    state.committee_number = 1;
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_state(&mut wtxn, subnet_id, &state).unwrap();
    wtxn.commit().unwrap();
    activate(&db, subnet_id, BLOCK);

    IpcUnstakeCollateralMsg {
        subnet_id,
        amount: COLLATERAL,
        pubkey: Some(pk),
    }
    .save_to_db(
        &db,
        BLOCK + 1,
        block_hash,
        Txid::from_byte_array([0x62; 32]),
    )
    .unwrap();

    let after = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert!(matches!(
        after.pending_multisig_changes.last(),
        Some(db::PendingMultisigChange::Unstake { pubkey, amount })
            if *pubkey == pk && *amount == COLLATERAL
    ));
}

#[test]
fn a_join_before_activation_counts_its_collateral_once_against_the_total_power_limit() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(1);
    let subnet_id = subnet_with_whitelist(&db, vec![keys[0].x_only_public_key().0], 10);
    // Power p with p <= MAX_TOTAL_POWER < 2p
    let power = u64::from(crate::multisig::MAX_TOTAL_POWER / 2 + 1);
    let collateral = Amount::from_sat(power * UNIT.to_sat());
    let (checked, join_txid) = checked_join_with(&secp, &keys[0], subnet_id, TIMEOUT, collateral);
    assert!(
        process_join_ue(&db, &checked, join_txid, BLOCK, BlockHash::all_zeros())
            .unwrap()
            .is_none()
    );
    assert_eq!(db.get_ue_collaterals(subnet_id).unwrap().len(), 1);
}

#[test]
fn process_join_ue_bootstraps_records_and_rejects_duplicates() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(3);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();

    // First join: not yet bootstrapped, record written
    let (checked, join_txid) = checked_join(&secp, &keys[0], subnet_id, TIMEOUT);
    assert!(process_join_ue(&db, &checked, join_txid, BLOCK, block_hash)
        .unwrap()
        .is_none());
    let record = db
        .get_ue_collateral(subnet_id, join_txid, &pks[0])
        .unwrap()
        .unwrap();
    assert_eq!(record.pubkey, pks[0]);
    assert_eq!(record.outpoint, checked.outpoint);
    assert_eq!(record.value, COLLATERAL);
    assert_eq!(record.timeout, TIMEOUT);
    assert!(!record.spent);
    assert!(is_ue_validator(&db, subnet_id, join_txid, &pks[0]).unwrap());
    assert!(!is_ue_validator(&db, subnet_id, join_txid, &pks[1]).unwrap());

    // The same key again, in another reveal: rejected, and only the first record stays
    let (dup, dup_txid) = checked_join(&secp, &keys[0], subnet_id, TIMEOUT);
    assert!(process_join_ue(&db, &dup, dup_txid, BLOCK, block_hash).is_err());
    assert!(db
        .get_ue_collateral(subnet_id, dup_txid, &pks[0])
        .unwrap()
        .is_none());
    assert!(db
        .get_ue_collateral(subnet_id, join_txid, &pks[0])
        .unwrap()
        .is_some());

    // Second whitelisted key bootstraps the subnet
    let (checked2, join_txid2) = checked_join(&secp, &keys[1], subnet_id, TIMEOUT);
    let state = process_join_ue(&db, &checked2, join_txid2, BLOCK, block_hash)
        .unwrap()
        .expect("bootstrapped");
    assert_eq!(state.committee.validators.len(), 2);
    assert!(state
        .committee
        .validators
        .iter()
        .any(|v| v.join_txid == join_txid2));
    assert_eq!(db.get_ue_collaterals(subnet_id).unwrap().len(), 2);

    // After activation a third key joins; the next cutoff seats it once slashable
    activate(&db, subnet_id, BLOCK);
    let (checked3, join_txid3) = checked_join(&secp, &keys[2], subnet_id, TIMEOUT + 500);
    assert!(
        process_join_ue(&db, &checked3, join_txid3, BLOCK + 1, block_hash)
            .unwrap()
            .is_none()
    );
    for (pk, txid) in [
        (pks[0], join_txid),
        (pks[1], join_txid2),
        (pks[2], join_txid3),
    ] {
        make_slashable(&db, subnet_id, txid, &pk, BLOCK + 1);
    }
    cutoff(&db, BLOCK + 10);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert!(state.latest_committee().is_validator(&pks[2]));

    // Reprocessing the same reveal (a restart) is a no-op: no new stake changes
    let changes = db.get_unconfirmed_stake_changes(subnet_id).unwrap().len();
    assert!(
        process_join_ue(&db, &checked3, join_txid3, BLOCK + 1, block_hash)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.get_unconfirmed_stake_changes(subnet_id).unwrap().len(),
        changes
    );
    assert!(db
        .get_ue_collateral(subnet_id, join_txid3, &pks[2])
        .unwrap()
        .is_some());

    // A collateral below the subnet's minimum is rejected without a record
    let mut small = checked_join(&secp, &keys[2], subnet_id, TIMEOUT).0;
    small.msg.join.collateral = Amount::from_sat(9_999);
    let small_txid = Txid::from_slice(&[9u8; 32]).unwrap();
    assert!(process_join_ue(&db, &small, small_txid, BLOCK + 1, block_hash).is_err());
    assert!(db
        .get_ue_collateral(subnet_id, small_txid, &pks[2])
        .unwrap()
        .is_none());
}

#[test]
fn cutoffs_drop_the_collaterals_that_mature_within_three_epochs() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(3);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();
    let late = TIMEOUT + 500;
    for (key, timeout) in keys.iter().zip([TIMEOUT, TIMEOUT, late]) {
        if timeout == late {
            activate(&db, subnet_id, BLOCK);
        }
        let (checked, join_txid) = checked_join(&secp, key, subnet_id, timeout);
        process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
        make_slashable(&db, subnet_id, join_txid, &key.x_only_public_key().0, BLOCK);
    }
    assert!(withdrawals(&db, subnet_id).is_empty());

    // The cutoff whose boundary is below T seats all three with their collaterals
    let h_86 = BLOCK + 86 * 10;
    assert_eq!(cutoff(&db, h_86)[0].epoch, 87);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(state.latest_committee().validators.len(), 3);
    assert!(withdrawals(&db, subnet_id).is_empty());

    // The cutoff whose boundary reaches T drops both validators with that timeout, each
    // with its whole collateral
    cutoff(&db, h_86 + 10);
    let mut withdrawn = withdrawals(&db, subnet_id);
    withdrawn.sort();
    let mut expected = vec![(pks[0], COLLATERAL), (pks[1], COLLATERAL)];
    expected.sort();
    assert_eq!(withdrawn, expected);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(state.latest_committee().pubkeys(), vec![pks[2]]);

    // A dropped collateral is not withdrawn again
    assert!(cutoff(&db, h_86 + 20)[0].new_committee.is_none());
    assert_eq!(withdrawals(&db, subnet_id).len(), 2);

    // The only member stays past its timeout: a configuration needs a key
    assert!(cutoff(&db, BLOCK + 137 * 10)[0].new_committee.is_none());
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(state.latest_committee().pubkeys(), vec![pks[2]]);
    assert_eq!(withdrawals(&db, subnet_id).len(), 2);
}

#[test]
fn replayed_block_gives_the_same_state() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(3);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();
    for (key, timeout) in keys.iter().take(2).zip([TIMEOUT, TIMEOUT + 1000]) {
        let (checked, join_txid) = checked_join(&secp, key, subnet_id, timeout);
        process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
        make_slashable(&db, subnet_id, join_txid, &key.x_only_public_key().0, BLOCK);
    }
    activate(&db, subnet_id, BLOCK);

    // The cutoff block whose boundary reaches key 0's timeout, then a join-ue of a third key
    let h = BLOCK + 87 * 10;
    let (checked3, join_txid3) = checked_join(&secp, &keys[2], subnet_id, TIMEOUT + 2000);
    let run_block = || {
        cutoff(&db, h);
        process_join_ue(&db, &checked3, join_txid3, h, block_hash).unwrap()
    };
    run_block();
    let snapshot = |db: &HeedDb| {
        let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        (
            state.latest_committee().pubkeys(),
            db.get_unconfirmed_stake_changes(subnet_id).unwrap().len(),
            db.get_ue_collaterals(subnet_id).unwrap(),
        )
    };
    let first = snapshot(&db);
    assert_eq!(first.0, vec![pks[1]]);
    assert_eq!(first.1, 2, "one deposit, one withdrawal");

    // The same block again, as after a crash before the processed height was saved
    run_block();
    assert_eq!(snapshot(&db), first);
}

#[test]
fn activation_switch_reconciles_with_the_database() {
    let db = create_test_db();
    // Nothing processed yet: any value is accepted and recorded
    check_ue_activation(&db, None).unwrap();
    check_ue_activation(&db, Some(5)).unwrap();
    assert_eq!(db.get_ue_activation_height().unwrap(), Some(5));
    check_ue_activation(&db, Some(5)).unwrap();

    // Blocks up to 10 processed with the switch at 5: it cannot change
    db.set_last_processed_block(10).unwrap();
    check_ue_activation(&db, Some(5)).unwrap();
    assert!(check_ue_activation(&db, Some(7)).is_err());
    assert!(check_ue_activation(&db, Some(20)).is_err());
    assert!(check_ue_activation(&db, None).is_err());
    assert_eq!(db.get_ue_activation_height().unwrap(), Some(5));

    // A switch wholly in the future can be set, moved and unset (block 11 counts as processed)
    let db = create_test_db();
    db.set_last_processed_block(10).unwrap();
    assert!(check_ue_activation(&db, Some(10)).is_err());
    assert!(check_ue_activation(&db, Some(11)).is_err());
    check_ue_activation(&db, Some(12)).unwrap();
    check_ue_activation(&db, Some(30)).unwrap();
    check_ue_activation(&db, None).unwrap();
    assert_eq!(db.get_ue_activation_height().unwrap(), None);

    // A join-ue accepted in block 10 before a crash (recorded height 9): the switch is fixed
    let db = create_test_db();
    check_ue_activation(&db, Some(10)).unwrap();
    db.set_last_processed_block(9).unwrap();
    assert!(check_ue_activation(&db, None).is_err());
    assert!(check_ue_activation(&db, Some(11)).is_err());
    check_ue_activation(&db, Some(10)).unwrap();
    assert_eq!(db.get_ue_activation_height().unwrap(), Some(10));
}

#[test]
fn one_reveal_can_carry_a_ue_join_and_an_ordinary_join() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(3);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();
    for key in &keys[..2] {
        let (checked, join_txid) = checked_join(&secp, key, subnet_id, TIMEOUT + 500);
        process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
        make_slashable(&db, subnet_id, join_txid, &key.x_only_public_key().0, BLOCK);
    }
    activate(&db, subnet_id, BLOCK);

    // A replaced reveal carries the UE join of key 2 and an ordinary join of a fresh key
    let ordinary = generate_keypairs(1)[0].x_only_public_key().0;
    let (checked, join_txid) = checked_join(&secp, &keys[2], subnet_id, TIMEOUT + 100);
    process_join_ue(&db, &checked, join_txid, BLOCK + 1, block_hash).unwrap();
    make_slashable(&db, subnet_id, join_txid, &pks[2], BLOCK + 1);
    let mut plain = test_join_msg(ordinary);
    plain.subnet_id = subnet_id;
    plain.collateral = COLLATERAL;
    plain
        .save_to_db(&db, BLOCK + 1, block_hash, join_txid)
        .unwrap();

    assert!(is_ue_validator(&db, subnet_id, join_txid, &pks[2]).unwrap());
    assert!(!is_ue_validator(&db, subnet_id, join_txid, &ordinary).unwrap());
    // The next cutoff seats both
    cutoff(&db, BLOCK + 10);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    let committee = state.latest_committee();
    assert!(committee.is_validator(&pks[2]) && committee.is_validator(&ordinary));

    // The cutoff whose boundary reaches the UE member's timeout drops it; the ordinary
    // member, with no timeout, is carried
    cutoff(&db, BLOCK + 97 * 10);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert!(!state.latest_committee().is_validator(&pks[2]));
    assert!(state.latest_committee().is_validator(&ordinary));
    assert!(state.latest_committee().is_validator(&pks[0]));
}

#[test]
fn stake_and_unstake_of_a_ue_member_are_refused() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(2);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();
    let (a, txid_a) = checked_join(&secp, &keys[0], subnet_id, TIMEOUT);
    process_join_ue(&db, &a, txid_a, BLOCK, block_hash).unwrap();
    let (b, txid_b) = checked_join(&secp, &keys[1], subnet_id, TIMEOUT + 1000);
    process_join_ue(&db, &b, txid_b, BLOCK, block_hash).unwrap();
    activate(&db, subnet_id, BLOCK);
    make_slashable(&db, subnet_id, txid_a, &pks[0], BLOCK);
    make_slashable(&db, subnet_id, txid_b, &pks[1], BLOCK);
    let ordinary = generate_keypairs(1)[0].x_only_public_key().0;
    let mut plain = test_join_msg(ordinary);
    plain.subnet_id = subnet_id;
    plain.collateral = COLLATERAL;
    plain
        .save_to_db(
            &db,
            BLOCK + 1,
            block_hash,
            Txid::from_slice(&[3u8; 32]).unwrap(),
        )
        .unwrap();
    let outsider = generate_keypairs(1)[0].x_only_public_key().0;

    let err = refuse_ue_member(&db, subnet_id, &pks[0]).unwrap_err();
    assert!(
        matches!(&err, IpcLibError::IpcValidateError(e) if e.to_string().contains("unstakeue")),
        "{err}"
    );
    refuse_ue_member(&db, subnet_id, &ordinary).unwrap();
    refuse_ue_member(&db, subnet_id, &outsider).unwrap();

    // The cutoffs are not affected: the one whose boundary reaches A's timeout withdraws
    // its whole collateral
    cutoff(&db, BLOCK + 10);
    assert!(withdrawals(&db, subnet_id).is_empty());
    cutoff(&db, BLOCK + 87 * 10);
    assert_eq!(withdrawals(&db, subnet_id), vec![(pks[0], COLLATERAL)]);
    // A has left and is not a member: not refused
    refuse_ue_member(&db, subnet_id, &pks[0]).unwrap();
}

#[test]
fn a_key_that_left_cannot_join_again() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(2);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();
    for (key, timeout) in keys.iter().zip([TIMEOUT, TIMEOUT + 1000]) {
        let (checked, join_txid) = checked_join(&secp, key, subnet_id, timeout);
        process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
        make_slashable(&db, subnet_id, join_txid, &key.x_only_public_key().0, BLOCK);
    }
    activate(&db, subnet_id, BLOCK);

    // Key 0 leaves at the cutoff whose boundary reaches its timeout and is rotated out
    cutoff(&db, BLOCK + 87 * 10);
    rotate(&db, subnet_id);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert!(!state.latest_committee().is_validator(&pks[0]));

    // Its new join-ue is refused: a key joins a subnet once
    let (again, txid_again) = checked_join(&secp, &keys[0], subnet_id, TIMEOUT + 1000);
    let err = process_join_ue(&db, &again, txid_again, BLOCK + 20, block_hash).unwrap_err();
    assert!(err.to_string().contains("joins a subnet once"), "{err}");
    assert!(db
        .get_ue_collateral(subnet_id, txid_again, &pks[0])
        .unwrap()
        .is_none());
    assert_eq!(db.get_ue_collaterals(subnet_id).unwrap().len(), 2);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert!(!state.latest_committee().is_validator(&pks[0]));
    assert!(state.latest_committee().is_validator(&pks[1]));
}

#[test]
fn a_key_with_an_earlier_multisig_join_cannot_join_ue() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(3);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1], pks[2]], 2);
    let block_hash = BlockHash::all_zeros();
    let multisig_join = |pubkey: XOnlyPublicKey, block: u64, txid_byte: u8| {
        let mut join = test_join_msg(pubkey);
        join.subnet_id = subnet_id;
        join.collateral = COLLATERAL;
        join.save_to_db(
            &db,
            block,
            block_hash,
            Txid::from_slice(&[txid_byte; 32]).unwrap(),
        )
        .unwrap();
    };

    // Key 0 joins through the multisig before bootstrap, key 1 bootstraps with join-ue,
    // key 2 joins through the multisig after activation
    multisig_join(pks[0], BLOCK, 1);
    let (checked, join_txid) = checked_join(&secp, &keys[1], subnet_id, TIMEOUT);
    process_join_ue(&db, &checked, join_txid, BLOCK, block_hash)
        .unwrap()
        .expect("bootstrapped");
    activate(&db, subnet_id, BLOCK);
    make_slashable(&db, subnet_id, join_txid, &pks[1], BLOCK);
    multisig_join(pks[2], BLOCK + 1, 2);

    // The next cutoff seats key 2 and the rotation checkpoint adopts the configuration
    cutoff(&db, BLOCK + 10);
    rotate(&db, subnet_id);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(state.committee.validators.len(), 3);

    // Keys 0 and 2 unstake their whole collateral; the next cutoff drops them
    for (pk, txid_byte) in [(pks[0], 3u8), (pks[2], 4)] {
        let amount = state
            .committee
            .validators
            .iter()
            .find(|v| v.pubkey == pk)
            .unwrap()
            .collateral;
        IpcUnstakeCollateralMsg {
            subnet_id,
            amount,
            pubkey: Some(pk),
        }
        .save_to_db(
            &db,
            BLOCK + 15,
            block_hash,
            Txid::from_slice(&[txid_byte; 32]).unwrap(),
        )
        .unwrap();
    }
    cutoff(&db, BLOCK + 20);
    rotate(&db, subnet_id);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(state.latest_committee().pubkeys(), vec![pks[1]]);

    // Their join-ue is refused: the genesis join and the multisig join count
    for key in [&keys[0], &keys[2]] {
        let (again, txid_again) = checked_join(&secp, key, subnet_id, TIMEOUT + 1000);
        let err = process_join_ue(&db, &again, txid_again, BLOCK + 20, block_hash).unwrap_err();
        assert!(err.to_string().contains("joins a subnet once"), "{err}");
    }
    assert_eq!(db.get_ue_collaterals(subnet_id).unwrap().len(), 1);

    // The check the provider makes before the reveal exists
    assert!(refuse_earlier_join(&db, subnet_id, &pks[0], None).is_err());
    assert!(refuse_earlier_join(&db, subnet_id, &pks[1], None).is_err());
    assert!(refuse_earlier_join(&db, subnet_id, &pks[2], None).is_err());
    let fresh = generate_keypairs(1)[0].x_only_public_key().0;
    assert!(refuse_earlier_join(&db, subnet_id, &fresh, None).is_ok());
}

#[test]
fn cutoffs_without_a_rotation_checkpoint_continue_from_the_waiting_configuration() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(3);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();
    for (key, timeout) in keys.iter().zip([TIMEOUT, TIMEOUT, TIMEOUT + 500]) {
        if key.x_only_public_key().0 == pks[2] {
            activate(&db, subnet_id, BLOCK);
        }
        let (checked, join_txid) = checked_join(&secp, key, subnet_id, timeout);
        process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
        make_slashable(&db, subnet_id, join_txid, &key.x_only_public_key().0, BLOCK);
    }

    // The first cutoff computes C_2 from C_gen, committee 1
    let (number, committee) = cutoff(&db, BLOCK + 10)[0]
        .new_committee
        .clone()
        .expect("a new configuration");
    assert_eq!((number, committee.validators.len()), (2, 3));

    // No rotation checkpoint yet: the next cutoff computes C_3 from C_2, which has the same
    // members, so C_3 is C_2 with its number
    let outcomes = cutoff(&db, BLOCK + 20);
    assert_eq!(outcomes[0].epoch, 3);
    assert!(outcomes[0].new_committee.is_none());
    assert_eq!(
        db.get_epoch_committee_number(subnet_id, 3).unwrap(),
        Some(2)
    );
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!((state.epoch, state.last_rotation_epoch), (3, 1));
    assert_eq!(
        state.waiting_committee.as_ref().unwrap().validators.len(),
        3
    );

    // The rotation checkpoint of epoch 3 adopts it; the cutoff whose boundary reaches T
    // numbers the configuration of the two remaining keys 3
    rotate(&db, subnet_id);
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!((state.committee_number, state.last_rotation_epoch), (2, 3));
    let (number, committee) = cutoff(&db, BLOCK + 87 * 10)[0]
        .new_committee
        .clone()
        .expect("a new configuration");
    assert_eq!((number, committee.pubkeys()), (3, vec![pks[2]]));
}

/// A UE collateral record of `pubkey` with EOTS key `pk_eots`, as `process_join_ue` writes it
fn ue_record(
    subnet_id: SubnetId,
    pubkey: XOnlyPublicKey,
    pk_eots: XOnlyPublicKey,
    join_txid: Txid,
) -> UeCollateral {
    let join = test_join_msg(pubkey);
    UeCollateral {
        subnet_id,
        pubkey,
        outpoint: OutPoint::new(join_txid, 1),
        value: COLLATERAL,
        timeout: TIMEOUT,
        pk_eots,
        committee_number: 0,
        ip: join.ip,
        backup_address: join.backup_address,
        join_txid,
        spent: false,
    }
}

#[test]
fn an_eots_key_joins_once_across_subnets_and_within_a_reveal() {
    setup();
    let db = create_test_db();
    let pks: Vec<_> = generate_keypairs(3)
        .iter()
        .map(|k| k.x_only_public_key().0)
        .collect();
    let pk_eots = pks[2];
    let (subnet_a, subnet_b) = (generate_subnet_id(), generate_subnet_id());
    let txid = Txid::from_slice(&[1u8; 32]).unwrap();
    let other = Txid::from_slice(&[2u8; 32]).unwrap();
    let mut wtxn = db.write_txn().unwrap();
    db.add_ue_collateral(&mut wtxn, &ue_record(subnet_a, pks[0], pk_eots, txid))
        .unwrap();
    wtxn.commit().unwrap();

    // The join's own record is a replay
    assert!(refuse_earlier_eots_key(&db, subnet_a, &pks[0], &pk_eots, Some(txid)).is_ok());
    // Another input of the same reveal, a join in another subnet, the provider's check
    for (subnet_id, pubkey, join_txid) in [
        (subnet_a, pks[1], Some(txid)),
        (subnet_b, pks[0], Some(other)),
        (subnet_b, pks[1], Some(other)),
        (subnet_a, pks[0], None),
    ] {
        let err =
            refuse_earlier_eots_key(&db, subnet_id, &pubkey, &pk_eots, join_txid).unwrap_err();
        assert!(err.to_string().contains("an EOTS key joins once"), "{err}");
    }
    assert!(refuse_earlier_eots_key(&db, subnet_b, &pks[1], &pks[1], Some(other)).is_ok());
}

#[test]
fn a_zero_collateral_fails_the_subnet_checks_not_the_message_checks() {
    setup();
    let db = create_test_db();
    let pk = generate_keypairs(1)[0].x_only_public_key().0;
    let subnet_id = subnet_with_whitelist(&db, vec![pk], 1);
    let genesis = db.get_subnet_genesis_info(subnet_id).unwrap().unwrap();
    let mut join = test_join_msg(pk);
    join.subnet_id = subnet_id;
    assert!(join.validate().is_ok());
    let err = join.validate_for_subnet(&genesis, &None).unwrap_err();
    assert!(
        matches!(err, IpcValidateError::InvalidField("collateral", _)),
        "{err}"
    );
    join.collateral = COLLATERAL;
    assert!(join.validate_for_subnet(&genesis, &None).is_ok());
}

#[test]
fn malformed_messages_and_numeric_edges() {
    let secp = setup();
    let keypair = test_keypair();
    let (pubkey, _) = keypair.x_only_public_key();
    let msg = signed_msg(&keypair, TIMEOUT);
    let bytes = msg.to_bytes().unwrap();
    for n in 0..bytes.len() {
        assert!(
            IpcJoinUeMsg::from_bytes(&bytes[..n]).is_err(),
            "prefix of {n} bytes"
        );
    }

    // The block height at its maximum cannot be followed by a timeout
    let (commit, reveal, _) = join_ue_txs(&secp, &msg);
    assert!(check_join_ue(&secp, &reveal, 0, &commit, u64::MAX, &context()).is_err());

    // The exit leaf of each timeout around the script-number push sizes
    for (timeout, push) in [
        (1u32, &[0x51][..]),
        (16, &[0x60][..]),
        (17, &[0x01, 0x11][..]),
        (127, &[0x01, 0x7f][..]),
        (128, &[0x02, 0x80, 0x00][..]),
        (255, &[0x02, 0xff, 0x00][..]),
        (32767, &[0x02, 0xff, 0x7f][..]),
        (32768, &[0x03, 0x00, 0x80, 0x00][..]),
        (LOCKTIME_THRESHOLD - 1, &[0x04, 0xff, 0x64, 0xcd, 0x1d][..]),
    ] {
        let mut expected = push.to_vec();
        expected.extend([0xb1, 0x75, 0x20]); // OP_CLTV OP_DROP OP_PUSHBYTES_32
        expected.extend(pubkey.serialize());
        expected.push(0xac); // OP_CHECKSIG
        assert_eq!(
            ue_leaf_script(&pubkey, timeout).as_bytes(),
            expected,
            "{timeout}"
        );
    }
}

/// A private regtest bitcoind in a temporary directory, killed on drop.
struct Bitcoind {
    child: std::process::Child,
    _dir: tempfile::TempDir,
    pub client: bitcoincore_rpc::Client,
}

impl Drop for Bitcoind {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start_bitcoind() -> Bitcoind {
    use bitcoincore_rpc::{Auth, Client, RpcApi};
    let dir = tempfile::tempdir().unwrap();
    let (rpc_port, p2p_port) = (free_port(), free_port());
    let child = std::process::Command::new("bitcoind")
        .args([
            "-regtest",
            "-server=1",
            "-listen=0",
            "-txindex=1",
            "-acceptnonstdtxn=0",
            "-fallbackfee=0.0001",
            "-rpcuser=test",
            "-rpcpassword=test",
            &format!("-datadir={}", dir.path().display()),
            &format!("-rpcport={rpc_port}"),
            &format!("-port={p2p_port}"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("bitcoind in PATH");
    let auth = Auth::UserPass("test".into(), "test".into());
    let base = Client::new(&format!("http://127.0.0.1:{rpc_port}"), auth.clone()).unwrap();
    for _ in 0..100 {
        if base.get_block_count().is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    base.create_wallet("test", None, None, None, None).unwrap();
    let client = Client::new(&format!("http://127.0.0.1:{rpc_port}/wallet/test"), auth).unwrap();
    let mine_to = client.get_new_address(None, None).unwrap().assume_checked();
    client.generate_to_address(101, &mine_to).unwrap();
    Bitcoind {
        child,
        _dir: dir,
        client,
    }
}

/// Run with `cargo test --lib chain_join_ue -- --ignored`: needs bitcoind in PATH.
#[test]
#[ignore]
fn chain_join_ue_and_exit() {
    use bitcoincore_rpc::RpcApi;
    let secp = setup();
    let node = start_bitcoind();
    let rpc = &node.client;
    let keypair = test_keypair();
    let (pubkey, _) = keypair.x_only_public_key();
    let mine_to = rpc.get_new_address(None, None).unwrap().assume_checked();

    // join-ue with a timeout in the lowest epoch rule 2.6 allows: `min_timeout + 3`
    // epochs of ten blocks after the join's block, the tip's successor
    let tip = rpc.get_block_count().unwrap();
    let timeout = (tip + 1 + 4 * 10) as u32;
    let mut join = test_join_msg(pubkey);
    join.collateral = COLLATERAL;
    let msg = IpcJoinUeMsg::new_signed(
        &secp,
        join,
        timeout,
        &keypair.secret_key(),
        &eots_key_of(&keypair.x_only_public_key().0),
        test_commitment(),
    );
    let reveal_txid = msg.submit_to_bitcoin(rpc, &context()).unwrap();
    rpc.generate_to_address(1, &mine_to).unwrap();
    let join_height = rpc.get_block_count().unwrap();
    assert_eq!(join_height, tip + 1);

    // The monitor's view: the reveal and the commit it spends, from the chain
    let reveal = rpc.get_raw_transaction(&reveal_txid, None).unwrap();
    let commit_txid = reveal.input[0].previous_output.txid;
    let commit = rpc.get_raw_transaction(&commit_txid, None).unwrap();
    let accepted = check_join_ue(&secp, &reveal, 0, &commit, join_height, &context()).unwrap();
    assert_eq!(accepted.msg.join.collateral, COLLATERAL);
    assert_eq!(accepted.outpoint, OutPoint::new(commit_txid, 1));
    assert!(rpc
        .get_tx_out(&commit_txid, 1, Some(false))
        .unwrap()
        .is_some());
    assert_eq!(commit.output.len(), 3, "commitment, collateral, change");

    // Exit: refused below tip T, accepted from tip T, included in block T + 1
    let inputs = vec![UeExitInput {
        outpoint: accepted.outpoint,
        value: accepted.msg.join.collateral,
        script: accepted.msg.collateral_script(test_configuration()),
    }];
    let exit = build_ue_exit_tx(
        &secp,
        &inputs,
        &keypair,
        &mine_to,
        FeeRate::from_sat_per_vb(10).unwrap(),
    )
    .unwrap();
    while rpc.get_block_count().unwrap() < u64::from(timeout) - 1 {
        rpc.generate_to_address(1, &mine_to).unwrap();
    }
    let result = &rpc.test_mempool_accept(&[&exit]).unwrap()[0];
    assert!(!result.allowed, "accepted one block early: {:?}", result);
    assert!(result
        .reject_reason
        .as_deref()
        .unwrap_or("")
        .contains("non-final"));
    rpc.generate_to_address(1, &mine_to).unwrap();
    assert_eq!(rpc.get_block_count().unwrap(), u64::from(timeout));
    let result = &rpc.test_mempool_accept(&[&exit]).unwrap()[0];
    assert!(result.allowed, "refused at tip T: {:?}", result);
    let exit_txid = rpc.send_raw_transaction(&exit).unwrap();
    rpc.generate_to_address(1, &mine_to).unwrap();
    let block_hash = rpc.get_block_hash(u64::from(timeout) + 1).unwrap();
    let block = rpc.get_block(&block_hash).unwrap();
    assert!(block.txdata.iter().any(|tx| tx.compute_txid() == exit_txid));
    assert!(rpc
        .get_tx_out(&commit_txid, 1, Some(false))
        .unwrap()
        .is_none());
}

#[test]
fn activation_height_from_env() {
    std::env::remove_var(UE_ACTIVATION_HEIGHT_VAR);
    assert_eq!(ue_activation_height(), None);
    std::env::set_var(UE_ACTIVATION_HEIGHT_VAR, " 42 ");
    assert_eq!(ue_activation_height(), Some(42));
    std::env::set_var(UE_ACTIVATION_HEIGHT_VAR, "");
    assert_eq!(ue_activation_height(), None);
    std::env::remove_var(UE_ACTIVATION_HEIGHT_VAR);
}

/// Rule 2.6 with `K = 10`, `min_timeout = 1`, `max_timeout = 2`: `E_T` from the join's
/// block before activation, from `H_0` afterwards
#[test]
fn join_context_checks_the_timeout_epoch() {
    let context = |epoch, h_0| JoinContext {
        named: Vec::new(),
        epoch,
        h_0,
        epoch_length: 10,
        min_timeout: 1,
        max_timeout: 2,
    };
    // Before activation at block 100: epochs 4 to 5, so 130 < T <= 150
    let before = context(0, None);
    assert_eq!(before.timeout_epoch(131, 100), 4);
    assert!(before.check_timeout(130, 100).is_err());
    assert!(before.check_timeout(131, 100).is_ok());
    assert!(before.check_timeout(140, 100).is_ok());
    assert!(before.check_timeout(141, 100).is_ok());
    assert!(before.check_timeout(150, 100).is_ok());
    assert!(before.check_timeout(151, 100).is_err());
    // Epoch 2 with H_0 = 100, at block 125: epochs 6 to 7, so 150 < T <= 170
    let after = context(2, Some(100));
    assert_eq!(after.timeout_epoch(170, 125), 7);
    assert!(after.check_timeout(150, 125).is_err());
    assert!(after.check_timeout(151, 125).is_ok());
    assert!(after.check_timeout(170, 125).is_ok());
    assert!(after.check_timeout(171, 125).is_err());
    // T must be above block height + 1 whatever its epoch
    assert!(after.check_timeout(161, 160).is_err());
    assert!(after.check_timeout(162, 160).is_ok());
    // T must be a height
    let far = context(0, Some(LOCKTIME_THRESHOLD as u64 - 20));
    assert!(far
        .check_timeout(LOCKTIME_THRESHOLD, LOCKTIME_THRESHOLD as u64 - 20)
        .is_err());
}

/// A checkpoint carrying change number `number`, recorded by the monitor at block `height`
fn record_checkpoint(
    db: &HeedDb,
    subnet_id: SubnetId,
    is_rotation_checkpoint: bool,
    number: u64,
    height: u64,
) -> Result<crate::db::SubnetCheckpoint, IpcLibError> {
    let msg = crate::ipc_lib::IpcCheckpointSubnetMsg {
        subnet_id,
        checkpoint_hash: bitcoin::hashes::sha256::Hash::all_zeros(),
        checkpoint_height: height,
        next_committee_configuration_number: number,
        withdrawals: vec![],
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: vec![],
        unstakes: vec![],
        change_address: {
            let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
            Some(if is_rotation_checkpoint {
                state.latest_committee().multisig_address.clone()
            } else if state.last_rotation_epoch == 0 {
                let genesis_info = db.get_subnet_genesis_info(subnet_id).unwrap().unwrap();
                genesis_info.multisig_address().into_unchecked()
            } else {
                state.committee.multisig_address.clone()
            })
        },
        is_kill_checkpoint: false,
        is_rotation_checkpoint,
    };
    msg.save_to_db(
        db,
        height,
        BlockHash::all_zeros(),
        crate::test_utils::create_rand_txid(),
    )
}

fn unconfirmed_changes(db: &HeedDb, subnet_id: SubnetId) -> usize {
    db.get_unconfirmed_stake_changes(subnet_id).unwrap().len()
}

#[test]
fn a_rotation_checkpoint_carrying_the_previous_change_number_is_that_of_its_epoch() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(3);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
    let block_hash = BlockHash::all_zeros();
    for (key, timeout) in keys.iter().zip([TIMEOUT, TIMEOUT, TIMEOUT + 500]) {
        if key.x_only_public_key().0 == pks[2] {
            activate(&db, subnet_id, BLOCK);
        }
        let (checked, join_txid) = checked_join(&secp, key, subnet_id, timeout);
        process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
        make_slashable(&db, subnet_id, join_txid, &key.x_only_public_key().0, BLOCK);
    }
    // The cutoff H_1 releases the configuration updates of C_2
    let (number, c_2) = cutoff(&db, BLOCK + 10)[0]
        .new_committee
        .clone()
        .expect("a new configuration");
    let released = unconfirmed_changes(&db, subnet_id);
    assert!(released > 0);

    // The first checkpoint after it is the rotation checkpoint of epoch 2, though the subnet
    // has not applied them yet and carries the previous change number
    let rotation = record_checkpoint(&db, subnet_id, true, 0, BLOCK + 11).unwrap();
    assert!(rotation.is_rotation_checkpoint);
    let recorded = |cp: &crate::db::SubnetCheckpoint| {
        (
            cp.epoch,
            cp.signed_committee_number,
            cp.next_committee_number,
        )
    };
    assert_eq!(recorded(&rotation), (2, 1, number));
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    let adopted = (state.committee_number, state.last_rotation_epoch);
    assert_eq!(adopted, (number, 2));
    assert!(state.waiting_committee.is_none());
    assert_eq!(unconfirmed_changes(&db, subnet_id), released);

    // Epoch 2 has its rotation checkpoint
    assert!(record_checkpoint(&db, subnet_id, true, 0, BLOCK + 12).is_err());

    // The later checkpoint carrying the new change number is regular, signed by C_2
    let applied = c_2.configuration_number;
    let regular = record_checkpoint(&db, subnet_id, false, applied, BLOCK + 12).unwrap();
    assert!(!regular.is_rotation_checkpoint);
    assert_eq!(recorded(&regular), (2, number, number));
    let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!((state.committee_number, state.last_rotation_epoch), adopted);
    assert_eq!(unconfirmed_changes(&db, subnet_id), 0);
}

#[test]
fn the_rotation_checkpoint_of_epoch_1_with_c_1_equal_to_c_gen_leaves_c_wlist() {
    let secp = setup();
    let db = create_test_db();
    let keys = generate_keypairs(2);
    let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
    let subnet_id = subnet_with_whitelist(&db, pks, 2);
    let block_hash = BlockHash::all_zeros();
    for key in &keys {
        let (checked, join_txid) = checked_join(&secp, key, subnet_id, TIMEOUT);
        process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
        make_slashable(&db, subnet_id, join_txid, &key.x_only_public_key().0, BLOCK);
    }
    // Epoch 1 with C_1 = C_gen, before its rotation checkpoint
    activate(&db, subnet_id, BLOCK);
    let mut state = db.get_subnet_state(subnet_id).unwrap().unwrap();
    state.last_rotation_epoch = 0;
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_state(&mut wtxn, subnet_id, &state).unwrap();
    wtxn.commit().unwrap();

    // A regular checkpoint before it records `C_wlist`
    let before = record_checkpoint(&db, subnet_id, false, 0, BLOCK + 1).unwrap();
    assert!(!before.is_rotation_checkpoint);
    assert_eq!(before.next_committee_number, WHITELIST_COMMITTEE_NUMBER);

    // `C_wlist` signs it; it adopts C_1, which signs the next checkpoint
    let rotation = record_checkpoint(&db, subnet_id, true, 0, BLOCK + 2).unwrap();
    assert!(rotation.is_rotation_checkpoint);
    assert_eq!(rotation.epoch, 1);
    assert_eq!(rotation.signed_committee_number, WHITELIST_COMMITTEE_NUMBER);
    assert_eq!(rotation.next_committee_number, state.committee_number);
    let after = db.get_subnet_state(subnet_id).unwrap().unwrap();
    assert_eq!(after.last_rotation_epoch, 1);
    let regular = record_checkpoint(&db, subnet_id, false, 0, BLOCK + 3).unwrap();
    assert!(!regular.is_rotation_checkpoint);
    assert_eq!(regular.signed_committee_number, state.committee_number);
    assert_eq!(regular.next_committee_number, state.committee_number);
}

fn key(n: u8) -> Keypair {
    let mut bytes = [0; 32];
    bytes[31] = n;
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&bytes).unwrap())
}

/// A transaction spending the exit leaf of a single-leaf tree, signed by `key(signer)`, and its
/// prevouts
fn spend(
    timeout: u32,
    locktime: u32,
    sequence: u32,
    signer: u8,
    second_input: bool,
) -> (Transaction, Vec<TxOut>) {
    let secp = Secp256k1::new();
    let leaf = ue_leaf_script(&key(3).x_only_public_key().0, timeout);
    let internal_key = XOnlyPublicKey::from_slice(
        &hex::decode("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81799").unwrap(),
    )
    .unwrap();
    let info = TaprootBuilder::new()
        .add_leaf(0, leaf.clone())
        .unwrap()
        .finalize(&secp, internal_key)
        .unwrap();
    let prevout = TxOut {
        value: Amount::from_sat(100_000),
        script_pubkey: ScriptBuf::new_p2tr_tweaked(info.output_key()),
    };
    let input = TxIn {
        previous_output: OutPoint::new(Txid::from_byte_array([42; 32]), 1),
        script_sig: ScriptBuf::new(),
        sequence: Sequence(sequence),
        witness: Witness::new(),
    };
    let mut tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::from_consensus(locktime),
        input: vec![input],
        output: vec![TxOut {
            value: Amount::from_sat(99_000),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    let mut prevouts = vec![prevout];
    if second_input {
        tx.input.push(TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([43; 32]), 1),
            script_sig: ScriptBuf::new(),
            sequence: Sequence(0xffff_fffd),
            witness: Witness::new(),
        });
        prevouts.push(TxOut {
            value: Amount::from_sat(5_000),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        });
    }
    let hash = SighashCache::new(&tx)
        .taproot_script_spend_signature_hash(
            0,
            &Prevouts::All(&prevouts),
            TapLeafHash::from_script(&leaf, LeafVersion::TapScript),
            TapSighashType::Default,
        )
        .unwrap();
    let sig =
        secp.sign_schnorr_no_aux_rand(&Message::from_digest(hash.to_byte_array()), &key(signer));
    let cb = info
        .control_block(&(leaf.clone(), LeafVersion::TapScript))
        .unwrap();
    tx.input[0].witness =
        Witness::from_slice(&[sig.serialize().to_vec(), leaf.into_bytes(), cb.serialize()]);
    (tx, prevouts)
}

/// Whether input 0 of `tx` verifies against `prevouts`
fn verifies(tx: &Transaction, prevouts: &[TxOut]) -> bool {
    verify_input(tx, prevouts, 0).is_ok()
}

#[test]
fn exit_leaf_accepts_the_exact_cltv_locktime() {
    let (tx, prevouts) = spend(128, 128, 0xffff_fffd, 3, false);
    assert!(verifies(&tx, &prevouts));
}

#[test]
fn exit_leaf_allows_a_later_block_height_locktime() {
    let (tx, prevouts) = spend(128, 129, 0xffff_fffd, 3, false);
    assert!(verifies(&tx, &prevouts));
}

#[test]
fn exit_leaf_rejects_a_timestamp_locktime_for_a_height_timeout() {
    let (tx, prevouts) = spend(128, 500_000_000, 0xffff_fffd, 3, false);
    assert!(!verifies(&tx, &prevouts));
}

#[test]
fn an_exit_signature_valid_for_another_output_value_is_rejected() {
    let (mut tx, prevouts) = spend(128, 128, 0xffff_fffd, 3, false);
    assert!(verifies(&tx, &prevouts));
    tx.output[0].value = Amount::from_sat(98_999);
    assert!(!verifies(&tx, &prevouts));
}

#[test]
fn an_exit_signature_valid_for_another_destination_is_rejected() {
    let (mut tx, prevouts) = spend(128, 128, 0xffff_fffd, 3, false);
    assert!(verifies(&tx, &prevouts));
    tx.output[0].script_pubkey = ScriptBuf::from_bytes(vec![0x00]);
    assert!(!verifies(&tx, &prevouts));
}

#[test]
fn exit_signature_commits_to_the_other_inputs_amount() {
    let (tx, mut prevouts) = spend(128, 128, 0xffff_fffd, 3, true);
    assert!(verifies(&tx, &prevouts));
    prevouts[1].value = Amount::from_sat(5_001);
    assert!(!verifies(&tx, &prevouts));
}

#[test]
fn exit_signature_commits_to_the_other_inputs_script() {
    let (tx, mut prevouts) = spend(128, 128, 0xffff_fffd, 3, true);
    assert!(verifies(&tx, &prevouts));
    prevouts[1].script_pubkey = ScriptBuf::from_bytes(vec![0x00]);
    assert!(!verifies(&tx, &prevouts));
}

fn edge_context() -> JoinContext {
    JoinContext {
        named: vec![],
        epoch: 7,
        h_0: Some(1000),
        epoch_length: 10,
        min_timeout: 2,
        max_timeout: 5,
    }
}

#[test]
fn timeout_epoch_uses_a_ceiling_at_every_boundary() {
    let c = edge_context();
    for (t, e) in [
        (1000, 0),
        (1001, 1),
        (1010, 1),
        (1011, 2),
        (1110, 11),
        (1111, 12),
        (1120, 12),
    ] {
        assert_eq!(c.timeout_epoch(t, 1070), e);
    }
}

#[test]
fn bootstrap_timeout_origin_moves_with_the_inclusion_block() {
    let c = JoinContext {
        epoch: 0,
        h_0: None,
        min_timeout: 1,
        max_timeout: 1,
        ..edge_context()
    };
    assert!(c.check_timeout(140, 100).is_ok());
    assert!(c.check_timeout(140, 109).is_ok());
    assert!(c.check_timeout(140, 110).is_err());
    assert!(c.check_timeout(141, 100).is_err());
    assert!(c.check_timeout(141, 101).is_ok());
}

#[test]
fn active_timeout_origin_is_stable_across_inclusion_heights() {
    let c = edge_context();
    for block in [1070, 1071, 1079] {
        assert_eq!(c.timeout_epoch(1111, block), 12);
        assert!(c.check_timeout(1111, block).is_ok());
        assert!(c.check_timeout(1110, block).is_err());
    }
}

#[test]
fn one_block_epoch_still_requires_two_blocks_before_timeout() {
    let c = JoinContext {
        epoch: 0,
        h_0: Some(100),
        epoch_length: 1,
        min_timeout: 1,
        max_timeout: 1,
        named: vec![],
    };
    assert!(c.check_timeout(104, 102).is_ok());
    assert!(c.check_timeout(104, 103).is_err());
    assert!(c.check_timeout(104, 104).is_err());
}

#[test]
fn timeout_height_limit_is_independent_of_its_epoch() {
    let c = JoinContext {
        epoch: 0,
        h_0: Some(499_999_960),
        min_timeout: 1,
        max_timeout: 2,
        ..edge_context()
    };
    assert!(c.check_timeout(499_999_999, 499_999_961).is_ok());
    assert!(c.check_timeout(500_000_000, 499_999_961).is_err());
}

#[test]
fn timeout_bound_addition_does_not_wrap_u32() {
    let c = JoinContext {
        epoch: 1,
        h_0: Some(100),
        min_timeout: u32::MAX,
        max_timeout: u32::MAX,
        ..edge_context()
    };
    assert!(c.check_timeout(150, 110).is_err());
}

fn arb_join_msg() -> impl Strategy<Value = IpcJoinSubnetMsg> {
    (
        any::<[u8; 32]>(),
        (any::<u32>(), any::<u16>()),
        0..3usize,
        arb_xonly(),
    )
        .prop_map(
            |(subnet_txid, (ip, port), address, pubkey)| IpcJoinSubnetMsg {
                subnet_id: SubnetId::from_txid(&Txid::from_byte_array(subnet_txid)),
                collateral: Amount::ZERO,
                ip: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), port)),
                backup_address: address_at(address),
                pubkey,
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn join_ue_round_trips_and_any_mutation_is_rejected(
        keypair in arb_keypair(),
        join in arb_join_msg(),
        timeout in 102u32..500_000_000,
        collateral in arb_amount(),
        flip in (any::<prop::sample::Index>(), 1u8..=255),
        growth in -3i8..=3,
    ) {
        let secp = setup();
        let (pubkey, _) = keypair.x_only_public_key();
        let join = IpcJoinSubnetMsg { pubkey, ..join };
        let eots_sk = secp256k1::SecretKey::from_slice(&[9u8; 32]).unwrap();
        let commitment = crate::eots::EotsNonceCommitment { start: 1, count: 1, root: [2u8; 32] };
        let context = JoinContext {
            named: vec![(0, Configuration { keys: vec![(pubkey, 1)], threshold: 1 })],
            epoch: 0,
            h_0: None,
            epoch_length: 10,
            min_timeout: 1,
            max_timeout: 1000,
        };
        let msg = IpcJoinUeMsg::new_signed(&secp, join, timeout, &keypair.secret_key(), &eots_sk, commitment);
        let data = msg.to_bytes().unwrap();
        let parsed = IpcJoinUeMsg::from_bytes(&data).unwrap();
        prop_assert_eq!(parsed.timeout, timeout);
        prop_assert_eq!(parsed.join.pubkey, pubkey);
        prop_assert_eq!(parsed.signature, msg.signature);

        // Accepted four epochs below the timeout, refused at the Bitcoin block before it
        let (commit, reveal, _) = unsigned_join_ue_txs(&secp, &msg, collateral, &context.named[0].1).unwrap();
        let block_height = u64::from(timeout) - 40;
        prop_assert!(check_join_ue(&secp, &reveal, 0, &commit, block_height, &context).is_ok());
        prop_assert!(check_join_ue(&secp, &reveal, 0, &commit, u64::from(timeout) - 1, &context).is_err());

        // One byte flipped, or bytes added or removed: rejected
        let mut mutated = data.clone();
        let at = flip.0.index(mutated.len());
        mutated[at] ^= flip.1;
        let collateral_output = commit.output[1].clone();
        let (commit_m, reveal_m) =
            unsigned_commit_reveal(&secp, &mutated, vec![collateral_output.clone()]).unwrap();
        prop_assert!(check_join_ue(&secp, &reveal_m, 0, &commit_m, block_height, &context).is_err());
        prop_assert!(IpcJoinUeMsg::from_bytes(&mutated).is_err());
        let mut resized = data.clone();
        if growth >= 0 {
            resized.extend(std::iter::repeat(0u8).take(growth as usize + 1));
        } else {
            resized.truncate(resized.len() - (-growth) as usize);
        }
        let (commit_r, reveal_r) =
            unsigned_commit_reveal(&secp, &resized, vec![collateral_output]).unwrap();
        prop_assert!(check_join_ue(&secp, &reveal_r, 0, &commit_r, block_height, &context).is_err());
    }
}
