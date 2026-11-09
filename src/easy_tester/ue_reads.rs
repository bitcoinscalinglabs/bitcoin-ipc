//! Reads of the unilateral-exit state for scenarios, shared by the db tester and the
//! monitor tester (which opens the monitor's database read-only). Each read flattens its
//! result into `path -> value` fields that `expect result.<path> = <value>` checks.

use std::collections::{BTreeMap, HashMap};

use bitcoin::XOnlyPublicKey;

use crate::db::{DatabaseCore, StakingChange};
use crate::easy_tester::{error::EasyTesterError, model::OutputExpectTarget};
use crate::epochs::is_ue_validator;
use crate::SubnetId;

/// Validator pubkey -> scenario name
pub type Names = HashMap<XOnlyPublicKey, String>;

#[derive(Debug, Clone)]
pub struct UeRead {
    pub what: String,
    pub fields: BTreeMap<String, String>,
}

fn name(names: &Names, pubkey: &XOnlyPublicKey) -> String {
    names
        .get(pubkey)
        .cloned()
        .unwrap_or_else(|| pubkey.to_string())
}

fn db_err(e: impl std::fmt::Display) -> EasyTesterError {
    EasyTesterError::runtime(format!("db read failed: {e}"))
}

pub fn read_validators<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    subnet_name: &str,
    names: &Names,
    current: bool,
) -> Result<UeRead, EasyTesterError> {
    let state = db
        .get_subnet_state(subnet_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            EasyTesterError::runtime(format!("subnet '{subnet_name}' is not bootstrapped"))
        })?;
    let committee = if current {
        &state.committee
    } else {
        state.latest_committee()
    };
    let mut fields = BTreeMap::new();
    fields.insert("count".to_string(), committee.validators.len().to_string());
    fields.insert(
        "waiting".to_string(),
        state.waiting_committee.is_some().to_string(),
    );
    let mut list = Vec::new();
    for v in &committee.validators {
        let n = name(names, &v.pubkey);
        fields.insert(format!("{n}.collateral"), v.collateral.to_sat().to_string());
        let ue = is_ue_validator(db, subnet_id, v.join_txid, &v.pubkey).map_err(db_err)?;
        fields.insert(format!("{n}.ue"), ue.to_string());
        list.push(n);
    }
    list.sort();
    fields.insert("names".to_string(), list.join(","));
    let what = format!(
        "validators {} {}",
        subnet_name,
        if current { "current" } else { "latest" }
    );
    print_read(&what, &fields);
    Ok(UeRead { what, fields })
}

pub fn read_subnet_state<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    subnet_name: &str,
) -> Result<UeRead, EasyTesterError> {
    let state = db
        .get_subnet_state(subnet_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            EasyTesterError::runtime(format!("subnet '{subnet_name}' is not bootstrapped"))
        })?;
    let mut fields = BTreeMap::new();
    fields.insert("activated".to_string(), state.is_activated().to_string());
    fields.insert("epoch".to_string(), state.epoch.to_string());
    fields.insert(
        "h_0".to_string(),
        state.h_0.map_or("none".to_string(), |h| h.to_string()),
    );
    fields.insert(
        "last_rotation_epoch".to_string(),
        state.last_rotation_epoch.to_string(),
    );
    fields.insert(
        "committee_number".to_string(),
        state.committee_number.to_string(),
    );
    fields.insert(
        "pending_multisig_changes".to_string(),
        state.pending_multisig_changes.len().to_string(),
    );
    let what = format!("subnet_state {subnet_name}");
    print_read(&what, &fields);
    Ok(UeRead { what, fields })
}

pub fn read_subnet_genesis<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    subnet_name: &str,
) -> Result<UeRead, EasyTesterError> {
    let mut fields = BTreeMap::new();
    match db.get_subnet_genesis_info(subnet_id).map_err(db_err)? {
        Some(info) => {
            fields.insert("exists".to_string(), "true".to_string());
            fields.insert("bootstrapped".to_string(), info.bootstrapped.to_string());
            fields.insert(
                "min_validators".to_string(),
                info.create_subnet_msg.min_validators.to_string(),
            );
            fields.insert(
                "genesis_validators".to_string(),
                info.genesis_validators.len().to_string(),
            );
        }
        None => {
            fields.insert("exists".to_string(), "false".to_string());
        }
    }
    let what = format!("subnet_genesis {subnet_name}");
    print_read(&what, &fields);
    Ok(UeRead { what, fields })
}

pub fn read_ue_collaterals<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    subnet_name: &str,
    names: &Names,
) -> Result<UeRead, EasyTesterError> {
    let records = db.get_ue_collaterals(subnet_id).map_err(db_err)?;
    let mut fields = BTreeMap::new();
    fields.insert("count".to_string(), records.len().to_string());
    fields.insert(
        "spent_count".to_string(),
        records.iter().filter(|r| r.spent).count().to_string(),
    );
    let slashable = db.get_all_slash_presigs(subnet_id).map_err(db_err)?;
    fields.insert("slashable_count".to_string(), slashable.len().to_string());
    let mut per_name: HashMap<String, Vec<&crate::db::UeCollateral>> = HashMap::new();
    for r in &records {
        per_name.entry(name(names, &r.pubkey)).or_default().push(r);
    }
    for (n, mut rs) in per_name {
        rs.sort_by_key(|r| r.timeout);
        let last = rs.last().expect("non-empty");
        fields.insert(format!("{n}.count"), rs.len().to_string());
        fields.insert(format!("{n}.value"), last.value.to_sat().to_string());
        fields.insert(format!("{n}.timeout"), last.timeout.to_string());
        fields.insert(format!("{n}.spent"), last.spent.to_string());
        fields.insert(
            format!("{n}.slashable"),
            slashable
                .iter()
                .any(|p| p.outpoint == last.outpoint)
                .to_string(),
        );
    }
    let what = format!("ue_collaterals {subnet_name}");
    print_read(&what, &fields);
    Ok(UeRead { what, fields })
}

pub fn read_pending_withdraw_changes<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    subnet_name: &str,
    names: &Names,
) -> Result<UeRead, EasyTesterError> {
    let changes = db
        .get_unconfirmed_stake_changes(subnet_id)
        .map_err(db_err)?;
    let mut fields = BTreeMap::new();
    let mut count = 0;
    for sc in &changes {
        if let StakingChange::Withdraw { amount } = sc.change {
            count += 1;
            let n = name(names, &sc.validator_xpk);
            let previous: u64 = fields
                .get(&n)
                .and_then(|v: &String| v.parse().ok())
                .unwrap_or(0);
            fields.insert(n, (previous + amount.to_sat()).to_string());
        }
    }
    fields.insert("count".to_string(), count.to_string());
    let what = format!("withdrawals {subnet_name}");
    print_read(&what, &fields);
    Ok(UeRead { what, fields })
}

fn print_read(what: &str, fields: &BTreeMap<String, String>) {
    println!("OUTPUT read {what}");
    for (k, v) in fields {
        println!("  {k} = {v}");
    }
}

/// `expect result.<path> = <value>` on the last UE read: numbers compare as numbers,
/// everything else as strings.
pub fn expect_ue(
    last: &UeRead,
    target: &OutputExpectTarget,
    expected: &str,
) -> Result<String, EasyTesterError> {
    let got = last.fields.get(&target.path).ok_or_else(|| {
        EasyTesterError::runtime(format!(
            "EXPECT failed (line {}): result.{} not in the last read ({})",
            target.line_no, target.path, last.what
        ))
    })?;
    let matches = match (got.parse::<u64>(), expected.parse::<u64>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => got == expected,
    };
    if !matches {
        return Err(EasyTesterError::runtime(format!(
            "EXPECT failed (line {}): result.{} expected '{}', got '{}'",
            target.line_no, target.path, expected, got
        )));
    }
    Ok(format!("result.{} == {}", target.path, got))
}
