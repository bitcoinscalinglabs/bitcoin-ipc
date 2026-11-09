//! Unilateral-exit (UE) collateral: its script, the checks of a `join-ue` and the exit spend.

use bitcoin::{
    absolute::LockTime,
    hashes::Hash,
    key::{Keypair, Secp256k1},
    opcodes::all::{OP_CHECKSIG, OP_CHECKSIGVERIFY, OP_CLTV, OP_DROP},
    script::Builder,
    secp256k1::{self, Message},
    sighash::{Prevouts, SighashCache, TapSighashType},
    taproot::{ControlBlock, LeafVersion, TaprootBuilder, TaprootSpendInfo},
    transaction::{self, Transaction, TxIn, TxOut},
    Address, Amount, BlockHash, FeeRate, OutPoint, ScriptBuf, Sequence, Txid, Witness,
    XOnlyPublicKey,
};
use std::collections::BTreeMap;

use thiserror::Error;

use crate::bitcoin_utils::{reveal_input, unsigned_commit_reveal, unspenable_internal_key};
use crate::db::{self, DatabaseCore, UeCollateral};
use crate::epochs::{epoch_committee_number, is_ue_validator, WHITELIST_COMMITTEE_NUMBER};
use crate::ipc_lib::{IpcJoinUeMsg, IpcLibError, IpcValidate, IpcValidateError, SubnetId};
use crate::multisig::{Configuration, Power, MAX_TOTAL_POWER};

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

    #[error(transparent)]
    MultisigError(#[from] crate::multisig::MultisigError),

    #[error("join-ue rejected: {0}")]
    InvalidJoinUe(String),

    #[error("exit transaction: {0}")]
    InvalidExit(String),

    #[error("slash transaction: {0}")]
    InvalidSlash(String),

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

/// `<key> OP_CHECKSIGVERIFY` followed by the weighted multisig of the named configuration.
fn key_and_multisig_leaf(
    key: &XOnlyPublicKey,
    configuration: &Configuration,
) -> Result<ScriptBuf, CollateralError> {
    let prefix = Builder::new()
        .push_x_only_key(key)
        .push_opcode(OP_CHECKSIGVERIFY)
        .into_script();
    let multisig = configuration.multisig_script()?;
    Ok(ScriptBuf::from_bytes(
        [prefix.into_bytes(), multisig.into_bytes()].concat(),
    ))
}

/// The collateral script of a validator: its key, its EOTS key, its timeout and the
/// configuration its slash and cooperative-exit leaves name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollateralScript {
    pub pubkey: XOnlyPublicKey,
    pub pk_eots: XOnlyPublicKey,
    pub timeout: u32,
    pub configuration: Configuration,
}

impl CollateralScript {
    pub fn new(
        pubkey: XOnlyPublicKey,
        pk_eots: XOnlyPublicKey,
        timeout: u32,
        configuration: Configuration,
    ) -> Self {
        Self {
            pubkey,
            pk_eots,
            timeout,
            configuration,
        }
    }

    /// The unilateral-exit leaf `U(pk, T)`.
    pub fn exit_leaf(&self) -> ScriptBuf {
        ue_leaf_script(&self.pubkey, self.timeout)
    }

    /// The slash leaf `L(pk_eots, C)`.
    pub fn slash_leaf(&self) -> Result<ScriptBuf, CollateralError> {
        key_and_multisig_leaf(&self.pk_eots, &self.configuration)
    }

    /// The cooperative-exit leaf `X(pk, C)`.
    pub fn cooperative_exit_leaf(&self) -> Result<ScriptBuf, CollateralError> {
        key_and_multisig_leaf(&self.pubkey, &self.configuration)
    }

    /// The taproot spend info: unspendable internal key, `X` at depth 1, `U` and `L` at depth 2.
    pub fn spend_info(
        &self,
        secp: &Secp256k1<secp256k1::All>,
    ) -> Result<TaprootSpendInfo, CollateralError> {
        TaprootBuilder::new()
            .add_leaf(1, self.cooperative_exit_leaf()?)?
            .add_leaf(2, self.exit_leaf())?
            .add_leaf(2, self.slash_leaf()?)?
            .finalize(secp, unspenable_internal_key())
            .map_err(|_| CollateralError::TaprootBuilderNotFinalizable)
    }

    pub fn script_pubkey(
        &self,
        secp: &Secp256k1<secp256k1::All>,
    ) -> Result<ScriptBuf, CollateralError> {
        let info = self.spend_info(secp)?;
        Ok(ScriptBuf::new_p2tr(
            secp,
            info.internal_key(),
            info.merkle_root(),
        ))
    }

    pub fn control_block(
        &self,
        secp: &Secp256k1<secp256k1::All>,
        leaf: &ScriptBuf,
    ) -> Result<ControlBlock, CollateralError> {
        self.spend_info(secp)?
            .control_block(&(leaf.clone(), LeafVersion::TapScript))
            .ok_or(CollateralError::TaprootBuilderNotFinalizable)
    }

}

/// The configurations a `join-ue` may name, with their committee numbers (an error in epoch 0).
pub fn named_configurations<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
) -> Result<Vec<(u64, Configuration)>, IpcLibError> {
    let genesis_info = db.get_subnet_genesis_info(subnet_id)?.ok_or_else(|| {
        IpcValidateError::InvalidMsg(format!("Subnet {} not found.", subnet_id))
    })?;
    let Some(state) = db.get_subnet_state(subnet_id)? else {
        return Ok(vec![(
            WHITELIST_COMMITTEE_NUMBER,
            genesis_info.whitelist_configuration(),
        )]);
    };
    if !state.is_activated() {
        return Err(IpcValidateError::InvalidMsg(format!(
            "Subnet {subnet_id} is in epoch 0: it accepts no join transaction"
        ))
        .into());
    }
    let mut numbers = Vec::new();
    for epoch in (0..=state.epoch).rev() {
        if let Some(number) = epoch_committee_number(db, subnet_id, epoch)? {
            numbers.push(number);
            if numbers.len() == 2 {
                break;
            }
        }
    }
    let mut named = Vec::new();
    for number in numbers {
        let configuration = configuration_by_number(db, subnet_id, number)?.ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!("Subnet {subnet_id} has no committee {number}"))
        })?;
        named.push((number, configuration));
    }
    Ok(named)
}

/// The configurations and epoch timing that a `join-ue` is checked against.
#[derive(Debug, Clone)]
pub struct JoinContext {
    pub named: Vec<(u64, Configuration)>,
    /// The current epoch, 0 before activation
    pub epoch: u64,
    /// `H_0`, `None` before activation
    pub h_0: Option<u64>,
    pub epoch_length: u64,
    pub min_timeout: u32,
    pub max_timeout: u32,
}

impl JoinContext {
    /// The epoch `E_T` that contains `timeout`, taking `block_height` as `H_0` before activation.
    pub fn timeout_epoch(&self, timeout: u32, block_height: u64) -> u64 {
        let origin = self.h_0.unwrap_or(block_height);
        u64::from(timeout)
            .saturating_sub(origin)
            .div_ceil(self.epoch_length)
    }

    /// Checks the timeout against the bounds on its height and its epoch.
    pub fn check_timeout(&self, timeout: u32, block_height: u64) -> Result<(), String> {
        let min_timeout = block_height
            .checked_add(1)
            .ok_or_else(|| "block height overflow".to_string())?;
        if u64::from(timeout) <= min_timeout || timeout >= LOCKTIME_THRESHOLD {
            return Err(format!(
                "timeout {timeout} must be above {min_timeout} and below {LOCKTIME_THRESHOLD}"
            ));
        }
        let timeout_epoch = self.timeout_epoch(timeout, block_height);
        let lowest = self.epoch + u64::from(self.min_timeout) + 3;
        let highest = self.epoch + u64::from(self.max_timeout) + 3;
        if timeout_epoch < lowest || timeout_epoch > highest {
            return Err(format!(
                "timeout {timeout} is in epoch {timeout_epoch}, allowed epochs {lowest} to {highest}"
            ));
        }
        Ok(())
    }
}

/// The [`JoinContext`] of `subnet_id` from the monitor's state.
pub fn join_context<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
) -> Result<JoinContext, IpcLibError> {
    let named = named_configurations(db, subnet_id)?;
    let genesis_info = db
        .get_subnet_genesis_info(subnet_id)?
        .ok_or_else(|| IpcValidateError::InvalidMsg(format!("Subnet {} not found.", subnet_id)))?;
    let params = &genesis_info.create_subnet_msg;
    let state = db.get_subnet_state(subnet_id)?;
    Ok(JoinContext {
        named,
        epoch: state.as_ref().map_or(0, |s| s.epoch),
        h_0: state.and_then(|s| s.h_0),
        epoch_length: params.epoch_length,
        min_timeout: params.min_timeout,
        max_timeout: params.max_timeout,
    })
}

/// The configuration of committee `number`, `C_wlist` for 0.
pub fn configuration_by_number<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    number: u64,
) -> Result<Option<Configuration>, IpcLibError> {
    if number == WHITELIST_COMMITTEE_NUMBER {
        return Ok(db
            .get_subnet_genesis_info(subnet_id)?
            .map(|g| g.whitelist_configuration()));
    }
    Ok(db
        .get_committee(subnet_id, number)?
        .map(|c| c.configuration()))
}

/// The collateral script of a recorded collateral.
pub fn collateral_script_of<D: DatabaseCore>(
    db: &D,
    record: &UeCollateral,
) -> Result<CollateralScript, IpcLibError> {
    let configuration = configuration_by_number(db, record.subnet_id, record.committee_number)?
        .ok_or_else(|| {
            IpcValidateError::InvalidMsg(format!(
                "Subnet {} has no committee {}",
                record.subnet_id, record.committee_number
            ))
        })?;
    Ok(CollateralScript::new(
        record.pubkey,
        record.pk_eots,
        record.timeout,
        configuration,
    ))
}

/// The Bitcoin block of a collateral's join: that of the EOTS nonce commitment the join carried.
pub fn join_block_height<D: DatabaseCore>(
    db: &D,
    record: &UeCollateral,
) -> Result<u64, db::DbError> {
    Ok(db
        .get_eots_commitments(record.subnet_id, &record.pk_eots)?
        .iter()
        .find(|c| c.txid == record.join_txid)
        .map_or(0, |c| c.block_height))
}

/// The UE activation height from the environment, `None` when unset or empty.
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

/// A checked `join-ue` with `join.collateral` taken from the commit transaction.
#[derive(Debug, Clone)]
pub struct CheckedJoinUe {
    pub msg: IpcJoinUeMsg,
    pub outpoint: OutPoint,
    /// The committee number the collateral script names.
    pub committee_number: u64,
}

/// Checks the `join-ue` in input `input_index` of `reveal` against `commit` and `context`.
pub fn check_join_ue(
    secp: &Secp256k1<secp256k1::All>,
    reveal: &Transaction,
    input_index: usize,
    commit: &Transaction,
    block_height: u64,
    context: &JoinContext,
) -> Result<CheckedJoinUe, CollateralError> {
    let err = CollateralError::InvalidJoinUe;

    let input = reveal
        .input
        .get(input_index)
        .ok_or_else(|| err(format!("reveal has no input {input_index}")))?;
    let commit_txid = commit.compute_txid();
    if input.previous_output != OutPoint::new(commit_txid, 0) {
        return Err(err(format!(
            "input {input_index} spends {} instead of {}:0",
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
    let found = reveal_input(input)
        .ok_or_else(|| err(format!("input {input_index} does not execute a data leaf")))?;
    found.check_exact().map_err(|e| err(e.to_string()))?;

    let mut msg =
        IpcJoinUeMsg::from_bytes(&found.data).map_err(|e| err(format!("message: {e}")))?;

    if commit.output.len() != 2 && commit.output.len() != 3 {
        return Err(err(format!(
            "commit has {} outputs, expected 2 or 3",
            commit.output.len()
        )));
    }

    let collateral = &commit.output[UE_COLLATERAL_VOUT as usize];
    msg.join.collateral = collateral.value;
    msg.validate()
        .map_err(|e| err(format!("invalid message: {e}")))?;

    let mut committee_number = None;
    for (number, configuration) in &context.named {
        let script = msg.collateral_script(configuration.clone());
        if collateral.script_pubkey == script.script_pubkey(secp)? {
            committee_number = Some(*number);
            break;
        }
    }
    let Some(committee_number) = committee_number else {
        return Err(err(
            "collateral output is not the collateral script of the declared keys and timeout naming an accepted configuration".to_string(),
        ));
    };

    context
        .check_timeout(msg.timeout, block_height)
        .map_err(err)?;

    Ok(CheckedJoinUe {
        msg,
        outpoint: OutPoint::new(commit_txid, UE_COLLATERAL_VOUT),
        committee_number,
    })
}

/// A matured UE collateral to spend in an exit transaction.
#[derive(Debug, Clone)]
pub struct UeExitInput {
    pub outpoint: OutPoint,
    pub value: Amount,
    pub script: CollateralScript,
}

/// Builds and signs the exit transaction spending `inputs`, all collaterals of `keypair`,
/// to `to`, paying `fee_rate` for the exact weight.
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

    if let Some(other) = inputs.iter().find(|i| i.script.pubkey != pubkey) {
        return Err(err(format!(
            "collateral {} belongs to {}, not to the signing key",
            other.outpoint, other.script.pubkey
        )));
    }
    let lock_height = inputs
        .iter()
        .map(|i| i.script.timeout)
        .max()
        .expect("non-empty");
    let lock_time = LockTime::from_height(lock_height)
        .map_err(|e| err(format!("timeout {lock_height} is not a height: {e}")))?;

    let prevouts = inputs
        .iter()
        .map(|i| {
            Ok(TxOut {
                value: i.value,
                script_pubkey: i.script.script_pubkey(secp)?,
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

    // Placeholder witnesses give the final weight for the fee
    let witnesses = inputs
        .iter()
        .map(|i| {
            let leaf = i.script.exit_leaf();
            let control_block = i.script.control_block(secp, &leaf)?;
            Ok((leaf, control_block))
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
    let dust = tx.output[0].script_pubkey.minimal_non_dust();
    if payout < dust {
        return Err(err(format!(
            "the collateral {total} minus the fee {fee} is below the dust limit {dust}"
        )));
    }
    tx.output[0].value = payout;

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

/// Records the collateral and the join of a checked `join-ue`, a no-op if already applied.
/// Returns the subnet state when the join bootstraps the subnet.
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

    let existed = db
        .get_ue_collateral(join.subnet_id, join_txid, &join.pubkey)?
        .is_some();
    let applied = subnet_state
        .as_ref()
        .map(|s| s.latest_committee().validators.iter())
        .into_iter()
        .flatten()
        .chain(genesis_info.genesis_validators.iter())
        .any(|v| v.join_txid == join_txid && v.pubkey == join.pubkey);
    if existed && applied {
        log::debug!("join-ue {} is already applied", join_txid);
        return Ok(None);
    }

    join.validate_for_subnet(&genesis_info, &subnet_state)?;
    if let Some(state) = &subnet_state {
        let members = crate::epochs::next_multisig_members(db, &genesis_info, state)?;
        join.validate_for_members(state, &members)?;
    }
    join.validate()?;
    refuse_earlier_join(db, join.subnet_id, &join.pubkey, Some(join_txid))?;
    refuse_earlier_eots_key(
        db,
        join.subnet_id,
        &join.pubkey,
        &checked.msg.pk_eots,
        Some(join_txid),
    )?;
    refuse_excess_power(
        db,
        &genesis_info,
        subnet_state.as_ref(),
        block_height,
        None,
        join.collateral,
    )?;

    // Written before the join; removed again if the join fails
    if !existed {
        let record = UeCollateral {
            subnet_id: join.subnet_id,
            pubkey: join.pubkey,
            outpoint: checked.outpoint,
            value: join.collateral,
            timeout: checked.msg.timeout,
            pk_eots: checked.msg.pk_eots,
            committee_number: checked.committee_number,
            ip: join.ip,
            backup_address: join.backup_address.clone(),
            join_txid,
            spent: false,
        };
        let mut wtxn = db.write_txn()?;
        db.add_ue_collateral(&mut wtxn, &record)?;
        db.add_eots_commitment(
            &mut wtxn,
            &db::EotsCommitmentRecord {
                subnet_id: join.subnet_id,
                pk_eots: checked.msg.pk_eots,
                commitment: checked.msg.nonce_commitment,
                block_height,
                txid: join_txid,
            },
        )?;
        wtxn.commit()?;
    }

    // After activation there is no bootstrap
    if subnet_state.is_some() {
        return Ok(None);
    }

    match join.save_to_db(db, block_height, block_hash, join_txid) {
        Ok(bootstrapped) => Ok(bootstrapped),
        Err(e) => {
            if !existed {
                let mut wtxn = db.write_txn()?;
                db.delete_ue_collateral(&mut wtxn, join.subnet_id, join_txid, &join.pubkey)?;
                wtxn.commit()?;
            }
            Err(e)
        }
    }
}

/// Refuses a collateral of `new_collateral` that raises the total power of the configuration after
/// the pending changes above `MAX_TOTAL_POWER`. For a stake, `staker` is the multisig member whose
/// collateral becomes `new_collateral`; for a join it is `None`.
pub fn refuse_excess_power<D: DatabaseCore>(
    db: &D,
    genesis_info: &db::SubnetGenesisInfo,
    state: Option<&db::SubnetState>,
    block_height: u64,
    staker: Option<&XOnlyPublicKey>,
    new_collateral: Amount,
) -> Result<(), IpcLibError> {
    let params = &genesis_info.create_subnet_msg;
    let subnet_id = genesis_info.subnet_id;
    let (origin, epoch) = state
        .and_then(|s| s.h_0.map(|h_0| (h_0, s.epoch)))
        .unwrap_or((block_height, 0));
    let boundary = params.cutoff_height(origin, epoch + 3);

    let mut values: BTreeMap<XOnlyPublicKey, Amount> = BTreeMap::new();
    for record in db.get_ue_collaterals(subnet_id)? {
        if record.spent || u64::from(record.timeout) <= boundary {
            continue;
        }
        let value = values.entry(record.pubkey).or_insert(Amount::ZERO);
        *value = value.checked_add(record.value).ok_or_else(|| {
            IpcValidateError::InvalidMsg("collateral value overflow".to_string())
        })?;
    }
    let mut total: Power = 0;
    for value in values.values() {
        total = total.saturating_add(params.power_of(value)?);
    }
    match state {
        Some(state) => {
            let members = crate::epochs::next_multisig_members(db, genesis_info, state)?;
            for member in members.values() {
                if Some(&member.pubkey) != staker {
                    total = total.saturating_add(member.power);
                }
            }
        }
        None => {
            for member in &genesis_info.genesis_validators {
                if Some(&member.pubkey) != staker
                    && !is_ue_validator(db, subnet_id, member.join_txid, &member.pubkey)?
                {
                    total = total.saturating_add(member.power);
                }
            }
        }
    }
    total = total.saturating_add(params.power_of(&new_collateral)?);
    if total > MAX_TOTAL_POWER {
        return Err(IpcValidateError::InvalidMsg(format!(
            "the total power {total} with this collateral exceeds the maximum {MAX_TOTAL_POWER}"
        ))
        .into());
    }
    Ok(())
}

/// Records the spend of every collateral that an input of `tx` spends and returns them.
pub fn record_collateral_spends<D: DatabaseCore>(
    db: &D,
    tx: &Transaction,
) -> Result<Vec<UeCollateral>, db::DbError> {
    let mut spent = Vec::new();
    for input in &tx.input {
        let Some(record) = db.get_ue_collateral_by_outpoint(&input.previous_output)? else {
            continue;
        };
        if record.spent {
            continue;
        }
        let mut wtxn = db.write_txn()?;
        db.set_ue_collateral_spent(&mut wtxn, &input.previous_output)?;
        wtxn.commit()?;
        spent.push(record);
    }
    Ok(spent)
}

/// Refuses a join-ue when an earlier join of the subnet carries `pubkey`, other than `join_txid`.
pub fn refuse_earlier_join<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    pubkey: &XOnlyPublicKey,
    join_txid: Option<Txid>,
) -> Result<(), IpcLibError> {
    let genesis = db
        .get_subnet_genesis_info(subnet_id)?
        .map(|g| g.genesis_validators)
        .unwrap_or_default();
    let earlier = genesis
        .iter()
        .filter(|v| v.pubkey == *pubkey)
        .map(|v| v.join_txid)
        .chain(
            db.get_ue_collaterals(subnet_id)?
                .iter()
                .filter(|r| r.pubkey == *pubkey)
                .map(|r| r.join_txid),
        )
        .chain(
            db.get_all_stake_changes(subnet_id)?
                .iter()
                .filter(|sc| {
                    matches!(sc.change, db::StakingChange::Join { .. })
                        && sc.validator_xpk == *pubkey
                })
                .map(|sc| sc.txid),
        )
        .find(|txid| Some(*txid) != join_txid);
    match earlier {
        Some(txid) => Err(IpcValidateError::InvalidMsg(format!(
            "Validator {pubkey} joined subnet {subnet_id} before (join {txid}): a key joins a subnet once"
        ))
        .into()),
        None => Ok(()),
    }
}

/// Refuses a join-ue when an earlier join-ue of any subnet carries `pk_eots`, other than its own.
pub fn refuse_earlier_eots_key<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    pubkey: &XOnlyPublicKey,
    pk_eots: &XOnlyPublicKey,
    join_txid: Option<Txid>,
) -> Result<(), IpcLibError> {
    let own = |r: &UeCollateral| {
        r.subnet_id == subnet_id && r.pubkey == *pubkey && Some(r.join_txid) == join_txid
    };
    let earlier = db
        .get_all_ue_collaterals()?
        .into_iter()
        .find(|r| r.pk_eots == *pk_eots && !own(r));
    match earlier {
        Some(record) => Err(IpcValidateError::InvalidMsg(format!(
            "EOTS key {pk_eots} was registered in subnet {} before (join {}): an EOTS key joins once",
            record.subnet_id, record.join_txid
        ))
        .into()),
        None => Ok(()),
    }
}

/// An unsigned commit and reveal pair for `msg` as the provider builds them, for tests.
pub fn unsigned_join_ue_txs(
    secp: &Secp256k1<secp256k1::All>,
    msg: &IpcJoinUeMsg,
    collateral: Amount,
    configuration: &Configuration,
) -> Result<(Transaction, Transaction, Vec<u8>), CollateralError> {
    let data = msg
        .to_bytes()
        .map_err(|e| CollateralError::InvalidJoinUe(e.to_string()))?;
    let outputs = vec![
        TxOut {
            value: collateral,
            script_pubkey: msg
                .collateral_script(configuration.clone())
                .script_pubkey(secp)?,
        },
        TxOut {
            value: Amount::from_sat(50_000),
            script_pubkey: ScriptBuf::new_op_return([1]),
        },
    ];
    let (commit, reveal) = unsigned_commit_reveal(secp, &data, outputs)
        .map_err(|e| CollateralError::InvalidJoinUe(e.to_string()))?;
    Ok((commit, reveal, data))
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

/// Refuses a stake or unstake through the multisig for a join-ue member of the latest committee.
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

/// Checks the configured activation height against the stored one at startup and stores it.
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
mod tests;
