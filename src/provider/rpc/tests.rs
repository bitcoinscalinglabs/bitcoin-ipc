use super::*;
use crate::collateral::process_join_ue;
use crate::db::{
    SlashPresigRecord, SubnetCommittee, SubnetValidator, SubnetValidators, UeCollateral,
};
use crate::ipc_lib::IpcMessage;
use crate::test_utils::{
    checked, create, create_rand_blockhash, create_rand_eth_addr, create_rand_txid, create_test_db,
    database, generate_subnet, generate_subnet_id, generate_xonly_pubkeys, pubkey, rotation,
    secret, slash_entry, subnet, EpochZeroFixture,
};
use base64::Engine;
use bitcoin::{key::Secp256k1, Address, Amount, BlockHash, OutPoint, Txid};
use bitcoincore_rpc::{json::ListUnspentResultEntry, jsonrpc};
use jsonrpc_v2::{Data, Params};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

#[test]
fn latest_checkpoint_at_a_bitcoin_height_waits_for_the_monitor() {
    let db = create_test_db();
    let mut subnet = generate_subnet(3);
    subnet.last_checkpoint_number = Some(1);
    let subnet_id = subnet.id;
    let checkpoint = |number: u64, block_height: u64| db::SubnetCheckpoint {
        checkpoint_number: number,
        checkpoint_hash: bitcoin::hashes::sha256::Hash::all_zeros(),
        checkpoint_height: 10 * (number + 1),
        block_height,
        txid: create_rand_txid(),
        batch_transfer_txid: None,
        batch_transfer_block_height: None,
        signed_committee_number: 0,
        next_committee_number: 0,
        next_configuration_number: 0,
        is_kill_checkpoint: false,
        epoch: 1,
        is_rotation_checkpoint: false,
    };
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_state(&mut wtxn, subnet_id, &subnet).unwrap();
    db.save_checkpoint(&mut wtxn, subnet_id, &checkpoint(0, 100), 0)
        .unwrap();
    db.save_checkpoint(&mut wtxn, subnet_id, &checkpoint(1, 120), 1)
        .unwrap();
    wtxn.commit().unwrap();
    let mut txn = db.write_txn().unwrap();
    db.set_last_processed_block(&mut txn, 125).unwrap();
    txn.commit().unwrap();

    let at = |height: Option<u64>| latest_checkpoint_at(&db, subnet_id, None, height);
    // Processed heights answer by the height alone
    assert_eq!(at(Some(110)).unwrap().unwrap().checkpoint_number, 0);
    assert_eq!(at(Some(120)).unwrap().unwrap().checkpoint_number, 1);
    assert_eq!(at(Some(125)).unwrap().unwrap().checkpoint_number, 1);
    assert!(at(Some(99)).unwrap().is_none());
    assert_eq!(at(None).unwrap().unwrap().checkpoint_number, 1);
    // A Bitcoin height not yet processed is refused, not answered with checkpoint 1
    assert!(matches!(
        at(Some(126)),
        Err(RpcError::MonitorBehind {
            requested: 126,
            processed: 125
        })
    ));
    // Once that height is processed, the same question has an answer
    let mut txn = db.write_txn().unwrap();
    db.set_last_processed_block(&mut txn, 126).unwrap();
    txn.commit().unwrap();
    assert_eq!(at(Some(126)).unwrap().unwrap().checkpoint_number, 1);
    // Without a Bitcoin height the processed height does not matter
    let mut txn = db.write_txn().unwrap();
    db.set_last_processed_block(&mut txn, 0).unwrap();
    txn.commit().unwrap();
    assert_eq!(at(None).unwrap().unwrap().checkpoint_number, 1);
    assert_eq!(
        latest_checkpoint_at(&db, subnet_id, Some(0), None)
            .unwrap()
            .unwrap()
            .checkpoint_number,
        0
    );
}

#[test]
fn eots_records_hold_what_was_recorded_above_a_height_and_at_or_below_the_answer_height() {
    let db = create_test_db();
    let mut subnet = generate_subnet(3);
    subnet.last_checkpoint_number = Some(0);
    let subnet_id = subnet.id;
    let checkpoint = db::SubnetCheckpoint {
        checkpoint_number: 0,
        checkpoint_hash: bitcoin::hashes::sha256::Hash::all_zeros(),
        checkpoint_height: 10,
        block_height: 21,
        txid: create_rand_txid(),
        batch_transfer_txid: None,
        batch_transfer_block_height: None,
        signed_committee_number: 0,
        next_committee_number: 0,
        next_configuration_number: 0,
        is_kill_checkpoint: false,
        epoch: 1,
        is_rotation_checkpoint: false,
    };
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_state(&mut wtxn, subnet_id, &subnet).unwrap();
    db.save_checkpoint(&mut wtxn, subnet_id, &checkpoint, 0)
        .unwrap();
    // Validators 1 to 3 join at Bitcoin heights 21 to 23
    for n in 1..=3u8 {
        let join_txid = create_rand_txid();
        db.add_ue_collateral(
            &mut wtxn,
            &UeCollateral {
                subnet_id,
                pubkey: pubkey(n),
                outpoint: bitcoin::OutPoint::new(join_txid, 1),
                value: bitcoin::Amount::from_sat(1_000_000),
                timeout: 1_000,
                pk_eots: pubkey(20 + n),
                committee_number: 0,
                ip: "127.0.0.1:8080".parse().unwrap(),
                backup_address: crate::test_utils::create_rand_addr().into_unchecked(),
                join_txid,
                spent: false,
            },
        )
        .unwrap();
        db.add_eots_commitment(
            &mut wtxn,
            &db::EotsCommitmentRecord {
                subnet_id,
                pk_eots: pubkey(20 + n),
                commitment: crate::eots::EotsNonceCommitment {
                    start: 1,
                    count: 100,
                    root: [n; 32],
                },
                block_height: 20 + u64::from(n),
                txid: join_txid,
            },
        )
        .unwrap();
    }
    wtxn.commit().unwrap();
    // The join at 23 is written, its Bitcoin block not processed yet
    let mut txn = db.write_txn().unwrap();
    db.set_last_processed_block(&mut txn, 22).unwrap();
    txn.commit().unwrap();

    let at =
        |after: Option<u64>, max: Option<u64>| eots_records_at(&db, subnet_id, after, max).unwrap();
    let validators = |records: &GetEotsRecordsResponse| {
        records
            .eots_keys
            .iter()
            .map(|k| k.pubkey)
            .collect::<Vec<_>>()
    };
    let all = at(None, None);
    assert_eq!(all.block_height, 22);
    let mut expected = vec![pubkey(1), pubkey(2)];
    expected.sort();
    assert_eq!(validators(&all), expected);
    assert_eq!(all.checkpoint.unwrap().checkpoint_number, 0);
    let second = all
        .eots_keys
        .iter()
        .find(|k| k.pubkey == pubkey(2))
        .unwrap();
    assert_eq!(second.pk_eots, pubkey(22));
    assert_eq!(second.commitments.len(), 1);
    assert_eq!(second.commitments[0].block_height, 22);
    assert_eq!(second.commitments[0].root, hex::encode([2u8; 32]));

    assert_eq!(validators(&at(Some(21), None)), [pubkey(2)]);
    assert!(at(Some(22), None).eots_keys.is_empty());
    let earlier = at(None, Some(21));
    assert_eq!(earlier.block_height, 21);
    assert_eq!(validators(&earlier), [pubkey(1)]);
    assert!(earlier.checkpoint.is_some());
    assert!(at(None, Some(20)).checkpoint.is_none());
}

#[test]
fn eots_records_at_a_bitcoin_height_wait_for_the_monitor() {
    let db = create_test_db();
    let subnet = generate_subnet(3);
    let mut txn = db.write_txn().unwrap();
    db.set_last_processed_block(&mut txn, 125).unwrap();
    txn.commit().unwrap();
    // No state of the subnet is recorded yet: the height is not processed, the subnet is not missing
    assert!(matches!(
        eots_records_at(&db, subnet.id, None, Some(126)),
        Err(RpcError::MonitorBehind {
            requested: 126,
            processed: 125
        })
    ));
    assert!(matches!(
        eots_records_at(&db, subnet.id, None, Some(125)),
        Err(RpcError::InvalidParams(_))
    ));
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_state(&mut wtxn, subnet.id, &subnet).unwrap();
    wtxn.commit().unwrap();
    assert!(matches!(
        eots_records_at(&db, subnet.id, None, Some(126)),
        Err(RpcError::MonitorBehind {
            requested: 126,
            processed: 125
        })
    ));
    assert_eq!(
        eots_records_at(&db, subnet.id, None, Some(125))
            .unwrap()
            .block_height,
        125
    );
}

#[test]
fn carried_entries_keep_complete_valid_ones_only() {
    use crate::collateral::{self, CollateralScript};
    use crate::slash::{sign_slash, PostedSlashSignatures};
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let db = create_test_db();
    let subnet_id = generate_subnet_id();
    let keys = (1u8..9)
        .map(|seed| bitcoin::secp256k1::SecretKey::from_slice(&[seed; 32]).unwrap())
        .collect::<Vec<_>>();
    let pks = keys
        .iter()
        .map(|k| k.x_only_public_key(&secp).0)
        .collect::<Vec<_>>();
    // A subnet created with a whitelist of three keys, min 2: configuration 0 has threshold 2
    let genesis = db::SubnetGenesisInfo {
        subnet_id,
        create_subnet_msg: IpcCreateSubnetMsg {
            min_validator_stake: bitcoin::Amount::from_sat(10_000),
            power_unit_exponent: 4,
            min_validators: 2,
            bottomup_check_period: 10,
            active_validators_limit: 10,
            min_cross_msg_fee: bitcoin::Amount::from_sat(1),
            whitelist: vec![pks[0], pks[1], pks[2]],
            epoch_length: 10,
            min_timeout: 1,
            max_timeout: 1000,
            min_deposit: bitcoin::Amount::from_sat(330),
            kill_vote_period: 36,
            kill_delay_period: 5,
        },
        create_msg_block_height: 1,
        bootstrapped: false,
        genesis_block_height: None,
        genesis_validators: vec![],
        genesis_balance_entries: vec![],
    };
    let record = UeCollateral {
        subnet_id,
        pubkey: pks[3],
        outpoint: bitcoin::OutPoint::new(create_rand_txid(), 1),
        value: bitcoin::Amount::from_sat(1_000_000),
        timeout: 1_000,
        pk_eots: pks[0],
        committee_number: 0,
        ip: "127.0.0.1:8080".parse().unwrap(),
        backup_address: crate::test_utils::create_rand_addr().into_unchecked(),
        join_txid: create_rand_txid(),
        spent: false,
    };
    let mut wtxn = db.write_txn().unwrap();
    db.save_subnet_genesis_info(&mut wtxn, subnet_id, &genesis)
        .unwrap();
    db.add_ue_collateral(&mut wtxn, &record).unwrap();
    wtxn.commit().unwrap();
    let script: CollateralScript =
        collateral::collateral_script_of(&db, &db.read_txn().unwrap(), &record).unwrap();
    let sign = |k: &bitcoin::secp256k1::SecretKey| {
        sign_slash(
            &secp,
            &script,
            record.outpoint,
            record.value,
            &k.keypair(&secp),
        )
        .unwrap()
    };
    let (s0, s1) = (sign(&keys[0]), sign(&keys[1]));
    let mut bad = sign(&keys[2]);
    bad.1[1][0] ^= 1;
    // Posted without their signers, in any order: each resolves to the key it verifies for.
    // Two valid of three: carried with the valid ones only, in index order; one valid: left out
    let posted = vec![PostedSlashSignatures::new(
        record.outpoint,
        vec![s1.1, bad.1, s0.1, s1.1],
    )];
    let carried = carried_slash_entries(&db, &db.read_txn().unwrap(), subnet_id, &posted);
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].signatures, vec![s0, s1]);
    assert!(s0.0 < s1.0);
    let posted = vec![PostedSlashSignatures::new(
        record.outpoint,
        vec![s0.1, bad.1],
    )];
    assert!(carried_slash_entries(&db, &db.read_txn().unwrap(), subnet_id, &posted).is_empty());
    // An unknown output is left out; a slashable one is carried again
    let mut unknown = PostedSlashSignatures::new(record.outpoint, vec![s0.1, s1.1]);
    unknown.vout = 3;
    assert!(carried_slash_entries(&db, &db.read_txn().unwrap(), subnet_id, &[unknown]).is_empty());
    let mut wtxn = db.write_txn().unwrap();
    db.add_slash_presigs(
        &mut wtxn,
        &db::SlashPresigRecord {
            subnet_id,
            outpoint: record.outpoint,
            signatures: vec![s0, s1],
            block_height: 5,
            batch_transfer_txid: create_rand_txid(),
        },
    )
    .unwrap();
    wtxn.commit().unwrap();
    let posted = vec![PostedSlashSignatures::new(
        record.outpoint,
        vec![s0.1, s1.1],
    )];
    assert_eq!(
        carried_slash_entries(&db, &db.read_txn().unwrap(), subnet_id, &posted).len(),
        1
    );

    // A second collateral whose join is in an earlier block comes first, whatever its outpoint
    let older = UeCollateral {
        pubkey: pks[4],
        outpoint: bitcoin::OutPoint::new(bitcoin::Txid::from_byte_array([0xff; 32]), 1),
        pk_eots: pks[5],
        join_txid: create_rand_txid(),
        ..record.clone()
    };
    let newer = UeCollateral {
        pubkey: pks[6],
        outpoint: bitcoin::OutPoint::new(bitcoin::Txid::from_byte_array([0x00; 32]), 1),
        pk_eots: pks[7],
        join_txid: create_rand_txid(),
        ..record.clone()
    };
    let mut wtxn = db.write_txn().unwrap();
    for (collateral, block_height) in [(&older, 10), (&newer, 20)] {
        db.add_ue_collateral(&mut wtxn, collateral).unwrap();
        db.add_eots_commitment(
            &mut wtxn,
            &db::EotsCommitmentRecord {
                subnet_id,
                pk_eots: collateral.pk_eots,
                commitment: crate::eots::EotsNonceCommitment {
                    start: 1,
                    count: 1,
                    root: [0; 32],
                },
                block_height,
                txid: collateral.join_txid,
            },
        )
        .unwrap();
    }
    wtxn.commit().unwrap();
    let entry_of = |collateral: &UeCollateral| {
        let script =
            collateral::collateral_script_of(&db, &db.read_txn().unwrap(), collateral).unwrap();
        let signatures = keys[..2]
            .iter()
            .map(|k| {
                sign_slash(
                    &secp,
                    &script,
                    collateral.outpoint,
                    collateral.value,
                    &k.keypair(&secp),
                )
                .unwrap()
                .1
            })
            .collect();
        PostedSlashSignatures::new(collateral.outpoint, signatures)
    };
    let posted = vec![entry_of(&newer), entry_of(&older)];
    let carried = carried_slash_entries(&db, &db.read_txn().unwrap(), subnet_id, &posted);
    assert_eq!(
        carried.iter().map(|e| e.outpoint()).collect::<Vec<_>>(),
        vec![older.outpoint, newer.outpoint]
    );
}

/// Synthetic entries of 350 signatures each, about 23 kB: 17 fit a standard reveal
#[test]
fn checkpoint_carries_the_longest_prefix_of_entries_that_fits() {
    let entry = |i: u8| crate::slash::SlashSignatureEntry {
        txid: [i; 32],
        vout: 1,
        signatures: (0..350u16)
            .map(|index| (index, [[index as u8; 32]; 2]))
            .collect(),
    };
    let mut msg = IpcCheckpointSubnetMsg {
        subnet_id: generate_subnet_id(),
        checkpoint_hash: bitcoin::hashes::sha256::Hash::from_byte_array([0; 32]),
        checkpoint_height: 1,
        next_committee_configuration_number: 0,
        withdrawals: vec![],
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: (0..20).map(entry).collect(),
        unstakes: vec![],
        change_address: None,
        is_kill_checkpoint: false,
        is_rotation_checkpoint: false,
    };
    let max = bitcoin::Weight::from_wu(u64::from(bitcoin::policy::MAX_STANDARD_TX_WEIGHT));
    assert!(msg.batch_transfer_reveal_weight().unwrap() > max);
    fit_slash_entries(&mut msg).unwrap();
    assert_eq!(msg.slash_signatures.len(), 17);
    assert_eq!(msg.slash_signatures, (0..17).map(entry).collect::<Vec<_>>());
    assert!(msg.batch_transfer_reveal_weight().unwrap() <= max);
    msg.slash_signatures.push(entry(17));
    assert!(msg.batch_transfer_reveal_weight().unwrap() > max);
    // Entries that fit are all kept
    msg.slash_signatures = (0..5).map(entry).collect();
    fit_slash_entries(&mut msg).unwrap();
    assert_eq!(msg.slash_signatures.len(), 5);
}

#[test]
fn eots_votes_are_refused_at_checkpointed_heights() {
    let checkpoint = |height: u64| db::SubnetCheckpoint {
        checkpoint_number: 3,
        checkpoint_hash: bitcoin::hashes::sha256::Hash::from_byte_array([0; 32]),
        checkpoint_height: height,
        block_height: 100,
        txid: create_rand_txid(),
        batch_transfer_txid: None,
        batch_transfer_block_height: None,
        signed_committee_number: 1,
        next_committee_number: 1,
        next_configuration_number: 0,
        is_kill_checkpoint: false,
        epoch: 1,
        is_rotation_checkpoint: false,
    };
    assert!(refuse_checkpointed_height(None, 1).is_ok());
    let latest = checkpoint(50);
    assert!(refuse_checkpointed_height(Some(&latest), 50).is_err());
    assert!(refuse_checkpointed_height(Some(&latest), 10).is_err());
    assert!(refuse_checkpointed_height(Some(&latest), 51).is_ok());
}

fn validator(pubkey: bitcoin::XOnlyPublicKey) -> SubnetValidator {
    SubnetValidator {
        pubkey,
        subnet_address: create_rand_eth_addr(),
        power: 10,
        collateral: bitcoin::Amount::from_sat(1_000_000),
        backup_address: bitcoin::Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8000".parse().unwrap(),
        join_txid: create_rand_txid(),
    }
}

/// A rotation pays only ordinary members' withdrawals.
#[test]
fn checkpoint_outputs_skip_ue_members() {
    use crate::db::{StakeChangeRequest, StakingChange};
    let subnet_id = generate_subnet_id();
    let keys = generate_xonly_pubkeys(3);
    let (ue, ordinary, outsider) = (keys[0], keys[1], keys[2]);
    let committee = vec![validator(ue), validator(ordinary)].to_committee(&subnet_id, 0);
    let ue_members = std::collections::HashSet::from([ue]);
    let withdraw = |pubkey: bitcoin::XOnlyPublicKey, sats: u64| StakeChangeRequest {
        change: StakingChange::Withdraw {
            amount: bitcoin::Amount::from_sat(sats),
        },
        validator_xpk: pubkey,
        validator_subnet_address: create_rand_eth_addr(),
        configuration_number: 1,
        committee_after_change: committee.clone(),
        block_height: 100,
        block_hash: create_rand_blockhash(),
        checkpoint_block_height: None,
        checkpoint_block_hash: None,
        txid: create_rand_txid(),
    };
    let changes = vec![
        withdraw(ue, 1_000_000),
        withdraw(ordinary, 300_000),
        withdraw(outsider, 5),
        StakeChangeRequest {
            change: StakingChange::Deposit {
                amount: bitcoin::Amount::from_sat(7),
            },
            ..withdraw(ordinary, 0)
        },
    ];

    let unstakes = rotation_unstakes(&changes, &committee, &ue_members);
    assert_eq!(unstakes.len(), 1);
    assert_eq!(unstakes[0].pubkey, ordinary);
    assert_eq!(unstakes[0].amount.to_sat(), 300_000);
    assert_eq!(unstakes[0].address, committee.validators[1].backup_address);

    // Without UE members a rotation pays everyone
    let none = std::collections::HashSet::new();
    assert_eq!(rotation_unstakes(&changes, &committee, &none).len(), 2);
}

/// The switch refuses while unset or above the tip.
#[test]
fn switch_gates_join_ue_and_unstakeue() {
    assert!(check_ue_active(None, 1_000)
        .unwrap_err()
        .contains("not set"));
    assert!(check_ue_active(Some(500), 499)
        .unwrap_err()
        .contains("active from block 500"));
    check_ue_active(Some(500), 500).unwrap();
    check_ue_active(Some(500), 501).unwrap();
}

/// `epochs` is optional in the request and absent in the join message.
#[test]
fn join_request_epochs_is_optional() {
    let json = serde_json::json!({
        "subnet_id": generate_subnet_id(),
        "collateral": 100_000,
        "ip": "127.0.0.1:8000",
        "backup_address": "bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd",
        "pubkey": generate_xonly_pubkeys(1)[0],
    });
    let req: JoinSubnetRequest = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(req.epochs, None);
    let mut with_epochs = json;
    with_epochs["epochs"] = serde_json::json!(20);
    let req: JoinSubnetRequest = serde_json::from_value(with_epochs).unwrap();
    assert_eq!(req.epochs, Some(20));
    let msg: IpcJoinSubnetMsg = req.into();
    assert_eq!(msg.collateral.to_sat(), 100_000);
}

/// The genesis information of `subnet_id`, created with `whitelist`, min 2
fn genesis(subnet_id: SubnetId, whitelist: Vec<bitcoin::XOnlyPublicKey>) -> db::SubnetGenesisInfo {
    db::SubnetGenesisInfo {
        subnet_id,
        create_subnet_msg: IpcCreateSubnetMsg {
            min_validator_stake: bitcoin::Amount::from_sat(10_000),
            power_unit_exponent: 4,
            min_validators: 2,
            bottomup_check_period: 10,
            active_validators_limit: 10,
            min_cross_msg_fee: bitcoin::Amount::from_sat(1),
            whitelist,
            epoch_length: 10,
            min_timeout: 1,
            max_timeout: 1000,
            min_deposit: bitcoin::Amount::from_sat(330),
            kill_vote_period: 36,
            kill_delay_period: 5,
        },
        create_msg_block_height: 1,
        bootstrapped: true,
        genesis_block_height: Some(1),
        genesis_validators: vec![],
        genesis_balance_entries: vec![],
    }
}

fn plan(genesis: &db::SubnetGenesisInfo, subnet: &db::SubnetState) -> CheckpointPlan {
    CheckpointPlan::of_state(genesis, subnet).unwrap_or_else(|_| panic!("no checkpoint plan"))
}

#[test]
fn an_unchanged_epoch_gets_a_rotation_checkpoint_that_sweeps_nothing() {
    let mut subnet = generate_subnet(3);
    let genesis = genesis(subnet.id, generate_xonly_pubkeys(3));
    // Epoch 2 with `C_2` = `C_1`, after the rotation checkpoint of epoch 1
    subnet.epoch = 2;
    subnet.last_rotation_epoch = 1;
    subnet.waiting_committee = None;
    let rotation = plan(&genesis, &subnet);
    assert!(rotation.rotation);
    assert!(!rotation.sweep);
    assert_eq!(rotation.signer.address, subnet.committee.address_checked());
    assert_eq!(rotation.change_address, rotation.signer.address);

    // After it, the checkpoints of the epoch are regular
    subnet.last_rotation_epoch = 2;
    let regular = plan(&genesis, &subnet);
    assert!(!regular.rotation);
    assert!(!regular.sweep);
}

#[test]
fn epoch_1_with_c_1_equal_to_c_gen_sweeps_the_whitelist_multisig() {
    let mut subnet = generate_subnet(2);
    let mut whitelist: Vec<_> = subnet
        .committee
        .validators
        .iter()
        .map(|v| v.pubkey)
        .collect();
    whitelist.extend(generate_xonly_pubkeys(1));
    let genesis = genesis(subnet.id, whitelist);
    // Epoch 1 with `C_1` = `C_gen`, before its rotation checkpoint: `C_wlist` holds the funds
    subnet.epoch = 1;
    subnet.last_rotation_epoch = 0;
    subnet.waiting_committee = None;
    let wlist =
        ipc_lib::CheckpointSigner::of_configuration(&subnet.id, &genesis.whitelist_configuration())
            .unwrap();
    let rotation = plan(&genesis, &subnet);
    assert!(rotation.rotation);
    assert!(rotation.sweep);
    assert_eq!(rotation.signer.address, wlist.address);
    assert_eq!(rotation.change_address, subnet.committee.address_checked());
}

#[derive(Default)]
struct WalletState {
    funds: Vec<ListUnspentResultEntry>,
    /// In the mempool unless mined, dropped or forgotten
    submitted: Vec<bitcoin::Transaction>,
    /// By Bitcoin block height
    mined: std::collections::BTreeMap<Txid, u64>,
    /// Out of the mempool, unconfirmed
    dropped: std::collections::BTreeSet<Txid>,
    /// Unknown to the wallet
    forgotten: std::collections::BTreeSet<Txid>,
    tip: u64,
    /// The outcome of the next `sendrawtransaction` calls, in order; `None` sends normally
    send_failures: std::collections::VecDeque<Option<SendFailure>>,
    /// Outputs spent by other transactions: in the mempool (`None`) or in a Bitcoin block
    spent: std::collections::BTreeMap<OutPoint, Option<u64>>,
    /// The transactions of others in Bitcoin blocks, by height
    blocks: std::collections::BTreeMap<u64, Vec<bitcoin::Transaction>>,
    /// The next `sendrawtransaction` waits at this gate
    gate: Option<Arc<Gate>>,
    /// The tip after the next `fundrawtransaction`: a Bitcoin block arrives while a pair is built
    tip_after_funding: Option<u64>,
}

/// A wait in the wallet, released by the test or after 10 seconds.
#[derive(Default)]
struct Gate {
    stalled: std::sync::atomic::AtomicBool,
    timed_out: std::sync::atomic::AtomicBool,
    open: Mutex<bool>,
    opened: std::sync::Condvar,
}

impl Gate {
    fn wait(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.stalled.store(true, SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut open = self.open.lock().unwrap();
        while !*open {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                self.timed_out.store(true, SeqCst);
                break;
            }
            open = self.opened.wait_timeout(open, left).unwrap().0;
        }
    }

    fn stalled(&self) -> bool {
        self.stalled.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }

    /// Entered, not opened, not timed out: the wait is on.
    fn held(&self) -> bool {
        self.stalled()
            && !self.timed_out.load(std::sync::atomic::Ordering::SeqCst)
            && !*self.open.lock().unwrap()
    }
}

fn block_hash_at(height: u64) -> BlockHash {
    let mut bytes = [0xb1; 32];
    bytes[..8].copy_from_slice(&height.to_le_bytes());
    BlockHash::from_byte_array(bytes)
}

#[derive(Clone, Copy)]
enum SendFailure {
    /// The node has the transaction, the response is lost
    Lost,
    /// The node refuses the transaction
    Refused,
}

fn send_error() -> jsonrpc::error::RpcError {
    jsonrpc::error::RpcError {
        code: -25,
        message: "connection lost".to_string(),
        data: None,
    }
}

/// Bitcoin Core's error for a transaction it does not know
fn unknown_transaction() -> jsonrpc::error::RpcError {
    jsonrpc::error::RpcError {
        code: -5,
        message: "No such mempool or wallet transaction".to_string(),
        data: None,
    }
}

#[derive(Clone, Default)]
struct WalletRpc(Arc<Mutex<WalletState>>);

impl jsonrpc::client::Transport for WalletRpc {
    fn send_request(&self, request: jsonrpc::Request) -> Result<jsonrpc::Response, jsonrpc::Error> {
        let args: Vec<serde_json::Value> = request
            .params
            .map(|p| serde_json::from_str(p.get()).unwrap())
            .unwrap_or_default();
        let result = match request.method {
            "getblockcount" => serde_json::json!(self.0.lock().unwrap().tip),
            "getnewaddress" => {
                serde_json::json!(
                    Address::p2tr(&Secp256k1::new(), pubkey(9), None, crate::NETWORK).to_string()
                )
            }
            "estimatesmartfee" => serde_json::json!({"feerate": 0.0001, "blocks": 6}),
            "listunspent" => {
                let addresses: Vec<String> = serde_json::from_value(args[2].clone()).unwrap();
                let state = self.0.lock().unwrap();
                serde_json::to_value(
                    state
                        .funds
                        .iter()
                        .filter(|fund| {
                            addresses.contains(
                                &fund
                                    .address
                                    .as_ref()
                                    .unwrap()
                                    .clone()
                                    .assume_checked()
                                    .to_string(),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap()
            }
            "fundrawtransaction" => {
                // Wallet templates have no inputs and use legacy serialization. Decode
                // their fields separately, then provide a synthetic wallet input.
                use bitcoin::consensus::Decodable;
                let bytes = hex::decode(args[0].as_str().unwrap()).unwrap();
                let mut cursor = bitcoin::io::Cursor::new(bytes);
                let mut tx = bitcoin::Transaction {
                    version: Decodable::consensus_decode(&mut cursor).unwrap(),
                    input: Decodable::consensus_decode(&mut cursor).unwrap(),
                    output: Decodable::consensus_decode(&mut cursor).unwrap(),
                    lock_time: Decodable::consensus_decode(&mut cursor).unwrap(),
                };
                assert!(tx.input.is_empty());
                tx.input.push(bitcoin::TxIn {
                    previous_output: OutPoint::new(Txid::from_byte_array([0xfd; 32]), 0),
                    ..Default::default()
                });
                serde_json::json!({
                    "hex": bitcoin::consensus::encode::serialize_hex(&tx),
                    "fee": 0.00001,
                    "changepos": -1
                })
            }
            "signrawtransactionwithwallet" => {
                let mut state = self.0.lock().unwrap();
                if let Some(tip) = state.tip_after_funding.take() {
                    state.tip = tip;
                }
                serde_json::json!({"hex": args[0], "complete": true})
            }
            "gettxout" => {
                let txid: Txid = serde_json::from_value(args[0].clone()).unwrap();
                let vout = args[1].as_u64().unwrap() as u32;
                let with_mempool = args.get(2).and_then(|v| v.as_bool()).unwrap_or(true);
                let state = self.0.lock().unwrap();
                let outpoint = OutPoint::new(txid, vout);
                let output = serde_json::json!({
                    "bestblock": block_hash_at(state.tip),
                    "confirmations": 1,
                    "value": 0.001,
                    "scriptPubKey": {"asm": "1", "hex": "51", "type": "nonstandard"},
                    "coinbase": false,
                });
                let unspent = match state.spent.get(&outpoint) {
                    Some(None) => !with_mempool,
                    Some(Some(_)) => false,
                    None => {
                        let own = state.submitted.iter().any(|tx| tx.compute_txid() == txid);
                        let mined = state.mined.contains_key(&txid);
                        let in_mempool = own
                            && !mined
                            && !state.dropped.contains(&txid)
                            && !state.forgotten.contains(&txid);
                        !own || mined || (in_mempool && with_mempool)
                    }
                };
                if unspent {
                    output
                } else {
                    serde_json::Value::Null
                }
            }
            "getblockhash" => serde_json::json!(block_hash_at(args[0].as_u64().unwrap())),
            "getblock" => {
                let hash: BlockHash = serde_json::from_value(args[0].clone()).unwrap();
                let height = u64::from_le_bytes(hash.to_byte_array()[..8].try_into().unwrap());
                let state = self.0.lock().unwrap();
                let block = bitcoin::Block {
                    header: bitcoin::block::Header {
                        version: bitcoin::block::Version::ONE,
                        prev_blockhash: BlockHash::all_zeros(),
                        merkle_root: bitcoin::TxMerkleNode::all_zeros(),
                        time: 0,
                        bits: bitcoin::CompactTarget::from_consensus(0),
                        nonce: 0,
                    },
                    txdata: state.blocks.get(&height).cloned().unwrap_or_default(),
                };
                serde_json::json!(bitcoin::consensus::encode::serialize_hex(&block))
            }
            "testmempoolaccept" => {
                let hexes: Vec<String> = serde_json::from_value(args[0].clone()).unwrap();
                serde_json::json!(hexes
                    .iter()
                    .map(|raw| {
                        let tx: bitcoin::Transaction =
                            bitcoin::consensus::deserialize(&hex::decode(raw).unwrap()).unwrap();
                        serde_json::json!({"txid": tx.compute_txid(), "allowed": true})
                    })
                    .collect::<Vec<_>>())
            }
            "sendrawtransaction" => {
                let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(
                    &hex::decode(args[0].as_str().unwrap()).unwrap(),
                )
                .unwrap();
                let txid = tx.compute_txid();
                let gate = self.0.lock().unwrap().gate.take();
                if let Some(gate) = gate {
                    gate.wait();
                }
                let mut state = self.0.lock().unwrap();
                let failure = state.send_failures.pop_front().flatten();
                if !matches!(failure, Some(SendFailure::Refused)) {
                    state.submitted.push(tx);
                }
                if failure.is_some() {
                    return Ok(jsonrpc::Response {
                        result: None,
                        error: Some(send_error()),
                        id: request.id,
                        jsonrpc: Some("2.0".into()),
                    });
                }
                serde_json::json!(txid)
            }
            "getmempoolentry" => {
                let txid: Txid = serde_json::from_value(args[0].clone()).unwrap();
                let state = self.0.lock().unwrap();
                let in_mempool = state.submitted.iter().any(|tx| tx.compute_txid() == txid)
                    && !state.mined.contains_key(&txid)
                    && !state.dropped.contains(&txid)
                    && !state.forgotten.contains(&txid);
                if !in_mempool {
                    return Ok(jsonrpc::Response {
                        result: None,
                        error: Some(unknown_transaction()),
                        id: request.id,
                        jsonrpc: Some("2.0".into()),
                    });
                }
                serde_json::json!({"vsize": 200})
            }
            "gettransaction" => {
                let txid: Txid = serde_json::from_value(args[0].clone()).unwrap();
                let state = self.0.lock().unwrap();
                if !state.submitted.iter().any(|tx| tx.compute_txid() == txid)
                    || state.forgotten.contains(&txid)
                {
                    return Ok(jsonrpc::Response {
                        result: None,
                        error: Some(unknown_transaction()),
                        id: request.id,
                        jsonrpc: Some("2.0".into()),
                    });
                }
                match state.mined.get(&txid) {
                    Some(height) => serde_json::json!({
                        "txid": txid,
                        "confirmations": 1,
                        "blockheight": height,
                    }),
                    None => serde_json::json!({"txid": txid, "confirmations": 0}),
                }
            }
            method => panic!("unexpected Bitcoin RPC method: {method}"),
        };
        Ok(jsonrpc::Response {
            result: Some(jsonrpc::arg(result)),
            error: None,
            id: request.id,
            jsonrpc: Some("2.0".into()),
        })
    }

    fn send_batch(
        &self,
        requests: &[jsonrpc::Request],
    ) -> Result<Vec<jsonrpc::Response>, jsonrpc::Error> {
        requests
            .iter()
            .cloned()
            .map(|r| self.send_request(r))
            .collect()
    }

    fn fmt_target(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("test wallet")
    }
}

fn fund_utxos(address: &Address, count: u8) -> Vec<ListUnspentResultEntry> {
    (0..count)
        .map(|n| ListUnspentResultEntry {
            txid: Txid::from_byte_array([n; 32]),
            vout: 1,
            address: Some(address.clone().into_unchecked()),
            label: None,
            redeem_script: None,
            witness_script: None,
            script_pub_key: address.script_pubkey(),
            amount: Amount::from_sat(10_000_000),
            confirmations: 10,
            spendable: true,
            solvable: true,
            descriptor: None,
            safe: true,
        })
        .collect()
}

fn provider(f: EpochZeroFixture, wallet: &WalletRpc) -> (tempfile::TempDir, Arc<ServerData>) {
    let client = Arc::new(bitcoincore_rpc::Client::from_jsonrpc(
        jsonrpc::Client::with_transport(wallet.clone()),
    ));
    (
        f._dir,
        Arc::new(ServerData {
            db: Arc::new(f.db),
            btc_rpc: client.clone(),
            btc_watchonly_rpc: client,
            validator: Some((pubkey(1), secret(1))),
            eots_keys: None,
        }),
    )
}

async fn provider_checkpoint(data: &Arc<ServerData>, msg: IpcCheckpointSubnetMsg) -> bitcoin::Psbt {
    provider_checkpoint_and_reveal(data, msg).await.0
}

async fn provider_checkpoint_and_reveal(
    data: &Arc<ServerData>,
    msg: IpcCheckpointSubnetMsg,
) -> (bitcoin::Psbt, Option<bitcoin::Transaction>) {
    // The slash pre-signatures go to the handler as posted on the subnet, without their signers
    let posted: Vec<_> = msg
        .slash_signatures
        .iter()
        .map(|e| {
            crate::slash::PostedSlashSignatures::new(
                e.outpoint(),
                e.signatures.iter().map(|(_, s)| *s).collect(),
            )
        })
        .collect();
    let mut params = serde_json::to_value(&msg).unwrap();
    params["slash_pre_signatures"] = serde_json::to_value(&posted).unwrap();
    let response = gen_checkpoint_psbt(Data::new(data.clone()), Params(params))
        .await
        .unwrap_or_else(|e| {
            panic!(
                "checkpoint RPC failed: {}",
                serde_json::to_string(&e).unwrap()
            )
        });
    let response = serde_json::to_value(response).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(response["unsigned_psbt_base64"].as_str().unwrap())
        .unwrap();
    let reveal = response["batch_transfer_tx_hex"]
        .as_str()
        .map(|raw| bitcoin::consensus::deserialize(&hex::decode(raw).unwrap()).unwrap());
    (bitcoin::Psbt::deserialize(&bytes).unwrap(), reveal)
}

/// An unsigned checkpoint transaction of the epoch zero fixture, with the posted form of the
/// signatures of the keys `signers` (whitelist keys or not).
async fn checkpoint_to_finalize(
    signers: &[u8],
) -> (
    tempfile::TempDir,
    WalletRpc,
    Arc<ServerData>,
    FinalizeCheckpointPsbtParams,
    Vec<Vec<String>>,
) {
    let f = EpochZeroFixture::new([9000; 4]).await;
    let id = f.id;
    let mut request = f.message(&[]);
    request.change_address = None;
    let wallet = WalletRpc::default();
    wallet.0.lock().unwrap().funds = fund_utxos(&f.signer().address, 3);
    let (dir, data) = provider(f, &wallet);
    let psbt = provider_checkpoint(&data, request).await;
    let secp = Secp256k1::new();
    let posted = signers
        .iter()
        .map(|n| {
            let keypair = bitcoin::key::Keypair::from_secret_key(&secp, &secret(*n));
            let (_, sigs) = multisig::sign_spend_psbt(&secp, psbt.clone(), keypair).unwrap();
            sigs.iter().map(|sig| sig.to_string()).collect()
        })
        .collect();
    let params = FinalizeCheckpointPsbtParams {
        subnet_id: id,
        unsigned_psbt_base64: base64::engine::general_purpose::STANDARD.encode(psbt.serialize()),
        signatures: vec![],
        batch_transfer_tx_hex: None,
    };
    (dir, wallet, data, params, posted)
}

/// A checkpoint that spends outputs of the whitelist configuration (keys 1 to 6) finalizes after
/// the current configuration changed to keys 11 to 14: the keys come from the leaf of each input.
/// The signatures of key 11 and a malformed row are dropped, a row of key 5 posted twice counts
/// once.
#[tokio::test]
async fn finalize_checkpoint_psbt_takes_the_keys_from_the_signing_configuration() {
    let signers = [11, 5, 6, 2];
    let (_dir, wallet, data, mut params, posted) = checkpoint_to_finalize(&signers).await;
    let id = params.subnet_id;
    let mut state = data.db.get_subnet_state(id).unwrap().unwrap();
    state.committee = (11..=14)
        .map(|n| validator(pubkey(n)))
        .collect::<Vec<_>>()
        .to_committee(&id, state.committee.configuration_number + 1);
    let mut wtxn = data.db.write_txn().unwrap();
    data.db.save_subnet_state(&mut wtxn, id, &state).unwrap();
    wtxn.commit().unwrap();

    let inputs = posted[0].len();
    params.signatures = posted.clone();
    params.signatures.push(vec!["zz".to_string(); inputs]);
    params.signatures.push(posted[1].clone());
    finalize_checkpoint_psbt(Data::new(data), Params(params))
        .await
        .unwrap_or_else(|e| panic!("finalize failed: {}", serde_json::to_string(&e).unwrap()));

    let wallet_state = wallet.0.lock().unwrap();
    assert_eq!(wallet_state.submitted.len(), 1);
    let tx = &wallet_state.submitted[0];
    let mut whitelist: Vec<_> = (1..=6).map(pubkey).collect();
    whitelist.sort();
    for (input_index, input) in tx.input.iter().enumerate() {
        let witness: Vec<&[u8]> = input.witness.iter().collect();
        let (keys, _) = multisig::parse_multisig_script(bitcoin::Script::from_bytes(
            witness[witness.len() - 2],
        ))
        .unwrap();
        assert_eq!(
            keys.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            whitelist
        );
        assert_eq!(witness.len(), keys.len() + 2);
        // signatures in reverse key order
        for (position, (key, _)) in keys.iter().enumerate() {
            let expected = signers
                .iter()
                .position(|n| pubkey(*n) == *key)
                .map(|signer| hex::decode(&posted[signer][input_index]).unwrap())
                .unwrap_or_default();
            assert_eq!(
                witness[keys.len() - 1 - position],
                expected,
                "input {input_index}"
            );
        }
    }
    multisig::tests::verify_transaction(tx, |outpoint| {
        wallet_state
            .funds
            .iter()
            .find(|fund| fund.txid == outpoint.txid && fund.vout == outpoint.vout)
            .map(|fund| bitcoin::TxOut {
                value: fund.amount,
                script_pubkey: fund.script_pub_key.clone(),
            })
    })
    .unwrap();
}

#[tokio::test]
async fn finalize_checkpoint_psbt_with_a_malformed_signature_below_the_threshold_submits_nothing() {
    let (_dir, wallet, data, mut params, mut posted) = checkpoint_to_finalize(&[1, 2, 3]).await;
    posted[2] = vec!["zz".to_string(); posted[2].len()];
    params.signatures = posted;
    assert!(finalize_checkpoint_psbt(Data::new(data), Params(params))
        .await
        .is_err());
    assert!(wallet.0.lock().unwrap().submitted.is_empty());
}

#[tokio::test]
async fn checkpoint_carrying_slash_pre_signatures_without_them_is_rejected() {
    let f = EpochZeroFixture::new([9000; 4]).await;
    let request = f.message(&[]);
    let wallet = WalletRpc::default();
    let (_dir, data) = provider(f, &wallet);
    let mut params = serde_json::to_value(&request).unwrap();
    params["slash_pre_signature_count"] = 2.into();
    let err = gen_checkpoint_psbt(Data::new(data), Params(params))
        .await
        .err()
        .expect("a checkpoint without the slash pre-signatures it carries was accepted");
    assert!(serde_json::to_string(&err)
        .unwrap()
        .contains("slash_pre_signatures missing"));
}

#[tokio::test]
async fn checkpoint_epochs_across_periods_and_epoch_lengths() {
    let cells = [30_u64, 50, 100].into_iter().flat_map(|p| {
        [p, p + 15, 2 * p + 15, 3 * p + 15]
            .into_iter()
            .map(move |k| (p, k))
    });
    for (p, k) in cells.chain([(30, 10)]) {
        let full_invariants = k >= p + 15;
        let h_boot = 102;
        let h_zero = h_boot + p;
        let end = h_zero + 8 * k;
        let f = EpochZeroFixture::new([9000; 4]).await;
        let id = f.id;
        let mut genesis = f.db.get_subnet_genesis_info(id).unwrap().unwrap();
        genesis.create_subnet_msg.bottomup_check_period = p;
        genesis.create_subnet_msg.epoch_length = k;
        let mut txn = f.db.write_txn().unwrap();
        f.db.save_subnet_genesis_info(&mut txn, id, &genesis)
            .unwrap();
        txn.commit().unwrap();
        let mut request = f.message(&[1, 2, 3, 4]);
        request.change_address = None;
        let whitelist = f.signer().address;
        let wallet = WalletRpc::default();
        wallet.0.lock().unwrap().funds = fund_utxos(&whitelist, 3);
        let (_dir, data) = provider(f, &wallet);
        let db = &*data.db;
        let mut recorded = Vec::new();
        let mut configurations = std::collections::BTreeMap::<u64, SubnetCommittee>::new();
        let mut saw_entry = false;
        let mut saw_departure = false;

        for height in h_boot + 1..=end {
            let hash = BlockHash::from_byte_array([height as u8; 32]);
            // A fifth UE collateral becomes slashable in a batch reveal;
            // its timeout removes it at H_5, after several unchanged epochs.
            if full_invariants && height == h_zero + k + 1 {
                let state = db.get_subnet_state(id).unwrap().unwrap();
                let mut join = checked(id, 5, 25, 15);
                join.committee_number = state.committee_number;
                join.msg = IpcJoinUeMsg::new_signed(
                    &Secp256k1::new(),
                    join.msg.join.clone(),
                    (h_zero + 8 * k).try_into().unwrap(),
                    &secret(5),
                    &secret(25),
                    join.msg.nonce_commitment,
                );
                let mut txn = db.write_txn().unwrap();
                process_join_ue(
                    db,
                    &mut txn,
                    &join,
                    Txid::from_byte_array([35; 32]),
                    height,
                    hash,
                )
                .unwrap();
                txn.commit().unwrap();
                let record = db
                    .get_ue_collateral(id, Txid::from_byte_array([35; 32]), &pubkey(5))
                    .unwrap()
                    .unwrap();
                request.slash_signatures = vec![slash_entry(db, &record, &[1, 2, 3])];
            }

            // Transactions precede cutoff processing, including at H_e.
            if (height - h_boot) % p == 0 {
                let before = db.get_subnet_state(id).unwrap().unwrap();
                request.checkpoint_height = height - h_boot;
                request.next_committee_configuration_number =
                    before.latest_committee().configuration_number;
                let funds = wallet.0.lock().unwrap().funds.clone();
                let (psbt, reveal) = provider_checkpoint_and_reveal(&data, request.clone()).await;
                let tx = psbt.unsigned_tx;
                let parsed =
                    IpcCheckpointSubnetMsg::from_checkpoint_tx(db, &db.read_txn().unwrap(), &tx)
                        .unwrap();
                let mut txn = db.write_txn().unwrap();
                let cp = parsed
                    .save_to_db(db, &mut txn, height, hash, tx.compute_txid())
                    .unwrap();
                txn.commit().unwrap();
                let expected_epoch = if height <= h_zero {
                    0
                } else {
                    (height - h_zero - 1) / k + 1
                };
                assert_eq!(
                    before.epoch, expected_epoch,
                    "P={p}, K={k}: cutoff-height convention"
                );
                assert_eq!(
                    cp.is_rotation_checkpoint,
                    before.epoch > before.last_rotation_epoch,
                    "P={p}, K={k}, Bitcoin height {height}: provider rotation flag"
                );
                if cp.is_rotation_checkpoint {
                    assert_eq!(
                        cp.epoch, before.epoch,
                        "P={p}, K={k}: late rotation's own epoch"
                    );
                }
                if full_invariants {
                    let destination = if cp.epoch == 0 {
                        whitelist.clone()
                    } else {
                        assert_eq!(
                            cp.next_configuration_number,
                            configurations[&cp.epoch].configuration_number,
                            "P={p}, K={k}: recorded change number (invariant 6)"
                        );
                        configurations[&cp.epoch].address_checked()
                    };
                    assert_eq!(
                        tx.output.last().unwrap().script_pubkey,
                        destination.script_pubkey(),
                        "P={p}, K={k}, epoch {}: change multisig (invariant 3)",
                        cp.epoch
                    );
                    let previous = if before.last_rotation_epoch == 0 {
                        whitelist.clone()
                    } else {
                        before.committee.address_checked()
                    };
                    if cp.is_rotation_checkpoint && previous != destination {
                        let spent: std::collections::BTreeSet<_> =
                            tx.input.iter().map(|vin| vin.previous_output).collect();
                        assert!(
                            funds
                                .iter()
                                .any(|u| u.script_pub_key == previous.script_pubkey()),
                            "P={p}, K={k}: sweep fixture must have previous funds"
                        );
                        assert!(
                            funds
                                .iter()
                                .filter(|u| u.script_pub_key == previous.script_pubkey())
                                .all(|u| spent.contains(&OutPoint::new(u.txid, u.vout))),
                            "P={p}, K={k}, epoch {}: previous funds swept (invariant 4)",
                            cp.epoch
                        );
                    }
                }
                let stored = db
                    .get_checkpoint(id, cp.checkpoint_number)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    (stored.epoch, stored.is_rotation_checkpoint),
                    (cp.epoch, cp.is_rotation_checkpoint)
                );
                recorded.push(cp);
                let mut txn = db.write_txn().unwrap();
                db.save_transaction(&mut txn, &tx).unwrap();
                txn.commit().unwrap();
                // The wallet holds the unspent outputs, including the change.
                {
                    let mut state = wallet.0.lock().unwrap();
                    state.funds.retain(|u| {
                        !tx.input
                            .iter()
                            .any(|vin| vin.previous_output == OutPoint::new(u.txid, u.vout))
                    });
                    let mut change = funds[0].clone();
                    change.txid = tx.compute_txid();
                    change.vout = (tx.output.len() - 1).try_into().unwrap();
                    change.script_pub_key = tx.output.last().unwrap().script_pubkey.clone();
                    change.address = parsed.change_address.clone();
                    change.amount = tx.output.last().unwrap().value;
                    state.funds.push(change);
                }
                // The pre-signatures reach the database through the batch transfer reveal.
                if let Some(reveal) = reveal {
                    let (_, IpcMessage::BatchTransfer(mut batch)) =
                        IpcMessage::from_reveal_inputs(&reveal)
                            .into_iter()
                            .next()
                            .unwrap()
                    else {
                        panic!("batch reveal")
                    };
                    let mut txn = db.write_txn().unwrap();
                    batch
                        .process(db, &mut txn, height, hash, reveal.compute_txid())
                        .unwrap();
                    txn.commit().unwrap();
                }
                request.slash_signatures.clear();
            }

            // No height is skipped: process_cutoffs observes every H_e.
            let mut txn = db.write_txn().unwrap();
            let outcomes = crate::epochs::process_cutoffs(db, &mut txn, height, hash).unwrap();
            txn.commit().unwrap();
            for outcome in outcomes {
                assert_eq!(outcome.subnet_id, id);
                let current = db
                    .get_subnet_state(id)
                    .unwrap()
                    .unwrap()
                    .latest_committee()
                    .clone();
                if full_invariants {
                    let changes = db.get_all_stake_changes(id).unwrap();
                    let at_cutoff: Vec<_> = changes
                        .iter()
                        .filter(|c| c.block_height == height)
                        .collect();
                    let previous_number = configurations
                        .values()
                        .next_back()
                        .map_or(0, |c: &SubnetCommittee| c.configuration_number);
                    assert_eq!(
                        current.configuration_number,
                        at_cutoff
                            .last()
                            .map_or(previous_number, |c| c.configuration_number),
                        "P={p}, K={k}, epoch {}: change number (invariant 6)",
                        outcome.epoch
                    );
                    if !at_cutoff.is_empty() {
                        assert!(current.configuration_number > previous_number);
                    }
                    let has_five = current.is_validator(&pubkey(5));
                    let had_five = configurations
                        .values()
                        .next_back()
                        .is_some_and(|c| c.is_validator(&pubkey(5)));
                    if has_five != had_five {
                        assert!(
                            current.configuration_number > previous_number,
                            "P={p}, K={k}: entry/departure increases change number (invariant 6)"
                        );
                    }
                    saw_departure |= saw_entry && !has_five;
                    saw_entry |= has_five;
                }
                configurations.insert(outcome.epoch, current);
            }
            let last_recorded_rotation = recorded
                .iter()
                .rev()
                .find(|cp| cp.is_rotation_checkpoint)
                .map_or(0, |cp| cp.epoch);
            assert_eq!(
                db.get_subnet_state(id)
                    .unwrap()
                    .unwrap()
                    .last_rotation_epoch,
                last_recorded_rotation,
                "P={p}, K={k}, height {height}: a cutoff cannot invent a rotation"
            );
        }
        let state = db.get_subnet_state(id).unwrap().unwrap();
        assert_eq!(state.h_0, Some(h_zero), "P={p}, K={k}: activation fixture");
        let mut empty_epochs = 0;
        for epoch in 1..state.epoch {
            let checkpoints: Vec<_> = recorded
                .iter()
                .filter(|cp| {
                    h_zero + (epoch - 1) * k < cp.block_height
                        && cp.block_height <= h_zero + epoch * k
                })
                .collect();
            let rotations = checkpoints
                .iter()
                .filter(|cp| cp.is_rotation_checkpoint)
                .count();
            assert!(
                rotations <= 1,
                "P={p}, K={k}, epoch {epoch}: never two rotations"
            );
            if checkpoints.is_empty() {
                empty_epochs += 1;
                assert_eq!(rotations, 0);
            } else {
                assert!(
                    checkpoints[0].is_rotation_checkpoint,
                    "P={p}, K={k}, epoch {epoch}: next checkpoint rotates its own epoch"
                );
                assert!(checkpoints[1..].iter().all(|cp| !cp.is_rotation_checkpoint));
            }
            if full_invariants {
                assert_eq!(rotations, 1, "P={p}, K={k}, epoch {epoch}: invariant 1");
            }
        }
        if full_invariants {
            assert!(
                saw_entry && saw_departure,
                "P={p}, K={k}: exercise entry and departure"
            );
        } else if k < p {
            assert!(
                empty_epochs > 0,
                "K<P fixture must exercise an epoch with no checkpoint"
            );
        }
    }
}

#[tokio::test]
async fn epoch_one_sweeps_whitelist_funds_and_routes_deposits_when_c_1_equals_c_gen() {
    let f = EpochZeroFixture::new([135; 4]).await;
    let first = f.message(&[1, 2, 3, 4]);
    let (tx, reveal) = f.transactions(&first);
    f.accept(&first, &tx, 110);
    f.reveal(&reveal, 111).unwrap();
    f.cutoff(111);

    let id = f.id;
    let state = f.state();
    assert_eq!(state.h_0, Some(111));
    assert_eq!(state.epoch, 1);
    assert_eq!(state.last_rotation_epoch, 0);
    let g = f.db.get_subnet_genesis_info(id).unwrap().unwrap();
    let c_gen = g.to_subnet().committee;
    let c_one = state.latest_committee().clone();
    assert_eq!(
        c_one.validator_weighted_keys(),
        c_gen.validator_weighted_keys()
    );
    assert_eq!(c_one.threshold, c_gen.threshold);
    assert_eq!(c_one.multisig_address, c_gen.multisig_address);
    assert_eq!(c_one.configuration_number, 0);
    assert!(f.db.get_all_stake_changes(id).unwrap().is_empty());
    let whitelist = f.signer().address;
    let destination = c_one.address_checked();
    assert_ne!(whitelist, destination);
    let funds = fund_utxos(&whitelist, 3);
    let wallet = WalletRpc::default();
    wallet.0.lock().unwrap().funds = funds.clone();
    let mut request = f.message(&[]);
    request.checkpoint_height = 60;
    request.change_address = None;
    let (_dir, data) = provider(f, &wallet);

    fund_subnet(
        Data::new(data.clone()),
        Params(IpcFundSubnetMsg {
            subnet_id: id,
            amount: Amount::from_sat(555_000),
            address: alloy_primitives::Address::from([7; 20]),
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("deposit RPC failed: {}", serde_json::to_string(&e).unwrap()));
    {
        let state = wallet.0.lock().unwrap();
        assert_eq!(state.submitted.len(), 1);
        assert_eq!(
            state.submitted[0].output[1].value,
            Amount::from_sat(555_000)
        );
        assert_eq!(
            state.submitted[0].output[1].script_pubkey,
            destination.script_pubkey()
        );
    }

    let psbt = provider_checkpoint(&data, request).await;
    let tx = &psbt.unsigned_tx;
    assert_eq!(
        tx.output[0].script_pubkey.as_bytes().last().copied(),
        Some(0x04)
    );
    let actual: std::collections::BTreeSet<_> =
        tx.input.iter().map(|i| i.previous_output).collect();
    let expected: std::collections::BTreeSet<_> = funds
        .iter()
        .map(|u| OutPoint::new(u.txid, u.vout))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(tx.input.len(), funds.len());
    assert!(psbt
        .inputs
        .iter()
        .all(|i| { i.witness_utxo.as_ref().unwrap().script_pubkey == whitelist.script_pubkey() }));
    assert_eq!(
        tx.output.last().unwrap().script_pubkey,
        destination.script_pubkey()
    );
    let msg =
        IpcCheckpointSubnetMsg::from_checkpoint_tx(&*data.db, &data.db.read_txn().unwrap(), tx)
            .unwrap();
    assert!(msg.is_rotation_checkpoint);
    assert_eq!(msg.next_committee_configuration_number, 0);
    let mut txn = data.db.write_txn().unwrap();
    let cp = msg
        .save_to_db(
            &*data.db,
            &mut txn,
            113,
            BlockHash::from_byte_array([113; 32]),
            tx.compute_txid(),
        )
        .unwrap();
    txn.commit().unwrap();
    assert!(cp.is_rotation_checkpoint);
    assert_eq!(cp.epoch, 1);
    assert_eq!(
        data.db
            .get_subnet_state(id)
            .unwrap()
            .unwrap()
            .last_rotation_epoch,
        1
    );
    assert!(data.db.get_all_stake_changes(id).unwrap().is_empty());
}

#[tokio::test]
async fn unchanged_epoch_has_a_flagged_rotation_without_updates_or_a_fund_sweep() {
    let f = EpochZeroFixture::new([1000; 4]).await;
    let first = f.message(&[1, 2, 3, 4]);
    let (tx, reveal) = f.transactions(&first);
    f.accept(&first, &tx, 110);
    f.reveal(&reveal, 111).unwrap();
    f.cutoff(111);
    let mut txn = f.db.write_txn().unwrap();
    rotation(&f, 4)
        .save_to_db(
            &f.db,
            &mut txn,
            112,
            BlockHash::from_byte_array([112; 32]),
            Txid::from_byte_array([0x97; 32]),
        )
        .unwrap();
    txn.commit().unwrap();
    let changes = serde_json::to_value(f.db.get_all_stake_changes(f.id).unwrap()).unwrap();
    let previous = f.state().committee;
    f.cutoff(121);
    let state = f.state();
    assert_eq!(state.epoch, 2);
    assert_eq!(state.last_rotation_epoch, 1);
    assert_eq!(
        state.latest_committee().multisig_address,
        previous.multisig_address
    );
    assert_eq!(state.latest_committee().configuration_number, 4);
    assert!(state.waiting_committee.is_none());
    assert_eq!(
        serde_json::to_value(f.db.get_all_stake_changes(f.id).unwrap()).unwrap(),
        changes
    );

    let id = f.id;
    let address = previous.address_checked();
    let funds = fund_utxos(&address, 128);
    let wallet = WalletRpc::default();
    wallet.0.lock().unwrap().funds = funds.clone();
    let mut request = f.message(&[]);
    request.checkpoint_height = 90;
    request.next_committee_configuration_number = 4;
    request.change_address = None;
    let (_dir, data) = provider(f, &wallet);
    let psbt = provider_checkpoint(&data, request).await;
    let tx = &psbt.unsigned_tx;
    assert_eq!(
        tx.output[0].script_pubkey.as_bytes().last().copied(),
        Some(0x04)
    );
    assert!(!tx.input.is_empty());
    // The fees fit in one fund UTXO: the unchanged multisig does not force all of them.
    assert!(
        tx.input.len() < funds.len(),
        "an unchanged multisig must not force a sweep"
    );
    assert!(tx.input.iter().all(|i| funds
        .iter()
        .any(|u| { i.previous_output == OutPoint::new(u.txid, u.vout) })));
    assert_eq!(
        tx.output.last().unwrap().script_pubkey,
        address.script_pubkey()
    );
    let msg =
        IpcCheckpointSubnetMsg::from_checkpoint_tx(&*data.db, &data.db.read_txn().unwrap(), tx)
            .unwrap();
    assert!(msg.is_rotation_checkpoint);
    assert_eq!(msg.next_committee_configuration_number, 4);
    let mut txn = data.db.write_txn().unwrap();
    let cp = msg
        .save_to_db(
            &*data.db,
            &mut txn,
            122,
            BlockHash::from_byte_array([122; 32]),
            tx.compute_txid(),
        )
        .unwrap();
    txn.commit().unwrap();
    assert!(cp.is_rotation_checkpoint);
    assert_eq!(cp.epoch, 2);
    let state = data.db.get_subnet_state(id).unwrap().unwrap();
    assert_eq!(state.last_rotation_epoch, 2);
    assert_eq!(state.committee.configuration_number, 4);
    assert_eq!(
        serde_json::to_value(data.db.get_all_stake_changes(id).unwrap()).unwrap(),
        changes
    );
    assert!(data
        .db
        .get_unconfirmed_stake_changes(id)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn unslashable_outputs_leave_out_spent_and_slashable_collaterals_oldest_first() {
    let f = EpochZeroFixture::new([1000; 4]).await;
    let id = f.id;
    let record = |n: u8| {
        f.db.get_ue_collaterals(id)
            .unwrap()
            .into_iter()
            .find(|r| r.pubkey == pubkey(n))
            .unwrap()
    };
    // Validator 1 joins at a later Bitcoin block; validator 5's output is in the transaction of
    // validator 4's.
    let mut later =
        f.db.get_eots_commitments(id, &record(1).pk_eots)
            .unwrap()
            .remove(0);
    later.block_height = 105;
    let mut shared = record(4);
    shared.pubkey = pubkey(5);
    shared.outpoint.vout = 0;
    let slashable = SlashPresigRecord {
        subnet_id: id,
        outpoint: record(2).outpoint,
        signatures: slash_entry(&f.db, &record(2), &[1, 2, 3]).signatures,
        block_height: 103,
        batch_transfer_txid: Txid::from_byte_array([0x71; 32]),
    };
    let mut txn = f.db.write_txn().unwrap();
    f.db.add_eots_commitment(&mut txn, &later).unwrap();
    f.db.add_ue_collateral(&mut txn, &shared).unwrap();
    f.db.add_slash_presigs(&mut txn, &slashable).unwrap();
    f.db.set_ue_collateral_spent(&mut txn, &record(3).outpoint)
        .unwrap();
    txn.commit().unwrap();
    let (first, fourth) = (record(1), record(4));

    let (_dir, data) = provider(f, &WalletRpc::default());
    let outputs =
        get_unslashable_outputs(Data::new(data), Params(GetSubnetParams { subnet_id: id }))
            .await
            .unwrap_or_else(|e| panic!("{}", serde_json::to_string(&e).unwrap()));

    assert_eq!(
        outputs
            .iter()
            .map(|o| (o.pubkey, OutPoint::new(o.txid, o.vout), o.block_height))
            .collect::<Vec<_>>(),
        vec![
            (pubkey(5), shared.outpoint, 102),
            (pubkey(4), fourth.outpoint, 102),
            (pubkey(1), first.outpoint, 105),
        ]
    );
    let mut whitelist: Vec<_> = (1..=6).map(pubkey).collect();
    whitelist.sort();
    for output in &outputs {
        assert_eq!(output.committee_number, 0);
        assert_eq!(
            output.keys.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            whitelist
        );
        assert!(output.signer);
    }
}

#[tokio::test]
async fn fundsubnet_refuses_a_deposit_below_the_subnet_minimum_before_broadcasting() {
    let f = EpochZeroFixture::new([1000; 4]).await;
    let mut genesis = f.db.get_subnet_genesis_info(f.id).unwrap().unwrap();
    genesis.create_subnet_msg.min_deposit = Amount::from_sat(777);
    let mut txn = f.db.write_txn().unwrap();
    f.db.save_subnet_genesis_info(&mut txn, f.id, &genesis)
        .unwrap();
    txn.commit().unwrap();
    let id = f.id;
    let wallet = WalletRpc::default();
    let (_dir, data) = provider(f, &wallet);

    let Err(error) = fund_subnet(
        Data::new(data),
        Params(IpcFundSubnetMsg {
            subnet_id: id,
            amount: Amount::from_sat(776),
            address: alloy_primitives::Address::from([7; 20]),
        }),
    )
    .await
    else {
        panic!("a deposit below the minimum is accepted");
    };

    assert!(serde_json::to_string(&error)
        .unwrap()
        .contains("Deposit must be at least"));
    assert!(wallet.0.lock().unwrap().submitted.is_empty());
}

#[tokio::test]
async fn unstakeue_skips_spent_collateral() {
    let f = EpochZeroFixture::new([1000; 4]).await;
    let spent =
        f.db.get_ue_collaterals(f.id)
            .unwrap()
            .into_iter()
            .find(|r| r.pubkey == pubkey(1))
            .unwrap();
    let mut txn = f.db.write_txn().unwrap();
    f.db.set_ue_collateral_spent(&mut txn, &spent.outpoint)
        .unwrap();
    txn.commit().unwrap();
    let id = f.id;
    let wallet = WalletRpc::default();
    let (_dir, data) = provider(f, &wallet);

    let Err(error) = unstake_ue(Data::new(data), Params(GetSubnetParams { subnet_id: id })).await
    else {
        panic!("an exit of spent collateral is accepted");
    };

    assert!(serde_json::to_string(&error)
        .unwrap()
        .contains("has no unspent join-ue collateral"));
    assert!(wallet.0.lock().unwrap().submitted.is_empty());
}

/// A join that finds no EOTS key generates the key and the nonce seed files, which a provider
/// started again on the same files loads.
/// A provider whose EOTS keys live in `dir`, with the monitor at Bitcoin height 100.
fn provider_with_keys(
    db: crate::db::HeedDb,
    dir: &std::path::Path,
    wallet: &WalletRpc,
    configured: Option<&crate::provider::eots_signer::EotsKeyFiles>,
) -> ServerData {
    let mut txn = db.write_txn().unwrap();
    db.set_last_processed_block(&mut txn, 100).unwrap();
    txn.commit().unwrap();
    wallet.0.lock().unwrap().tip = 100;
    let client = Arc::new(bitcoincore_rpc::Client::from_jsonrpc(
        jsonrpc::Client::with_transport(wallet.clone()),
    ));
    ServerData {
        db: Arc::new(db),
        btc_rpc: client.clone(),
        btc_watchonly_rpc: client,
        validator: Some((pubkey(1), secret(1))),
        eots_keys: Some(Arc::new(
            crate::provider::eots_keys::EotsKeys::load(dir.to_path_buf(), configured).unwrap(),
        )),
    }
}

/// The EOTS key and nonce commitment of the join-ue message in the submitted reveal `txid`.
fn pk_eots_of(
    wallet: &WalletRpc,
    txid: bitcoin::Txid,
) -> (bitcoin::XOnlyPublicKey, crate::eots::EotsNonceCommitment) {
    let reveal = wallet
        .0
        .lock()
        .unwrap()
        .submitted
        .iter()
        .find(|tx| tx.compute_txid() == txid)
        .unwrap()
        .clone();
    let [(_, IpcMessage::JoinUe { msg, .. })] = &IpcMessage::from_reveal_inputs(&reveal)[..] else {
        panic!("the reveal carries one join-ue message");
    };
    (msg.pk_eots, msg.nonce_commitment)
}

fn join(data: &ServerData, id: SubnetId, nonces: (u64, u32)) -> Result<bitcoin::Txid, String> {
    let genesis = data.db.get_subnet_genesis_info(id).unwrap().unwrap();
    join_subnet_ue(
        data,
        &genesis,
        checked(id, 1, 22, 1).msg.join,
        2,
        nonces,
        None,
    )
    .map_err(|e| serde_json::to_string(&e).unwrap())
}

fn join_record(data: &ServerData, id: SubnetId) -> JoinRecord {
    data.eots_keys
        .as_ref()
        .unwrap()
        .join_record(id)
        .unwrap()
        .unwrap()
}

fn submitted_txids(wallet: &WalletRpc) -> Vec<Txid> {
    wallet
        .0
        .lock()
        .unwrap()
        .submitted
        .iter()
        .map(|tx| tx.compute_txid())
        .collect()
}

fn submitted(wallet: &WalletRpc) -> usize {
    wallet.0.lock().unwrap().submitted.len()
}

fn key_files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("eots.") && (n.ends_with(".sk") || n.ends_with(".seed")))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn join_ue_generates_an_eots_key_and_seed_per_subnet() {
    let (dir, db) = database().await;
    let (first, second) = (subnet(93), subnet(94));
    create(&db, first);
    create(&db, second);
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, None);
    assert!(key_files_in(dir.path()).is_empty());

    let first_txid = join(&data, first, (7, 50)).unwrap();
    let second_txid = join(&data, second, (3, 40)).unwrap();
    let (pk_first, commitment_first) = pk_eots_of(&wallet, first_txid);
    let (pk_second, commitment_second) = pk_eots_of(&wallet, second_txid);
    assert_ne!(pk_first, pk_second);
    let keys = data.eots_keys.as_ref().unwrap();
    let files_of = |id| crate::provider::eots_signer::EotsKeyFiles::of_subnet(dir.path(), id);
    let hex20 = |n: u8| hex::encode([n; 20]);
    assert_eq!(
        key_files_in(dir.path()),
        [
            format!("eots.{}.seed", hex20(93)),
            format!("eots.{}.sk", hex20(93)),
            format!("eots.{}.seed", hex20(94)),
            format!("eots.{}.sk", hex20(94)),
        ]
    );
    let seed_of = |id| {
        crate::eots::EotsSeed::from_bytes(
            crate::provider::eots_signer::read_hex32(&files_of(id).seed).unwrap(),
        )
    };
    assert_ne!(seed_of(first), seed_of(second));
    assert_eq!(
        commitment_first,
        crate::eots::commit_nonces(&seed_of(first), &first, 7, 50)
            .unwrap()
            .0
    );
    assert_eq!(
        commitment_second,
        crate::eots::commit_nonces(&seed_of(second), &second, 3, 40)
            .unwrap()
            .0
    );
    let no_join = |_: &bitcoin::XOnlyPublicKey| Ok(None);
    for (id, pk) in [(first, pk_first), (second, pk_second)] {
        let signer = keys.signer_of(id, no_join).unwrap().unwrap();
        let signer = signer.lock().unwrap();
        assert_eq!(signer.pk_eots(), pk);
        assert_eq!(signer.key_path(), files_of(id).key);
        assert_eq!(
            keys.join_record(id).unwrap().unwrap().pk_eots,
            pk,
            "the join sent is recorded"
        );
    }
    assert_eq!(
        keys.join_record(first).unwrap().unwrap().txid(),
        Some(first_txid)
    );
    assert!(keys.signer_of(subnet(95), no_join).unwrap().is_none());

    // The vote databases stay open in `data`: the restart loads a copy of the files
    let again = tempfile::tempdir().unwrap();
    for name in key_files_in(dir.path()) {
        std::fs::copy(dir.path().join(&name), again.path().join(&name)).unwrap();
    }
    let restarted =
        crate::provider::eots_keys::EotsKeys::load(again.path().to_path_buf(), None).unwrap();
    assert_eq!(restarted.signers().len(), 2);
    for (id, pk) in [(first, pk_first), (second, pk_second)] {
        let signer = restarted.signer_of(id, no_join).unwrap().unwrap();
        let mut signer = signer.lock().unwrap();
        assert_eq!(signer.pk_eots(), pk);
        assert_eq!(signer.seed(), seed_of(id));
        // The vote at a committed height is signed by the subnet's key
        let commitment = if id == first {
            commitment_first
        } else {
            commitment_second
        };
        let vote = signer
            .sign(id, &[commitment], commitment.start, &[0x1b; 32])
            .unwrap();
        assert_eq!(vote.pk_eots, pk.serialize());
    }
}

#[tokio::test]
async fn join_ue_is_refused_while_an_earlier_join_awaits_the_monitor() {
    let (dir, db) = database().await;
    let id = subnet(92);
    create(&db, id);
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, None);
    let keys = data.eots_keys.as_ref().unwrap();

    let txid = join(&data, id, (7, 50)).unwrap();
    let (pk_eots, _) = pk_eots_of(&wallet, txid);
    assert_eq!(submitted(&wallet), 2, "commit and reveal");
    let files_before = key_files_in(dir.path());

    // In the mempool
    let refused = join(&data, id, (7, 50)).unwrap_err();
    assert!(refused.contains("unconfirmed, in the mempool"), "{refused}");
    assert!(refused.contains(&txid.to_string()), "{refused}");
    assert_eq!(submitted(&wallet), 2, "no transaction");

    // In a Bitcoin block the monitor has not processed
    wallet.0.lock().unwrap().mined.insert(txid, 103);
    let refused = join(&data, id, (7, 50)).unwrap_err();
    assert!(refused.contains("in Bitcoin block 103"), "{refused}");
    assert!(refused.contains("last processed 100"), "{refused}");
    assert_eq!(submitted(&wallet), 2);
    assert_eq!(key_files_in(dir.path()), files_before, "no new key");
    assert_eq!(keys.join_record(id).unwrap().unwrap().txid(), Some(txid));

    // Processed by the monitor without being accepted: the join is sent again with the same key
    wallet.0.lock().unwrap().mined.insert(txid, 99);
    let retry = join(&data, id, (7, 50)).unwrap();
    assert_ne!(retry, txid);
    assert_eq!(submitted(&wallet), 4);
    assert_eq!(pk_eots_of(&wallet, retry).0, pk_eots);
    assert_eq!(key_files_in(dir.path()), files_before);
    assert_eq!(keys.join_record(id).unwrap().unwrap().txid(), Some(retry));

    // The pair dropped from the mempool: sent again
    let commit = wallet.0.lock().unwrap().submitted[2].compute_txid();
    wallet.0.lock().unwrap().dropped.extend([commit, retry]);
    let dropped = join(&data, id, (7, 50)).unwrap();
    assert_eq!(submitted(&wallet), 6);
    assert_eq!(pk_eots_of(&wallet, dropped).0, pk_eots);

    // The pair unknown to the wallet: sent again
    let commit = wallet.0.lock().unwrap().submitted[4].compute_txid();
    wallet.0.lock().unwrap().forgotten.extend([commit, dropped]);
    assert!(join(&data, id, (7, 50)).is_ok());
    assert_eq!(submitted(&wallet), 8);
}

#[tokio::test]
async fn join_ue_is_refused_after_an_accepted_join_of_the_subnet() {
    let (dir, db) = database().await;
    let id = subnet(91);
    create(&db, id);
    let accepted = checked(id, 1, 22, 1);
    let mut txn = db.write_txn().unwrap();
    process_join_ue(
        &db,
        &mut txn,
        &accepted,
        Txid::from_byte_array([35; 32]),
        99,
        BlockHash::from_byte_array([99; 32]),
    )
    .unwrap();
    txn.commit().unwrap();
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, None);

    let refused = join(&data, id, (7, 50)).unwrap_err();
    assert!(refused.contains("a key joins a subnet once"), "{refused}");
    assert_eq!(submitted(&wallet), 0, "no transaction");
    assert!(key_files_in(dir.path()).is_empty(), "no new key");
    // Another subnet is open to the validator
    create(&data.db, subnet(90));
    assert!(join(&data, subnet(90), (7, 50)).is_ok());
}

#[tokio::test]
async fn a_configured_eots_key_joins_and_votes_as_before() {
    let (dir, db) = database().await;
    let id = subnet(89);
    create(&db, id);
    let configured = crate::provider::eots_signer::EotsKeyFiles {
        key: dir.path().join("configured").join("eots.sk"),
        seed: dir.path().join("configured").join("eots.seed"),
    };
    std::fs::create_dir_all(dir.path().join("configured")).unwrap();
    std::fs::write(&configured.key, hex::encode(secret(21).secret_bytes())).unwrap();
    std::fs::write(&configured.seed, hex::encode([0x5e; 32])).unwrap();
    let seed = crate::eots::EotsSeed::from_bytes([0x5e; 32]);
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, Some(&configured));
    let keys = data.eots_keys.as_ref().unwrap();

    let txid = join(&data, id, (7, 50)).unwrap();
    let (pk_eots, commitment) = pk_eots_of(&wallet, txid);
    assert_eq!(pk_eots, pubkey(21));
    assert_eq!(
        commitment,
        crate::eots::commit_nonces(&seed, &id, 7, 50).unwrap().0
    );
    assert!(key_files_in(dir.path()).is_empty(), "no key generated");
    // The join accepted, the vote of the subnet is signed with the configured key
    let mut txn = data.db.write_txn().unwrap();
    process_join_ue(
        &*data.db,
        &mut txn,
        &checked(id, 1, 21, 1),
        txid,
        99,
        BlockHash::from_byte_array([99; 32]),
    )
    .unwrap();
    txn.commit().unwrap();
    let signer = eots_signer_of(&data, id)
        .unwrap_or_else(|e| panic!("{}", serde_json::to_string(&e).unwrap()))
        .expect("the key of the subnet");
    assert_eq!(signer.lock().unwrap().pk_eots(), pubkey(21));
    // Another subnet gets its own key, and the join to it is not refused
    let other = subnet(88);
    create(&data.db, other);
    let other_txid = join(&data, other, (7, 50)).unwrap();
    assert_ne!(pk_eots_of(&wallet, other_txid).0, pubkey(21));
    assert_eq!(key_files_in(dir.path()).len(), 2);
    assert_eq!(
        keys.signer_of(other, |_| Ok(None))
            .unwrap()
            .unwrap()
            .lock()
            .unwrap()
            .pk_eots(),
        pk_eots_of(&wallet, other_txid).0
    );
    assert!(eots_signer_of(&data, subnet(87))
        .unwrap_or_else(|e| panic!("{}", serde_json::to_string(&e).unwrap()))
        .is_none());
}

/// A join to `id` with `initial_balance`, through the handler's join-ue path.
fn join_with_balance(
    data: &ServerData,
    id: SubnetId,
    initial_balance: Option<InitialBalance>,
) -> Result<bitcoin::Txid, String> {
    let genesis = data.db.get_subnet_genesis_info(id).unwrap().unwrap();
    join_subnet_ue(
        data,
        &genesis,
        checked(id, 1, 22, 1).msg.join,
        2,
        (7, 50),
        initial_balance,
    )
    .map_err(|e| serde_json::to_string(&e).unwrap())
}

fn set_processed(data: &ServerData, height: u64) {
    let mut txn = data.db.write_txn().unwrap();
    data.db.set_last_processed_block(&mut txn, height).unwrap();
    txn.commit().unwrap();
}

fn configured_key(dir: &std::path::Path) -> crate::provider::eots_signer::EotsKeyFiles {
    let files = crate::provider::eots_signer::EotsKeyFiles {
        key: dir.join("configured").join("eots.sk"),
        seed: dir.join("configured").join("eots.seed"),
    };
    drop(files.load_or_create().unwrap());
    files
}

/// Two joins at once: `a` and `b` start together, each returns the result of its join.
fn join_both(
    data: &Arc<ServerData>,
    a: SubnetId,
    b: SubnetId,
) -> [Result<bitcoin::Txid, String>; 2] {
    let barrier = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        let threads = [a, b].map(|id| {
            let (data, barrier) = (data.clone(), barrier.clone());
            scope.spawn(move || {
                barrier.wait();
                join(&data, id, (7, 50))
            })
        });
        threads.map(|t| t.join().unwrap())
    })
}

#[tokio::test]
async fn joins_at_the_same_time_send_one_pair_per_subnet_and_never_share_a_key() {
    let (dir, db) = database().await;
    let id = subnet(89);
    create(&db, id);
    let wallet = WalletRpc::default();
    let data = Arc::new(provider_with_keys(db, dir.path(), &wallet, None));

    // To one subnet: one join is sent, the other finds it unconfirmed
    let results = join_both(&data, id, id);
    let sent: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
    let refused: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(sent.len(), 1, "{results:?}");
    assert!(
        refused[0].contains("unconfirmed, in the mempool"),
        "{refused:?}"
    );
    assert_eq!(submitted(&wallet), 2, "one commit and one reveal");
    drop(data);

    // To two subnets with an unused configured key: one join carries it, the other a new key
    let (dir, db) = database().await;
    let (first, second) = (subnet(88), subnet(87));
    create(&db, first);
    create(&db, second);
    let wallet = WalletRpc::default();
    let configured = configured_key(dir.path());
    let data = Arc::new(provider_with_keys(
        db,
        dir.path(),
        &wallet,
        Some(&configured),
    ));
    let pk_configured = data.eots_keys.as_ref().unwrap().signers()[0]
        .lock()
        .unwrap()
        .pk_eots();
    let [a, b] = join_both(&data, first, second);
    let (pk_a, _) = pk_eots_of(&wallet, a.unwrap());
    let (pk_b, _) = pk_eots_of(&wallet, b.unwrap());
    assert_eq!(submitted(&wallet), 4);
    assert_ne!(pk_a, pk_b, "one EOTS key in joins to two subnets");
    assert!(pk_a == pk_configured || pk_b == pk_configured);
}

#[tokio::test]
async fn the_configured_key_stays_with_the_subnet_it_was_chosen_for_after_a_failed_send() {
    let (dir, db) = database().await;
    let (first, second) = (subnet(86), subnet(85));
    create(&db, first);
    create(&db, second);
    let wallet = WalletRpc::default();
    let configured = configured_key(dir.path());
    let data = provider_with_keys(db, dir.path(), &wallet, Some(&configured));
    let keys = data.eots_keys.as_ref().unwrap();
    let pk_configured = keys.signers()[0].lock().unwrap().pk_eots();

    // The node refuses the commit transaction of the first join
    wallet
        .0
        .lock()
        .unwrap()
        .send_failures
        .push_back(Some(SendFailure::Refused));
    let failed = join(&data, first, (7, 50)).unwrap_err();
    assert!(failed.contains("connection lost"), "{failed}");
    assert_eq!(submitted(&wallet), 0);
    assert_eq!(
        keys.join_record(first).unwrap().unwrap().pk_eots,
        pk_configured,
        "the configured key is bound to the first subnet"
    );

    // A join to another subnet gets a new key, and the retry keeps the configured key
    let (pk_second, _) = pk_eots_of(&wallet, join(&data, second, (7, 50)).unwrap());
    assert_ne!(pk_second, pk_configured);
    let (pk_first, _) = pk_eots_of(&wallet, join(&data, first, (7, 50)).unwrap());
    assert_eq!(pk_first, pk_configured);
    assert_eq!(submitted(&wallet), 4);
}

#[tokio::test]
async fn a_join_that_may_have_reached_bitcoin_is_known_across_a_restart() {
    let (dir, db) = database().await;
    let id = subnet(84);
    create(&db, id);
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, None);
    let keys = data.eots_keys.as_ref().unwrap();

    // The join record cannot be written: nothing is sent
    let blocked = crate::provider::eots_keys::subnet_file(dir.path(), id, "join.tmp");
    std::fs::create_dir(&blocked).unwrap();
    let failed = join(&data, id, (7, 50)).unwrap_err();
    std::fs::remove_dir(&blocked).unwrap();
    assert!(failed.contains("join record"), "{failed}");
    assert_eq!(submitted(&wallet), 0, "no transaction without its record");

    // The response to the reveal transaction is lost: the join is recorded and refused later
    wallet
        .0
        .lock()
        .unwrap()
        .send_failures
        .extend([None, Some(SendFailure::Lost)]);
    let lost = join(&data, id, (7, 50)).unwrap_err();
    assert!(lost.contains("connection lost"), "{lost}");
    assert_eq!(submitted(&wallet), 2, "both reached the node");
    let txid = wallet.0.lock().unwrap().submitted[1].compute_txid();
    assert_eq!(keys.join_record(id).unwrap().unwrap().txid(), Some(txid));
    let refused = join(&data, id, (7, 50)).unwrap_err();
    assert!(refused.contains("unconfirmed, in the mempool"), "{refused}");
    assert!(refused.contains(&txid.to_string()), "{refused}");
    assert_eq!(submitted(&wallet), 2);

    // After a restart as well (one vote database opens once per process: the keys are dropped)
    let ServerData { db, eots_keys, .. } = data;
    drop(eots_keys);
    let db = Arc::try_unwrap(db).ok().unwrap();
    let data = provider_with_keys(db, dir.path(), &wallet, None);
    let refused = join(&data, id, (7, 50)).unwrap_err();
    assert!(refused.contains("unconfirmed, in the mempool"), "{refused}");
    assert_eq!(submitted(&wallet), 2);

    // The pair dropped: the next join is sent, and its reveal transaction is refused by the node
    let commit = wallet.0.lock().unwrap().submitted[0].compute_txid();
    wallet.0.lock().unwrap().dropped.extend([commit, txid]);
    wallet
        .0
        .lock()
        .unwrap()
        .send_failures
        .extend([None, Some(SendFailure::Refused)]);
    assert!(join(&data, id, (7, 50)).is_err());
    assert_eq!(submitted(&wallet), 3, "the commit transaction alone");
    // The next join sends the recorded reveal transaction again and is refused
    let again = join(&data, id, (7, 50)).unwrap_err();
    assert!(again.contains("sent again, unconfirmed"), "{again}");
    assert_eq!(submitted(&wallet), 4);
    let reveal = wallet.0.lock().unwrap().submitted[3].clone();
    assert_eq!(
        reveal.input[0].previous_output.txid,
        wallet.0.lock().unwrap().submitted[2].compute_txid(),
        "the reveal of the recorded pair"
    );
    assert_eq!(
        data.eots_keys
            .as_ref()
            .unwrap()
            .join_record(id)
            .unwrap()
            .unwrap()
            .txid(),
        Some(reveal.compute_txid())
    );
}

#[tokio::test]
async fn join_ue_is_refused_while_the_monitor_has_not_caught_up() {
    let (dir, db) = database().await;
    let id = subnet(83);
    create(&db, id);
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, None);

    set_processed(&data, 99);
    let refused = join(&data, id, (7, 50)).unwrap_err();
    assert!(
        refused.contains("has processed Bitcoin block 99 of 100"),
        "{refused}"
    );
    assert!(refused.contains("caught up"), "{refused}");
    assert_eq!(submitted(&wallet), 0, "no transaction");
    assert!(key_files_in(dir.path()).is_empty(), "no new key");

    set_processed(&data, 100);
    assert!(join(&data, id, (7, 50)).is_ok());
    assert_eq!(submitted(&wallet), 2);
}

#[tokio::test]
async fn a_refused_join_with_an_initial_balance_sends_no_prefund_transaction() {
    let (dir, db) = database().await;
    let id = subnet(82);
    create(&db, id);
    let accepted = checked(id, 1, 22, 1);
    let mut txn = db.write_txn().unwrap();
    process_join_ue(
        &db,
        &mut txn,
        &accepted,
        Txid::from_byte_array([34; 32]),
        99,
        BlockHash::from_byte_array([98; 32]),
    )
    .unwrap();
    txn.commit().unwrap();
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, None);
    let balance = InitialBalance {
        amount: Amount::from_sat(5_000),
        address: alloy_primitives::Address::from([7; 20]),
    };

    // Joined before: nothing is sent
    let refused = join_with_balance(&data, id, Some(balance.clone())).unwrap_err();
    assert!(refused.contains("a key joins a subnet once"), "{refused}");
    assert_eq!(submitted(&wallet), 0, "no prefund transaction");

    // A join awaiting the monitor: nothing is sent either
    let open = subnet(81);
    create(&data.db, open);
    let txid = join_with_balance(&data, open, Some(balance.clone())).unwrap();
    assert_eq!(submitted(&wallet), 3, "prefund, commit and reveal");
    let prefund = wallet.0.lock().unwrap().submitted[0].clone();
    assert!(
        prefund.output[0].script_pubkey.is_op_return() && prefund.output[1].value.to_sat() == 5_000,
        "the prefund transaction comes first: {prefund:?}"
    );
    assert_eq!(wallet.0.lock().unwrap().submitted[2].compute_txid(), txid);
    let refused = join_with_balance(&data, open, Some(balance)).unwrap_err();
    assert!(refused.contains("unconfirmed, in the mempool"), "{refused}");
    assert_eq!(submitted(&wallet), 3);
}

#[tokio::test]
async fn a_prefund_transaction_that_may_have_reached_bitcoin_is_sent_once() {
    let (dir, db) = database().await;
    let id = subnet(80);
    create(&db, id);
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, None);
    let balance = InitialBalance {
        amount: Amount::from_sat(5_000),
        address: alloy_primitives::Address::from([7; 20]),
    };

    // The response to the prefund transaction is lost: the error names it and the incomplete join
    wallet
        .0
        .lock()
        .unwrap()
        .send_failures
        .push_back(Some(SendFailure::Lost));
    let lost = join_with_balance(&data, id, Some(balance.clone())).unwrap_err();
    assert_eq!(
        submitted(&wallet),
        1,
        "the prefund transaction reached the node"
    );
    let prefund = submitted_txids(&wallet)[0];
    assert!(lost.contains(&prefund.to_string()), "{lost}");
    assert!(lost.contains("incomplete"), "{lost}");
    let record = join_record(&data, id);
    assert_eq!(record.prefund.map(|tx| tx.compute_txid()), Some(prefund));
    assert_eq!(record.transactions, None);

    // The retry, with the same initial balance, sends the pair alone and keeps the record
    let txid = join_with_balance(&data, id, Some(balance.clone())).unwrap();
    assert_eq!(
        submitted_txids(&wallet).len(),
        3,
        "prefund, commit and reveal"
    );
    assert_eq!(submitted_txids(&wallet)[2], txid);
    let record = join_record(&data, id);
    assert_eq!(
        record.prefund.as_ref().map(|tx| tx.compute_txid()),
        Some(prefund)
    );
    assert_eq!(record.txid(), Some(txid));

    // The pair dropped and the prefund transaction mined: a join without an initial balance sends
    // the pair alone
    let commit = submitted_txids(&wallet)[1];
    {
        let mut state = wallet.0.lock().unwrap();
        state.dropped.extend([commit, txid]);
        state.mined.insert(prefund, 100);
    }
    let again = join(&data, id, (7, 50)).unwrap();
    assert_eq!(submitted_txids(&wallet).len(), 5);
    assert_eq!(submitted_txids(&wallet)[4], again);
    assert!(
        submitted_txids(&wallet)[3..].iter().all(|t| *t != prefund),
        "no second prefund transaction"
    );

    // A prefund transaction recorded but refused by the node is never sent again, whatever
    // happened to it: a failed retry names it, the next one sends the pair alone
    let other = subnet(79);
    create(&data.db, other);
    wallet
        .0
        .lock()
        .unwrap()
        .send_failures
        .push_back(Some(SendFailure::Refused));
    let refused = join_with_balance(&data, other, Some(balance.clone())).unwrap_err();
    assert!(refused.contains("incomplete"), "{refused}");
    assert_eq!(submitted(&wallet), 5, "nothing reached the node");
    let recorded = join_record(&data, other).prefund.unwrap().compute_txid();
    assert!(refused.contains(&recorded.to_string()), "{refused}");
    wallet
        .0
        .lock()
        .unwrap()
        .send_failures
        .push_back(Some(SendFailure::Refused));
    let failed = join_with_balance(&data, other, Some(balance.clone())).unwrap_err();
    assert!(failed.contains(&recorded.to_string()), "{failed}");
    assert!(failed.contains("incomplete"), "{failed}");
    assert_eq!(submitted(&wallet), 5, "the commit transaction was refused");
    let more = InitialBalance {
        amount: Amount::from_sat(6_000),
        ..balance
    };
    let txid = join_with_balance(&data, other, Some(more)).unwrap();
    assert_eq!(submitted_txids(&wallet).len(), 7, "the pair alone");
    assert_eq!(submitted_txids(&wallet)[6], txid);
    assert!(
        submitted_txids(&wallet).iter().all(|t| *t != recorded),
        "the recorded prefund transaction is never sent"
    );
    let record = join_record(&data, other);
    assert_eq!(
        record.prefund.as_ref().map(|tx| tx.compute_txid()),
        Some(recorded)
    );
    assert_eq!(record.txid(), Some(txid));
}

#[tokio::test]
async fn a_spent_reveal_input_lets_a_new_pair_through_once_the_monitor_processed_the_spend() {
    let (dir, db) = database().await;
    let id = subnet(77);
    create(&db, id);
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, None);

    let txid = join(&data, id, (7, 50)).unwrap();
    let (pk_eots, _) = pk_eots_of(&wallet, txid);
    let commit = submitted_txids(&wallet)[0];
    let outpoint = OutPoint::new(commit, 0);
    // The commit transaction is mined, the reveal transaction is out of the mempool
    {
        let mut state = wallet.0.lock().unwrap();
        state.mined.insert(commit, 99);
        state.dropped.insert(txid);
    }

    // Its input spent by another transaction in the mempool: refused, nothing sent
    wallet.0.lock().unwrap().spent.insert(outpoint, None);
    let refused = join(&data, id, (7, 50)).unwrap_err();
    assert!(refused.contains("another transaction"), "{refused}");
    assert!(refused.contains("in the mempool"), "{refused}");
    assert_eq!(
        submitted(&wallet),
        2,
        "the reveal transaction is not sent again"
    );

    // Spent in a Bitcoin block the monitor has processed: a new pair with the subnet's key
    wallet.0.lock().unwrap().spent.insert(outpoint, Some(100));
    let retry = join(&data, id, (7, 50)).unwrap();
    assert_eq!(submitted(&wallet), 4);
    assert_ne!(retry, txid);
    assert_eq!(pk_eots_of(&wallet, retry).0, pk_eots);
    assert_eq!(join_record(&data, id).txid(), Some(retry));

    // The Bitcoin block of a spend the monitor has not processed is found
    let spender = bitcoin::Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: outpoint,
            ..Default::default()
        }],
        output: vec![],
    };
    {
        let mut state = wallet.0.lock().unwrap();
        state.blocks.insert(101, vec![spender]);
        state.tip = 102;
    }
    let spend = |processed| {
        unprocessed_spend(&data, outpoint, processed, 102)
            .ok()
            .unwrap()
    };
    assert_eq!(spend(100), Some(101));
    assert_eq!(spend(101), None);
    assert_eq!(spend(102), None);
}

#[tokio::test]
async fn the_lagging_monitor_check_uses_the_tip_read_after_the_join_guard() {
    let (dir, db) = database().await;
    let (first, second) = (subnet(76), subnet(75));
    create(&db, first);
    create(&db, second);
    let wallet = WalletRpc::default();
    let data = Arc::new(provider_with_keys(db, dir.path(), &wallet, None));

    // A join waits at the node while sending; a Bitcoin block arrives; a second join waits for it
    let gate = Arc::new(Gate::default());
    wallet.0.lock().unwrap().gate = Some(gate.clone());
    let stalled = {
        let data = data.clone();
        std::thread::spawn(move || join(&data, first, (7, 50)))
    };
    while !gate.stalled() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    wallet.0.lock().unwrap().tip = 101;
    let waiting = {
        let data = data.clone();
        std::thread::spawn(move || join(&data, second, (7, 50)))
    };
    std::thread::sleep(std::time::Duration::from_millis(50));
    gate.open();
    assert!(stalled.join().unwrap().is_ok());
    let refused = waiting.join().unwrap().unwrap_err();
    assert!(
        refused.contains("has processed Bitcoin block 100 of 101"),
        "{refused}"
    );
    assert_eq!(submitted(&wallet), 2, "the first pair alone");

    // A Bitcoin block arrives while the pair is built: refused before the first broadcast
    set_processed(&data, 101);
    wallet.0.lock().unwrap().tip_after_funding = Some(102);
    let refused = join(&data, second, (7, 50)).unwrap_err();
    assert!(
        refused.contains("has processed Bitcoin block 101 of 102"),
        "{refused}"
    );
    assert_eq!(submitted(&wallet), 2);
    set_processed(&data, 102);
    assert!(join(&data, second, (7, 50)).is_ok());
    assert_eq!(submitted(&wallet), 4);
}

#[tokio::test]
async fn a_stalled_join_leaves_signeotsvote_and_a_read_only_method_answering() {
    let (dir, db) = database().await;
    let (joined, joining) = (subnet(74), subnet(73));
    create(&db, joined);
    create(&db, joining);
    let wallet = WalletRpc::default();
    let data = Arc::new(provider_with_keys(db, dir.path(), &wallet, None));

    // The join to `joined` is accepted: its EOTS key signs the subnet's votes
    let txid = join(&data, joined, (7, 50)).unwrap();
    let reveal = wallet.0.lock().unwrap().submitted[1].clone();
    let [(_, IpcMessage::JoinUe { msg, .. })] = &IpcMessage::from_reveal_inputs(&reveal)[..] else {
        panic!("the reveal carries one join-ue message");
    };
    let mut accepted = crate::collateral::CheckedJoinUe {
        msg: msg.clone(),
        outpoint: OutPoint::new(reveal.input[0].previous_output.txid, 1),
        committee_number: 0,
    };
    // The collateral is read from the commit transaction, not from the message
    accepted.msg.join.collateral = Amount::from_sat(100_000);
    let mut txn = data.db.write_txn().unwrap();
    // Three other joins first: the fourth bootstraps the subnet
    for owner in 2..=4 {
        process_join_ue(
            &*data.db,
            &mut txn,
            &checked(joined, owner, 30 + owner, owner),
            Txid::from_byte_array([owner; 32]),
            98,
            BlockHash::from_byte_array([98; 32]),
        )
        .unwrap();
    }
    process_join_ue(
        &*data.db,
        &mut txn,
        &accepted,
        txid,
        99,
        BlockHash::from_byte_array([99; 32]),
    )
    .unwrap();
    txn.commit().unwrap();

    // A join to `joining` stalls at the node while sending, on this single-threaded runtime
    let gate = Arc::new(Gate::default());
    wallet.0.lock().unwrap().gate = Some(gate.clone());
    let genesis = data.db.get_subnet_genesis_info(joining).unwrap().unwrap();
    let stalled = tokio::spawn(spawn_join_subnet_ue(
        data.clone(),
        genesis,
        checked(joining, 1, 22, 1).msg.join,
        2,
        (7, 50),
        None,
    ));
    while !gate.stalled() {
        tokio::task::yield_now().await;
    }
    assert!(gate.held(), "the join waits at the node");

    // Meanwhile a vote is signed and a read-only method answers
    let vote = sign_eots_vote(
        Data::new(data.clone()),
        Params(SignEotsVoteParams {
            subnet_id: joined,
            height: 10,
            block_hash: hex::encode([0x1a; 32]),
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("{}", serde_json::to_string(&e).unwrap()));
    assert_eq!(vote.height, 10);
    assert_eq!(vote.pk_eots, msg.pk_eots.to_string());
    assert_eq!(
        get_confirmed_block_height(Data::new(data.clone()))
            .await
            .ok(),
        Some(100)
    );
    assert!(gate.held(), "answered while the join waited");
    assert!(!stalled.is_finished(), "the join still waits");
    gate.open();
    assert!(stalled.await.unwrap().is_ok());
    assert_eq!(submitted(&wallet), 4);
}

#[tokio::test]
async fn a_failed_sync_of_the_join_record_sends_nothing() {
    use crate::provider::eots_keys::sync_faults::{self, SyncStep};
    let (dir, db) = database().await;
    let id = subnet(72);
    create(&db, id);
    let wallet = WalletRpc::default();
    let data = provider_with_keys(db, dir.path(), &wallet, None);
    let balance = InitialBalance {
        amount: Amount::from_sat(5_000),
        address: alloy_primitives::Address::from([7; 20]),
    };
    // The subnet's key exists, from a join whose commit transaction the node refused
    wallet
        .0
        .lock()
        .unwrap()
        .send_failures
        .push_back(Some(SendFailure::Refused));
    assert!(join(&data, id, (7, 50)).is_err());
    assert_eq!(submitted(&wallet), 0);
    let files = key_files_in(dir.path());
    assert_eq!(files.len(), 2);

    // The file sync of the record fails: no prefund transaction, no pair
    sync_faults::fail(dir.path(), SyncStep::File);
    let failed = join_with_balance(&data, id, Some(balance.clone())).unwrap_err();
    sync_faults::clear(dir.path());
    assert!(failed.contains("join record"), "{failed}");
    assert!(failed.contains("File sync failed"), "{failed}");
    assert_eq!(submitted(&wallet), 0);

    // The directory sync fails: the same, but the record is on disk, so the prefund transaction
    // is recorded and the error names it
    sync_faults::fail(dir.path(), SyncStep::Directory);
    let failed = join_with_balance(&data, id, Some(balance.clone())).unwrap_err();
    assert!(failed.contains("Directory sync failed"), "{failed}");
    assert!(failed.contains("incomplete"), "{failed}");
    let recorded = join_record(&data, id).prefund.unwrap().compute_txid();
    assert!(failed.contains(&recorded.to_string()), "{failed}");
    assert_eq!(submitted(&wallet), 0);
    let failed = join(&data, id, (7, 50)).unwrap_err();
    sync_faults::clear(dir.path());
    assert!(failed.contains("join record"), "{failed}");
    assert_eq!(submitted(&wallet), 0);
    assert_eq!(key_files_in(dir.path()), files);

    // Synced, the join goes through: the pair alone, the recorded prefund transaction is never sent
    assert!(join_with_balance(&data, id, Some(balance)).is_ok());
    assert_eq!(submitted(&wallet), 2);
    assert!(submitted_txids(&wallet).iter().all(|t| *t != recorded));
}
