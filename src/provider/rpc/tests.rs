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
    db.set_last_processed_block(125).unwrap();

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
    db.set_last_processed_block(126).unwrap();
    assert_eq!(at(Some(126)).unwrap().unwrap().checkpoint_number, 1);
    // Without a Bitcoin height the processed height does not matter
    db.set_last_processed_block(0).unwrap();
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
    db.set_last_processed_block(22).unwrap();

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
    db.set_last_processed_block(125).unwrap();
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
    let script: CollateralScript = collateral::collateral_script_of(&db, &record).unwrap();
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
    let carried = carried_slash_entries(&db, subnet_id, &posted);
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].signatures, vec![s0, s1]);
    assert!(s0.0 < s1.0);
    let posted = vec![PostedSlashSignatures::new(
        record.outpoint,
        vec![s0.1, bad.1],
    )];
    assert!(carried_slash_entries(&db, subnet_id, &posted).is_empty());
    // An unknown output is left out; a slashable one is carried again
    let mut unknown = PostedSlashSignatures::new(record.outpoint, vec![s0.1, s1.1]);
    unknown.vout = 3;
    assert!(carried_slash_entries(&db, subnet_id, &[unknown]).is_empty());
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
    assert_eq!(carried_slash_entries(&db, subnet_id, &posted).len(), 1);

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
        let script = collateral::collateral_script_of(&db, collateral).unwrap();
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
    let carried = carried_slash_entries(&db, subnet_id, &posted);
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
    submitted: Vec<bitcoin::Transaction>,
    tip: u64,
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
                serde_json::json!({"hex": args[0], "complete": true})
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
                self.0.lock().unwrap().submitted.push(tx);
                serde_json::json!(txid)
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
            eots_signer: None,
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
                process_join_ue(db, &join, Txid::from_byte_array([35; 32]), height, hash).unwrap();
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
                let parsed = IpcCheckpointSubnetMsg::from_checkpoint_tx(db, &tx).unwrap();
                let cp = parsed
                    .save_to_db(db, height, hash, tx.compute_txid())
                    .unwrap();
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
                    batch
                        .process(db, height, hash, reveal.compute_txid())
                        .unwrap();
                }
                request.slash_signatures.clear();
            }

            // No height is skipped: process_cutoffs observes every H_e.
            for outcome in crate::epochs::process_cutoffs(db, height, hash).unwrap() {
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
    let msg = IpcCheckpointSubnetMsg::from_checkpoint_tx(&*data.db, tx).unwrap();
    assert!(msg.is_rotation_checkpoint);
    assert_eq!(msg.next_committee_configuration_number, 0);
    let cp = msg
        .save_to_db(
            &*data.db,
            113,
            BlockHash::from_byte_array([113; 32]),
            tx.compute_txid(),
        )
        .unwrap();
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
    rotation(&f, 4)
        .save_to_db(
            &f.db,
            112,
            BlockHash::from_byte_array([112; 32]),
            Txid::from_byte_array([0x97; 32]),
        )
        .unwrap();
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
    let msg = IpcCheckpointSubnetMsg::from_checkpoint_tx(&*data.db, tx).unwrap();
    assert!(msg.is_rotation_checkpoint);
    assert_eq!(msg.next_committee_configuration_number, 4);
    let cp = msg
        .save_to_db(
            &*data.db,
            122,
            BlockHash::from_byte_array([122; 32]),
            tx.compute_txid(),
        )
        .unwrap();
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

#[tokio::test]
async fn join_ue_takes_the_eots_key_and_nonces_from_the_eots_signer() {
    let (dir, db) = database().await;
    let id = subnet(92);
    create(&db, id);
    db.set_last_processed_block(100).unwrap();
    let genesis = db.get_subnet_genesis_info(id).unwrap().unwrap();
    let seed = crate::eots::EotsSeed::from_bytes([0x5e; 32]);
    let signer = crate::provider::eots_signer::EotsSigner::load(
        secret(21),
        seed,
        &dir.path().join("eots.sk"),
    )
    .unwrap();
    let wallet = WalletRpc::default();
    wallet.0.lock().unwrap().tip = 100;
    let client = Arc::new(bitcoincore_rpc::Client::from_jsonrpc(
        jsonrpc::Client::with_transport(wallet.clone()),
    ));
    let data = ServerData {
        db: Arc::new(db),
        btc_rpc: client.clone(),
        btc_watchonly_rpc: client,
        validator: Some((pubkey(1), secret(1))),
        eots_signer: Some(Arc::new(Mutex::new(signer))),
    };

    let join_txid = join_subnet_ue(
        &data,
        &genesis,
        checked(id, 1, 22, 1).msg.join,
        2,
        100,
        (7, 50),
    )
    .unwrap_or_else(|e| panic!("{}", serde_json::to_string(&e).unwrap()));

    let reveal = wallet
        .0
        .lock()
        .unwrap()
        .submitted
        .iter()
        .find(|tx| tx.compute_txid() == join_txid)
        .unwrap()
        .clone();
    let [(_, IpcMessage::JoinUe { msg, .. })] = &IpcMessage::from_reveal_inputs(&reveal)[..] else {
        panic!("the reveal carries one join-ue message");
    };
    assert_eq!(msg.pk_eots, pubkey(21));
    assert_eq!(
        msg.nonce_commitment,
        crate::eots::commit_nonces(&seed, &id, 7, 50).unwrap().0
    );
}
