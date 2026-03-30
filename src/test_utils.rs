use bitcoin::{
    hashes::Hash,
    key::{Keypair, Secp256k1},
    secp256k1::{PublicKey, SecretKey},
    Amount, BlockHash, ScriptBuf, Txid, XOnlyPublicKey,
};
use rand::Rng;

use crate::{
    db,
    ipc_lib::{
        IpcCrossSubnetErcTransfer, IpcCrossSubnetTransfer, IpcErcTokenRegistration, IpcUnstake,
        IpcWithdrawal,
    },
    SubnetId,
};
use crate::{
    multisig::{collateral_to_power, Power, WeightedKey},
    NETWORK,
};

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

#[allow(dead_code)]
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
        killed: db::SubnetKillState::NotKilled,
        marked_for_kill_checkpoint_number: None,
    }
}

pub fn create_test_db() -> crate::db::HeedDb {
    // Ensure FVM address network is set for SubnetId serde roundtrips.
    crate::eth_utils::set_fvm_network();

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

pub fn create_rand_addr() -> bitcoin::Address {
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let sk = SecretKey::new(&mut rand::thread_rng());
    let pk = PublicKey::from_secret_key(&secp, &sk);
    let xpk = XOnlyPublicKey::from(pk);
    bitcoin::Address::p2tr(&secp, xpk, None, NETWORK)
}

pub fn create_rand_eth_addr() -> alloy_primitives::Address {
    alloy_primitives::Address::from_slice(&rand::random::<[u8; 20]>())
}

pub fn create_rand_ipc_unstake(amount_sats: Option<u64>) -> IpcUnstake {
    let amount = Amount::from_sat(amount_sats.unwrap_or(40000));
    let pubkey = generate_xonly_pubkeys(1)[0];
    let address = create_rand_addr().into_unchecked();

    IpcUnstake {
        amount,
        address,
        pubkey,
    }
}

pub fn create_rand_ipc_withdrawal(amount_sats: Option<u64>) -> IpcWithdrawal {
    let amount = Amount::from_sat(amount_sats.unwrap_or(50000));
    let address = create_rand_addr().into_unchecked();

    IpcWithdrawal { amount, address }
}

pub fn create_rand_ipc_cross_subnet_transfer(
    destination_subnet: &db::SubnetState,
    amount_sats: Option<u64>,
) -> IpcCrossSubnetTransfer {
    let amount = Amount::from_sat(amount_sats.unwrap_or(30000));
    let subnet_user_address = create_rand_eth_addr();

    IpcCrossSubnetTransfer {
        amount,
        destination_subnet_id: destination_subnet.id,
        subnet_multisig_address: None,
        subnet_user_address,
    }
}

pub fn create_rand_erc_token_registration() -> IpcErcTokenRegistration {
    IpcErcTokenRegistration {
        home_token_address: create_rand_eth_addr(),
        name: "TestToken".to_string(),
        symbol: "TT".to_string(),
        decimals: 18,
        initial_supply: alloy_primitives::U256::from(1_000_000u64),
    }
}

pub fn create_rand_erc_transfer(
    home_subnet_id: SubnetId,
    destination_subnet_id: SubnetId,
) -> IpcCrossSubnetErcTransfer {
    let amount = alloy_primitives::U256::from(1000u64);

    IpcCrossSubnetErcTransfer {
        home_subnet_id,
        home_token_address: create_rand_eth_addr(),
        amount,
        destination_subnet_id,
        recipient: create_rand_eth_addr(),
    }
}

pub fn create_rand_utxo_entry(
    amount: Option<Amount>,
) -> bitcoincore_rpc::json::ListUnspentResultEntry {
    bitcoincore_rpc::json::ListUnspentResultEntry {
        txid: create_rand_txid(),
        vout: 0,
        address: None,
        label: None,
        redeem_script: None,
        witness_script: None,
        script_pub_key: ScriptBuf::new(),
        amount: amount.unwrap_or(Amount::from_btc(500.0).unwrap()),
        confirmations: 1,
        spendable: true,
        solvable: true,
        descriptor: None,
        safe: true,
    }
}

#[cfg(test)]
mod tests {
    use crate::{eth_utils::evm_address_to_delegated_fvm, ipc_lib::L1_DELEGATED_NAMESPACE};

    use super::*;

    #[test]
    fn generate_keypairs_for_testing() {
        let keypairs = generate_keypairs(2);

        for keypair in keypairs.iter() {
            let (x_only, _parity) = keypair.x_only_public_key();
            let addr = alloy_primitives::Address::from_raw_public_key(
                &keypair.public_key().serialize_uncompressed()[1..],
            );

            println!(
                "SK = {}\nXPK = {}\nADDR = {}\nFIL = {}\n",
                keypair.secret_key().display_secret(),
                x_only,
                addr,
                evm_address_to_delegated_fvm(&addr, L1_DELEGATED_NAMESPACE),
            );
        }

        // uncomment next line to fail the test and print the keypairs
        // assert!(false);
    }
}
