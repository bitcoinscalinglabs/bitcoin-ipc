use bitcoin::{
    hashes::Hash,
    key::{Keypair, Secp256k1},
    secp256k1::{PublicKey, SecretKey},
    BlockHash, Txid, XOnlyPublicKey,
};
use rand::Rng;

use crate::multisig::{collateral_to_power, Power, WeightedKey};
use crate::{db, SubnetId};

pub fn generate_xonly_pubkeys(n: usize) -> Vec<XOnlyPublicKey> {
    let secp = Secp256k1::new();
    (0..n)
        .map(|_| {
            let secret_key = SecretKey::new(&mut rand::thread_rng());
            let public_key = PublicKey::from_secret_key(&secp, &secret_key);
            XOnlyPublicKey::from(public_key)
        })
        .collect()
}

pub fn generate_equal_weighted_keys(n: usize) -> Vec<WeightedKey> {
    generate_xonly_pubkeys(n)
        .into_iter()
        .map(|pubkey| (pubkey, 1))
        .collect()
}

pub fn generate_random_weighted_keys(n: usize) -> Vec<WeightedKey> {
    let power: Power = rand::thread_rng().gen_range(0..=5000);

    generate_xonly_pubkeys(n)
        .into_iter()
        .map(|pubkey| (pubkey, power))
        .collect()
}

pub fn generate_keypairs(n: usize) -> Vec<Keypair> {
    let secp = Secp256k1::new();
    let mut keypairs: Vec<Keypair> = (0..n)
        .map(|_| {
            let mut secret_key = SecretKey::new(&mut rand::thread_rng());
            if secret_key.x_only_public_key(&secp).1 == bitcoin::key::Parity::Odd {
                secret_key = secret_key.negate();
            }
            Keypair::from_secret_key(&secp, &secret_key)
        })
        .collect();
    // sort keypairs by x-only public key
    keypairs.sort_by_key(|k| k.x_only_public_key());
    keypairs
}

pub fn generate_subnet_id() -> SubnetId {
    SubnetId::from_txid(&Txid::from_slice(&rand::random::<[u8; 32]>()).unwrap())
}

pub fn generate_subnet(n_val: usize) -> db::SubnetState {
    use crate::{
        db, eth_utils::eth_addr_from_x_only_pubkey, multisig::create_subnet_multisig_address,
        NETWORK,
    };
    use bitcoin::Amount;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::str::FromStr;

    assert!(n_val >= 1, "number of validators must be at least 1");

    let subnet_id = generate_subnet_id();
    let keypairs = generate_keypairs(n_val);

    // Create subnet validators
    let validators: Vec<db::SubnetValidator> = keypairs
        .iter()
        .enumerate()
        .map(|(i, kp)| {
            let (pubkey, _) = kp.x_only_public_key();
            let subnet_address = eth_addr_from_x_only_pubkey(pubkey);

            // Generate a random IP for testing
            let ip = SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, (i + 1) as u8)),
                8080 + i as u16,
            );

            // Generate a random txid for the join transaction
            let join_txid = Txid::from_slice(&rand::random::<[u8; 32]>()).unwrap();

            // Use regtest address format for testing
            let backup_address =
                bitcoin::Address::from_str("bcrt1qvr3jycfxtrkk8u6hp5caxc25tueek5f90mpnsv").unwrap();

            let collateral = Amount::from_sat(100000);
            let power = collateral_to_power(&collateral, &Amount::from_sat(10000))
                .expect("expect good collateral");

            db::SubnetValidator {
                pubkey,
                subnet_address,
                collateral,
                power,
                backup_address,
                ip,
                join_txid,
            }
        })
        .collect();

    // Create subnet parameters
    let min_validators = std::cmp::max(1, n_val / 2) as Power; // Set threshold to n/2 rounded up

    // Create committee
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let committee_keys: Vec<WeightedKey> = validators.iter().map(|v| (v.pubkey, 1)).collect();

    // Create multisig address
    let multisig_address =
        create_subnet_multisig_address(&secp, &subnet_id, &committee_keys, min_validators, NETWORK)
            .unwrap();
    let multisig_address = multisig_address.as_unchecked();

    let committee = db::SubnetCommittee {
        configuration_number: 0,
        validators,
        threshold: min_validators,
        multisig_address: multisig_address.clone(),
    };

    // Create SubnetState
    db::SubnetState {
        id: subnet_id,
        committee_number: 1,
        committee,
        waiting_committee: None,
        last_checkpoint_number: None,
    }
}

pub fn create_test_db() -> crate::db::HeedDb {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().to_str().unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(crate::db::HeedDb::new(db_path, false))
        .unwrap()
}

pub fn create_rand_txid() -> Txid {
    Txid::from_slice(&rand::random::<[u8; 32]>()).unwrap()
}
pub fn create_rand_blockhash() -> BlockHash {
    BlockHash::from_slice(&rand::random::<[u8; 32]>()).unwrap()
}
pub fn create_rand_addr() -> alloy_primitives::Address {
    alloy_primitives::Address::from_slice(&rand::random::<[u8; 20]>())
}
