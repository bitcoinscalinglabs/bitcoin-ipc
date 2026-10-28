//! Unilateral-exit (UE) collateral: the one-leaf collateral script, the strict
//! checks of a `join-ue`, and the exit spend after maturity.
//!
//! A UE collateral is output 1 of the `join-ue` commit transaction, locked under a
//! taproot output with an unspendable internal key and a single leaf
//! `<T> OP_CHECKLOCKTIMEVERIFY OP_DROP <pk> OP_CHECKSIG`, so that only the
//! validator can spend it, and only from block `T + 1` on.

use bitcoin::{
    absolute::LockTime,
    hashes::Hash,
    key::{Keypair, Secp256k1},
    opcodes::all::{OP_CHECKSIG, OP_CLTV, OP_DROP},
    script::Builder,
    secp256k1::{self, Message},
    sighash::{Prevouts, SighashCache, TapSighashType},
    taproot::{ControlBlock, LeafVersion, TaprootBuilder, TaprootSpendInfo},
    transaction::{self, Transaction, TxIn, TxOut},
    Address, Amount, BlockHash, FeeRate, OutPoint, ScriptBuf, Sequence, Txid, Witness,
    XOnlyPublicKey,
};
use log::warn;
use thiserror::Error;

use crate::bitcoin_utils::{
    concatenate_op_push_data, make_push_data_script, unspenable_internal_key,
};
use crate::db::{self, DatabaseCore, UeCollateral};
use crate::ipc_lib::{
    IpcJoinUeMsg, IpcLibError, IpcUnstakeCollateralMsg, IpcValidate, IpcValidateError, SubnetId,
};

/// Timeouts at or above this value are interpreted by Bitcoin as timestamps, not heights.
pub const LOCKTIME_THRESHOLD: u32 = 500_000_000;

/// Index of the collateral output in the `join-ue` commit transaction.
pub const UE_COLLATERAL_VOUT: u32 = 1;

/// Name of the environment variable holding the UE activation height.
pub const UE_ACTIVATION_HEIGHT_VAR: &str = "UE_ACTIVATION_HEIGHT";

#[derive(Debug, Error)]
pub enum CollateralError {
    #[error("taproot builder is not finalizable")]
    TaprootBuilderNotFinalizable,

    #[error(transparent)]
    TaprootBuilderError(#[from] bitcoin::taproot::TaprootBuilderError),

    #[error("join-ue rejected: {0}")]
    InvalidJoinUe(String),

    #[error("exit transaction: {0}")]
    InvalidExit(String),

    #[error("fee rate overflow")]
    FeeRateOverflow,

    #[error(transparent)]
    SighashError(#[from] bitcoin::sighash::TaprootError),
}

/// The leaf script `<T> OP_CHECKLOCKTIMEVERIFY OP_DROP <pk> OP_CHECKSIG`.
pub fn ue_leaf_script(pubkey: &XOnlyPublicKey, timeout: u32) -> ScriptBuf {
    Builder::new()
        .push_int(timeout as i64)
        .push_opcode(OP_CLTV)
        .push_opcode(OP_DROP)
        .push_x_only_key(pubkey)
        .push_opcode(OP_CHECKSIG)
        .into_script()
}

/// The taproot spend info of the UE collateral: unspendable internal key, one leaf.
pub fn ue_spend_info(
    secp: &Secp256k1<secp256k1::All>,
    pubkey: &XOnlyPublicKey,
    timeout: u32,
) -> Result<TaprootSpendInfo, CollateralError> {
    TaprootBuilder::new()
        .add_leaf(0, ue_leaf_script(pubkey, timeout))?
        .finalize(secp, unspenable_internal_key())
        .map_err(|_| CollateralError::TaprootBuilderNotFinalizable)
}

/// The script pubkey of the UE collateral output for `pubkey` and `timeout`.
pub fn ue_script_pubkey(
    secp: &Secp256k1<secp256k1::All>,
    pubkey: &XOnlyPublicKey,
    timeout: u32,
) -> Result<ScriptBuf, CollateralError> {
    let info = ue_spend_info(secp, pubkey, timeout)?;
    Ok(ScriptBuf::new_p2tr(
        secp,
        info.internal_key(),
        info.merkle_root(),
    ))
}

fn ue_control_block(
    secp: &Secp256k1<secp256k1::All>,
    pubkey: &XOnlyPublicKey,
    timeout: u32,
) -> Result<ControlBlock, CollateralError> {
    ue_spend_info(secp, pubkey, timeout)?
        .control_block(&(ue_leaf_script(pubkey, timeout), LeafVersion::TapScript))
        .ok_or(CollateralError::TaprootBuilderNotFinalizable)
}

/// The UE activation height from the environment, `None` if the feature is off: the
/// variable unset or empty (docker compose passes an empty value for an unset variable).
pub fn ue_activation_height() -> Option<u64> {
    std::env::var(UE_ACTIVATION_HEIGHT_VAR)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("{UE_ACTIVATION_HEIGHT_VAR} must be a valid u64"))
        })
}

/// An accepted `join-ue`: the message, with `join.collateral` filled from the commit
/// transaction, and the collateral outpoint.
#[derive(Debug, Clone)]
pub struct CheckedJoinUe {
    pub msg: IpcJoinUeMsg,
    pub outpoint: OutPoint,
}

/// The strict checks of a `join-ue` whose data `data` was found in `reveal`, mined in
/// block `block_height`. `commit` is the transaction that `reveal`'s input 0 spends.
///
/// Every rule must hold:
/// 1. input 0 of `reveal` spends output 0 of `commit`, a P2TR output, through a single leaf
///    (33-byte control block) that is exactly the data script of `data`;
/// 2. `data` parses as a `join-ue` with no bytes left over, signed by its key;
/// 3. `commit` has 2 or 3 outputs;
/// 4. output 1 of `commit` is locked under the UE script of the key and timeout of the message;
/// 5. the message, with the collateral filled in, validates;
/// 6. `block_height + 1 < timeout < LOCKTIME_THRESHOLD`.
///
/// The minimum collateral is checked by the join handler, which knows the subnet.
pub fn check_join_ue(
    secp: &Secp256k1<secp256k1::All>,
    reveal: &Transaction,
    commit: &Transaction,
    data: &[u8],
    block_height: u64,
) -> Result<CheckedJoinUe, CollateralError> {
    let err = CollateralError::InvalidJoinUe;

    // 1. Input 0 spends output 0 of the commit, through exactly our data script
    let input = reveal
        .input
        .first()
        .ok_or_else(|| err("reveal has no inputs".to_string()))?;
    let commit_txid = commit.compute_txid();
    if input.previous_output != OutPoint::new(commit_txid, 0) {
        return Err(err(format!(
            "input 0 spends {} instead of {}:0",
            input.previous_output, commit_txid
        )));
    }
    if !commit
        .output
        .first()
        .is_some_and(|o| o.script_pubkey.is_p2tr())
    {
        return Err(err("commit output 0 is not P2TR".to_string()));
    }
    let (leaf, control_block) = script_path_leaf(input)?;
    if control_block.len() != 33 {
        return Err(err("commitment output is not a single leaf".to_string()));
    }
    if leaf != make_push_data_script(data).as_bytes() {
        return Err(err(
            "witness leaf is not the data script of the message".to_string()
        ));
    }

    // 2. The data is a signed join-ue
    let mut msg = IpcJoinUeMsg::from_bytes(data).map_err(|e| err(format!("message: {e}")))?;

    // 3. The commit has the commitment output, the collateral and optional change
    if commit.output.len() != 2 && commit.output.len() != 3 {
        return Err(err(format!(
            "commit has {} outputs, expected 2 or 3",
            commit.output.len()
        )));
    }

    // 4. The collateral is locked under exactly the UE script of the declared key and timeout
    let collateral = &commit.output[UE_COLLATERAL_VOUT as usize];
    if collateral.script_pubkey != ue_script_pubkey(secp, &msg.join.pubkey, msg.timeout)? {
        return Err(err(
            "collateral output is not the UE script of the declared key and timeout".to_string(),
        ));
    }

    // 5. The message validates once the collateral is known, as for today's join
    msg.join.collateral = collateral.value;
    msg.validate()
        .map_err(|e| err(format!("invalid message: {e}")))?;

    // 6. The timeout is a height after the join
    let min_timeout = block_height
        .checked_add(1)
        .ok_or_else(|| err("block height overflow".to_string()))?;
    if u64::from(msg.timeout) <= min_timeout || msg.timeout >= LOCKTIME_THRESHOLD {
        return Err(err(format!(
            "timeout {} must be above {} and below {}",
            msg.timeout, min_timeout, LOCKTIME_THRESHOLD
        )));
    }

    Ok(CheckedJoinUe {
        msg,
        outpoint: OutPoint::new(commit_txid, UE_COLLATERAL_VOUT),
    })
}

/// A matured UE collateral to spend in an exit transaction.
#[derive(Debug, Clone)]
pub struct UeExitInput {
    pub outpoint: OutPoint,
    pub value: Amount,
    pub timeout: u32,
}

/// Builds and signs the exit transaction spending `inputs`, all collaterals of `keypair`,
/// to `to`, paying `fee_rate` for the exact weight.
///
/// `nLockTime` is the largest timeout and every input has `nSequence = 0xFFFFFFFD`, as
/// `OP_CHECKLOCKTIMEVERIFY` requires; block `max timeout + 1` is the first that can include it.
pub fn build_ue_exit_tx(
    secp: &Secp256k1<secp256k1::All>,
    inputs: &[UeExitInput],
    keypair: &Keypair,
    to: &Address,
    fee_rate: FeeRate,
) -> Result<Transaction, CollateralError> {
    let err = CollateralError::InvalidExit;
    if inputs.is_empty() {
        return Err(err("no inputs".to_string()));
    }
    let (pubkey, _) = keypair.x_only_public_key();

    let lock_height = inputs.iter().map(|i| i.timeout).max().expect("non-empty");
    let lock_time = LockTime::from_height(lock_height)
        .map_err(|e| err(format!("timeout {lock_height} is not a height: {e}")))?;

    let prevouts = inputs
        .iter()
        .map(|i| {
            Ok(TxOut {
                value: i.value,
                script_pubkey: ue_script_pubkey(secp, &pubkey, i.timeout)?,
            })
        })
        .collect::<Result<Vec<_>, CollateralError>>()?;

    let total = inputs
        .iter()
        .try_fold(Amount::ZERO, |acc, i| acc.checked_add(i.value))
        .ok_or_else(|| err("input value overflow".to_string()))?;

    let mut tx = Transaction {
        version: transaction::Version::TWO,
        lock_time,
        input: inputs
            .iter()
            .map(|i| TxIn {
                previous_output: i.outpoint,
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                ..Default::default()
            })
            .collect(),
        output: vec![TxOut {
            value: total,
            script_pubkey: to.script_pubkey(),
        }],
    };

    // The fee is computed on the final weight: fill the witnesses with placeholders first
    let witnesses = inputs
        .iter()
        .map(|i| {
            Ok((
                ue_leaf_script(&pubkey, i.timeout),
                ue_control_block(secp, &pubkey, i.timeout)?,
            ))
        })
        .collect::<Result<Vec<_>, CollateralError>>()?;
    for (txin, (leaf, control_block)) in tx.input.iter_mut().zip(&witnesses) {
        txin.witness =
            Witness::from_slice(&[vec![0u8; 64], leaf.to_bytes(), control_block.serialize()]);
    }
    let fee = fee_rate
        .fee_wu(tx.weight())
        .ok_or(CollateralError::FeeRateOverflow)?;
    let payout = total
        .checked_sub(fee)
        .ok_or_else(|| err(format!("fee {fee} exceeds the collateral {total}")))?;
    tx.output[0].value = payout;

    // Sign every input with the script-path sighash of its leaf
    let mut sighash_cache = SighashCache::new(&tx);
    let mut signatures = Vec::with_capacity(inputs.len());
    for (index, (leaf, _)) in witnesses.iter().enumerate() {
        let sighash = sighash_cache.taproot_script_spend_signature_hash(
            index,
            &Prevouts::All(&prevouts),
            leaf.tapscript_leaf_hash(),
            TapSighashType::Default,
        )?;
        let msg = Message::from_digest(sighash.to_byte_array());
        signatures.push(secp.sign_schnorr(&msg, keypair));
    }
    for ((txin, (leaf, control_block)), signature) in
        tx.input.iter_mut().zip(&witnesses).zip(signatures)
    {
        txin.witness = Witness::from_slice(&[
            signature.serialize().to_vec(),
            leaf.to_bytes(),
            control_block.serialize(),
        ]);
    }

    Ok(tx)
}

/// The leaf script and control block of a script-path spend (BIP341: the last two witness
/// elements, after an optional annex, a last element starting with 0x50).
fn script_path_leaf(input: &TxIn) -> Result<(&[u8], &[u8]), CollateralError> {
    let witness: Vec<&[u8]> = input.witness.iter().collect();
    let annex = witness.len() >= 2 && witness[witness.len() - 1].first() == Some(&0x50);
    let elements = &witness[..witness.len() - usize::from(annex)];
    let [.., leaf, control_block] = elements else {
        return Err(CollateralError::InvalidJoinUe(
            "input 0 is not a script-path spend".to_string(),
        ));
    };
    Ok((leaf, control_block))
}

/// The data carried by a `join-ue` reveal: the pushes of the executed leaf of input 0.
pub fn join_ue_leaf_data(reveal: &Transaction) -> Result<Vec<u8>, CollateralError> {
    let err = CollateralError::InvalidJoinUe;
    let input = reveal
        .input
        .first()
        .ok_or_else(|| err("reveal has no inputs".to_string()))?;
    let (leaf, _) = script_path_leaf(input)?;
    concatenate_op_push_data(leaf).map_err(|e| err(format!("leaf of input 0: {e}")))
}

/// The monitor's handling of a checked `join-ue`: today's join handler on the inner join,
/// plus the collateral record. The record is written first, so that a crash between the
/// two leaves a record the next run reuses; a rejected join removes it again. A reveal
/// that is already applied (a block replayed after a crash) is accepted again as a no-op.
/// Returns the subnet state when this join bootstraps the subnet.
pub fn process_join_ue<D: DatabaseCore>(
    db: &D,
    checked: &CheckedJoinUe,
    join_txid: Txid,
    block_height: u64,
    block_hash: BlockHash,
) -> Result<Option<db::SubnetState>, IpcLibError> {
    let join = &checked.msg.join;
    let genesis_info = db.get_subnet_genesis_info(join.subnet_id)?.ok_or_else(|| {
        IpcValidateError::InvalidMsg(format!("Subnet {} not found.", join.subnet_id))
    })?;
    let subnet_state = db.get_subnet_state(join.subnet_id)?;

    let existed = db.get_ue_collateral(join.subnet_id, join_txid)?.is_some();
    let applied = subnet_state
        .as_ref()
        .map(|s| s.latest_committee().validators.iter())
        .into_iter()
        .flatten()
        .chain(genesis_info.genesis_validators.iter())
        .any(|v| v.join_txid == join_txid);
    if existed && applied {
        log::debug!("join-ue {} is already applied", join_txid);
        return Ok(None);
    }

    join.validate_for_subnet(&genesis_info, &subnet_state)?;
    join.validate()?;

    if !existed {
        let record = UeCollateral {
            subnet_id: join.subnet_id,
            pubkey: join.pubkey,
            outpoint: checked.outpoint,
            value: join.collateral,
            timeout: checked.msg.timeout,
            backup_address: join.backup_address.clone(),
            join_txid,
            removed: false,
        };
        let mut wtxn = db.write_txn()?;
        db.add_ue_collateral(&mut wtxn, &record)?;
        wtxn.commit()?;
    }

    match join.save_to_db(db, block_height, block_hash, join_txid) {
        Ok(bootstrapped) => Ok(bootstrapped),
        Err(e) => {
            if !existed {
                let mut wtxn = db.write_txn()?;
                db.delete_ue_collateral(&mut wtxn, join.subnet_id, join_txid)?;
                wtxn.commit()?;
            }
            Err(e)
        }
    }
}

/// What the removal pass did with one matured UE collateral.
#[derive(Debug, Clone)]
pub struct UeRemoval {
    pub subnet_id: SubnetId,
    pub pubkey: XOnlyPublicKey,
    pub join_txid: Txid,
    pub outcome: String,
}

/// At block `block_height`, before its transactions: every UE validator whose collateral
/// matures at `block_height + 1` leaves the next committee through today's unstake code,
/// with its whole collateral. A record acts only on the membership its join created (same
/// key and `join_txid`). It is marked removed once the validator is removed or there is
/// nothing to do (not a member, subnet killed or pending kill, or the only member, kept as
/// an empty multisig cannot be built). If the unstake code refuses the removal (for example
/// a waiting member whose collateral exceeds the current committee's total), the record
/// stays and the pass retries at the next block, until the next checkpoint rotates the
/// committee. Subnets that are not bootstrapped have no state yet and keep their records
/// for later.
///
/// The decisions depend only on the state the previous block left, so that a block replayed
/// after a crash gives the same result: a subnet bootstrapped, or rotated by a checkpoint,
/// in this block is left for the next one.
pub fn remove_matured_ue_validators<D: DatabaseCore>(
    db: &D,
    block_height: u64,
    block_hash: BlockHash,
) -> Result<Vec<UeRemoval>, IpcLibError> {
    let mut removals = Vec::new();
    for subnet in db.get_all_subnets()? {
        let changed_in_this_block = db
            .get_subnet_genesis_info(subnet.id)?
            .and_then(|g| g.genesis_block_height)
            .is_none_or(|h| h >= block_height)
            || match subnet.last_checkpoint_number {
                Some(n) => db
                    .get_checkpoint(subnet.id, n)?
                    .is_some_and(|c| c.block_height >= block_height),
                None => false,
            };
        if changed_in_this_block {
            continue;
        }
        let max_timeout = u32::try_from(block_height.saturating_add(1)).unwrap_or(u32::MAX);
        for record in db.get_pending_ue_collaterals(subnet.id, max_timeout)? {
            // Re-read: an earlier removal in this pass may have changed the committee
            let state = db.get_subnet_state(subnet.id)?.ok_or_else(|| {
                IpcValidateError::InvalidMsg(format!("Subnet {} not found.", subnet.id))
            })?;
            let committee = state.latest_committee();
            let member = committee
                .validators
                .iter()
                .find(|v| v.pubkey == record.pubkey && v.join_txid == record.join_txid);

            let (outcome, done) = if state.is_killed_or_pending() {
                (
                    "subnet killed or pending kill, nothing to do".to_string(),
                    true,
                )
            } else if let Some(member) = member {
                if committee.validators.len() <= 1 {
                    ("only member of the committee, kept".to_string(), true)
                } else {
                    let unstake = IpcUnstakeCollateralMsg {
                        subnet_id: subnet.id,
                        amount: member.collateral,
                        pubkey: Some(member.pubkey),
                    };
                    match unstake.save_to_db(db, block_height, block_hash, record.join_txid) {
                        Ok(()) => ("removed from the next committee".to_string(), true),
                        Err(IpcLibError::IpcValidateError(e)) => {
                            warn!(
                                "Subnet {} UE validator {} not removed at block {}, retrying next block: {}",
                                subnet.id, record.pubkey, block_height, e
                            );
                            (format!("not removed, retrying: {e}"), false)
                        }
                        Err(e) => return Err(e),
                    }
                }
            } else {
                ("not in the committee, nothing to do".to_string(), true)
            };

            if done {
                let mut wtxn = db.write_txn()?;
                db.set_ue_collateral_removed(&mut wtxn, subnet.id, record.join_txid)?;
                wtxn.commit()?;
            }
            removals.push(UeRemoval {
                subnet_id: subnet.id,
                pubkey: record.pubkey,
                join_txid: record.join_txid,
                outcome,
            });
        }
    }
    Ok(removals)
}

/// An unsigned commit and reveal pair for `msg` with `collateral` on output 1, shaped as
/// the provider builds them, and the data they carry. For tests and the db tester, which
/// have no wallet: the commit spends a dummy outpoint.
pub fn unsigned_join_ue_txs(
    secp: &Secp256k1<secp256k1::All>,
    msg: &IpcJoinUeMsg,
    collateral: Amount,
) -> Result<(Transaction, Transaction, Vec<u8>), CollateralError> {
    let data = msg
        .to_bytes()
        .map_err(|e| CollateralError::InvalidJoinUe(e.to_string()))?;
    let commit_script = make_push_data_script(&data);
    let info = TaprootBuilder::new()
        .add_leaf(0, commit_script.clone())?
        .finalize(secp, unspenable_internal_key())
        .map_err(|_| CollateralError::TaprootBuilderNotFinalizable)?;
    let commit = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::all_zeros(), 5),
            ..Default::default()
        }],
        output: vec![
            TxOut {
                value: Amount::from_sat(1_000),
                script_pubkey: ScriptBuf::new_p2tr(secp, info.internal_key(), info.merkle_root()),
            },
            TxOut {
                value: collateral,
                script_pubkey: ue_script_pubkey(secp, &msg.join.pubkey, msg.timeout)?,
            },
            TxOut {
                value: Amount::from_sat(50_000),
                script_pubkey: ScriptBuf::new_op_return([1]),
            },
        ],
    };
    let control_block = info
        .control_block(&(commit_script.clone(), LeafVersion::TapScript))
        .ok_or(CollateralError::TaprootBuilderNotFinalizable)?;
    let reveal = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(commit.compute_txid(), 0),
            witness: Witness::from_slice(&[commit_script.to_bytes(), control_block.serialize()]),
            ..Default::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(500),
            script_pubkey: ScriptBuf::new_op_return([2]),
        }],
    };
    Ok((commit, reveal, data))
}

/// Whether a committee member joined with `join-ue`: its `join_txid` has a record for its
/// key. The key matters because a replaced reveal can carry an ordinary join of another
/// key in the same transaction.
pub fn is_ue_validator<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    join_txid: Txid,
    pubkey: &XOnlyPublicKey,
) -> Result<bool, db::DbError> {
    Ok(db
        .get_ue_collateral(subnet_id, join_txid)?
        .is_some_and(|r| r.pubkey == *pubkey))
}

/// Whether `pubkey` is a member of `committee` that joined with `join-ue`.
pub fn is_ue_member<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    committee: &db::SubnetCommittee,
    pubkey: &XOnlyPublicKey,
) -> Result<bool, db::DbError> {
    match committee.validators.iter().find(|v| v.pubkey == *pubkey) {
        Some(v) => is_ue_validator(db, subnet_id, v.join_txid, pubkey),
        None => Ok(false),
    }
}

/// Refuses a stake or unstake through the multisig (`IPCSTK`, `IPCUST`) for a join-ue member
/// of the subnet's latest committee, as the provider does: its collateral is in its own
/// output and leaves it only by `unstakeue`.
pub fn refuse_ue_member<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    pubkey: &XOnlyPublicKey,
) -> Result<(), IpcLibError> {
    let Some(state) = db.get_subnet_state(subnet_id)? else {
        return Ok(());
    };
    if is_ue_member(db, subnet_id, state.latest_committee(), pubkey)? {
        return Err(IpcValidateError::InvalidMsg(format!(
            "Validator {pubkey} joined subnet {subnet_id} with join-ue; its collateral is \
             not in the multisig and is returned by unstakeue after it matures."
        ))
        .into());
    }
    Ok(())
}

/// Reconciles the configured switch with the one the database recorded, at startup. The
/// height may change only while no processed block is affected, counting the block after
/// the recorded height, which may have been partly processed before a crash: with blocks
/// up to `last` processed, a switch at height `a` is in force from `a` on, so a change
/// between `s` and `a` is allowed only if `last + 1 < min(s, a)`. The new value is then
/// recorded. A database with no processed block accepts any value.
pub fn check_ue_activation<D: DatabaseCore>(db: &D, configured: Option<u64>) -> Result<(), String> {
    let stored = db.get_ue_activation_height().map_err(|e| e.to_string())?;
    let last = db.get_last_processed_block().map_err(|e| e.to_string())?;
    if stored != configured && last > 0 {
        let earliest = stored
            .into_iter()
            .chain(configured)
            .min()
            .expect("one is set");
        if last.saturating_add(1) >= earliest {
            return Err(format!(
                "{UE_ACTIVATION_HEIGHT_VAR} is {configured:?} but the database was processed up to block {last} with {stored:?}; the switch can change only above the processed blocks and the one after them"
            ));
        }
    }
    if stored != configured {
        db.set_ue_activation_height(configured)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseCore, HeedDb, StakingChange, SubnetKillState};
    use crate::ipc_lib::{IpcCreateSubnetMsg, IpcJoinSubnetMsg, IPC_JOIN_UE_TAG};
    use crate::test_utils::generate_keypairs;
    use crate::test_utils::{create_test_db, generate_subnet_id};
    use crate::{SubnetId, NETWORK};
    use bitcoin::consensus::encode;
    use bitcoin::Txid;
    use fvm_shared::address::{set_current_network, Network as FvmNetwork};
    use std::str::FromStr;

    /// The subnet id string form depends on this process-global; every test sets it
    fn setup() -> Secp256k1<secp256k1::All> {
        set_current_network(FvmNetwork::Testnet);
        Secp256k1::new()
    }

    /// Verifies with libbitcoinconsensus that `tx` spends `spent`, one per input.
    fn verify_spend(tx: &Transaction, spent: &[TxOut]) -> Result<(), bitcoinconsensus::Error> {
        let serialized_tx = encode::serialize(tx);
        let utxos: Vec<_> = spent
            .iter()
            .map(|o| bitcoinconsensus::Utxo {
                script_pubkey: o.script_pubkey.as_bytes().as_ptr(),
                script_pubkey_len: o.script_pubkey.len() as u32,
                value: o.value.to_sat() as i64,
            })
            .collect();
        for (idx, output) in spent.iter().enumerate() {
            bitcoinconsensus::verify_with_flags(
                output.script_pubkey.as_bytes(),
                output.value.to_sat(),
                &serialized_tx,
                Some(&utxos),
                idx,
                bitcoinconsensus::VERIFY_ALL_PRE_TAPROOT | bitcoinconsensus::VERIFY_TAPROOT,
            )?;
        }
        Ok(())
    }

    fn test_keypair() -> Keypair {
        let secp = Secp256k1::new();
        let sk = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
        Keypair::from_secret_key(&secp, &sk)
    }

    fn test_join_msg(pubkey: XOnlyPublicKey) -> IpcJoinSubnetMsg {
        let create_txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                .unwrap();
        IpcJoinSubnetMsg {
            subnet_id: SubnetId::from_txid(&create_txid),
            collateral: Amount::ZERO,
            ip: "127.0.0.1:8080".parse().unwrap(),
            backup_address: Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
                .unwrap(),
            pubkey,
        }
    }

    fn signed_msg(keypair: &Keypair, timeout: u32) -> IpcJoinUeMsg {
        let secp = Secp256k1::new();
        let (pubkey, _) = keypair.x_only_public_key();
        IpcJoinUeMsg::new_signed(&secp, test_join_msg(pubkey), timeout, &keypair.secret_key())
    }

    const TIMEOUT: u32 = 1_000;
    const BLOCK: u64 = 100;
    const COLLATERAL: Amount = Amount::from_sat(100_000);

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
        unsigned_join_ue_txs(secp, msg, collateral).unwrap()
    }

    #[test]
    fn leaf_script_and_script_pubkey_are_stable() {
        let secp = setup();
        let (pubkey, _) = test_keypair().x_only_public_key();
        let leaf = ue_leaf_script(&pubkey, TIMEOUT);
        assert_eq!(
            leaf.to_string(),
            format!(
                "OP_PUSHBYTES_2 e803 OP_CLTV OP_DROP OP_PUSHBYTES_32 {} OP_CHECKSIG",
                pubkey
            )
        );
        let spk = ue_script_pubkey(&secp, &pubkey, TIMEOUT).unwrap();
        assert!(spk.is_p2tr());
        // Fixed vectors for this key and timeout, so that any change of the output is noticed
        let control_block = ue_control_block(&secp, &pubkey, TIMEOUT)
            .unwrap()
            .serialize();
        assert_eq!(
            (
                spk.to_hex_string(),
                Address::from_script(&spk, NETWORK).unwrap().to_string(),
                hex::encode(&control_block)
            ),
            (
                "5120dbf730e4179424eae873522c7a14f1f058795548a0bf70605f064d003b814fcc".to_string(),
                "bcrt1pm0mnpeqhjsjw46rn2gk859837pv8j42g5zlhqczlqexsqwupflxq2y2hlx".to_string(),
                // Leaf version 0xc0 with even parity, then the unspendable internal key
                "c079be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81799".to_string()
            )
        );
        // Another timeout or key gives another output
        assert_ne!(spk, ue_script_pubkey(&secp, &pubkey, TIMEOUT + 1).unwrap());
        let (other, _) = generate_keypairs(1)[0].x_only_public_key();
        assert_ne!(spk, ue_script_pubkey(&secp, &other, TIMEOUT).unwrap());
        // Address round trip
        let address = Address::from_script(&spk, NETWORK).unwrap();
        assert_eq!(address.script_pubkey(), spk);
    }

    #[test]
    fn check_join_ue_accepts_a_correct_join() {
        let secp = setup();
        let keypair = test_keypair();
        let msg = signed_msg(&keypair, TIMEOUT);
        let (commit, reveal, data) = join_ue_txs(&secp, &msg);
        let accepted = check_join_ue(&secp, &reveal, &commit, &data, BLOCK).unwrap();
        assert_eq!(accepted.outpoint, OutPoint::new(commit.compute_txid(), 1));
        assert_eq!(accepted.msg.join.collateral, COLLATERAL);
        assert_eq!(accepted.msg.join.pubkey, msg.join.pubkey);
        assert_eq!(accepted.msg.timeout, TIMEOUT);

        // A commit with only the two required outputs is accepted too
        let mut commit2 = commit.clone();
        commit2.output.pop();
        let mut reveal2 = reveal.clone();
        reveal2.input[0].previous_output = OutPoint::new(commit2.compute_txid(), 0);
        assert!(check_join_ue(&secp, &reveal2, &commit2, &data, BLOCK).is_ok());

        // A replaced reveal with other outputs and extra inputs is still accepted
        let mut replaced = reveal.clone();
        replaced.output = vec![TxOut {
            value: Amount::from_sat(1),
            script_pubkey: ScriptBuf::new_op_return([9]),
        }];
        replaced.input.push(TxIn {
            previous_output: OutPoint::new(Txid::all_zeros(), 9),
            ..Default::default()
        });
        assert!(check_join_ue(&secp, &replaced, &commit, &data, BLOCK).is_ok());
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
        assert!(check_join_ue(&secp, &bad, &commit, &data, BLOCK).is_err());

        // The data sits in input 1, not input 0
        let mut bad = reveal.clone();
        bad.input.insert(
            0,
            TxIn {
                previous_output: OutPoint::new(Txid::all_zeros(), 9),
                ..Default::default()
            },
        );
        assert!(check_join_ue(&secp, &bad, &commit, &data, BLOCK).is_err());

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
        assert!(check_join_ue(&secp, &bad, &commit, &data, BLOCK).is_err());

        // Trailing bytes after the message, in the script and in the data
        let mut longer = data.clone();
        longer.push(0);
        let mut bad = reveal.clone();
        bad.input[0].witness =
            Witness::from_slice(&[make_push_data_script(&longer).to_bytes(), vec![0u8; 33]]);
        assert!(check_join_ue(&secp, &bad, &commit, &longer, BLOCK).is_err());
        assert!(check_join_ue(&secp, &bad, &commit, &data, BLOCK).is_err());

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
        assert!(check_join_ue(&secp, &bad, &commit, &data, BLOCK).is_err());

        // A key-path spend or a two-leaf control block
        let mut bad = reveal.clone();
        bad.input[0].witness = Witness::from_slice(&[vec![0u8; 64]]);
        assert!(check_join_ue(&secp, &bad, &commit, &data, BLOCK).is_err());
        let mut bad = reveal.clone();
        bad.input[0].witness =
            Witness::from_slice(&[make_push_data_script(&data).to_bytes(), vec![0u8; 65]]);
        assert!(check_join_ue(&secp, &bad, &commit, &data, BLOCK).is_err());

        // An annex after the control block is allowed: anyone can add one to the reveal
        let mut annexed = reveal.clone();
        annexed.input[0].witness = Witness::from_slice(&[
            make_push_data_script(&data).to_bytes(),
            control_block,
            vec![0x50, 1, 2],
        ]);
        assert!(check_join_ue(&secp, &annexed, &commit, &data, BLOCK).is_ok());

        // Commit output 0 is not P2TR
        let mut bad_commit = commit.clone();
        bad_commit.output[0].script_pubkey = ScriptBuf::new_op_return([3]);
        let mut bad = reveal.clone();
        bad.input[0].previous_output = OutPoint::new(bad_commit.compute_txid(), 0);
        assert!(check_join_ue(&secp, &bad, &bad_commit, &data, BLOCK).is_err());
    }

    #[test]
    fn check_join_ue_rejects_bad_signature() {
        let secp = setup();
        let keypair = test_keypair();
        let (pubkey, _) = keypair.x_only_public_key();

        // Signed by another key
        let other = generate_keypairs(1)[0];
        let msg =
            IpcJoinUeMsg::new_signed(&secp, test_join_msg(pubkey), TIMEOUT, &other.secret_key());
        let (commit, reveal, data) = join_ue_txs(&secp, &msg);
        assert!(check_join_ue(&secp, &reveal, &commit, &data, BLOCK).is_err());

        // Signed on the same fields with another timeout
        let mut msg = signed_msg(&keypair, TIMEOUT);
        msg.timeout = TIMEOUT + 1;
        let (commit, reveal, data) = join_ue_txs(&secp, &msg);
        assert!(check_join_ue(&secp, &reveal, &commit, &data, BLOCK).is_err());
    }

    #[test]
    fn check_join_ue_rejects_bad_commit_outputs() {
        let secp = setup();
        let keypair = test_keypair();
        let (pubkey, _) = keypair.x_only_public_key();
        let msg = signed_msg(&keypair, TIMEOUT);
        let (commit, reveal, data) = join_ue_txs(&secp, &msg);

        let with_commit = |commit: Transaction| {
            let mut reveal = reveal.clone();
            reveal.input[0].previous_output = OutPoint::new(commit.compute_txid(), 0);
            check_join_ue(&secp, &reveal, &commit, &data, BLOCK)
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
        bad.output[1].script_pubkey = ue_script_pubkey(&secp, &pubkey, TIMEOUT + 1).unwrap();
        assert!(with_commit(bad).is_err());

        // Output 1 under another key
        let (other, _) = generate_keypairs(1)[0].x_only_public_key();
        let mut bad = commit.clone();
        bad.output[1].script_pubkey = ue_script_pubkey(&secp, &other, TIMEOUT).unwrap();
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
            let (commit, reveal, data) = join_ue_txs(&secp, &msg);
            check_join_ue(&secp, &reveal, &commit, &data, BLOCK)
        };

        // T = h + 1 is too early, T = h + 2 is the first accepted
        assert!(check(BLOCK as u32 + 1).is_err());
        assert!(check(BLOCK as u32 + 2).is_ok());

        // T at the locktime threshold is a timestamp
        assert!(check(LOCKTIME_THRESHOLD).is_err());
        assert!(check(LOCKTIME_THRESHOLD - 1).is_ok());
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
        // The old join message does not parse as a join-ue
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
                timeout: TIMEOUT,
            },
            UeExitInput {
                outpoint: OutPoint::new(Txid::from_slice(&[1u8; 32]).unwrap(), 1),
                value: COLLATERAL,
                timeout: TIMEOUT - 10,
            },
        ];
        let prevouts = inputs
            .iter()
            .map(|i| TxOut {
                value: i.value,
                script_pubkey: ue_script_pubkey(secp, pubkey, i.timeout).unwrap(),
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
                let sig =
                    secp.sign_schnorr(&Message::from_digest(sighash.to_byte_array()), keypair);
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
    fn leaf_data_comes_from_the_executed_leaf() {
        let secp = setup();
        let msg = signed_msg(&test_keypair(), TIMEOUT);
        let (_, reveal, data) = join_ue_txs(&secp, &msg);
        assert_eq!(join_ue_leaf_data(&reveal).unwrap(), data);

        // With an annex
        let mut annexed = reveal.clone();
        let mut witness = annexed.input[0].witness.to_vec();
        witness.push(vec![0x50, 7]);
        annexed.input[0].witness = Witness::from_slice(&witness);
        assert_eq!(join_ue_leaf_data(&annexed).unwrap(), data);

        // A key-path spend has no leaf
        let mut keypath = reveal.clone();
        keypath.input[0].witness = Witness::from_slice(&[vec![0u8; 64]]);
        assert!(join_ue_leaf_data(&keypath).is_err());
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
                min_validators,
                bottomup_check_period: 10,
                active_validators_limit: 10,
                min_cross_msg_fee: Amount::from_sat(1),
                whitelist,
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
        let msg = IpcJoinUeMsg::new_signed(secp, join, timeout, &keypair.secret_key());
        let (commit, reveal, data) = join_ue_txs_with(secp, &msg, collateral);
        let checked = check_join_ue(secp, &reveal, &commit, &data, BLOCK).unwrap();
        (checked, reveal.compute_txid())
    }

    /// Replaces the current committee by the waiting one, as a checkpoint does
    fn rotate(db: &HeedDb, subnet_id: SubnetId) {
        let mut state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        let next = state.waiting_committee.take().expect("a waiting committee");
        state.rotate_to_committee(next);
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
    fn only_member_cannot_unstake_everything_on_chain() {
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

        // A hand-built full unstake of the only member is refused instead of emptying the
        // committee (an empty committee has no multisig address)
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
        let record = db.get_ue_collateral(subnet_id, join_txid).unwrap().unwrap();
        assert_eq!(record.pubkey, pks[0]);
        assert_eq!(record.outpoint, checked.outpoint);
        assert_eq!(record.value, COLLATERAL);
        assert_eq!(record.timeout, TIMEOUT);
        assert!(!record.removed);
        assert!(is_ue_validator(&db, subnet_id, join_txid, &pks[0]).unwrap());
        assert!(!is_ue_validator(&db, subnet_id, join_txid, &pks[1]).unwrap());

        // The same key again, in another reveal: rejected, and only the first record stays
        let (dup, dup_txid) = checked_join(&secp, &keys[0], subnet_id, TIMEOUT);
        assert!(process_join_ue(&db, &dup, dup_txid, BLOCK, block_hash).is_err());
        assert!(db.get_ue_collateral(subnet_id, dup_txid).unwrap().is_none());
        assert!(db
            .get_ue_collateral(subnet_id, join_txid)
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

        // After bootstrap a third key joins the waiting committee
        let (checked3, join_txid3) = checked_join(&secp, &keys[2], subnet_id, TIMEOUT + 500);
        assert!(
            process_join_ue(&db, &checked3, join_txid3, BLOCK + 1, block_hash)
                .unwrap()
                .is_none()
        );
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
            .get_ue_collateral(subnet_id, join_txid3)
            .unwrap()
            .is_some());

        // A collateral below the subnet's minimum is rejected without a record
        let mut small = checked_join(&secp, &keys[2], subnet_id, TIMEOUT).0;
        small.msg.join.collateral = Amount::from_sat(9_999);
        let small_txid = Txid::from_slice(&[9u8; 32]).unwrap();
        assert!(process_join_ue(&db, &small, small_txid, BLOCK + 1, block_hash).is_err());
        assert!(db
            .get_ue_collateral(subnet_id, small_txid)
            .unwrap()
            .is_none());
    }

    #[test]
    fn removal_pass_removes_matured_validators_once() {
        let secp = setup();
        let db = create_test_db();
        let keys = generate_keypairs(3);
        let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
        let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
        let block_hash = BlockHash::all_zeros();
        let late = TIMEOUT + 500;
        for (key, timeout) in keys.iter().zip([TIMEOUT, TIMEOUT, late]) {
            let (checked, join_txid) = checked_join(&secp, key, subnet_id, timeout);
            process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
        }
        assert!(withdrawals(&db, subnet_id).is_empty());

        // Two blocks before maturity nothing happens
        let h = u64::from(TIMEOUT) - 2;
        assert!(remove_matured_ue_validators(&db, h, block_hash)
            .unwrap()
            .is_empty());

        // At T - 1 both validators with that timeout leave, with their whole collateral
        let removals = remove_matured_ue_validators(&db, h + 1, block_hash).unwrap();
        assert_eq!(removals.len(), 2);
        assert!(removals
            .iter()
            .all(|r| r.outcome == "removed from the next committee"));
        let mut withdrawn = withdrawals(&db, subnet_id);
        withdrawn.sort();
        let mut expected = vec![(pks[0], COLLATERAL), (pks[1], COLLATERAL)];
        expected.sort();
        assert_eq!(withdrawn, expected);
        let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        assert_eq!(state.latest_committee().pubkeys(), vec![pks[2]]);
        assert!(db
            .get_ue_collaterals(subnet_id)
            .unwrap()
            .iter()
            .all(|r| r.removed == (r.timeout == TIMEOUT)));

        // The pass examines a record once
        assert!(remove_matured_ue_validators(&db, h + 1, block_hash)
            .unwrap()
            .is_empty());

        // The only member stays, and its record is marked removed
        let removals = remove_matured_ue_validators(&db, u64::from(late) - 1, block_hash).unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].outcome, "only member of the committee, kept");
        assert_eq!(withdrawals(&db, subnet_id).len(), 2);
        assert!(db
            .get_ue_collaterals(subnet_id)
            .unwrap()
            .iter()
            .all(|r| r.removed));

        // A subnet pending kill or killed: nothing to do, the record is marked removed
        for (i, killed) in [
            SubnetKillState::ToBeKilled,
            SubnetKillState::Killed { parent_height: 1 },
        ]
        .into_iter()
        .enumerate()
        {
            let mut state = db.get_subnet_state(subnet_id).unwrap().unwrap();
            state.killed = killed;
            let mut wtxn = db.write_txn().unwrap();
            db.save_subnet_state(&mut wtxn, subnet_id, &state).unwrap();
            let mut record = db.get_ue_collaterals(subnet_id).unwrap()[0].clone();
            record.join_txid = Txid::from_slice(&[8 + i as u8; 32]).unwrap();
            record.removed = false;
            db.add_ue_collateral(&mut wtxn, &record).unwrap();
            wtxn.commit().unwrap();
            let removals = remove_matured_ue_validators(&db, u64::from(late), block_hash).unwrap();
            assert_eq!(removals.len(), 1);
            assert!(removals[0].outcome.contains("killed"));
            assert_eq!(withdrawals(&db, subnet_id).len(), 2);
            assert!(
                db.get_ue_collateral(subnet_id, record.join_txid)
                    .unwrap()
                    .unwrap()
                    .removed
            );
        }
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
        }

        // Block T - 1: the removal pass, then a join-ue of a third key
        let h = u64::from(TIMEOUT) - 1;
        let (checked3, join_txid3) = checked_join(&secp, &keys[2], subnet_id, TIMEOUT + 2000);
        let run_block = || {
            remove_matured_ue_validators(&db, h, block_hash).unwrap();
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
        assert_eq!(first.0, vec![pks[1], pks[2]]);
        assert_eq!(first.1, 3, "one withdraw, one join, one deposit");

        // The same block again, as after a crash before the processed height was saved
        run_block();
        assert_eq!(snapshot(&db), first);
    }

    #[test]
    fn waiting_validator_removal_retries_until_rotation() {
        let secp = setup();
        let db = create_test_db();
        let keys = generate_keypairs(2);
        let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
        let subnet_id = subnet_with_whitelist(&db, vec![pks[0]], 1);
        let block_hash = BlockHash::all_zeros();
        let (small, txid_small) = checked_join_with(
            &secp,
            &keys[0],
            subnet_id,
            TIMEOUT + 1000,
            Amount::from_sat(50_000),
        );
        process_join_ue(&db, &small, txid_small, BLOCK, block_hash)
            .unwrap()
            .expect("bootstrapped");
        let (large, txid_large) = checked_join_with(
            &secp,
            &keys[1],
            subnet_id,
            TIMEOUT,
            Amount::from_sat(100_000),
        );
        process_join_ue(&db, &large, txid_large, BLOCK + 1, block_hash).unwrap();

        // The waiting member's collateral exceeds the current committee's total, which
        // today's unstake check refuses: the record stays and the pass retries
        let h = u64::from(TIMEOUT) - 1;
        let removals = remove_matured_ue_validators(&db, h, block_hash).unwrap();
        assert_eq!(removals.len(), 1);
        assert!(removals[0].outcome.starts_with("not removed, retrying"));
        assert!(
            !db.get_ue_collateral(subnet_id, txid_large)
                .unwrap()
                .unwrap()
                .removed
        );
        assert!(withdrawals(&db, subnet_id).is_empty());

        // After a checkpoint rotates the committee, the next block removes it
        rotate(&db, subnet_id);
        let removals = remove_matured_ue_validators(&db, h + 1, block_hash).unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].outcome, "removed from the next committee");
        assert_eq!(
            withdrawals(&db, subnet_id),
            vec![(pks[1], Amount::from_sat(100_000))]
        );
        assert!(
            db.get_ue_collateral(subnet_id, txid_large)
                .unwrap()
                .unwrap()
                .removed
        );
    }

    #[test]
    fn collateral_matured_before_bootstrap_is_removed_after_it() {
        let secp = setup();
        let db = create_test_db();
        let keys = generate_keypairs(2);
        let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
        let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
        let block_hash = BlockHash::all_zeros();
        let early = BLOCK as u32 + 2;
        let (checked, join_txid) = checked_join(&secp, &keys[0], subnet_id, early);
        process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();

        // Not bootstrapped: the record waits
        assert!(remove_matured_ue_validators(&db, BLOCK + 1, block_hash)
            .unwrap()
            .is_empty());
        assert!(
            !db.get_ue_collateral(subnet_id, join_txid)
                .unwrap()
                .unwrap()
                .removed
        );

        // The second join bootstraps the subnet; the first block after that removes it
        let (checked2, join_txid2) = checked_join(&secp, &keys[1], subnet_id, TIMEOUT);
        process_join_ue(&db, &checked2, join_txid2, BLOCK + 50, block_hash)
            .unwrap()
            .expect("bootstrapped");
        let removals = remove_matured_ue_validators(&db, BLOCK + 51, block_hash).unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].outcome, "removed from the next committee");
        let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        assert_eq!(state.latest_committee().pubkeys(), vec![pks[1]]);
    }

    #[test]
    fn activation_switch_reconciles_with_the_database() {
        let db = create_test_db();
        // Nothing processed yet: any value is accepted and recorded
        check_ue_activation(&db, None).unwrap();
        check_ue_activation(&db, Some(5)).unwrap();
        assert_eq!(db.get_ue_activation_height().unwrap(), Some(5));
        check_ue_activation(&db, Some(5)).unwrap();

        // Blocks up to 10 processed with the switch at 5: it can no longer change
        db.set_last_processed_block(10).unwrap();
        check_ue_activation(&db, Some(5)).unwrap();
        assert!(check_ue_activation(&db, Some(7)).is_err());
        assert!(check_ue_activation(&db, Some(20)).is_err());
        assert!(check_ue_activation(&db, None).is_err());
        assert_eq!(db.get_ue_activation_height().unwrap(), Some(5));

        // A switch wholly in the future can be set, moved and unset; block 11 may have been
        // partly processed before a crash, so it counts as processed
        let db = create_test_db();
        db.set_last_processed_block(10).unwrap();
        assert!(check_ue_activation(&db, Some(10)).is_err());
        assert!(check_ue_activation(&db, Some(11)).is_err());
        check_ue_activation(&db, Some(12)).unwrap();
        check_ue_activation(&db, Some(30)).unwrap();
        check_ue_activation(&db, None).unwrap();
        assert_eq!(db.get_ue_activation_height().unwrap(), None);

        // A join-ue accepted in block 10 before a crash, the recorded height still 9: the
        // switch at 10 cannot move or go away
        let db = create_test_db();
        check_ue_activation(&db, Some(10)).unwrap();
        db.set_last_processed_block(9).unwrap();
        assert!(check_ue_activation(&db, None).is_err());
        assert!(check_ue_activation(&db, Some(11)).is_err());
        check_ue_activation(&db, Some(10)).unwrap();
        assert_eq!(db.get_ue_activation_height().unwrap(), Some(10));
    }

    #[test]
    fn replayed_bootstrap_block_removes_nothing_early() {
        let secp = setup();
        let db = create_test_db();
        let keys = generate_keypairs(2);
        let subnet_id = subnet_with_whitelist(
            &db,
            keys.iter().map(|k| k.x_only_public_key().0).collect(),
            2,
        );
        let block_hash = BlockHash::all_zeros();
        // The first key matures before the subnet bootstraps
        let (first, first_txid) = checked_join(&secp, &keys[0], subnet_id, BLOCK as u32 + 2);
        process_join_ue(&db, &first, first_txid, BLOCK, block_hash).unwrap();

        // Block H: the pass finds no bootstrapped subnet, then the second join bootstraps it
        let h = BLOCK + 5;
        assert!(remove_matured_ue_validators(&db, h, block_hash)
            .unwrap()
            .is_empty());
        let (second, second_txid) = checked_join(&secp, &keys[1], subnet_id, TIMEOUT);
        process_join_ue(&db, &second, second_txid, h, block_hash)
            .unwrap()
            .expect("bootstrapped");

        // Block H replayed after a crash: still nothing, the subnet bootstrapped in H
        assert!(remove_matured_ue_validators(&db, h, block_hash)
            .unwrap()
            .is_empty());
        assert!(process_join_ue(&db, &second, second_txid, h, block_hash)
            .unwrap()
            .is_none());
        assert!(withdrawals(&db, subnet_id).is_empty());

        // Block H + 1 removes the first key, once
        let removals = remove_matured_ue_validators(&db, h + 1, block_hash).unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].outcome, "removed from the next committee");
        assert_eq!(withdrawals(&db, subnet_id).len(), 1);
    }

    #[test]
    fn replayed_rotation_block_retries_nothing_early() {
        let secp = setup();
        let db = create_test_db();
        let keys = generate_keypairs(2);
        let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
        let subnet_id = subnet_with_whitelist(&db, vec![pks[0]], 1);
        let block_hash = BlockHash::all_zeros();
        let (small, txid_small) = checked_join_with(
            &secp,
            &keys[0],
            subnet_id,
            TIMEOUT + 1000,
            Amount::from_sat(50_000),
        );
        process_join_ue(&db, &small, txid_small, BLOCK, block_hash)
            .unwrap()
            .expect("bootstrapped");
        let (large, txid_large) = checked_join_with(
            &secp,
            &keys[1],
            subnet_id,
            TIMEOUT,
            Amount::from_sat(100_000),
        );
        process_join_ue(&db, &large, txid_large, BLOCK + 1, block_hash).unwrap();

        // Block H: the removal is refused, then a checkpoint in H rotates the committee
        let h = u64::from(TIMEOUT) - 1;
        let removals = remove_matured_ue_validators(&db, h, block_hash).unwrap();
        assert!(removals[0].outcome.starts_with("not removed, retrying"));
        rotate(&db, subnet_id);
        let mut state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        state.last_checkpoint_number = Some(0);
        let checkpoint = db::SubnetCheckpoint {
            checkpoint_number: 0,
            checkpoint_hash: bitcoin::hashes::sha256::Hash::all_zeros(),
            checkpoint_height: 1,
            block_height: h,
            txid: Txid::all_zeros(),
            batch_transfer_txid: None,
            batch_transfer_block_height: None,
            signed_committee_number: 1,
            next_committee_number: 2,
            next_configuration_number: 2,
            is_kill_checkpoint: false,
        };
        let mut wtxn = db.write_txn().unwrap();
        db.save_subnet_state(&mut wtxn, subnet_id, &state).unwrap();
        db.save_checkpoint(&mut wtxn, subnet_id, &checkpoint, 0)
            .unwrap();
        wtxn.commit().unwrap();

        // Block H replayed: the rotation happened in H, so the pass waits, as it did
        assert!(remove_matured_ue_validators(&db, h, block_hash)
            .unwrap()
            .is_empty());
        assert!(withdrawals(&db, subnet_id).is_empty());

        // Block H + 1 removes it
        let removals = remove_matured_ue_validators(&db, h + 1, block_hash).unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].outcome, "removed from the next committee");
        assert_eq!(
            withdrawals(&db, subnet_id),
            vec![(pks[1], Amount::from_sat(100_000))]
        );
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
        }

        // A replaced reveal carries the UE join of key 2 and an ordinary join of a fresh
        // key, both accepted under the same reveal txid
        let ordinary = generate_keypairs(1)[0].x_only_public_key().0;
        let (checked, join_txid) = checked_join(&secp, &keys[2], subnet_id, TIMEOUT + 100);
        process_join_ue(&db, &checked, join_txid, BLOCK + 1, block_hash).unwrap();
        let mut plain = test_join_msg(ordinary);
        plain.subnet_id = subnet_id;
        plain.collateral = COLLATERAL;
        plain
            .save_to_db(&db, BLOCK + 1, block_hash, join_txid)
            .unwrap();

        assert!(is_ue_validator(&db, subnet_id, join_txid, &pks[2]).unwrap());
        assert!(!is_ue_validator(&db, subnet_id, join_txid, &ordinary).unwrap());
        let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        let committee = state.latest_committee();
        assert!(committee.is_validator(&pks[2]) && committee.is_validator(&ordinary));

        // At T - 1 only the UE member leaves
        let removals =
            remove_matured_ue_validators(&db, u64::from(TIMEOUT) + 99, block_hash).unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].pubkey, pks[2]);
        let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        assert!(!state.latest_committee().is_validator(&pks[2]));
        assert!(state.latest_committee().is_validator(&ordinary));
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

        // The removal pass is not affected: at T - 1 A's whole collateral is withdrawn, once
        let removals =
            remove_matured_ue_validators(&db, u64::from(TIMEOUT) - 1, block_hash).unwrap();
        assert_eq!(removals[0].outcome, "removed from the next committee");
        assert_eq!(withdrawals(&db, subnet_id), vec![(pks[0], COLLATERAL)]);
        // A is no longer a member, so the rule leaves its key to the usual checks
        refuse_ue_member(&db, subnet_id, &pks[0]).unwrap();
    }

    #[test]
    fn an_old_record_does_not_act_on_a_new_registration_of_the_key() {
        let secp = setup();
        let db = create_test_db();
        let keys = generate_keypairs(2);
        let pks: Vec<_> = keys.iter().map(|k| k.x_only_public_key().0).collect();
        let subnet_id = subnet_with_whitelist(&db, vec![pks[0], pks[1]], 2);
        let block_hash = BlockHash::all_zeros();
        for key in &keys {
            let (checked, join_txid) = checked_join(&secp, key, subnet_id, TIMEOUT);
            process_join_ue(&db, &checked, join_txid, BLOCK, block_hash).unwrap();
        }

        // Key 0 leaves by hand, a checkpoint rotates it out, and it joins again with a
        // later timeout while its first record is still open
        IpcUnstakeCollateralMsg {
            subnet_id,
            amount: COLLATERAL,
            pubkey: Some(pks[0]),
        }
        .save_to_db(
            &db,
            BLOCK + 10,
            block_hash,
            Txid::from_slice(&[5u8; 32]).unwrap(),
        )
        .unwrap();
        rotate(&db, subnet_id);
        let (again, txid_again) = checked_join(&secp, &keys[0], subnet_id, TIMEOUT + 1000);
        process_join_ue(&db, &again, txid_again, BLOCK + 20, block_hash).unwrap();

        // At the first timeout the old record finds no membership of its own: key 0 stays
        let removals =
            remove_matured_ue_validators(&db, u64::from(TIMEOUT) - 1, block_hash).unwrap();
        let first = removals.iter().find(|r| r.pubkey == pks[0]).unwrap();
        assert_eq!(first.outcome, "not in the committee, nothing to do");
        let state = db.get_subnet_state(subnet_id).unwrap().unwrap();
        assert!(state.latest_committee().is_validator(&pks[0]));
        assert!(!state.latest_committee().is_validator(&pks[1]));

        // At its own timeout the new record removes it
        let removals =
            remove_matured_ue_validators(&db, u64::from(TIMEOUT) + 999, block_hash).unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].outcome, "only member of the committee, kept");
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
        let (commit, reveal, data) = join_ue_txs(&secp, &msg);
        assert!(check_join_ue(&secp, &reveal, &commit, &data, u64::MAX).is_err());

        // Timeouts around the script-number push sizes keep one leaf shape per value
        for (timeout, expected_push) in [
            (16u32, "OP_PUSHNUM_16"),
            (17, "OP_PUSHBYTES_1 11"),
            (127, "OP_PUSHBYTES_1 7f"),
            (128, "OP_PUSHBYTES_2 8000"),
            (32767, "OP_PUSHBYTES_2 ff7f"),
            (32768, "OP_PUSHBYTES_3 008000"),
            (LOCKTIME_THRESHOLD - 1, "OP_PUSHBYTES_4 ff64cd1d"),
        ] {
            let leaf = ue_leaf_script(&pubkey, timeout).to_string();
            assert!(leaf.starts_with(expected_push), "{timeout}: {leaf}");
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
        let client =
            Client::new(&format!("http://127.0.0.1:{rpc_port}/wallet/test"), auth).unwrap();
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

        // join-ue with a timeout 5 blocks after the tip
        let tip = rpc.get_block_count().unwrap();
        let timeout = tip as u32 + 5;
        let mut join = test_join_msg(pubkey);
        join.collateral = COLLATERAL;
        let msg = IpcJoinUeMsg::new_signed(&secp, join, timeout, &keypair.secret_key());
        let reveal_txid = msg.submit_to_bitcoin(rpc).unwrap();
        rpc.generate_to_address(1, &mine_to).unwrap();
        let join_height = rpc.get_block_count().unwrap();
        assert_eq!(join_height, tip + 1);

        // The monitor's view: the reveal and the commit it spends, from the chain
        let reveal = rpc.get_raw_transaction(&reveal_txid, None).unwrap();
        let commit_txid = reveal.input[0].previous_output.txid;
        let commit = rpc.get_raw_transaction(&commit_txid, None).unwrap();
        let data =
            crate::bitcoin_utils::concatenate_op_push_data(reveal.input[0].witness.nth(0).unwrap())
                .unwrap();
        let accepted = check_join_ue(&secp, &reveal, &commit, &data, join_height).unwrap();
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
            timeout,
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
}
