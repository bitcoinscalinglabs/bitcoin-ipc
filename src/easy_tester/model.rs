use bitcoin::{
    hashes::Hash,
    key::Secp256k1,
    secp256k1::SecretKey,
    Amount, BlockHash, Txid, XOnlyPublicKey,
};
use rand::RngCore;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::{ipc_lib::IpcCreateSubnetMsg, SubnetId, NETWORK};

#[derive(Debug, Clone)]
pub struct ValidatorSpec {
    pub name: String,
    pub secret_key: SecretKey,
    pub pubkey: XOnlyPublicKey,
    pub default_ip: SocketAddr,
    pub default_backup_address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
}

#[derive(Debug, Clone)]
pub struct SubnetSpec {
    pub name: String,
    pub min_validators: u16,
    pub whitelist_names: Vec<String>,
    pub whitelist_pubkeys: Vec<XOnlyPublicKey>,
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

#[derive(Debug, Clone)]
pub enum ScenarioCommand {
    Block { height: u64 },
    Create { subnet_name: String },
    Join {
        subnet_name: String,
        validator_name: String,
        collateral_sats: u64,
    },
}

#[derive(Debug, Clone)]
pub struct ParsedTestFile {
    pub setup: SetupSpec,
    pub scenario: Vec<ScenarioCommand>,
}

pub fn parse_u64_allow_underscores(s: &str) -> Result<u64, String> {
    let normalized: String = s.chars().filter(|c| *c != '_').collect();
    normalized
        .parse::<u64>()
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

    let ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, (ordinal as u8).max(1))), 8080);
    let backup = create_rand_backup_address(xonly_pubkey);

    ValidatorSpec {
        name: name.to_string(),
        secret_key: sk,
        pubkey: xonly_pubkey,
        default_ip: ip,
        default_backup_address: backup,
    }
}

pub fn build_create_subnet_msg(subnet: &SubnetSpec) -> IpcCreateSubnetMsg {
    let min_validators = subnet.min_validators;
    let active_validators_limit = std::cmp::max(min_validators, 100);

    IpcCreateSubnetMsg {
        min_validator_stake: Amount::from_sat(100_000),
        min_validators,
        bottomup_check_period: 1,
        active_validators_limit,
        min_cross_msg_fee: Amount::from_sat(1),
        whitelist: subnet.whitelist_pubkeys.clone(),
    }
}

pub fn subnet_id_from_txid(txid: &Txid) -> SubnetId {
    SubnetId::from_txid(txid)
}

