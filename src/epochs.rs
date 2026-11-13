//! Computation of the configuration `C_e` at each cutoff and of the changes from `C_{e-1}` to it.

use std::collections::BTreeMap;

use bitcoin::{hashes::Hash, Amount, BlockHash, Txid, XOnlyPublicKey};
use heed::{RoTxn, RwTxn};
use log::{info, warn};

use crate::db::{
    DatabaseCore, DbError, PendingMultisigChange, StakeChangeRequest, StakingChange,
    SubnetCommittee, SubnetGenesisInfo, SubnetState, SubnetValidator,
};
use crate::eth_utils::eth_addr_from_x_only_pubkey;
use crate::ipc_lib::{IpcLibError, IpcValidateError, SubnetId};
use crate::multisig::{MultisigError, MIN_CONFIGURATION_SIZE};

/// The number of `C_wlist`.
pub const WHITELIST_COMMITTEE_NUMBER: u64 = 0;

/// What a cutoff did to one subnet.
#[derive(Debug, Clone)]
pub struct CutoffOutcome {
    pub subnet_id: SubnetId,
    /// The epoch that begins at the cutoff.
    pub epoch: u64,
    /// The new configuration with its number, when it differs from the latest one.
    pub new_committee: Option<(u64, SubnetCommittee)>,
}

/// Logs a configuration that may not be able to make progress.
pub fn warn_if_small(subnet_id: SubnetId, epoch: u64, members: usize) {
    if members < MIN_CONFIGURATION_SIZE {
        warn!(
            "Subnet {subnet_id}: the configuration of epoch {epoch} has {members} members, fewer than {MIN_CONFIGURATION_SIZE}, and may not be able to make progress"
        );
    }
}

/// The number of the configuration of epoch `epoch`.
pub fn epoch_committee_number<D: DatabaseCore>(
    db: &D,
    txn: &RoTxn,
    subnet_id: SubnetId,
    epoch: u64,
) -> Result<Option<u64>, DbError> {
    if epoch == 0 {
        return Ok(Some(WHITELIST_COMMITTEE_NUMBER));
    }
    db.get_epoch_committee_number_txn(txn, subnet_id, epoch)
}

/// Whether the member with `join_txid` and `pubkey` joined with `join-ue`.
pub fn is_ue_validator<D: DatabaseCore>(
    db: &D,
    txn: &RoTxn,
    subnet_id: SubnetId,
    join_txid: Txid,
    pubkey: &XOnlyPublicKey,
) -> Result<bool, DbError> {
    Ok(db
        .get_ue_collateral_txn(txn, subnet_id, join_txid, pubkey)?
        .is_some())
}

/// The multisig members of the configuration `committee` (those without a collateral record) after the pending
/// changes, by key.
pub fn multisig_members_after<D: DatabaseCore>(
    db: &D,
    txn: &RoTxn,
    genesis_info: &SubnetGenesisInfo,
    committee: &SubnetCommittee,
    pending: &[PendingMultisigChange],
) -> Result<BTreeMap<XOnlyPublicKey, SubnetValidator>, IpcLibError> {
    let subnet_id = genesis_info.subnet_id;
    let params = &genesis_info.create_subnet_msg;
    let overflow = || IpcValidateError::InvalidMsg("collateral value overflow".to_string());

    let mut members = BTreeMap::new();
    for member in &committee.validators {
        if is_ue_validator(db, txn, subnet_id, member.join_txid, &member.pubkey)? {
            continue;
        }
        let mut member = member.clone();
        // In `C_gen` a multisig member has value 1, replaced here by its join collateral
        if committee.configuration_number == crate::db::GENESIS_COMMITTEE_CONF_NUM {
            if let Some(genesis) = genesis_info
                .genesis_validators
                .iter()
                .find(|v| v.pubkey == member.pubkey)
            {
                member.collateral = genesis.collateral;
                member.power = params.power_of(&genesis.collateral)?;
            }
        }
        members.insert(member.pubkey, member);
    }
    for change in pending {
        match change {
            PendingMultisigChange::Join(validator) => {
                members.insert(validator.pubkey, validator.clone());
            }
            PendingMultisigChange::Stake { pubkey, amount } => {
                if let Some(member) = members.get_mut(pubkey) {
                    member.collateral = member
                        .collateral
                        .checked_add(*amount)
                        .ok_or_else(overflow)?;
                    member.power = params.power_of(&member.collateral)?;
                }
            }
            PendingMultisigChange::Unstake { pubkey, amount } => {
                if let Some(member) = members.get_mut(pubkey) {
                    let rest = member
                        .collateral
                        .checked_sub(*amount)
                        .unwrap_or(Amount::ZERO);
                    match params.power_of(&rest) {
                        Ok(power) => {
                            member.collateral = rest;
                            member.power = power;
                        }
                        Err(MultisigError::InsufficientCollateral) => {
                            members.remove(pubkey);
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
    }
    Ok(members)
}

/// The multisig members of the next configuration as filled so far: those of the current
/// configuration after the pending changes.
pub fn next_multisig_members<D: DatabaseCore>(
    db: &D,
    txn: &RoTxn,
    genesis_info: &SubnetGenesisInfo,
    state: &SubnetState,
) -> Result<BTreeMap<XOnlyPublicKey, SubnetValidator>, IpcLibError> {
    multisig_members_after(
        db,
        txn,
        genesis_info,
        state.latest_committee(),
        &state.pending_multisig_changes,
    )
}

/// The members of the configuration at a cutoff, in ascending key order: validators with a
/// qualifying collateral and the multisig members of `base` after `pending`.
pub fn compute_configuration<D: DatabaseCore>(
    db: &D,
    txn: &RoTxn,
    genesis_info: &SubnetGenesisInfo,
    base: &SubnetCommittee,
    pending: &[PendingMultisigChange],
    boundary: u64,
) -> Result<Vec<SubnetValidator>, IpcLibError> {
    let subnet_id = genesis_info.subnet_id;
    let params = &genesis_info.create_subnet_msg;

    let mut seated: BTreeMap<XOnlyPublicKey, SubnetValidator> = BTreeMap::new();
    for record in db.get_ue_collaterals_txn(txn, subnet_id)? {
        if record.spent || u64::from(record.timeout) <= boundary {
            continue;
        }
        if db
            .get_slash_presigs_txn(txn, subnet_id, &record.outpoint)?
            .is_none()
        {
            continue;
        }
        match seated.get_mut(&record.pubkey) {
            Some(validator) => {
                validator.collateral =
                    validator
                        .collateral
                        .checked_add(record.value)
                        .ok_or_else(|| {
                            IpcValidateError::InvalidMsg("collateral value overflow".to_string())
                        })?;
            }
            None => {
                seated.insert(
                    record.pubkey,
                    SubnetValidator {
                        pubkey: record.pubkey,
                        subnet_address: eth_addr_from_x_only_pubkey(record.pubkey),
                        power: 0,
                        collateral: record.value,
                        backup_address: record.backup_address.clone(),
                        ip: record.ip,
                        join_txid: record.join_txid,
                    },
                );
            }
        }
    }
    for validator in seated.values_mut() {
        validator.power = params.power_of(&validator.collateral)?;
    }
    for (key, member) in multisig_members_after(db, txn, genesis_info, base, pending)? {
        seated.entry(key).or_insert(member);
    }
    Ok(seated.into_values().collect())
}

/// The changes from `base` to `target`, one per validator in ascending key order from
/// `first_number`, and the resulting configuration.
pub fn configuration_changes(
    subnet_id: SubnetId,
    base: &SubnetCommittee,
    target: &[SubnetValidator],
    first_number: u64,
    block_height: u64,
    block_hash: BlockHash,
) -> Result<(SubnetCommittee, Vec<StakeChangeRequest>), IpcLibError> {
    let before: BTreeMap<XOnlyPublicKey, &SubnetValidator> =
        base.validators.iter().map(|v| (v.pubkey, v)).collect();
    let after: BTreeMap<XOnlyPublicKey, &SubnetValidator> =
        target.iter().map(|v| (v.pubkey, v)).collect();

    let mut committee = base.clone();
    let mut changes = Vec::new();
    let mut number = first_number;
    let mut record = |change: StakingChange,
                      validator: &SubnetValidator,
                      number: u64,
                      committee: &SubnetCommittee| {
        changes.push(StakeChangeRequest {
            change,
            validator_xpk: validator.pubkey,
            validator_subnet_address: validator.subnet_address,
            configuration_number: number,
            committee_after_change: committee.clone(),
            block_height,
            block_hash,
            checkpoint_block_height: None,
            checkpoint_block_hash: None,
            // A cutoff is no transaction
            txid: Txid::all_zeros(),
        });
    };

    for key in before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        match (before.get(key), after.get(key)) {
            (None, Some(joined)) => {
                committee.join_new_validator(&subnet_id, joined)?;
                record(
                    StakingChange::Join {
                        pubkey: joined.pubkey.public_key(bitcoin::secp256k1::Parity::Even),
                    },
                    joined,
                    number,
                    &committee,
                );
                record(
                    StakingChange::Deposit {
                        amount: joined.collateral,
                    },
                    joined,
                    number + 1,
                    &committee,
                );
                number += 2;
            }
            (Some(old), Some(new))
                if old.collateral != new.collateral || old.power != new.power =>
            {
                committee.modify_validator(&subnet_id, new)?;
                let change = if new.collateral > old.collateral {
                    StakingChange::Deposit {
                        amount: new.collateral - old.collateral,
                    }
                } else {
                    StakingChange::Withdraw {
                        amount: old.collateral - new.collateral,
                    }
                };
                record(change, new, number, &committee);
                number += 1;
            }
            (Some(left), None) => {
                committee.remove_validator(&subnet_id, key)?;
                record(
                    StakingChange::Withdraw {
                        amount: left.collateral,
                    },
                    left,
                    number,
                    &committee,
                );
                number += 1;
            }
            _ => {}
        }
    }
    Ok((committee, changes))
}

/// Whether the join collateral of every member of `C_gen` is spent or slashable.
fn genesis_collaterals_slashable<D: DatabaseCore>(
    db: &D,
    txn: &RoTxn,
    state: &SubnetState,
) -> Result<bool, IpcLibError> {
    for record in db.get_ue_collaterals_txn(txn, state.id)? {
        if record.spent || !state.committee.is_validator(&record.pubkey) {
            continue;
        }
        if db
            .get_slash_presigs_txn(txn, state.id, &record.outpoint)?
            .is_none()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Runs the cutoff of every subnet that Bitcoin block `block_height` activates or ends an epoch of.
pub fn process_cutoffs<D: DatabaseCore>(
    db: &D,
    txn: &mut RwTxn,
    block_height: u64,
    block_hash: BlockHash,
) -> Result<Vec<CutoffOutcome>, IpcLibError> {
    let mut outcomes = Vec::new();
    for mut state in db.get_all_subnets_txn(txn)? {
        let genesis_info = db
            .get_subnet_genesis_info_txn(txn, state.id)?
            .ok_or_else(|| {
                IpcValidateError::InvalidMsg(format!("Subnet {} not found.", state.id))
            })?;
        let params = &genesis_info.create_subnet_msg;
        let epoch = match state.h_0 {
            None => {
                if !genesis_collaterals_slashable(db, txn, &state)? {
                    continue;
                }
                state.h_0 = Some(block_height);
                1
            }
            Some(h_0) => {
                if block_height <= h_0 || (block_height - h_0) % params.epoch_length != 0 {
                    continue;
                }
                (block_height - h_0) / params.epoch_length + 1
            }
        };
        outcomes.push(process_cutoff(
            db,
            txn,
            &genesis_info,
            state,
            epoch,
            block_height,
            block_hash,
        )?);
    }
    Ok(outcomes)
}

/// Computes and records the configuration at one subnet's cutoff.
fn process_cutoff<D: DatabaseCore>(
    db: &D,
    txn: &mut RwTxn,
    genesis_info: &SubnetGenesisInfo,
    mut state: SubnetState,
    epoch: u64,
    block_height: u64,
    block_hash: BlockHash,
) -> Result<CutoffOutcome, IpcLibError> {
    let subnet_id = state.id;
    let params = &genesis_info.create_subnet_msg;
    let h_0 = state.h_0.expect("activated");

    // Warns when the ending epoch has no rotation checkpoint
    if state.last_rotation_epoch < state.epoch {
        warn!(
            "Subnet {subnet_id}: no rotation checkpoint of epoch {} by its cutoff block {block_height}",
            state.epoch
        );
    }

    let base = state.latest_committee().clone();
    let boundary = params.cutoff_height(h_0, epoch + 2);
    let mut target = compute_configuration(
        db,
        txn,
        genesis_info,
        &base,
        &state.pending_multisig_changes,
        boundary,
    )?;
    // With no qualifying validator the configuration stays and the pending changes wait for the
    // next cutoff
    let qualifies = !target.is_empty();
    if !qualifies {
        warn!(
            "Subnet {subnet_id}: no validator qualifies for epoch {epoch}, C_{epoch} stays C_{}",
            epoch - 1
        );
        target = base.validators.clone();
    }
    warn_if_small(subnet_id, epoch, target.len());

    let first_number = db.get_next_stake_change_configuration_number_txn(txn, subnet_id)?;
    let (committee, changes) = configuration_changes(
        subnet_id,
        &base,
        &target,
        first_number,
        block_height,
        block_hash,
    )?;

    // Continues from the number of the configuration of the ending epoch, or of `C_gen` in epoch 0
    let current_number = if state.epoch == 0 {
        state.committee_number
    } else {
        epoch_committee_number(db, txn, subnet_id, state.epoch)?.unwrap_or(state.committee_number)
    };
    let changed = !changes.is_empty();
    let committee_number = if changed {
        current_number + 1
    } else {
        current_number
    };

    state.epoch = epoch;
    if qualifies {
        state.pending_multisig_changes.clear();
    }
    if changed {
        state.waiting_committee = Some(committee.clone());
    }
    for change in changes {
        db.add_stake_change(txn, subnet_id, change)?;
    }
    if changed {
        db.save_committee(txn, subnet_id, committee_number, &committee)?;
    }
    db.save_epoch_committee_number(txn, subnet_id, epoch, committee_number)?;
    db.save_subnet_state(txn, subnet_id, &state)?;

    if epoch == 1 {
        info!("Subnet {subnet_id} activated at block {block_height}: H_0 = {block_height}");
    }
    info!(
        "Subnet {subnet_id} cutoff at block {block_height}: epoch {epoch} begins with committee {committee_number} ({} members, last change number {})",
        committee.validators.len(),
        committee.configuration_number
    );
    Ok(CutoffOutcome {
        subnet_id,
        epoch,
        new_committee: changed.then_some((committee_number, committee)),
    })
}

#[cfg(test)]
mod tests;
