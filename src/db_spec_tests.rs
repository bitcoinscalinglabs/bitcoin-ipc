use super::*;
use crate::collateral::{process_join_ue, CheckedJoinUe};
use crate::ipc_lib::{IpcBatchTransferMsg, IpcJoinSubnetMsg, IpcJoinUeMsg};
use crate::slash::{sign_slash, SlashSignatureEntry};
use bitcoin::{
    hashes::Hash,
    secp256k1::{Secp256k1, SecretKey},
    Amount, OutPoint,
};

fn secret(n: u8) -> SecretKey {
    let mut bytes = [0; 32];
    bytes[31] = n;
    SecretKey::from_slice(&bytes).unwrap()
}

fn pubkey(n: u8) -> XOnlyPublicKey {
    secret(n).x_only_public_key(&Secp256k1::new()).0
}

fn subnet(n: u8) -> SubnetId {
    SubnetId::from_txid20(&[n; 20])
}

async fn database() -> (tempfile::TempDir, HeedDb) {
    // Database keys hold SubnetId's string form, which depends on this process-global.
    crate::eth_utils::set_fvm_network();
    let dir = tempfile::tempdir().unwrap();
    let db = HeedDb::new(dir.path().to_str().unwrap(), false)
        .await
        .unwrap();
    (dir, db)
}

fn create(db: &HeedDb, id: SubnetId) {
    let genesis = SubnetGenesisInfo {
        subnet_id: id,
        create_subnet_msg: IpcCreateSubnetMsg {
            min_validator_stake: Amount::from_sat(100_000),
            power_unit_exponent: 4,
            min_validators: 4,
            bottomup_check_period: 30,
            active_validators_limit: 10,
            min_cross_msg_fee: Amount::from_sat(1),
            whitelist: (1..=6).map(pubkey).collect(),
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
        genesis_validators: vec![],
        genesis_balance_entries: vec![],
    };
    let mut txn = db.write_txn().unwrap();
    db.save_subnet_genesis_info(&mut txn, id, &genesis).unwrap();
    txn.commit().unwrap();
}

// A CheckedJoinUe supplies the validation-layer interface directly. No excluded collateral
// tree, epoch, C_gen or slash-signature behavior is part of these admission/state tests.
fn checked(id: SubnetId, owner: u8, eots: u8, tx_marker: u8) -> CheckedJoinUe {
    let secp = Secp256k1::new();
    let join = IpcJoinSubnetMsg {
        subnet_id: id,
        collateral: Amount::from_sat(100_000),
        ip: "192.0.2.1:8080".parse().unwrap(),
        backup_address: Address::p2tr(&secp, pubkey(owner), None, crate::NETWORK).into_unchecked(),
        pubkey: pubkey(owner),
    };
    CheckedJoinUe {
        msg: IpcJoinUeMsg::new_signed(
            &secp,
            join,
            1000,
            &secret(owner),
            &secret(eots),
            EotsNonceCommitment {
                start: 129,
                count: 17,
                root: [0x35; 32],
            },
        ),
        outpoint: OutPoint::new(Txid::from_byte_array([tx_marker; 32]), 1),
        committee_number: 0,
    }
}

fn process(
    db: &HeedDb,
    join: &CheckedJoinUe,
    reveal_marker: u8,
) -> Result<Option<SubnetState>, crate::ipc_lib::IpcLibError> {
    process_join_ue(
        db,
        join,
        Txid::from_byte_array([reveal_marker; 32]),
        102,
        BlockHash::from_byte_array([0x42; 32]),
    )
}

fn assert_unregistered(db: &HeedDb, id: SubnetId, owner: u8, eots: u8, accepted_count: usize) {
    assert!(db
        .get_ue_collaterals(id)
        .unwrap()
        .iter()
        .all(|c| c.pubkey != pubkey(owner)));
    assert!(db
        .get_eots_commitments(id, &pubkey(eots))
        .unwrap()
        .is_empty());
    assert_eq!(
        db.get_subnet_genesis_info(id)
            .unwrap()
            .unwrap()
            .genesis_validators
            .len(),
        accepted_count
    );
    assert!(db.get_subnet_state(id).unwrap().is_none());
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
}

// lifecycle.md, State kept by the monitor: an accepted join records its commitment with L1 provenance.
#[tokio::test]
async fn accepted_join_records_the_exact_eots_commitment_and_its_provenance() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let join = checked(id, 1, 20, 11);
    process(&db, &join, 21).unwrap();
    let records = db.get_eots_commitments(id, &pubkey(20)).unwrap();
    assert_eq!(
        records,
        vec![EotsCommitmentRecord {
            subnet_id: id,
            pk_eots: pubkey(20),
            commitment: join.msg.nonce_commitment,
            block_height: 102,
            txid: Txid::from_byte_array([21; 32]),
        }]
    );
    assert!(db.get_eots_commitments(id, &pubkey(1)).unwrap().is_empty());
    assert!(db.get_subnet_state(id).unwrap().is_none());
}

// join.md, Verifying the transaction 2.7: another validator cannot reuse an accepted EOTS key.
#[tokio::test]
async fn eots_key_reuse_by_another_validator_rejects_all_join_state() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    process(&db, &checked(id, 1, 20, 11), 21).unwrap();
    let before = db.get_eots_commitments(id, &pubkey(20)).unwrap();
    assert!(process(&db, &checked(id, 2, 20, 12), 22).is_err());
    assert_eq!(db.get_ue_collaterals(id).unwrap().len(), 1);
    assert_eq!(db.get_eots_commitments(id, &pubkey(20)).unwrap(), before);
    assert_eq!(
        db.get_subnet_genesis_info(id)
            .unwrap()
            .unwrap()
            .genesis_validators
            .len(),
        1
    );
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
}

// join.md, Verifying the transaction 2.7: EOTS-key uniqueness is global; only validator-key uniqueness is subnet-scoped.
#[tokio::test]
async fn eots_key_used_in_another_subnet_cannot_register_a_different_validator() {
    let (_dir, db) = database().await;
    let (a, b) = (subnet(1), subnet(2));
    create(&db, a);
    create(&db, b);
    process(&db, &checked(a, 1, 20, 11), 21).unwrap();
    assert!(process(&db, &checked(b, 2, 20, 12), 22).is_err());
    assert_unregistered(&db, b, 2, 20, 0);
    assert_eq!(db.get_ue_collaterals(a).unwrap().len(), 1);
}

// join.md, Verifying the transaction 2.7: reusing an EOTS key is forbidden even for its original validator in another subnet.
#[tokio::test]
async fn eots_key_used_in_another_subnet_cannot_register_its_original_validator() {
    let (_dir, db) = database().await;
    let (a, b) = (subnet(1), subnet(2));
    create(&db, a);
    create(&db, b);
    process(&db, &checked(a, 1, 20, 11), 21).unwrap();
    assert!(process(&db, &checked(b, 1, 20, 12), 22).is_err());
    assert_unregistered(&db, b, 1, 20, 0);
}

// join.md, Verifying the transaction 2.7: a validator key may join another subnet using a fresh EOTS key.
#[tokio::test]
async fn fresh_eots_key_allows_the_same_validator_to_join_a_different_subnet() {
    let (_dir, db) = database().await;
    let (a, b) = (subnet(1), subnet(2));
    create(&db, a);
    create(&db, b);
    process(&db, &checked(a, 1, 20, 11), 21).unwrap();
    process(&db, &checked(b, 1, 21, 12), 22).unwrap();
    assert_eq!(db.get_ue_collaterals(a).unwrap().len(), 1);
    assert_eq!(db.get_ue_collaterals(b).unwrap().len(), 1);
    assert_eq!(db.get_eots_commitments(b, &pubkey(21)).unwrap().len(), 1);
}

// join.md, Verifying the transaction 3-4: a rejected low-collateral join must not reserve an EOTS key.
#[tokio::test]
async fn rejected_low_collateral_join_does_not_consume_the_eots_identity() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let mut low = checked(id, 1, 20, 11);
    low.msg.join.collateral = Amount::from_sat(99_999);
    assert!(process(&db, &low, 21).is_err());
    assert_unregistered(&db, id, 1, 20, 0);
    process(&db, &checked(id, 2, 20, 12), 22).unwrap();
    assert_eq!(db.get_ue_collaterals(id).unwrap()[0].pubkey, pubkey(2));
}

// join.md, Verifying the transaction 3-4: a non-whitelisted join cannot reserve its EOTS identity.
#[tokio::test]
async fn rejected_non_whitelisted_join_does_not_consume_the_eots_identity() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    assert!(process(&db, &checked(id, 7, 20, 11), 21).is_err());
    assert_unregistered(&db, id, 7, 20, 0);
    process(&db, &checked(id, 1, 20, 12), 22).unwrap();
    assert_eq!(db.get_eots_commitments(id, &pubkey(20)).unwrap().len(), 1);
}

// lifecycle.md, State kept by the monitor: accepted EOTS commitments persist across monitor restarts.
#[tokio::test]
async fn accepted_eots_commitment_retains_its_provenance_after_reopening() {
    let (dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let join = checked(id, 1, 20, 11);
    process(&db, &join, 21).unwrap();
    drop(db);
    let db = HeedDb::new(dir.path().to_str().unwrap(), false)
        .await
        .unwrap();
    assert_eq!(
        db.get_eots_commitments(id, &pubkey(20)).unwrap(),
        vec![EotsCommitmentRecord {
            subnet_id: id,
            pk_eots: pubkey(20),
            commitment: join.msg.nonce_commitment,
            block_height: 102,
            txid: Txid::from_byte_array([21; 32]),
        }]
    );
}

// slashing.md, EOTS key and EOTS nonce commitments: queries return every recorded commitment, including gaps.
#[tokio::test]
async fn commitment_query_does_not_drop_gapped_ranges_or_mix_keys_and_subnets() {
    let (_dir, db) = database().await;
    let (a, b) = (subnet(1), subnet(2));
    let mut txn = db.write_txn().unwrap();
    for (id, eots, start) in [
        (a, 20, 1000),
        (a, 20, 1),
        (a, 20, 30),
        (a, 21, 1),
        (b, 20, 1),
    ] {
        db.add_eots_commitment(
            &mut txn,
            &EotsCommitmentRecord {
                subnet_id: id,
                pk_eots: pubkey(eots),
                commitment: EotsNonceCommitment {
                    start,
                    count: 2,
                    root: [eots; 32],
                },
                block_height: 102,
                txid: Txid::from_byte_array([eots; 32]),
            },
        )
        .unwrap();
    }
    txn.commit().unwrap();
    let mut starts: Vec<_> = db
        .get_eots_commitments(a, &pubkey(20))
        .unwrap()
        .iter()
        .map(|r| r.commitment.start)
        .collect();
    starts.sort_unstable();
    assert_eq!(starts, vec![1, 30, 1000]);
    assert_eq!(db.get_eots_commitments(a, &pubkey(21)).unwrap().len(), 1);
    assert_eq!(db.get_eots_commitments(b, &pubkey(20)).unwrap().len(), 1);
    assert!(db.get_eots_commitments(b, &pubkey(21)).unwrap().is_empty());
}

// lifecycle.md, Configuration C_0: the threshold uses min_validators rather than whitelist length.
#[tokio::test]
async fn whitelist_configuration_threshold_does_not_grow_with_unused_whitelist_keys() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let config = db
        .get_subnet_genesis_info(id)
        .unwrap()
        .unwrap()
        .whitelist_configuration();
    assert_eq!(config.keys.len(), 6);
    assert!(config.keys.iter().all(|(_, power)| *power == 1));
    assert_eq!(config.threshold, 3); // floor(2 * 4 / 3) + 1
}

// Install the state of an active four-member configuration. Epoch computation is outside
// this fixture: these tests exercise join admission and the slashability validation boundary.
fn active_configuration(db: &HeedDb, id: SubnetId) {
    create(db, id);
    let mut genesis = db.get_subnet_genesis_info(id).unwrap().unwrap();
    genesis.bootstrapped = true;
    genesis.genesis_block_height = Some(101);
    genesis.genesis_validators = (1..=4)
        .map(|n| {
            let join = checked(id, n, n + 20, n + 10).msg.join;
            SubnetValidator {
                pubkey: join.pubkey,
                subnet_address: crate::eth_utils::eth_addr_from_x_only_pubkey(join.pubkey),
                power: 1,
                collateral: join.collateral,
                backup_address: join.backup_address,
                ip: join.ip,
                join_txid: Txid::from_byte_array([n + 30; 32]),
            }
        })
        .collect();
    let committee = genesis.genesis_validators.to_committee(&id, 0);
    assert_eq!(committee.threshold, 3); // floor(2 * 4 / 3) + 1
    let state = SubnetState {
        id,
        committee_number: 1,
        committee: committee.clone(),
        waiting_committee: None,
        last_checkpoint_number: Some(2),
        killed: SubnetKillState::NotKilled,
        marked_for_kill_checkpoint_number: None,
        epoch: 1,
        h_0: Some(0),
        last_rotation_epoch: 1,
        pending_multisig_changes: Vec::new(),
    };
    let mut txn = db.write_txn().unwrap();
    db.save_subnet_genesis_info(&mut txn, id, &genesis).unwrap();
    db.save_committee(&mut txn, id, 1, &committee).unwrap();
    db.save_subnet_state(&mut txn, id, &state).unwrap();
    txn.commit().unwrap();
}

fn accept_active_join(db: &HeedDb, id: SubnetId) -> UeCollateral {
    let mut join = checked(id, 5, 25, 15);
    join.committee_number = 1;
    process_join_ue(
        db,
        &join,
        Txid::from_byte_array([35; 32]),
        200,
        BlockHash::from_byte_array([0x43; 32]),
    )
    .unwrap();
    db.get_ue_collateral(id, Txid::from_byte_array([35; 32]), &pubkey(5))
        .unwrap()
        .unwrap()
}

fn slash_entry(db: &HeedDb, record: &UeCollateral, signers: &[u8]) -> SlashSignatureEntry {
    let secp = Secp256k1::new();
    let script = crate::collateral::collateral_script_of(db, record).unwrap();
    let mut signatures: Vec<_> = signers
        .iter()
        .map(|n| {
            sign_slash(
                &secp,
                &script,
                record.outpoint,
                record.value,
                &secret(*n).keypair(&secp),
            )
            .unwrap()
        })
        .collect();
    signatures.sort_by_key(|(index, _)| *index);
    SlashSignatureEntry::new(record.outpoint, signatures)
}

fn presign_batch(id: SubnetId, entries: Vec<SlashSignatureEntry>) -> IpcBatchTransferMsg {
    IpcBatchTransferMsg {
        subnet_id: id,
        checkpoint_txid: Txid::from_byte_array([70; 32]),
        checkpoint_vout: 1,
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: entries,
    }
}

fn genesis_presign_batch(db: &HeedDb, id: SubnetId, signers: &[u8]) -> IpcBatchTransferMsg {
    let join = checked(id, 1, 21, 11);
    process(db, &join, 31).unwrap();
    let record = db.get_ue_collaterals(id).unwrap().pop().unwrap();
    presign_batch(id, vec![slash_entry(db, &record, signers)])
}

// lifecycle.md, Configuration C_0; slashing.md, Slashable outputs 2: min_validators = 4 needs three signatures.
#[tokio::test]
async fn two_whitelisted_signatures_do_not_make_a_genesis_collateral_slashable() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let batch = genesis_presign_batch(&db, id, &[1, 5]);
    assert!(batch.newly_slashable(&db).unwrap().is_empty());
}

// lifecycle.md, Configuration C_0: any whitelisted key contributes, even without an accepted join.
#[tokio::test]
async fn three_of_six_whitelisted_keys_make_a_genesis_collateral_slashable() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let batch = genesis_presign_batch(&db, id, &[1, 5, 6]);
    let genesis = db.get_subnet_genesis_info(id).unwrap().unwrap();
    assert_eq!(genesis.genesis_validators.len(), 1);
    assert_eq!(genesis.genesis_validators[0].pubkey, pubkey(1));
    let newly = batch.newly_slashable(&db).unwrap();
    assert_eq!(newly.len(), 1);
    assert_eq!(newly[0].0.pubkey, pubkey(1));
    assert_eq!(newly[0].1.len(), 3);
}

// join.md, Verifying 5-6: an accepted post-activation reveal records collateral without seating its owner.
#[tokio::test]
async fn accepted_active_join_without_l1_presignatures_changes_no_configuration() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    assert_eq!(
        record.outpoint,
        OutPoint::new(Txid::from_byte_array([15; 32]), 1)
    );
    assert!(db
        .get_slash_presigs(id, &record.outpoint)
        .unwrap()
        .is_none());
    let state = db.get_subnet_state(id).unwrap().unwrap();
    assert!(!state.committee.is_validator(&pubkey(5)));
    assert!(state.waiting_committee.is_none());
    assert_eq!(state.committee.configuration_number, 0);
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
    assert!(presign_batch(id, vec![])
        .newly_slashable(&db)
        .unwrap()
        .is_empty());
}

// join.md, Verifying 2.7: accepting the first reveal reserves its validator key before it has a seat.
#[tokio::test]
async fn an_accepted_but_unseated_validator_cannot_join_again() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    accept_active_join(&db, id);
    let mut second = checked(id, 5, 26, 16);
    second.committee_number = 1;
    assert!(process_join_ue(
        &db,
        &second,
        Txid::from_byte_array([36; 32]),
        201,
        BlockHash::from_byte_array([0x44; 32]),
    )
    .is_err());
    assert_eq!(db.get_ue_collaterals(id).unwrap().len(), 1);
    assert!(db.get_eots_commitments(id, &pubkey(26)).unwrap().is_empty());
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
}

// slashing.md, Slashable outputs 2: two unit-power signatures are below a four-member threshold of three.
#[tokio::test]
async fn two_existing_validators_do_not_make_the_joiner_slashable() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    let batch = presign_batch(id, vec![slash_entry(&db, &record, &[1, 2])]);
    assert!(batch.newly_slashable(&db).unwrap().is_empty());
    assert!(db
        .get_slash_presigs(id, &record.outpoint)
        .unwrap()
        .is_none());
}

// slashing.md, After activation: the named configuration's quorum suffices without a signature from the joiner.
#[tokio::test]
async fn three_existing_validators_make_the_joiner_slashable_without_its_node() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    let entry = slash_entry(&db, &record, &[1, 2, 3]);
    let batch = presign_batch(id, vec![entry.clone()]);
    let newly = batch.newly_slashable(&db).unwrap();
    assert_eq!(newly.len(), 1);
    assert_eq!(newly[0].0, record);
    assert_eq!(newly[0].1, entry.signatures);
}

// slashing.md, Slashable outputs 1: one invalid signature rejects an entry even with a valid quorum beside it.
#[tokio::test]
async fn a_bad_extra_presignature_rejects_a_quorum_entry() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    let mut entry = slash_entry(&db, &record, &[1, 2, 3, 4]);
    entry.signatures[0].1 = [[0; 32]; 2];
    assert!(presign_batch(id, vec![entry]).newly_slashable(&db).is_err());
    assert!(db
        .get_slash_presigs(id, &record.outpoint)
        .unwrap()
        .is_none());
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
}

// slashing.md, Slashable outputs 1: pre-signatures commit to the particular collateral outpoint.
#[tokio::test]
async fn a_quorum_for_another_collateral_does_not_make_the_joiner_slashable() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    let mut different = record.clone();
    different.outpoint.vout = 2;
    let mut entry = slash_entry(&db, &different, &[1, 2, 3]);
    entry.vout = 1;
    assert!(presign_batch(id, vec![entry]).newly_slashable(&db).is_err());
}

mod epoch_state {
    use super::*;
    use crate::ipc_lib::{IpcFundSubnetMsg, IpcValidate};

    // lifecycle.md, State kept by the monitor / create-subnet.md, Verifying 3: create preserves all epoch parameters without a subnet state.
    #[tokio::test]
    async fn monitor_genesis_retains_the_seven_epoch_parameters_before_bootstrap() {
        let (_dir, db) = database().await;
        let mut params = IpcCreateSubnetMsg {
            min_validator_stake: Amount::from_sat(10_000),
            power_unit_exponent: 4,
            min_validators: 2,
            bottomup_check_period: 13,
            active_validators_limit: 20,
            min_cross_msg_fee: Amount::from_sat(1),
            whitelist: (1..=4).map(pubkey).collect(),
            epoch_length: 17,
            min_timeout: 2,
            max_timeout: 129,
            min_deposit: Amount::from_sat(777),
            kill_vote_period: 128,
            kill_delay_period: 0,
        };
        params.validate().unwrap();
        let txid = Txid::from_byte_array([0x19; 32]);
        params.save_to_db(&db, 102, txid).unwrap();
        params.epoch_length = 99; // the persisted parameter values must own their data
        let id = SubnetId::from_txid(&txid);
        let g = db.get_subnet_genesis_info(id).unwrap().unwrap();
        let p = &g.create_subnet_msg;
        assert_eq!(p.power_unit_exponent, 4);
        assert_eq!(p.epoch_length, 17);
        assert_eq!((p.min_timeout, p.max_timeout), (2, 129));
        assert_eq!(p.min_deposit.to_sat(), 777);
        assert_eq!((p.kill_vote_period, p.kill_delay_period), (128, 0));
        assert_eq!(g.create_msg_block_height, 102);
        assert!(!g.bootstrapped);
        assert!(db.get_subnet_state(id).unwrap().is_none());
    }

    // lifecycle.md, Computing C_gen: unequal genesis join collaterals give every member exactly one unit of power.
    #[tokio::test]
    async fn unequal_bootstrap_collaterals_produce_equal_genesis_power() {
        let (_dir, db) = database().await;
        let id = subnet(81);
        create(&db, id);
        for n in 1..=4 {
            let mut join = checked(id, n, n + 20, n + 10);
            join.msg.join.collateral = Amount::from_sat(100_000 + u64::from(n) * 23_457);
            process(&db, &join, n + 30).unwrap();
        }
        let state = db.get_subnet_state(id).unwrap().unwrap();
        assert_eq!(state.epoch, 0);
        assert_eq!(state.h_0, None);
        assert_eq!(state.last_rotation_epoch, 0);
        assert_eq!(state.committee.configuration_number, 0);
        assert!(state.committee.validators.iter().all(|v| v.power == 1));
        assert_eq!(state.committee.total_power(), 4);
        let g = db.get_subnet_genesis_info(id).unwrap().unwrap();
        assert_eq!(g.genesis_validators.len(), 4);
        assert_eq!(g.genesis_block_height, Some(102));
        for n in 1..=4 {
            assert_eq!(
                g.genesis_validators
                    .iter()
                    .find(|v| v.pubkey == pubkey(n))
                    .unwrap()
                    .collateral
                    .to_sat(),
                100_000 + u64::from(n) * 23_457
            );
        }
    }

    async fn bootstrapped() -> (tempfile::TempDir, HeedDb, SubnetId) {
        let (dir, db) = database().await;
        let id = subnet(82);
        create(&db, id);
        for n in 1..=4 {
            process(&db, &checked(id, n, n + 20, n + 10), n + 30).unwrap();
        }
        (dir, db, id)
    }

    // deposit.md, Verifying 3 / lifecycle.md, min_deposit: the subnet's minimum, above dust, also applies during epoch 0.
    #[tokio::test]
    async fn epoch_zero_deposit_validation_uses_the_subnet_minimum() {
        let (_dir, db, id) = bootstrapped().await;
        let mut g = db.get_subnet_genesis_info(id).unwrap().unwrap();
        g.create_subnet_msg.min_deposit = Amount::from_sat(777);
        let state = db.get_subnet_state(id).unwrap().unwrap();
        for (sats, allowed) in [(330, false), (776, false), (777, true), (778, true)] {
            let msg = IpcFundSubnetMsg {
                subnet_id: id,
                amount: Amount::from_sat(sats),
                address: alloy_primitives::Address::from([3; 20]),
            };
            assert_eq!(msg.validate_for_subnet(&g, &state).is_ok(), allowed);
        }
        assert!(db.get_all_rootnet_msgs(id).unwrap().is_empty());
    }
}

mod epoch_zero {
    use super::*;
    use crate::ipc_lib::{CheckpointSigner, IpcCheckpointSubnetMsg, IpcFundSubnetMsg, IpcMessage};

    struct Fixture {
        _dir: tempfile::TempDir,
        db: HeedDb,
        id: SubnetId,
    }
    impl Fixture {
        async fn new(timeouts: [u32; 4]) -> Self {
            let (dir, db) = database().await;
            let id = subnet(91);
            create(&db, id);
            for n in 1..=4 {
                let mut join = checked(id, n, n + 20, n + 10);
                join.msg = IpcJoinUeMsg::new_signed(
                    &Secp256k1::new(),
                    join.msg.join.clone(),
                    timeouts[n as usize - 1],
                    &secret(n),
                    &secret(n + 20),
                    join.msg.nonce_commitment,
                );
                process(&db, &join, n + 30).unwrap();
            }
            Self { _dir: dir, db, id }
        }
        fn state(&self) -> SubnetState {
            self.db.get_subnet_state(self.id).unwrap().unwrap()
        }
        fn signer(&self) -> CheckpointSigner {
            let g = self.db.get_subnet_genesis_info(self.id).unwrap().unwrap();
            CheckpointSigner::of_configuration(&self.id, &g.whitelist_configuration()).unwrap()
        }
        /// A regular checkpoint of epoch 0 whose reveal carries the slash pre-signatures of
        /// the collaterals of `owners`.
        fn message(&self, owners: &[u8]) -> IpcCheckpointSubnetMsg {
            let mut records = self.db.get_ue_collaterals(self.id).unwrap();
            records.sort_by_key(|r| r.outpoint);
            let signatures = records
                .iter()
                .filter(|r| owners.iter().any(|n| pubkey(*n) == r.pubkey))
                .map(|r| slash_entry(&self.db, r, &[1, 2, 3]))
                .collect();
            IpcCheckpointSubnetMsg {
                subnet_id: self.id,
                checkpoint_hash: bitcoin::hashes::sha256::Hash::from_byte_array([0x93; 32]),
                checkpoint_height: 30,
                next_committee_configuration_number: 0,
                withdrawals: vec![],
                transfers: vec![],
                token_registrations: vec![],
                token_supply_adjustments: vec![],
                token_transfers: vec![],
                slash_signatures: signatures,
                slash_pre_signature_count: None,
                unstakes: vec![],
                change_address: Some(self.signer().address.as_unchecked().clone()),
                is_kill_checkpoint: false,
                is_rotation_checkpoint: false,
            }
        }
        fn transactions(
            &self,
            msg: &IpcCheckpointSubnetMsg,
        ) -> (bitcoin::Transaction, bitcoin::Transaction) {
            let signer = self.signer();
            let unspent = bitcoincore_rpc::json::ListUnspentResultEntry {
                txid: Txid::from_byte_array([0x94; 32]),
                vout: 1,
                address: None,
                label: None,
                redeem_script: None,
                witness_script: None,
                script_pub_key: signer.address.script_pubkey(),
                amount: Amount::from_sat(10_000_000),
                confirmations: 10,
                spendable: true,
                solvable: true,
                descriptor: None,
                safe: true,
            };
            let change = msg
                .change_address
                .as_ref()
                .unwrap()
                .clone()
                .assume_checked();
            let tx = msg
                .build_checkpoint_psbt(
                    &signer,
                    &change,
                    false,
                    crate::DEFAULT_BTC_FEE_RATE,
                    &[unspent],
                )
                .unwrap()
                .unsigned_tx;
            let reveal = msg
                .make_reveal_batch_transfer_tx(
                    tx.compute_txid(),
                    crate::DEFAULT_BTC_FEE_RATE,
                    &change,
                )
                .unwrap()
                .unwrap();
            let mut txn = self.db.write_txn().unwrap();
            self.db.save_transaction(&mut txn, &tx).unwrap();
            txn.commit().unwrap();
            (tx, reveal)
        }
        fn reveal(
            &self,
            reveal: &bitcoin::Transaction,
            height: u64,
        ) -> Result<SubnetCheckpoint, crate::ipc_lib::IpcLibError> {
            let (_, IpcMessage::BatchTransfer(mut batch)) = IpcMessage::from_reveal_inputs(reveal)
                .into_iter()
                .next()
                .unwrap()
            else {
                panic!("batch reveal")
            };
            batch.process(
                &self.db,
                height,
                BlockHash::from_byte_array([height as u8; 32]),
                reveal.compute_txid(),
            )
        }
        fn accept(
            &self,
            msg: &IpcCheckpointSubnetMsg,
            tx: &bitcoin::Transaction,
            height: u64,
        ) -> SubnetCheckpoint {
            msg.save_to_db(
                &self.db,
                height,
                BlockHash::from_byte_array([height as u8; 32]),
                tx.compute_txid(),
            )
            .unwrap()
        }
        fn cutoff(&self, height: u64) -> Vec<crate::epochs::CutoffOutcome> {
            crate::epochs::process_cutoffs(
                &self.db,
                height,
                BlockHash::from_byte_array([height as u8; 32]),
            )
            .unwrap()
        }
    }

    // lifecycle.md, Epoch 0 / checkpoint.md, Before activation: a checkpoint of epoch 0 is a regular checkpoint of C_wlist and activates nothing.
    #[tokio::test]
    async fn an_epoch_zero_checkpoint_is_recorded_as_a_regular_checkpoint_of_c_wlist() {
        let f = Fixture::new([1000; 4]).await;
        let msg = f.message(&[1, 2, 3, 4]);
        let (tx, _reveal) = f.transactions(&msg);
        let cp = f.accept(&msg, &tx, 110);
        assert_eq!(
            cp.signed_committee_number,
            crate::collateral::WHITELIST_COMMITTEE_NUMBER
        );
        assert_eq!(cp.next_configuration_number, 0);
        assert!(!cp.is_rotation_checkpoint);
        assert_eq!(cp.epoch, 0);
        let state = f.state();
        assert_eq!(state.epoch, 0);
        assert_eq!(state.h_0, None);
        assert_eq!(state.last_checkpoint_number, Some(cp.checkpoint_number));
        assert!(f.db.get_all_stake_changes(f.id).unwrap().is_empty());
        assert!(f.db.get_all_slash_presigs(f.id).unwrap().is_empty());
    }

    // lifecycle.md, Activation: H_0 is the first cutoff height at which every unspent C_gen collateral is slashable, and C_1 follows the genesis unit values.
    #[tokio::test]
    async fn activation_changes_follow_the_genesis_unit_values_and_begin_at_one() {
        let f = Fixture::new([1000; 4]).await;
        let msg = f.message(&[1, 2, 3, 4]);
        let (tx, reveal) = f.transactions(&msg);
        f.accept(&msg, &tx, 110);
        f.reveal(&reveal, 111).unwrap();
        f.cutoff(111);
        let state = f.state();
        assert_eq!(state.h_0, Some(111));
        assert_eq!(state.epoch, 1);
        let changes = f.db.get_all_stake_changes(f.id).unwrap();
        assert_eq!(changes.len(), 4);
        assert_eq!(
            changes
                .iter()
                .map(|c| c.configuration_number)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert!(changes.iter().all(
            |c| matches!(c.change,StakingChange::Deposit { amount } if amount.to_sat()==90_000)
        ));
        assert!(changes
            .windows(2)
            .all(|w| w[0].validator_xpk < w[1].validator_xpk));
    }

    // lifecycle.md, C_1: when no C_gen collateral matures after the boundary, activation keeps C_gen with unit powers.
    #[tokio::test]
    async fn activation_with_no_qualifying_collateral_keeps_genesis_power() {
        let f = Fixture::new([135; 4]).await;
        let msg = f.message(&[1, 2, 3, 4]);
        let (tx, reveal) = f.transactions(&msg);
        f.accept(&msg, &tx, 110);
        f.reveal(&reveal, 111).unwrap();
        f.cutoff(111);
        let state = f.state();
        assert!(state.is_activated());
        assert_eq!(state.committee.validators.len(), 4);
        assert!(state.committee.validators.iter().all(|v| v.power == 1));
        assert!(f.db.get_all_stake_changes(f.id).unwrap().is_empty());
    }

    // deposit.md, Verifying 4 / user decision: epoch-0 deposits are recorded immediately at
    // their own Bitcoin blocks; activation neither releases nor duplicates them.
    #[tokio::test]
    async fn epoch_zero_deposits_are_recorded_at_their_own_blocks_with_continuing_nonces() {
        let f = Fixture::new([1000; 4]).await;
        for (i, sats) in [777, 330, 1001].into_iter().enumerate() {
            IpcFundSubnetMsg {
                subnet_id: f.id,
                amount: Amount::from_sat(sats),
                address: alloy_primitives::Address::from([i as u8 + 1; 20]),
            }
            .save_to_db(
                &f.db,
                103 + i as u64,
                BlockHash::from_byte_array([i as u8; 32]),
                Txid::from_byte_array([i as u8 + 101; 32]),
            )
            .unwrap();
            assert_eq!(f.db.get_all_rootnet_msgs(f.id).unwrap().len(), i + 1);
        }
        assert_eq!(f.state().h_0, None);
        let messages = f.db.get_all_rootnet_msgs(f.id).unwrap();
        assert_eq!(messages.len(), 3);
        for (i, (message, sats)) in messages.iter().zip([777, 330, 1001]).enumerate() {
            let RootnetMessage::FundSubnet {
                msg,
                nonce,
                block_height,
                block_hash,
                txid,
            } = message
            else {
                panic!("fund message")
            };
            assert_eq!(*nonce, i as u64);
            assert_eq!(msg.amount.to_sat(), sats);
            assert_eq!(
                msg.address,
                alloy_primitives::Address::from([i as u8 + 1; 20])
            );
            assert_eq!(*block_height, 103 + i as u64);
            assert_eq!(*block_hash, BlockHash::from_byte_array([i as u8; 32]));
            assert_eq!(*txid, Txid::from_byte_array([i as u8 + 101; 32]));
        }
        let before_activation = serde_json::to_value(&messages).unwrap();
        let msg = f.message(&[1, 2, 3, 4]);
        let (tx, reveal) = f.transactions(&msg);
        f.accept(&msg, &tx, 110);
        assert_eq!(f.db.get_all_rootnet_msgs(f.id).unwrap().len(), 3);
        f.reveal(&reveal, 111).unwrap();
        assert_eq!(f.db.get_all_rootnet_msgs(f.id).unwrap().len(), 3);
        f.cutoff(111);
        let state = f.state();
        assert_eq!(state.h_0, Some(111));
        assert_eq!(state.epoch, 1);
        assert_eq!(
            serde_json::to_value(f.db.get_all_rootnet_msgs(f.id).unwrap()).unwrap(),
            before_activation
        );
        // Processing the activation cutoff again changes no deposit message or nonce.
        f.cutoff(111);
        assert_eq!(f.state().h_0, Some(111));
        assert_eq!(f.db.get_all_rootnet_msgs(f.id).unwrap().len(), 3);
        assert_eq!(f.db.get_next_rootnet_msg_nonce(f.id).unwrap(), 3);
        IpcFundSubnetMsg {
            subnet_id: f.id,
            amount: Amount::from_sat(444),
            address: alloy_primitives::Address::from([4; 20]),
        }
        .save_to_db(
            &f.db,
            112,
            BlockHash::from_byte_array([112; 32]),
            Txid::from_byte_array([104; 32]),
        )
        .unwrap();
        let messages = f.db.get_all_rootnet_msgs(f.id).unwrap();
        assert_eq!(messages.len(), 4);
        assert!(matches!(
            &messages[3],
            RootnetMessage::FundSubnet {
                nonce: 3,
                block_height: 112,
                ..
            }
        ));
        assert_eq!(f.db.get_next_rootnet_msg_nonce(f.id).unwrap(), 4);
    }

    // lifecycle.md, Activation: a C_gen collateral that is spent does not block H_0 and is left out of C_1.
    #[tokio::test]
    async fn a_c_gen_collateral_slashed_in_epoch_zero_is_left_out_of_c_1() {
        let f = Fixture::new([1000; 4]).await;
        let msg = f.message(&[1, 2, 3, 4]);
        let (tx, reveal) = f.transactions(&msg);
        f.accept(&msg, &tx, 110);
        f.reveal(&reveal, 111).unwrap();
        let slashed = f
            .db
            .get_ue_collaterals(f.id)
            .unwrap()
            .into_iter()
            .find(|r| r.pubkey == pubkey(1))
            .unwrap();
        let mut txn = f.db.write_txn().unwrap();
        f.db.set_ue_collateral_spent(&mut txn, &slashed.outpoint)
            .unwrap();
        txn.commit().unwrap();
        f.cutoff(111);
        let state = f.state();
        assert_eq!(state.h_0, Some(111));
        let keys = state.latest_committee().pubkeys();
        assert!(!keys.contains(&pubkey(1)));
        for n in [2, 3, 4] {
            assert!(keys.contains(&pubkey(n)));
        }
    }

    // lifecycle.md, Activation: H_0 is the first cutoff height at which every unspent C_gen collateral is slashable.
    #[tokio::test]
    async fn h_0_is_the_first_height_at_which_every_unspent_c_gen_collateral_is_slashable() {
        let f = Fixture::new([1000; 4]).await;
        let first = f.message(&[1, 2, 3]);
        let (tx, reveal) = f.transactions(&first);
        f.accept(&first, &tx, 110);
        f.reveal(&reveal, 111).unwrap();
        f.cutoff(111);
        let state = f.state();
        assert_eq!(state.h_0, None);
        assert_eq!(state.epoch, 0);

        let mut second = f.message(&[4]);
        second.checkpoint_height = 31;
        second.checkpoint_hash = bitcoin::hashes::sha256::Hash::from_byte_array([0x95; 32]);
        let (tx, reveal) = f.transactions(&second);
        f.accept(&second, &tx, 120);
        f.reveal(&reveal, 121).unwrap();
        f.cutoff(121);
        let state = f.state();
        assert_eq!(state.h_0, Some(121));
        assert_eq!(state.epoch, 1);
    }

    async fn waiting_rotation() -> Fixture {
        let f = Fixture::new([1000; 4]).await;
        let first = f.message(&[1, 2, 3, 4]);
        let (tx, reveal) = f.transactions(&first);
        f.accept(&first, &tx, 110);
        f.reveal(&reveal, 111).unwrap();
        f.cutoff(111);
        let mut join = checked(f.id, 5, 25, 15);
        join.committee_number = f.state().committee_number;
        process_join_ue(
            &f.db,
            &join,
            Txid::from_byte_array([35; 32]),
            112,
            BlockHash::from_byte_array([112; 32]),
        )
        .unwrap();
        let record =
            f.db.get_ue_collateral(f.id, Txid::from_byte_array([35; 32]), &pubkey(5))
                .unwrap()
                .unwrap();
        let entry = slash_entry(&f.db, &record, &[1, 2, 3]);
        let presigs = SlashPresigRecord {
            subnet_id: f.id,
            outpoint: record.outpoint,
            signatures: entry.signatures,
            block_height: 114,
            batch_transfer_txid: Txid::from_byte_array([0x95; 32]),
        };
        let mut txn = f.db.write_txn().unwrap();
        f.db.add_slash_presigs(&mut txn, &presigs).unwrap();
        txn.commit().unwrap();
        crate::epochs::process_cutoffs(&f.db, 121, BlockHash::from_byte_array([121; 32])).unwrap();
        f
    }

    fn rotation(f: &Fixture, number: u64) -> IpcCheckpointSubnetMsg {
        let mut msg = f.message(&[]);
        msg.is_rotation_checkpoint = true;
        msg.checkpoint_height = 60;
        msg.next_committee_configuration_number = number;
        msg.change_address = Some(f.state().waiting_committee.unwrap().multisig_address);
        msg
    }

    // checkpoint.md, Verifying 5-6: a rotation cannot adopt a waiting configuration with a stale last change number.
    #[tokio::test]
    async fn rotation_rejects_a_stale_last_change_number_without_adopting_any_changes() {
        let f = waiting_rotation().await;
        let msg = rotation(&f, 5);
        assert!(msg
            .save_to_db(
                &f.db,
                122,
                BlockHash::from_byte_array([122; 32]),
                Txid::from_byte_array([0x96; 32])
            )
            .is_err());
        let state = f.state();
        assert_eq!(state.last_rotation_epoch, 0);
        assert_eq!(state.committee.validators.len(), 4);
        assert_eq!(state.waiting_committee.unwrap().validators.len(), 5);
        assert_eq!(state.last_checkpoint_number, Some(0));
    }

    // lifecycle.md, Adopting a new configuration / checkpoint.md, Verifying 7: the exact last number adopts all waiting changes.
    #[tokio::test]
    async fn rotation_adopts_the_waiting_configuration_and_records_its_last_change_number() {
        let f = waiting_rotation().await;
        let msg = rotation(&f, 6);
        let cp = msg
            .save_to_db(
                &f.db,
                122,
                BlockHash::from_byte_array([122; 32]),
                Txid::from_byte_array([0x96; 32]),
            )
            .unwrap();
        assert_eq!(cp.next_configuration_number, 6);
        assert_eq!(cp.epoch, 2);
        assert!(cp.is_rotation_checkpoint);
        let state = f.state();
        assert_eq!(state.last_rotation_epoch, 2);
        assert_eq!(state.committee.configuration_number, 6);
        assert!(state.waiting_committee.is_none());
        assert_eq!(state.committee.validators.len(), 5);
        assert!(f.db.get_unconfirmed_stake_changes(f.id).unwrap().is_empty());
    }
}
