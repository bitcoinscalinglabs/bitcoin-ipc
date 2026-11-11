use super::*;
use crate::collateral::process_join_ue;
use crate::db::{
    HeedDb, RootnetMessage, SlashPresigRecord, StakingChange, SubnetKillState, SubnetValidators,
    UeCollateral,
};
use crate::ipc_lib::{IpcCheckpointSubnetMsg, IpcCreateSubnetMsg, IpcFundSubnetMsg};
use crate::test_utils::{
    checked, create, database, process, pubkey, rotation, secret, slash_entry, subnet,
    EpochZeroFixture,
};
use bitcoin::secp256k1::Secp256k1;
use bitcoin::OutPoint;

fn id() -> SubnetId {
    // Database keys hold SubnetId's string form, which depends on this process-global.
    crate::eth_utils::set_fvm_network();
    SubnetId::from_txid20(&[0x51; 20])
}
fn hash() -> BlockHash {
    BlockHash::from_byte_array([0x62; 32])
}

fn member(n: u8, sats: u64) -> SubnetValidator {
    SubnetValidator {
        pubkey: pubkey(n),
        subnet_address: eth_addr_from_x_only_pubkey(pubkey(n)),
        power: (sats / 10_000) as _,
        collateral: Amount::from_sat(sats),
        backup_address: bitcoin::Address::p2tr(&Secp256k1::new(), pubkey(n), None, crate::NETWORK)
            .into_unchecked(),
        ip: "192.0.2.1:8080".parse().unwrap(),
        join_txid: Txid::from_byte_array([n; 32]),
    }
}

fn committee() -> SubnetCommittee {
    (1..=4)
        .map(|n| member(n, 100_000))
        .collect::<Vec<_>>()
        .to_committee(&id(), 7)
}

fn genesis() -> SubnetGenesisInfo {
    SubnetGenesisInfo {
        subnet_id: id(),
        create_subnet_msg: IpcCreateSubnetMsg {
            min_validator_stake: Amount::from_sat(100_000),
            power_unit_exponent: 4,
            min_validators: 4,
            bottomup_check_period: 30,
            active_validators_limit: 20,
            min_cross_msg_fee: Amount::from_sat(1),
            whitelist: (1..=6).map(pubkey).collect(),
            epoch_length: 10,
            min_timeout: 1,
            max_timeout: 1000,
            min_deposit: Amount::from_sat(330),
            kill_vote_period: 36,
            kill_delay_period: 5,
        },
        create_msg_block_height: 90,
        bootstrapped: true,
        genesis_block_height: Some(99),
        genesis_validators: (1..=4).map(|n| member(n, 100_000)).collect(),
        genesis_balance_entries: vec![],
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: HeedDb,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_second_collateral(100_000).await
    }

    async fn with_second_collateral(second_sats: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = HeedDb::new(dir.path().to_str().unwrap(), false)
            .await
            .unwrap();
        let mut genesis = genesis();
        let mut members = committee().validators;
        *members.iter_mut().find(|v| v.pubkey == pubkey(2)).unwrap() = member(2, second_sats);
        genesis.genesis_validators = members.clone();
        let (c, history) = configuration_changes(
            id(),
            &genesis.to_subnet().committee,
            &members,
            1,
            101,
            hash(),
        )
        .unwrap();
        let state = SubnetState {
            id: id(),
            committee_number: 1,
            committee: c.clone(),
            waiting_committee: None,
            last_checkpoint_number: Some(0),
            killed: SubnetKillState::NotKilled,
            marked_for_kill_checkpoint_number: None,
            epoch: 1,
            h_0: Some(100),
            last_rotation_epoch: 1,
            pending_multisig_changes: vec![],
        };
        let mut txn = db.write_txn().unwrap();
        db.save_subnet_genesis_info(&mut txn, id(), &genesis)
            .unwrap();
        db.save_subnet_state(&mut txn, id(), &state).unwrap();
        db.save_committee(&mut txn, id(), 1, &c).unwrap();
        db.save_epoch_committee_number(&mut txn, id(), 1, 1)
            .unwrap();
        for change in history {
            db.add_stake_change(&mut txn, id(), change).unwrap();
        }
        txn.commit().unwrap();
        Self { _dir: dir, db }
    }

    fn collateral(&self, n: u8, timeout: u32, slashable: bool, spent: bool) -> OutPoint {
        let v = member(n, 100_000);
        let record = UeCollateral {
            subnet_id: id(),
            pubkey: v.pubkey,
            outpoint: OutPoint::new(Txid::from_byte_array([n + 30; 32]), 1),
            value: v.collateral,
            timeout,
            pk_eots: pubkey(n + 20),
            committee_number: if n <= 4 { 0 } else { 1 },
            ip: v.ip,
            backup_address: v.backup_address,
            join_txid: v.join_txid,
            spent,
        };
        let presigs = if slashable {
            let secp = Secp256k1::new();
            let script = crate::collateral::CollateralScript::new(
                record.pubkey,
                record.pk_eots,
                timeout,
                if n <= 4 {
                    genesis().whitelist_configuration()
                } else {
                    committee().configuration()
                },
            );
            let mut signatures: Vec<_> = (1..=4)
                .map(|signer| {
                    crate::slash::sign_slash(
                        &secp,
                        &script,
                        record.outpoint,
                        record.value,
                        &secret(signer).keypair(&secp),
                    )
                    .unwrap()
                })
                .collect();
            signatures.sort_by_key(|(index, _)| *index);
            Some(SlashPresigRecord {
                subnet_id: id(),
                outpoint: record.outpoint,
                signatures,
                block_height: 109,
                batch_transfer_txid: Txid::from_byte_array([0x71; 32]),
            })
        } else {
            None
        };
        let mut txn = self.db.write_txn().unwrap();
        self.db.add_ue_collateral(&mut txn, &record).unwrap();
        if let Some(record) = presigs {
            self.db.add_slash_presigs(&mut txn, &record).unwrap();
        }
        txn.commit().unwrap();
        record.outpoint
    }

    fn state(&self) -> SubnetState {
        self.db.get_subnet_state(id()).unwrap().unwrap()
    }
    /// Puts the subnet back in epoch 0: bootstrapped, `H_0` not known
    fn epoch_zero(&self) {
        let mut state = self.state();
        state.epoch = 0;
        state.h_0 = None;
        state.last_checkpoint_number = None;
        state.last_rotation_epoch = 0;
        let mut txn = self.db.write_txn().unwrap();
        self.db.save_subnet_state(&mut txn, id(), &state).unwrap();
        txn.commit().unwrap();
    }
    fn cutoff(&self, height: u64) {
        let mut txn = self.db.write_txn().unwrap();
        process_cutoffs(&self.db, &mut txn, height, hash()).unwrap();
        txn.commit().unwrap();
    }
    fn changes(&self) -> Vec<StakeChangeRequest> {
        self.db
            .get_all_stake_changes(id())
            .unwrap()
            .into_iter()
            .filter(|c| c.block_height >= 110)
            .collect()
    }
}

#[test]
fn entrant_changes_deposit_the_whole_collateral_with_consecutive_numbers() {
    let base = committee();
    let mut target = base.validators.clone();
    target.push(member(5, 123_456));
    let (next, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].configuration_number, 8);
    assert!(matches!(changes[0].change, StakingChange::Join { .. }));
    assert_eq!(changes[1].configuration_number, 9);
    assert!(
        matches!(changes[1].change,StakingChange::Deposit { amount } if amount.to_sat()==123_456)
    );
    assert!(changes
        .iter()
        .all(|c| c.validator_xpk == pubkey(5) && c.block_height == 110 && c.block_hash == hash()));
    assert_eq!(next.configuration_number, 9);
}

#[test]
fn continuing_member_increase_records_the_value_difference() {
    let base = committee();
    let mut target = base.validators.clone();
    *target.iter_mut().find(|v| v.pubkey == pubkey(2)).unwrap() = member(2, 127_999);
    let (_, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(changes[0].change,StakingChange::Deposit { amount } if amount.to_sat()==27_999)
    );
}

#[test]
fn continuing_member_decrease_records_the_value_difference() {
    let mut base = committee();
    *base
        .validators
        .iter_mut()
        .find(|v| v.pubkey == pubkey(2))
        .unwrap() = member(2, 150_000);
    base = base.validators.to_committee(&id(), 7);
    let mut target = base.validators.clone();
    *target.iter_mut().find(|v| v.pubkey == pubkey(2)).unwrap() = member(2, 140_001);
    let (_, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(changes[0].change,StakingChange::Withdraw { amount } if amount.to_sat()==9_999)
    );
}

#[test]
fn a_value_change_with_unchanged_power_is_still_recorded() {
    let base = committee();
    let mut target = base.validators.clone();
    *target.iter_mut().find(|v| v.pubkey == pubkey(2)).unwrap() = member(2, 100_001);
    let (next, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(changes.len(), 1);
    assert!(matches!(changes[0].change,StakingChange::Deposit { amount } if amount.to_sat()==1));
    assert_eq!(
        next.validators
            .iter()
            .find(|v| v.pubkey == pubkey(2))
            .unwrap()
            .power,
        10
    );
    assert_eq!(next.configuration_number, 8);
}

#[test]
fn departure_withdraws_the_entire_value_including_its_power_remainder() {
    let mut before = (1..=4).map(|n| member(n, 100_000)).collect::<Vec<_>>();
    before.push(member(5, 123_456));
    let base = before.to_committee(&id(), 7);
    let target = before
        .into_iter()
        .filter(|v| v.pubkey != pubkey(5))
        .collect::<Vec<_>>();
    let (_, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(changes[0].change,StakingChange::Withdraw { amount } if amount.to_sat()==123_456)
    );
}

#[test]
fn replacing_every_member_by_higher_keys_gives_the_multisig_of_the_new_members() {
    let mut keys: Vec<u8> = (1..=40).collect();
    keys.sort_by_key(|n| pubkey(*n));
    let base = keys[..4]
        .iter()
        .map(|n| member(*n, 100_000))
        .collect::<Vec<_>>()
        .to_committee(&id(), 7);
    let target: Vec<_> = keys[36..].iter().map(|n| member(*n, 100_000)).collect();

    let (configuration, changes) =
        configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();

    assert_eq!(
        configuration.multisig_address,
        target.to_committee(&id(), 7).multisig_address
    );
    assert_eq!(
        changes
            .iter()
            .filter(|c| matches!(c.change, StakingChange::Withdraw { .. }))
            .count(),
        4
    );
}

#[test]
fn mixed_changes_follow_key_order_even_with_reversed_input_vectors() {
    let mut base = committee();
    base.validators.reverse();
    let mut target: Vec<_> = base
        .validators
        .iter()
        .filter(|v| v.pubkey != pubkey(1))
        .cloned()
        .collect();
    target.push(member(5, 120_000));
    target.reverse();
    let (_, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    let mut expected = vec![
        (pubkey(1), "withdraw"),
        (pubkey(5), "join"),
        (pubkey(5), "deposit"),
    ];
    expected.sort_by_key(|(k, _)| *k); // stable: Join stays before Deposit for the same key.
    let actual: Vec<_> = changes
        .iter()
        .map(|c| {
            (
                c.validator_xpk,
                match c.change {
                    StakingChange::Join { .. } => "join",
                    StakingChange::Deposit { .. } => "deposit",
                    StakingChange::Withdraw { .. } => "withdraw",
                },
            )
        })
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(
        changes
            .iter()
            .map(|c| c.configuration_number)
            .collect::<Vec<_>>(),
        [8, 9, 10]
    );
}

#[test]
fn an_unchanged_epoch_does_not_break_change_number_continuity() {
    let base = committee();
    let mut target = base.validators.clone();
    target.push(member(5, 120_000));
    let (c2, first) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(c2.configuration_number, 9);
    let (c3, unchanged) = configuration_changes(id(), &c2, &target, 10, 120, hash()).unwrap();
    assert!(unchanged.is_empty());
    assert_eq!(c3.configuration_number, 9);
    assert_eq!(c3.multisig_address, c2.multisig_address);
    let (c4, last) = configuration_changes(id(), &c3, &base.validators, 10, 130, hash()).unwrap();
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].configuration_number, 10);
    assert_eq!(c4.configuration_number, 10);
    assert_eq!(
        first
            .iter()
            .chain(&last)
            .map(|c| c.configuration_number)
            .collect::<Vec<_>>(),
        [8, 9, 10]
    );
}

#[tokio::test]
async fn eligibility_has_a_strict_timeout_boundary() {
    let f = Fixture::new().await;
    f.collateral(1, 139, true, false);
    f.collateral(2, 140, true, false);
    f.collateral(3, 141, true, false);
    f.collateral(4, 2000, true, false);
    let target = compute_configuration(
        &f.db,
        &f.db.read_txn().unwrap(),
        &genesis(),
        &committee(),
        &[],
        140,
    )
    .unwrap();
    let mut keys = target.iter().map(|v| v.pubkey).collect::<Vec<_>>();
    keys.sort();
    let mut expected = vec![pubkey(3), pubkey(4)];
    expected.sort();
    assert_eq!(keys, expected);
}

#[tokio::test]
async fn spent_slashable_collateral_is_excluded_from_the_next_configuration() {
    let f = Fixture::new().await;
    f.collateral(1, 2000, true, true);
    let target = compute_configuration(
        &f.db,
        &f.db.read_txn().unwrap(),
        &genesis(),
        &committee(),
        &[],
        140,
    )
    .unwrap();
    assert!(!target.iter().any(|v| v.pubkey == pubkey(1)));
}

#[tokio::test]
async fn configuration_waits_for_the_exact_cutoff_block() {
    let f = Fixture::new().await;
    for n in 1..=4 {
        f.collateral(n, 2000, true, false);
    }
    f.cutoff(109);
    assert_eq!(f.state().epoch, 1);
    assert!(f.db.get_epoch_committee_number(id(), 2).unwrap().is_none());
    f.cutoff(110);
    assert_eq!(f.state().epoch, 2);
    assert!(f.db.get_epoch_committee_number(id(), 2).unwrap().is_some());
    f.cutoff(111);
    assert_eq!(f.state().epoch, 2);
}

#[tokio::test]
async fn a_bootstrapped_subnet_has_no_cutoffs_until_h_zero_is_known() {
    let f = Fixture::new().await;
    f.epoch_zero();
    // A C_gen collateral that is not slashable: no height is H_0
    f.collateral(1, 2000, false, false);
    f.cutoff(110);
    f.cutoff(1000);
    assert_eq!(f.state().epoch, 0);
    assert_eq!(f.state().h_0, None);
    assert!(f.changes().is_empty());
}

#[tokio::test]
async fn an_empty_candidate_set_retains_the_previous_configuration() {
    let f = Fixture::new().await;
    for n in 1..=4 {
        f.collateral(n, 140, true, false);
    }
    f.cutoff(110);
    let state = f.state();
    assert_eq!(state.epoch, 2);
    assert_eq!(state.latest_committee().configuration_number, 4);
    assert_eq!(state.latest_committee().validators.len(), 4);
    assert!(f.changes().is_empty());
}

#[tokio::test]
async fn a_small_configuration_records_departures_and_remains_operational() {
    let f = Fixture::new().await;
    f.collateral(1, 140, true, false);
    for n in 2..=4 {
        f.collateral(n, 2000, true, false);
    }
    f.cutoff(110);
    let state = f.state();
    assert_eq!(state.latest_committee().validators.len(), 3);
    assert_eq!(state.epoch, 2);
    assert_eq!(state.committee.validators.len(), 4);
    assert_eq!(state.last_rotation_epoch, 1);
    assert_eq!(f.changes().len(), 1);
    f.cutoff(120);
    assert_eq!(f.state().epoch, 3);
    assert_eq!(f.changes().len(), 1); // whole-value departure recorded exactly once
}

#[tokio::test]
async fn a_quorum_recorded_at_the_cutoff_is_eligible_at_that_cutoff() {
    let f = Fixture::new().await;
    for n in 1..=4 {
        f.collateral(n, 2000, true, false);
    }
    let point = f.collateral(5, 2000, true, false);
    let mut presigs = f.db.get_slash_presigs(id(), &point).unwrap().unwrap();
    presigs.block_height = 110;
    let mut txn = f.db.write_txn().unwrap();
    f.db.add_slash_presigs(&mut txn, &presigs).unwrap();
    txn.commit().unwrap();
    f.cutoff(110);
    assert!(f
        .state()
        .latest_committee()
        .validators
        .iter()
        .any(|v| v.pubkey == pubkey(5)));
    assert!(f.changes().iter().all(|c| c.block_height == 110));
}

#[tokio::test]
async fn multisig_value_increase_waits_for_the_cutoff_and_is_consumed_once() {
    let f = Fixture::new().await;
    let mut state = f.state();
    state
        .pending_multisig_changes
        .push(PendingMultisigChange::Stake {
            pubkey: pubkey(2),
            amount: Amount::from_sat(25_001),
        });
    let mut txn = f.db.write_txn().unwrap();
    f.db.save_subnet_state(&mut txn, id(), &state).unwrap();
    txn.commit().unwrap();
    f.cutoff(109);
    assert_eq!(
        f.state()
            .latest_committee()
            .validators
            .iter()
            .find(|v| v.pubkey == pubkey(2))
            .unwrap()
            .collateral
            .to_sat(),
        100_000
    );
    f.cutoff(110);
    let state = f.state();
    let v = state
        .latest_committee()
        .validators
        .iter()
        .find(|v| v.pubkey == pubkey(2))
        .unwrap();
    assert_eq!(v.collateral.to_sat(), 125_001);
    assert_eq!(v.power, 12);
    assert!(state.pending_multisig_changes.is_empty());
    assert_eq!(f.changes().len(), 1);
    f.cutoff(120);
    assert_eq!(f.changes().len(), 1);
}

#[tokio::test]
async fn multisig_value_decrease_waits_for_the_cutoff() {
    // Keep the member above min_validator_stake after a partial decrease.
    let f = Fixture::with_second_collateral(150_000).await;
    let mut state = f.state();
    state
        .pending_multisig_changes
        .push(PendingMultisigChange::Unstake {
            pubkey: pubkey(2),
            amount: Amount::from_sat(25_001),
        });
    let mut txn = f.db.write_txn().unwrap();
    f.db.save_subnet_state(&mut txn, id(), &state).unwrap();
    txn.commit().unwrap();
    f.cutoff(109);
    assert!(f.changes().is_empty());
    f.cutoff(110);
    let changes = f.changes();
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(changes[0].change,StakingChange::Withdraw { amount } if amount.to_sat()==25_001)
    );
    assert_eq!(
        f.state()
            .latest_committee()
            .validators
            .iter()
            .find(|v| v.pubkey == pubkey(2))
            .unwrap()
            .power,
        12
    );
}

fn active_join(sats: u64) -> crate::collateral::CheckedJoinUe {
    let join = crate::ipc_lib::IpcJoinSubnetMsg {
        subnet_id: id(),
        collateral: Amount::from_sat(sats),
        pubkey: pubkey(5),
        ip: "192.0.2.5:8080".parse().unwrap(),
        backup_address: member(5, 100_000).backup_address,
    };
    crate::collateral::CheckedJoinUe {
        msg: crate::ipc_lib::IpcJoinUeMsg::new_signed(
            &Secp256k1::new(),
            join,
            2000,
            &secret(5),
            &secret(25),
            crate::eots::EotsNonceCommitment {
                start: 1,
                count: 1,
                root: [0x72; 32],
            },
        ),
        outpoint: OutPoint::new(Txid::from_byte_array([0x73; 32]), 1),
        committee_number: 1,
    }
}

#[tokio::test]
async fn active_join_accepts_the_exact_total_power_limit_with_a_fractional_unit() {
    let f = Fixture::new().await;
    for n in 1..=4 {
        f.collateral(n, 2000, true, false);
    }
    // Four members hold 10 units each; the extra 9999 sat do not make another unit.
    let sats = (2_147_483_647u64 - 40) * 10_000 + 9_999;
    let join = active_join(sats);
    let mut txn = f.db.write_txn().unwrap();
    crate::collateral::process_join_ue(
        &f.db,
        &mut txn,
        &join,
        Txid::from_byte_array([0x74; 32]),
        109,
        hash(),
    )
    .unwrap();
    txn.commit().unwrap();
    assert_eq!(f.db.get_ue_collaterals(id()).unwrap().len(), 5);
    assert_eq!(
        genesis()
            .create_subnet_msg
            .power_of(&Amount::from_sat(sats))
            .unwrap(),
        2_147_483_607
    );
}

#[tokio::test]
async fn active_join_rejects_one_power_unit_above_the_total_limit_without_reserving_its_identity() {
    let f = Fixture::new().await;
    for n in 1..=4 {
        f.collateral(n, 2000, true, false);
    }
    let join = active_join((2_147_483_647u64 - 39) * 10_000);
    let mut txn = f.db.write_txn().unwrap();
    assert!(crate::collateral::process_join_ue(
        &f.db,
        &mut txn,
        &join,
        Txid::from_byte_array([0x74; 32]),
        109,
        hash()
    )
    .is_err());
    txn.commit().unwrap();
    assert_eq!(f.db.get_ue_collaterals(id()).unwrap().len(), 4);
    assert!(f
        .db
        .get_eots_commitments(id(), &pubkey(25))
        .unwrap()
        .is_empty());
    assert!(f.changes().is_empty());
    assert_eq!(f.state().epoch, 1);
}

#[tokio::test]
async fn active_join_counts_pending_multisig_changes_in_the_total_power() {
    let f = Fixture::new().await;
    for n in 1..=4 {
        f.collateral(n, 2000, true, false);
    }
    let mut state = f.state();
    state.pending_multisig_changes = vec![PendingMultisigChange::Join(member(6, 100_000))];
    let mut txn = f.db.write_txn().unwrap();
    f.db.save_subnet_state(&mut txn, id(), &state).unwrap();
    txn.commit().unwrap();
    // The exact total power limit without the 10 units of the pending join
    let join = active_join((2_147_483_647u64 - 40) * 10_000);
    let mut txn = f.db.write_txn().unwrap();
    assert!(crate::collateral::process_join_ue(
        &f.db,
        &mut txn,
        &join,
        Txid::from_byte_array([0x74; 32]),
        109,
        hash()
    )
    .is_err());
    txn.commit().unwrap();
    assert_eq!(f.db.get_ue_collaterals(id()).unwrap().len(), 4);
}

/// The largest collateral of a multisig join beside the four multisig members of `Fixture::new`
const JOIN_AT_LIMIT: u64 = (2_147_483_647 - 40) * 10_000 + 9_999;
/// The largest stake of `pubkey(2)`, which holds 100_000 sat, in `Fixture::new`
const STAKE_AT_LIMIT: u64 = (2_147_483_647 - 30) * 10_000 + 9_999 - 100_000;

fn multisig_join(f: &Fixture, sats: u64) -> Result<Option<SubnetState>, IpcLibError> {
    let mut txn = f.db.write_txn().unwrap();
    let joined = active_join(sats).msg.join.save_to_db(
        &f.db,
        &mut txn,
        109,
        hash(),
        Txid::from_byte_array([0x74; 32]),
    );
    txn.commit().unwrap();
    joined
}

fn multisig_stake(f: &Fixture, sats: u64) -> Result<(), IpcLibError> {
    let mut txn = f.db.write_txn().unwrap();
    let staked = crate::ipc_lib::IpcStakeCollateralMsg {
        subnet_id: id(),
        amount: Amount::from_sat(sats),
        pubkey: pubkey(2),
    }
    .save_to_db(
        &f.db,
        &mut txn,
        109,
        hash(),
        Txid::from_byte_array([0x75; 32]),
    );
    txn.commit().unwrap();
    staked
}

fn refused_for_total_power<T>(result: Result<T, IpcLibError>) -> bool {
    matches!(
        result,
        Err(IpcLibError::IpcValidateError(IpcValidateError::InvalidMsg(m)))
            if m.contains("total power")
    )
}

#[tokio::test]
async fn multisig_join_accepts_the_exact_total_power_limit_and_refuses_one_unit_more() {
    let f = Fixture::new().await;
    assert!(refused_for_total_power(multisig_join(
        &f,
        JOIN_AT_LIMIT + 1
    )));
    assert!(f.state().pending_multisig_changes.is_empty());
    multisig_join(&f, JOIN_AT_LIMIT).unwrap();
    assert_eq!(f.state().pending_multisig_changes.len(), 1);
}

#[tokio::test]
async fn multisig_join_before_bootstrap_counts_every_validator_in_the_total_power() {
    let (_dir, db) = database().await;
    let id = subnet(83);
    create(&db, id);
    process(&db, &checked(id, 1, 21, 11), 31).unwrap();
    let join = |n: u8, sats: u64| {
        let mut join = checked(id, n, n + 20, n + 10).msg.join;
        join.collateral = Amount::from_sat(sats);
        let mut txn = db.write_txn().unwrap();
        let joined = join.save_to_db(
            &db,
            &mut txn,
            102,
            hash(),
            Txid::from_byte_array([n + 30; 32]),
        );
        txn.commit().unwrap();
        joined
    };
    join(2, 100_000).unwrap();
    let validators = || {
        db.get_subnet_genesis_info(id)
            .unwrap()
            .unwrap()
            .genesis_validators
            .len()
    };
    let before = validators();
    // `pubkey(1)` holds a collateral and `pubkey(2)` is a multisig member, of 10 units each
    assert!(refused_for_total_power(join(
        3,
        (2_147_483_647 - 19) * 10_000
    )));
    assert_eq!(validators(), before);
    join(3, (2_147_483_647 - 20) * 10_000).unwrap();
    assert_eq!(validators(), before + 1);
    assert!(db.get_subnet_state(id).unwrap().is_none());
}

#[tokio::test]
async fn multisig_stake_counts_the_power_of_the_staker_once_in_the_total_power() {
    let f = Fixture::new().await;
    assert!(refused_for_total_power(multisig_stake(
        &f,
        STAKE_AT_LIMIT + 1
    )));
    assert!(f.state().pending_multisig_changes.is_empty());
    multisig_stake(&f, STAKE_AT_LIMIT).unwrap();
    assert_eq!(f.state().pending_multisig_changes.len(), 1);
}

#[tokio::test]
async fn multisig_join_and_stake_count_pending_multisig_changes_in_the_total_power() {
    for pending in [
        PendingMultisigChange::Join(member(6, 100_000)),
        PendingMultisigChange::Stake {
            pubkey: pubkey(3),
            amount: Amount::from_sat(10_000),
        },
    ] {
        let f = Fixture::new().await;
        let mut state = f.state();
        state.pending_multisig_changes = vec![pending];
        let mut txn = f.db.write_txn().unwrap();
        f.db.save_subnet_state(&mut txn, id(), &state).unwrap();
        txn.commit().unwrap();
        assert!(refused_for_total_power(multisig_join(&f, JOIN_AT_LIMIT)));
        assert!(refused_for_total_power(multisig_stake(&f, STAKE_AT_LIMIT)));
        assert_eq!(f.state().pending_multisig_changes.len(), 1);
    }
}

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

#[tokio::test]
async fn an_epoch_zero_checkpoint_is_recorded_as_a_regular_checkpoint_of_c_wlist() {
    let f = EpochZeroFixture::new([1000; 4]).await;
    let msg = f.message(&[1, 2, 3, 4]);
    let (tx, _reveal) = f.transactions(&msg);
    let cp = f.accept(&msg, &tx, 110);
    assert_eq!(cp.signed_committee_number, WHITELIST_COMMITTEE_NUMBER);
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

#[tokio::test]
async fn activation_changes_follow_the_genesis_unit_values_and_begin_at_one() {
    let f = EpochZeroFixture::new([1000; 4]).await;
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
    assert!(changes
        .iter()
        .all(|c| matches!(c.change,StakingChange::Deposit { amount } if amount.to_sat()==90_000)));
    assert!(changes
        .windows(2)
        .all(|w| w[0].validator_xpk < w[1].validator_xpk));
}

#[tokio::test]
async fn activation_with_no_qualifying_collateral_keeps_genesis_power() {
    let f = EpochZeroFixture::new([135; 4]).await;
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

#[tokio::test]
async fn epoch_zero_deposits_are_recorded_at_their_own_blocks_with_continuing_nonces() {
    let f = EpochZeroFixture::new([1000; 4]).await;
    for (i, sats) in [777, 330, 1001].into_iter().enumerate() {
        let mut txn = f.db.write_txn().unwrap();
        IpcFundSubnetMsg {
            subnet_id: f.id,
            amount: Amount::from_sat(sats),
            address: alloy_primitives::Address::from([i as u8 + 1; 20]),
        }
        .save_to_db(
            &f.db,
            &mut txn,
            103 + i as u64,
            BlockHash::from_byte_array([i as u8; 32]),
            Txid::from_byte_array([i as u8 + 101; 32]),
        )
        .unwrap();
        txn.commit().unwrap();
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
    let mut txn = f.db.write_txn().unwrap();
    IpcFundSubnetMsg {
        subnet_id: f.id,
        amount: Amount::from_sat(444),
        address: alloy_primitives::Address::from([4; 20]),
    }
    .save_to_db(
        &f.db,
        &mut txn,
        112,
        BlockHash::from_byte_array([112; 32]),
        Txid::from_byte_array([104; 32]),
    )
    .unwrap();
    txn.commit().unwrap();
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

#[tokio::test]
async fn a_c_gen_collateral_slashed_in_epoch_zero_is_left_out_of_c_1() {
    let f = EpochZeroFixture::new([1000; 4]).await;
    let msg = f.message(&[1, 2, 3, 4]);
    let (tx, reveal) = f.transactions(&msg);
    f.accept(&msg, &tx, 110);
    f.reveal(&reveal, 111).unwrap();
    let slashed =
        f.db.get_ue_collaterals(f.id)
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

#[tokio::test]
async fn h_0_is_the_first_height_at_which_every_unspent_c_gen_collateral_is_slashable() {
    let f = EpochZeroFixture::new([1000; 4]).await;
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

async fn waiting_rotation() -> EpochZeroFixture {
    let f = EpochZeroFixture::new([1000; 4]).await;
    let first = f.message(&[1, 2, 3, 4]);
    let (tx, reveal) = f.transactions(&first);
    f.accept(&first, &tx, 110);
    f.reveal(&reveal, 111).unwrap();
    f.cutoff(111);
    // Record epoch 1's rotation before H_1; its four updates are already applied
    // when the epoch-2 checkpoint below carries the previous change number.
    let epoch_one = rotation(&f, 4);
    let mut txn = f.db.write_txn().unwrap();
    epoch_one
        .save_to_db(
            &f.db,
            &mut txn,
            112,
            BlockHash::from_byte_array([112; 32]),
            Txid::from_byte_array([0x97; 32]),
        )
        .unwrap();
    txn.commit().unwrap();
    let mut join = checked(f.id, 5, 25, 15);
    join.committee_number = f.state().committee_number;
    let mut txn = f.db.write_txn().unwrap();
    process_join_ue(
        &f.db,
        &mut txn,
        &join,
        Txid::from_byte_array([35; 32]),
        113,
        BlockHash::from_byte_array([113; 32]),
    )
    .unwrap();
    txn.commit().unwrap();
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
    let mut txn = f.db.write_txn().unwrap();
    crate::epochs::process_cutoffs(&f.db, &mut txn, 121, BlockHash::from_byte_array([121; 32]))
        .unwrap();
    txn.commit().unwrap();
    f
}

#[tokio::test]
async fn rotation_with_zero_or_an_older_change_number_precedes_regular_update_confirmation() {
    for applied in [0, 4, 5] {
        let f = waiting_rotation().await;
        let msg = rotation(&f, applied);
        let mut txn = f.db.write_txn().unwrap();
        let cp = msg
            .save_to_db(
                &f.db,
                &mut txn,
                122,
                BlockHash::from_byte_array([122; 32]),
                Txid::from_byte_array([0x96; 32]),
            )
            .unwrap();
        txn.commit().unwrap();
        assert!(cp.is_rotation_checkpoint);
        assert_eq!(cp.epoch, 2);
        assert_eq!(cp.checkpoint_number, 2);
        let recorded = f.db.get_checkpoint(f.id, 2).unwrap().unwrap();
        assert!(recorded.is_rotation_checkpoint);
        assert_eq!(recorded.epoch, 2);
        assert_eq!(recorded.txid, cp.txid);
        let state = f.state();
        assert_eq!(state.last_rotation_epoch, 2);
        assert_eq!(state.committee.configuration_number, 6);
        assert_eq!(state.committee.validators.len(), 5);
        assert!(state.waiting_committee.is_none());
        let pending = f.db.get_unconfirmed_stake_changes(f.id).unwrap();
        let expected_pending = if applied == 5 { vec![6] } else { vec![5, 6] };
        assert_eq!(
            pending
                .iter()
                .map(|c| c.configuration_number)
                .collect::<Vec<_>>(),
            expected_pending
        );

        // Applying the new change number cannot make another rotation in epoch 2.
        let mut regular = msg.clone();
        regular.next_committee_configuration_number = 6;
        regular.checkpoint_height += 30;
        regular.checkpoint_hash =
            bitcoin::hashes::sha256::Hash::hash(&regular.checkpoint_height.to_le_bytes());
        let before = serde_json::to_value(f.state()).unwrap();
        let mut txn = f.db.write_txn().unwrap();
        assert!(regular
            .save_to_db(
                &f.db,
                &mut txn,
                123,
                BlockHash::from_byte_array([123; 32]),
                Txid::from_byte_array([0x98; 32]),
            )
            .is_err());
        txn.commit().unwrap();
        assert_eq!(serde_json::to_value(f.state()).unwrap(), before);
        assert_eq!(
            f.db.get_unconfirmed_stake_changes(f.id).unwrap().len(),
            expected_pending.len()
        );

        regular.is_rotation_checkpoint = false;
        let mut txn = f.db.write_txn().unwrap();
        let cp = regular
            .save_to_db(
                &f.db,
                &mut txn,
                123,
                BlockHash::from_byte_array([123; 32]),
                Txid::from_byte_array([0x98; 32]),
            )
            .unwrap();
        txn.commit().unwrap();
        assert!(!cp.is_rotation_checkpoint);
        assert_eq!(cp.epoch, 2);
        assert_eq!(cp.checkpoint_number, 3);
        assert_eq!(f.state().last_rotation_epoch, 2);
        assert!(f.db.get_unconfirmed_stake_changes(f.id).unwrap().is_empty());
        for change in f.db.get_all_stake_changes(f.id).unwrap() {
            let height = if change.configuration_number <= 4 {
                112
            } else if change.configuration_number <= applied {
                122
            } else {
                123
            };
            assert_eq!(change.checkpoint_block_height, Some(height));
            assert_eq!(
                change.checkpoint_block_hash,
                Some(BlockHash::from_byte_array([height as u8; 32]))
            );
        }
    }
}

#[tokio::test]
async fn rotation_with_the_latest_change_number_confirms_all_released_updates() {
    let f = waiting_rotation().await;
    let msg = rotation(&f, 6);
    let mut txn = f.db.write_txn().unwrap();
    let cp = msg
        .save_to_db(
            &f.db,
            &mut txn,
            122,
            BlockHash::from_byte_array([122; 32]),
            Txid::from_byte_array([0x96; 32]),
        )
        .unwrap();
    txn.commit().unwrap();
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

#[tokio::test]
async fn rotation_with_an_unreleased_change_number_is_rejected_atomically() {
    let f = waiting_rotation().await;
    let before_state = serde_json::to_value(f.state()).unwrap();
    let before_changes = serde_json::to_value(f.db.get_all_stake_changes(f.id).unwrap()).unwrap();
    let msg = rotation(&f, 7);
    let mut txn = f.db.write_txn().unwrap();
    assert!(msg
        .save_to_db(
            &f.db,
            &mut txn,
            122,
            BlockHash::from_byte_array([122; 32]),
            Txid::from_byte_array([0x96; 32]),
        )
        .is_err());
    txn.commit().unwrap();
    assert_eq!(serde_json::to_value(f.state()).unwrap(), before_state);
    assert_eq!(
        serde_json::to_value(f.db.get_all_stake_changes(f.id).unwrap()).unwrap(),
        before_changes
    );
    assert!(f.db.get_checkpoint(f.id, 2).unwrap().is_none());
}

#[tokio::test]
async fn rotation_whose_change_does_not_pay_the_new_multisig_is_rejected() {
    let f = waiting_rotation().await;
    let state = f.state();
    let previous = state.committee.multisig_address.clone();
    assert_ne!(previous, state.latest_committee().multisig_address);
    let mut msg = rotation(&f, 6);
    msg.change_address = Some(previous);

    let mut txn = f.db.write_txn().unwrap();
    let err = msg
        .save_to_db(
            &f.db,
            &mut txn,
            122,
            BlockHash::from_byte_array([122; 32]),
            Txid::from_byte_array([0x96; 32]),
        )
        .unwrap_err();
    txn.commit().unwrap();

    assert!(
        matches!(
            err,
            crate::ipc_lib::IpcLibError::IpcValidateError(
                crate::ipc_lib::IpcValidateError::InvalidField("change_address", _)
            )
        ),
        "{err}"
    );
    assert_eq!(f.state().last_rotation_epoch, state.last_rotation_epoch);
    assert!(f.db.get_checkpoint(f.id, 2).unwrap().is_none());
}

/// Asserts that `msg` with each change of `wrong` is rejected and changes no state
fn assert_change_refused(
    f: &EpochZeroFixture,
    msg: &IpcCheckpointSubnetMsg,
    wrong: &[Option<bitcoin::Address<bitcoin::address::NetworkUnchecked>>],
    height: u64,
) {
    let before = serde_json::to_value(f.state()).unwrap();
    let number = f.state().last_checkpoint_number.map_or(0, |n| n + 1);
    for change in wrong {
        let mut msg = msg.clone();
        msg.change_address = change.clone();
        let mut txn = f.db.write_txn().unwrap();
        let err = msg
            .save_to_db(
                &f.db,
                &mut txn,
                height,
                BlockHash::from_byte_array([height as u8; 32]),
                Txid::from_byte_array([0x99; 32]),
            )
            .unwrap_err();
        txn.commit().unwrap();
        assert!(
            matches!(
                err,
                IpcLibError::IpcValidateError(IpcValidateError::InvalidField("change_address", _))
            ),
            "{change:?}: {err}"
        );
        assert_eq!(serde_json::to_value(f.state()).unwrap(), before);
        assert!(f.db.get_checkpoint(f.id, number).unwrap().is_none());
    }
}

#[tokio::test]
async fn every_checkpoint_but_the_kill_checkpoint_pays_its_change_to_ms_of_its_configuration() {
    let f = EpochZeroFixture::new([1000; 4]).await;
    let c_wlist = f.signer().address.as_unchecked().clone();
    let c_gen = f.state().committee.multisig_address.clone();
    assert_ne!(c_wlist, c_gen);
    let accept = |msg: &IpcCheckpointSubnetMsg, height: u64| {
        let mut txn = f.db.write_txn().unwrap();
        let checkpoint = msg
            .save_to_db(
                &f.db,
                &mut txn,
                height,
                BlockHash::from_byte_array([height as u8; 32]),
                Txid::from_byte_array([height as u8; 32]),
            )
            .unwrap();
        txn.commit().unwrap();
        checkpoint
    };

    // Epoch 0
    let first = f.message(&[1, 2, 3, 4]);
    assert_change_refused(&f, &first, &[Some(c_gen.clone()), None], 110);
    let (tx, reveal) = f.transactions(&first);
    f.accept(&first, &tx, 110);
    f.reveal(&reveal, 111).unwrap();
    f.cutoff(111);

    // Epoch 1, before its rotation checkpoint
    let mut regular = rotation(&f, 0);
    regular.is_rotation_checkpoint = false;
    regular.change_address = Some(c_wlist.clone());
    assert_change_refused(&f, &regular, &[Some(c_gen.clone()), None], 112);
    assert!(!accept(&regular, 112).is_rotation_checkpoint);

    let epoch_one = rotation(&f, 4);
    let c_1 = epoch_one.change_address.clone().unwrap();
    assert_change_refused(
        &f,
        &epoch_one,
        &[Some(c_wlist.clone()), Some(c_gen), None],
        113,
    );
    assert!(accept(&epoch_one, 113).is_rotation_checkpoint);

    // Epoch 1, after its rotation checkpoint
    let mut regular = rotation(&f, 0);
    regular.is_rotation_checkpoint = false;
    regular.change_address = Some(c_1);
    assert_change_refused(&f, &regular, &[Some(c_wlist), None], 114);
    assert!(!accept(&regular, 114).is_rotation_checkpoint);
}
