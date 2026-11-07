use super::*;
use crate::db::{
    HeedDb, SlashPresigRecord, StakingChange, SubnetKillState, SubnetValidators, UeCollateral,
};
use crate::ipc_lib::IpcCreateSubnetMsg;
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use bitcoin::OutPoint;

fn secret(n: u8) -> SecretKey {
    let mut bytes = [0; 32];
    bytes[31] = n;
    SecretKey::from_slice(&bytes).unwrap()
}

fn key(n: u8) -> XOnlyPublicKey {
    secret(n).x_only_public_key(&Secp256k1::new()).0
}
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
        pubkey: key(n),
        subnet_address: eth_addr_from_x_only_pubkey(key(n)),
        power: (sats / 10_000) as _,
        collateral: Amount::from_sat(sats),
        backup_address: bitcoin::Address::p2tr(&Secp256k1::new(), key(n), None, crate::NETWORK)
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
            whitelist: (1..=6).map(key).collect(),
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
        *members.iter_mut().find(|v| v.pubkey == key(2)).unwrap() = member(2, second_sats);
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
            pk_eots: key(n + 20),
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
    /// Records slash pre-signatures for `outpoint` at `height`: slashable from then on
    fn make_slashable(&self, outpoint: OutPoint, height: u64) {
        let mut txn = self.db.write_txn().unwrap();
        self.db
            .add_slash_presigs(
                &mut txn,
                &SlashPresigRecord {
                    subnet_id: id(),
                    outpoint,
                    signatures: vec![],
                    block_height: height,
                    batch_transfer_txid: Txid::from_byte_array([0x72; 32]),
                },
            )
            .unwrap();
        txn.commit().unwrap();
    }
    fn cutoff(&self, height: u64) {
        process_cutoffs(&self.db, height, hash()).unwrap();
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

// lifecycle.md, Adopting a new configuration / Changes: a Join precedes a Deposit of the entrant's whole value.
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
        .all(|c| c.validator_xpk == key(5) && c.block_height == 110 && c.block_hash == hash()));
    assert_eq!(next.configuration_number, 9);
}

// lifecycle.md, Adopting a new configuration / Changes: continuing collateral increases record only the difference.
#[test]
fn continuing_member_increase_records_the_value_difference() {
    let base = committee();
    let mut target = base.validators.clone();
    *target.iter_mut().find(|v| v.pubkey == key(2)).unwrap() = member(2, 127_999);
    let (_, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(changes[0].change,StakingChange::Deposit { amount } if amount.to_sat()==27_999)
    );
}

// lifecycle.md, Adopting a new configuration / Changes: continuing collateral decreases record only the difference.
#[test]
fn continuing_member_decrease_records_the_value_difference() {
    let mut base = committee();
    *base
        .validators
        .iter_mut()
        .find(|v| v.pubkey == key(2))
        .unwrap() = member(2, 150_000);
    base = base.validators.to_committee(&id(), 7);
    let mut target = base.validators.clone();
    *target.iter_mut().find(|v| v.pubkey == key(2)).unwrap() = member(2, 140_001);
    let (_, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(changes[0].change,StakingChange::Withdraw { amount } if amount.to_sat()==9_999)
    );
}

// lifecycle.md, Changes / Power and thresholds: value changes below one power unit still have change numbers.
#[test]
fn a_value_change_with_unchanged_power_is_still_recorded() {
    let base = committee();
    let mut target = base.validators.clone();
    *target.iter_mut().find(|v| v.pubkey == key(2)).unwrap() = member(2, 100_001);
    let (next, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(changes.len(), 1);
    assert!(matches!(changes[0].change,StakingChange::Deposit { amount } if amount.to_sat()==1));
    assert_eq!(
        next.validators
            .iter()
            .find(|v| v.pubkey == key(2))
            .unwrap()
            .power,
        10
    );
    assert_eq!(next.configuration_number, 8);
}

// lifecycle.md, Changes: a departure withdraws every satoshi, including a fractional power unit.
#[test]
fn departure_withdraws_the_entire_value_including_its_power_remainder() {
    let mut before = (1..=4).map(|n| member(n, 100_000)).collect::<Vec<_>>();
    before.push(member(5, 123_456));
    let base = before.to_committee(&id(), 7);
    let target = before
        .into_iter()
        .filter(|v| v.pubkey != key(5))
        .collect::<Vec<_>>();
    let (_, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(changes[0].change,StakingChange::Withdraw { amount } if amount.to_sat()==123_456)
    );
}

// lifecycle.md, Changes: simultaneous arrivals and departures are ordered by the 32 key bytes.
#[test]
fn mixed_changes_follow_key_order_even_with_reversed_input_vectors() {
    let mut base = committee();
    base.validators.reverse();
    let mut target: Vec<_> = base
        .validators
        .iter()
        .filter(|v| v.pubkey != key(1))
        .cloned()
        .collect();
    target.push(member(5, 120_000));
    target.reverse();
    let (_, changes) = configuration_changes(id(), &base, &target, 8, 110, hash()).unwrap();
    let mut expected = vec![(key(1), "withdraw"), (key(5), "join"), (key(5), "deposit")];
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

// lifecycle.md, Change numbers: an unchanged epoch consumes no number and the next change continues the sequence.
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
    let (c4, last) = configuration_changes(id(), &c3, &base.validators, 10, 130, hash()).unwrap();
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].configuration_number, 10);
    assert_eq!(c4.configuration_number, 10);
}

// lifecycle.md, C_e conditions 2-3: T==boundary is excluded, T==boundary+1 is included.
#[tokio::test]
async fn eligibility_has_a_strict_timeout_boundary() {
    let f = Fixture::new().await;
    f.collateral(1, 139, true, false);
    f.collateral(2, 140, true, false);
    f.collateral(3, 141, true, false);
    f.collateral(4, 2000, true, false);
    let target = compute_configuration(&f.db, &genesis(), &committee(), &[], 140).unwrap();
    let mut keys = target.iter().map(|v| v.pubkey).collect::<Vec<_>>();
    keys.sort();
    let mut expected = vec![key(3), key(4)];
    expected.sort();
    assert_eq!(keys, expected);
}

// slashing.md, Slashable outputs: a spent output with valid pre-signatures cannot seat its owner.
#[tokio::test]
async fn spent_slashable_collateral_is_excluded_from_the_next_configuration() {
    let f = Fixture::new().await;
    f.collateral(1, 2000, true, true);
    let target = compute_configuration(&f.db, &genesis(), &committee(), &[], 140).unwrap();
    assert!(!target.iter().any(|v| v.pubkey == key(1)));
}

// lifecycle.md, Epoch 0 / Activation: H_0 is the first Bitcoin height at which every unspent
// C_gen collateral is slashable; no earlier height is a cutoff.
#[tokio::test]
async fn h_0_is_the_first_height_at_which_every_unspent_c_gen_collateral_is_slashable() {
    let f = Fixture::new().await;
    f.epoch_zero();
    for n in 1..=3 {
        f.collateral(n, 2000, true, false);
    }
    let last = f.collateral(4, 2000, false, false);
    f.cutoff(110);
    f.cutoff(119);
    assert_eq!(f.state().epoch, 0);
    assert_eq!(f.state().h_0, None);
    f.make_slashable(last, 120);
    f.cutoff(120);
    assert_eq!(f.state().h_0, Some(120));
    assert_eq!(f.state().epoch, 1);
    // C_1 has the members of C_gen with their collaterals: no change to record
    assert!(f.changes().is_empty());
    for n in 1..=4 {
        assert!(f.state().latest_committee().is_validator(&key(n)));
    }
}

// lifecycle.md, Activation / C_1: a C_gen collateral slashed in epoch 0 neither delays H_0 nor
// seats its owner in C_1.
#[tokio::test]
async fn a_c_gen_collateral_slashed_in_epoch_zero_is_left_out_of_c_1() {
    let f = Fixture::new().await;
    f.epoch_zero();
    f.collateral(1, 2000, true, true);
    for n in 2..=4 {
        f.collateral(n, 2000, true, false);
    }
    f.cutoff(120);
    assert_eq!(f.state().h_0, Some(120));
    let c_1 = f.state().latest_committee().clone();
    assert!(!c_1.is_validator(&key(1)));
    for n in 2..=4 {
        assert!(c_1.is_validator(&key(n)));
    }
}

// lifecycle.md, Epochs / Processing Bitcoin L1 blocks: the next configuration appears after H_1, never at H_1-1.
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

// lifecycle.md, Epoch 0: elapsed Bitcoin blocks alone cannot start epoch 1.
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

// lifecycle.md, C_e: when no collateral qualifies the previous configuration remains, with no fake withdrawals.
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
    assert!(!state.is_killed());
}

// lifecycle.md, C_e / Protocol constants: fewer than four eligible members records changes and keeps the subnet live.
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
    assert!(!state.is_killed());
    assert_eq!(state.epoch, 2);
    assert_eq!(state.committee.validators.len(), 4);
    assert_eq!(state.last_rotation_epoch, 1);
    assert_eq!(f.changes().len(), 1);
    f.cutoff(120);
    assert_eq!(f.state().epoch, 3);
    assert!(!f.state().is_killed());
    assert_eq!(f.changes().len(), 1); // whole-value departure recorded exactly once
}

// lifecycle.md, Processing Bitcoin L1 blocks / slashing.md, After activation: a quorum in the cutoff block counts there.
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
        .any(|v| v.pubkey == key(5)));
    assert!(f.changes().iter().all(|c| c.block_height == 110));
}

// lifecycle.md, Adopting a new configuration: a pending multisig stake is applied only at a cutoff.
#[tokio::test]
async fn multisig_value_increase_waits_for_the_cutoff_and_is_consumed_once() {
    let f = Fixture::new().await;
    let mut state = f.state();
    state
        .pending_multisig_changes
        .push(PendingMultisigChange::Stake {
            pubkey: key(2),
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
            .find(|v| v.pubkey == key(2))
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
        .find(|v| v.pubkey == key(2))
        .unwrap();
    assert_eq!(v.collateral.to_sat(), 125_001);
    assert_eq!(v.power, 12);
    assert!(state.pending_multisig_changes.is_empty());
    assert_eq!(f.changes().len(), 1);
    f.cutoff(120);
    assert_eq!(f.changes().len(), 1);
}

// lifecycle.md, Adopting a new configuration: multisig unstakes affect value and power at the next cutoff.
#[tokio::test]
async fn multisig_value_decrease_waits_for_the_cutoff() {
    // Keep the member above min_validator_stake after a partial decrease.
    let f = Fixture::with_second_collateral(150_000).await;
    let mut state = f.state();
    state
        .pending_multisig_changes
        .push(PendingMultisigChange::Unstake {
            pubkey: key(2),
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
            .find(|v| v.pubkey == key(2))
            .unwrap()
            .power,
        12
    );
}

fn active_join(f: &Fixture, sats: u64) -> crate::collateral::CheckedJoinUe {
    let join = crate::ipc_lib::IpcJoinSubnetMsg {
        subnet_id: id(),
        collateral: Amount::from_sat(sats),
        pubkey: key(5),
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

// lifecycle.md, Power and thresholds / join.md, Verifying 3: the total-power ceiling includes MAX_TOTAL_POWER exactly.
#[tokio::test]
async fn active_join_accepts_the_exact_total_power_limit_with_a_fractional_unit() {
    let f = Fixture::new().await;
    for n in 1..=4 {
        f.collateral(n, 2000, true, false);
    }
    // Four members hold 10 units each; the extra 9999 sat do not make another unit.
    let sats = (2_147_483_647u64 - 40) * 10_000 + 9_999;
    let join = active_join(&f, sats);
    crate::collateral::process_join_ue(
        &f.db,
        &join,
        Txid::from_byte_array([0x74; 32]),
        109,
        hash(),
    )
    .unwrap();
    assert_eq!(f.db.get_ue_collaterals(id()).unwrap().len(), 5);
    assert_eq!(
        genesis()
            .create_subnet_msg
            .power_of(&Amount::from_sat(sats))
            .unwrap(),
        2_147_483_607
    );
}

// lifecycle.md, Power and thresholds / join.md, Verifying 4: exceeding total power by one rejects every join state change.
#[tokio::test]
async fn active_join_rejects_one_power_unit_above_the_total_limit_without_reserving_its_identity() {
    let f = Fixture::new().await;
    for n in 1..=4 {
        f.collateral(n, 2000, true, false);
    }
    let join = active_join(&f, (2_147_483_647u64 - 39) * 10_000);
    assert!(crate::collateral::process_join_ue(
        &f.db,
        &join,
        Txid::from_byte_array([0x74; 32]),
        109,
        hash()
    )
    .is_err());
    assert_eq!(f.db.get_ue_collaterals(id()).unwrap().len(), 4);
    assert!(f
        .db
        .get_eots_commitments(id(), &key(25))
        .unwrap()
        .is_empty());
    assert!(f.changes().is_empty());
    assert_eq!(f.state().epoch, 1);
}
