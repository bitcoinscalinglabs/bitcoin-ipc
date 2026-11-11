use bitcoin::{
    hashes::Hash, key::Secp256k1, secp256k1::SecretKey, Amount, BlockHash, Txid, XOnlyPublicKey,
};
use rand::RngCore;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::{easy_tester::reveal::RevealShape, ipc_lib::IpcCreateSubnetMsg, SubnetId, NETWORK};

#[derive(Debug, Clone)]
pub struct ValidatorSpec {
    pub name: String,
    pub secret_key: SecretKey,
    pub pubkey: XOnlyPublicKey,
    pub eots_secret_key: SecretKey,
    pub eots_seed: crate::eots::EotsSeed,
    pub default_ip: SocketAddr,
    pub default_backup_address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
}

#[derive(Debug, Clone)]
pub struct SubnetSpec {
    pub name: String,
    pub min_validators: u16,
    pub whitelist_names: Vec<String>,
    pub whitelist_pubkeys: Vec<XOnlyPublicKey>,
    /// `K`, the epoch length in Bitcoin blocks
    pub epoch_length: u64,
}

#[derive(Debug, Clone)]
pub struct SetupSpec {
    pub validators: HashMap<String, ValidatorSpec>,
    pub subnets: HashMap<String, SubnetSpec>,
}

impl SetupSpec {
    pub fn new() -> Self {
        Self {
            validators: HashMap::new(),
            subnets: HashMap::new(),
        }
    }
}

/// Configuration parsed from the separate config file (not the scenario file).
#[derive(Debug, Clone)]
pub enum TesterConfig {
    Db {
        activation_height: Option<u64>,
        snapshot_length: Option<u64>,
        /// Unilateral exit is on from this height; `None` = off
        ue_activation_height: Option<u64>,
    },
    Monitor {
        activation_height: Option<u64>,
        snapshot_length: Option<u64>,
        ue_activation_height: Option<u64>,
        monitor_log_level: Option<String>,
        provider_log_level: Option<String>,
    },
    /// Fendermint tester — subnets come from the config file, issuers from
    /// the scenario file.
    Fendermint {
        setup: FendermintSetup,
    },
}

// ── Fendermint-specific setup types ────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FendermintIssuer {
    pub name: String,
    pub ipc_address: String,
}

#[derive(Debug, Clone)]
pub struct FendermintSubnet {
    pub name: String,
    pub subnet_id: String,
    pub eth_rpc_url: String,
    pub provider_url: String,
}

#[derive(Debug, Clone)]
pub struct FendermintSetup {
    pub docker_container: String,
    pub issuers: HashMap<String, FendermintIssuer>,
    pub subnets: HashMap<String, FendermintSubnet>,
    pub subnet_order: Vec<String>,
    pub print_queries: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputDb {
    Subnet,
    SubnetGenesis,
    StakeChanges,
    KillRequests,
    Committee,
    RewardCandidates,
    RewardResults,
    RootnetMsgs,
    /// Read token balance: `read token_balance <subnet> <token_name>`
    TokenBalance,
    /// FendermintTester only: `read token_metadata <subnet> <token_name>`
    TokenMetadata,
    /// `read validators <subnet> [current]`: the latest (or the current) configuration
    Validators,
    /// `read ue_collaterals <subnet>`: the unilateral-exit collateral records
    UeCollaterals,
    /// `read withdrawals <subnet>`: the Withdraw stake changes not yet in a checkpoint
    Withdrawals,
    /// `read subnet_state <subnet>`: activation, epoch and `H_0`
    SubnetState,
}

/// One join of a `join-ue` line: the collateral matures at `timeout`. `subnet_name` is set
/// when the join targets another subnet than the line's (`<subnet>:<validator>`).
/// `collateral_spent`: a transaction spends the collateral UTXO before the reveal transaction
#[derive(Debug, Clone)]
pub struct JoinUeSpec {
    pub subnet_name: Option<String>,
    pub validator_name: String,
    pub collateral_sats: u64,
    pub timeout: u64,
    pub collateral_spent: bool,
}

#[derive(Debug, Clone)]
pub enum ScenarioCommand {
    Block {
        height: u64,
    },
    Create {
        subnet_name: String,
        shape: RevealShape,
    },
    /// `raw`: the transaction is hand-built and submitted without the provider
    Join {
        subnet_name: String,
        validator_name: String,
        collateral_sats: u64,
        raw: bool,
    },
    /// Join under the unilateral-exit script; several joins go in one reveal, in order
    JoinUe {
        subnet_name: String,
        joins: Vec<JoinUeSpec>,
        shape: RevealShape,
    },
    /// `raw`: the transaction is hand-built and submitted without the provider
    Stake {
        subnet_name: String,
        validator_name: String,
        amount_sats: u64,
        raw: bool,
    },
    Unstake {
        subnet_name: String,
        validator_name: String,
        amount_sats: u64,
        raw: bool,
    },
    /// Spend the validator's matured unilateral-exit collaterals to its backup address
    UnstakeUe {
        subnet_name: String,
        validator_name: String,
    },
    /// Slash pre-signatures of `owner`'s latest collateral by `signers`, carried by the next
    /// checkpoint of the subnet; `bad` corrupts the first signature
    Presign {
        subnet_name: String,
        owner: String,
        signers: Vec<String>,
        bad: bool,
    },
    /// One checkpoint per subnet; their batch transfers go in one reveal, in `shape`
    Checkpoint {
        subnets: Vec<String>,
        shape: RevealShape,
    },
    /// Register an ERC20 token on a subnet (queues ETR for next checkpoint).
    /// `issuer` is only set for FendermintTester scenarios.
    RegisterToken {
        subnet_name: String,
        issuer: Option<String>,
        name: String,
        symbol: String,
        initial_supply: alloy_primitives::U256,
    },
    /// Queue a mint supply adjustment (queues ETS for next checkpoint)
    MintToken {
        subnet_name: String,
        token_name: String,
        amount: alloy_primitives::U256,
    },
    /// Queue a burn supply adjustment (queues ETS for next checkpoint)
    BurnToken {
        subnet_name: String,
        token_name: String,
        amount: alloy_primitives::U256,
    },
    /// Pause execution for a fixed duration.
    Wait {
        seconds: u64,
    },
    /// Deposit native tokens (BTC) into a subnet for a given address.
    Deposit {
        subnet_name: String,
        address_name: String,
        amount_sats: u64,
    },
    /// Queue an ERC20 cross-subnet transfer (queues ETX for next checkpoint).
    /// Db/MonitorTester: 4-arg form (no actors).
    /// FendermintTester: 6-arg form with `src_actor` and `dst_actor`.
    ErcTransfer {
        src_subnet: String,
        src_actor: Option<String>,
        dst_subnet: String,
        dst_actor: Option<String>,
        token_name: String,
        amount: alloy_primitives::U256,
    },
    OutputRead {
        db: OutputDb,
        args: Vec<String>,
    },
    OutputExpect {
        target: OutputExpectTarget,
        /// The expected value as a raw string. Testers parse as u64 or compare as string.
        expected_value: String,
    },
}

/// Generic expect target — always starts with `result.`.
/// The tester interprets the path based on what was last read.
/// Examples:
///   result.count = 3
///   result.0.kind = 2
///   result.0.tokenDecimals = 18
///   result.rewards_list.validator1 = 100_000_000
///   result.total_rewarded_collateral = 1_000_000_000
#[derive(Debug, Clone)]
pub struct OutputExpectTarget {
    /// The dotted path after "result." (e.g. "count", "0.kind", "rewards_list.validator1")
    pub path: String,
    /// Line number in the scenario file where this expect command appears.
    pub line_no: usize,
}

#[derive(Debug, Clone)]
pub struct ParsedTest {
    pub setup: SetupSpec,
    pub scenario: Vec<(usize, ScenarioCommand)>,
}

#[derive(Debug, Clone)]
pub struct ParsedFendermintTest {
    pub setup: FendermintSetup,
    pub scenario: Vec<(usize, ScenarioCommand)>,
}

/// Strip underscores from a token that is entirely digits and underscores
/// (e.g. `1_000_000` → `"1000000"`). Strings with non-digit characters
/// (e.g. `erc_registration`, `subnet_a`) are returned unchanged.
pub fn normalize_numeric_literal(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_digit() || c == '_') {
        s.chars().filter(|c| *c != '_').collect()
    } else {
        s.to_string()
    }
}

pub fn parse_u64_allow_underscores(s: &str) -> Result<u64, String> {
    normalize_numeric_literal(s)
        .parse::<u64>()
        .map_err(|e| format!("invalid number '{s}': {e}"))
}

pub fn parse_u256_allow_underscores(s: &str) -> Result<alloy_primitives::U256, String> {
    normalize_numeric_literal(s)
        .parse::<alloy_primitives::U256>()
        .map_err(|e| format!("invalid number '{s}': {e}"))
}

pub fn parse_u16_allow_underscores(s: &str) -> Result<u16, String> {
    let v = parse_u64_allow_underscores(s)?;
    u16::try_from(v).map_err(|_| format!("number out of range for u16: '{s}'"))
}

pub fn create_rand_txid() -> Txid {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    Txid::from_slice(&bytes).expect("random bytes should make a txid")
}

pub fn create_rand_blockhash() -> BlockHash {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    BlockHash::from_slice(&bytes).expect("random bytes should make a blockhash")
}

pub fn create_rand_backup_address(
    pubkey: XOnlyPublicKey,
) -> bitcoin::Address<bitcoin::address::NetworkUnchecked> {
    let secp = bitcoin::secp256k1::Secp256k1::new();
    bitcoin::Address::p2tr(&secp, pubkey, None, NETWORK).into_unchecked()
}

pub fn generate_validator(name: &str, ordinal: usize) -> ValidatorSpec {
    let secp = Secp256k1::new();
    let mut sk = SecretKey::new(&mut rand::thread_rng());
    if sk.x_only_public_key(&secp).1 == bitcoin::key::Parity::Odd {
        sk = sk.negate();
    }

    let keypair = bitcoin::key::Keypair::from_secret_key(&secp, &sk);
    let (xonly_pubkey, _) = keypair.x_only_public_key();

    let ip = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, (ordinal as u8).max(1))),
        8080,
    );
    let backup = create_rand_backup_address(xonly_pubkey);

    ValidatorSpec {
        name: name.to_string(),
        secret_key: sk,
        pubkey: xonly_pubkey,
        eots_secret_key: SecretKey::new(&mut rand::thread_rng()),
        eots_seed: crate::eots::EotsSeed::new(&mut rand::thread_rng()),
        default_ip: ip,
        default_backup_address: backup,
    }
}

#[derive(Debug, Clone)]
pub struct PresignSpec {
    pub owner: String,
    pub signers: Vec<String>,
    pub bad: bool,
}

/// The slash pre-signature entries of queued `presign` lines, for the next checkpoint: each
/// signs the owner's latest recorded collateral in `subnet_id` with the signers' keys.
pub fn build_slash_entries<D: crate::db::DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    specs: &[PresignSpec],
    validators: &HashMap<String, ValidatorSpec>,
) -> Result<Vec<crate::slash::SlashSignatureEntry>, crate::easy_tester::EasyTesterError> {
    use crate::easy_tester::EasyTesterError;
    let secp = Secp256k1::new();
    let mut entries: Vec<crate::slash::SlashSignatureEntry> = Vec::new();
    for spec in specs {
        let owner = validators
            .get(&spec.owner)
            .ok_or_else(|| EasyTesterError::runtime(format!("unknown validator '{}'", spec.owner)))?;
        let record = db
            .get_ue_collaterals(subnet_id)
            .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?
            .into_iter()
            .filter(|r| r.pubkey == owner.pubkey)
            .max_by_key(|r| r.timeout)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!("'{}' has no join-ue collateral", spec.owner))
            })?;
        let script = crate::collateral::collateral_script_of(db, &record)
            .map_err(|e| EasyTesterError::runtime(format!("collateral script: {e}")))?;
        let mut signatures = Vec::new();
        for signer_name in &spec.signers {
            let signer = validators.get(signer_name).ok_or_else(|| {
                EasyTesterError::runtime(format!("unknown validator '{signer_name}'"))
            })?;
            let keypair = bitcoin::key::Keypair::from_secret_key(&secp, &signer.secret_key);
            let (index, mut signature) =
                crate::slash::sign_slash(&secp, &script, record.outpoint, record.value, &keypair)
                    .map_err(|e| EasyTesterError::runtime(format!("presign: {e}")))?;
            if spec.bad && signatures.is_empty() {
                signature[1][0] ^= 1;
            }
            signatures.push((index, signature));
        }
        signatures.sort_by_key(|(index, _)| *index);
        match entries.iter_mut().find(|e| e.outpoint() == record.outpoint) {
            Some(entry) => {
                entry.signatures.extend(signatures);
                entry.signatures.sort_by_key(|(index, _)| *index);
            }
            None => entries.push(crate::slash::SlashSignatureEntry::new(record.outpoint, signatures)),
        }
    }
    entries.sort_by_key(|e| (e.txid, e.vout));
    Ok(entries)
}

/// The number of heights a tester's EOTS nonce commitment covers, from height 1.
pub const TEST_EOTS_NONCE_COUNT: u32 = 1_000;

/// A signed `join-ue` message of `validator` with its EOTS nonce commitment for heights
/// `1..=TEST_EOTS_NONCE_COUNT`.
pub fn join_ue_msg(
    secp: &Secp256k1<bitcoin::secp256k1::All>,
    join: crate::ipc_lib::IpcJoinSubnetMsg,
    timeout: u32,
    validator: &ValidatorSpec,
) -> crate::ipc_lib::IpcJoinUeMsg {
    let (commitment, _) = crate::eots::commit_nonces(
        &validator.eots_seed,
        &join.subnet_id,
        1,
        TEST_EOTS_NONCE_COUNT,
    );
    crate::ipc_lib::IpcJoinUeMsg::new_signed(
        secp,
        join,
        timeout,
        &validator.secret_key,
        &validator.eots_secret_key,
        commitment,
    )
}

pub fn build_create_subnet_msg(subnet: &SubnetSpec) -> IpcCreateSubnetMsg {
    let min_validators = subnet.min_validators;
    let active_validators_limit = std::cmp::max(min_validators, 100);

    IpcCreateSubnetMsg {
        min_validator_stake: Amount::from_sat(100_000),
        power_unit_exponent: 4,
        min_validators,
        bottomup_check_period: 1,
        active_validators_limit,
        min_cross_msg_fee: Amount::from_sat(1),
        whitelist: subnet.whitelist_pubkeys.clone(),
        epoch_length: subnet.epoch_length,
        min_timeout: 1,
        max_timeout: 1000,
        min_deposit: Amount::from_sat(330),
        kill_vote_period: 36,
        kill_delay_period: 5,
    }
}

pub fn subnet_id_from_txid(txid: &Txid) -> SubnetId {
    SubnetId::from_txid(txid)
}
