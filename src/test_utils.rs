use bitcoin::{
    hashes::Hash,
    key::{Keypair, Secp256k1},
    secp256k1::{PublicKey, SecretKey},
    Amount, BlockHash, ScriptBuf, Txid, XOnlyPublicKey,
};
use proptest::prelude::{any, Strategy};
use rand::Rng;

use crate::collateral::{process_join_ue, CheckedJoinUe};
use crate::db::{DatabaseCore, HeedDb, SubnetGenesisInfo, UeCollateral};
use crate::eots::EotsNonceCommitment;
use crate::ipc_lib::{
    CheckpointSigner, IpcCheckpointSubnetMsg, IpcCreateSubnetMsg, IpcJoinSubnetMsg, IpcJoinUeMsg,
    IpcLibError, IpcMessage,
};
use crate::slash::{sign_slash, SlashSignatureEntry};
use bitcoin::{Address, OutPoint};

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
    // The FVM address network must be set for SubnetId serde roundtrips
    crate::eth_utils::set_fvm_network();
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
            let power = collateral_to_power(
                &collateral,
                &Amount::from_sat(10000),
                &Amount::from_sat(10_000),
            )
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
        // Activated at genesis, in epoch 1
        epoch: 1,
        h_0: Some(0),
        last_rotation_epoch: 1,
        pending_multisig_changes: Vec::new(),
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

pub fn secret(n: u8) -> SecretKey {
    let mut bytes = [0; 32];
    bytes[31] = n;
    SecretKey::from_slice(&bytes).unwrap()
}

pub fn pubkey(n: u8) -> XOnlyPublicKey {
    secret(n).x_only_public_key(&Secp256k1::new()).0
}

pub fn subnet(n: u8) -> SubnetId {
    SubnetId::from_txid20(&[n; 20])
}

pub async fn database() -> (tempfile::TempDir, HeedDb) {
    // Database keys hold SubnetId's string form, which depends on this process-global.
    crate::eth_utils::set_fvm_network();
    let dir = tempfile::tempdir().unwrap();
    let db = HeedDb::new(dir.path().to_str().unwrap(), false)
        .await
        .unwrap();
    (dir, db)
}

/// Stores the genesis information of the not yet bootstrapped subnet `id`: six whitelisted
/// keys `pubkey(1..=6)`, `min_validators` 4 and a minimum collateral of 100_000 sat.
pub fn create(db: &HeedDb, id: SubnetId) {
    let genesis = SubnetGenesisInfo {
        subnet_id: id,
        create_subnet_msg: IpcCreateSubnetMsg {
            min_validator_stake: Amount::from_sat(100_000),
            power_unit_exponent: 4,
            min_validators: 4,
            bottomup_check_period: 30,
            active_validators_limit: 10,
            min_cross_msg_fee: Amount::from_sat(1),
            whitelist: (1..=6).map(pubkey).collect(),
            epoch_length: 10,
            min_timeout: 1,
            max_timeout: 1000,
            min_deposit: Amount::from_sat(330),
            kill_vote_period: 36,
            kill_delay_period: 5,
        },
        create_msg_block_height: 100,
        bootstrapped: false,
        genesis_block_height: None,
        genesis_validators: vec![],
        genesis_balance_entries: vec![],
    };
    let mut txn = db.write_txn().unwrap();
    db.save_subnet_genesis_info(&mut txn, id, &genesis).unwrap();
    txn.commit().unwrap();
}

/// A validated join of the validator `owner` with the EOTS key `eots`, spending the
/// outpoint `(tx_marker, 1)`.
pub fn checked(id: SubnetId, owner: u8, eots: u8, tx_marker: u8) -> CheckedJoinUe {
    let secp = Secp256k1::new();
    let join = IpcJoinSubnetMsg {
        subnet_id: id,
        collateral: Amount::from_sat(100_000),
        ip: "192.0.2.1:8080".parse().unwrap(),
        backup_address: Address::p2tr(&secp, pubkey(owner), None, NETWORK).into_unchecked(),
        pubkey: pubkey(owner),
    };
    CheckedJoinUe {
        msg: IpcJoinUeMsg::new_signed(
            &secp,
            join,
            1000,
            &secret(owner),
            &secret(eots),
            EotsNonceCommitment {
                start: 129,
                count: 17,
                root: [0x35; 32],
            },
        ),
        outpoint: OutPoint::new(Txid::from_byte_array([tx_marker; 32]), 1),
        committee_number: 0,
    }
}

pub fn process(
    db: &HeedDb,
    join: &CheckedJoinUe,
    reveal_marker: u8,
) -> Result<Option<db::SubnetState>, IpcLibError> {
    let mut txn = db.write_txn()?;
    let state = process_join_ue(
        db,
        &mut txn,
        join,
        Txid::from_byte_array([reveal_marker; 32]),
        102,
        BlockHash::from_byte_array([0x42; 32]),
    )?;
    txn.commit()?;
    Ok(state)
}

/// The slash pre-signatures of `record` by the whitelisted keys `pubkey(signers)`.
pub fn slash_entry(db: &HeedDb, record: &UeCollateral, signers: &[u8]) -> SlashSignatureEntry {
    let secp = Secp256k1::new();
    let txn = db.read_txn().unwrap();
    let script = crate::collateral::collateral_script_of(db, &txn, record).unwrap();
    let mut signatures: Vec<_> = signers
        .iter()
        .map(|n| {
            sign_slash(
                &secp,
                &script,
                record.outpoint,
                record.value,
                &secret(*n).keypair(&secp),
            )
            .unwrap()
        })
        .collect();
    signatures.sort_by_key(|(index, _)| *index);
    SlashSignatureEntry::new(record.outpoint, signatures)
}

/// A subnet in epoch 0 with four joined validators `pubkey(1..=4)`.
pub struct EpochZeroFixture {
    pub _dir: tempfile::TempDir,
    pub db: HeedDb,
    pub id: SubnetId,
}

impl EpochZeroFixture {
    /// The join of validator `n` times out at `timeouts[n - 1]`.
    pub async fn new(timeouts: [u32; 4]) -> Self {
        let (dir, db) = database().await;
        let id = subnet(91);
        create(&db, id);
        for n in 1..=4 {
            let mut join = checked(id, n, n + 20, n + 10);
            join.msg = IpcJoinUeMsg::new_signed(
                &Secp256k1::new(),
                join.msg.join.clone(),
                timeouts[n as usize - 1],
                &secret(n),
                &secret(n + 20),
                join.msg.nonce_commitment,
            );
            process(&db, &join, n + 30).unwrap();
        }
        Self { _dir: dir, db, id }
    }

    pub fn state(&self) -> db::SubnetState {
        self.db.get_subnet_state(self.id).unwrap().unwrap()
    }

    pub fn signer(&self) -> CheckpointSigner {
        let g = self.db.get_subnet_genesis_info(self.id).unwrap().unwrap();
        CheckpointSigner::of_configuration(&self.id, &g.whitelist_configuration()).unwrap()
    }

    /// A regular checkpoint of epoch 0 whose reveal carries the slash pre-signatures of
    /// the collaterals of `owners`.
    pub fn message(&self, owners: &[u8]) -> IpcCheckpointSubnetMsg {
        let mut records = self.db.get_ue_collaterals(self.id).unwrap();
        records.sort_by_key(|r| r.outpoint);
        let signatures = records
            .iter()
            .filter(|r| owners.iter().any(|n| pubkey(*n) == r.pubkey))
            .map(|r| slash_entry(&self.db, r, &[1, 2, 3]))
            .collect();
        IpcCheckpointSubnetMsg {
            subnet_id: self.id,
            checkpoint_hash: bitcoin::hashes::sha256::Hash::from_byte_array([0x93; 32]),
            checkpoint_height: 30,
            next_committee_configuration_number: 0,
            withdrawals: vec![],
            transfers: vec![],
            token_registrations: vec![],
            token_supply_adjustments: vec![],
            token_transfers: vec![],
            slash_signatures: signatures,
            unstakes: vec![],
            change_address: Some(self.signer().address.as_unchecked().clone()),
            is_kill_checkpoint: false,
            is_rotation_checkpoint: false,
        }
    }

    /// The checkpoint transaction of `msg` and its batch transfer reveal. The checkpoint
    /// transaction is stored.
    pub fn transactions(
        &self,
        msg: &IpcCheckpointSubnetMsg,
    ) -> (bitcoin::Transaction, bitcoin::Transaction) {
        let signer = self.signer();
        let unspent = bitcoincore_rpc::json::ListUnspentResultEntry {
            txid: Txid::from_byte_array([0x94; 32]),
            vout: 1,
            address: None,
            label: None,
            redeem_script: None,
            witness_script: None,
            script_pub_key: signer.address.script_pubkey(),
            amount: Amount::from_sat(10_000_000),
            confirmations: 10,
            spendable: true,
            solvable: true,
            descriptor: None,
            safe: true,
        };
        let change = msg
            .change_address
            .as_ref()
            .unwrap()
            .clone()
            .assume_checked();
        let tx = msg
            .build_checkpoint_psbt(
                &signer,
                &change,
                false,
                crate::DEFAULT_BTC_FEE_RATE,
                &[unspent],
            )
            .unwrap()
            .unsigned_tx;
        let reveal = msg
            .make_reveal_batch_transfer_tx(tx.compute_txid(), crate::DEFAULT_BTC_FEE_RATE, &change)
            .unwrap()
            .unwrap();
        let mut txn = self.db.write_txn().unwrap();
        self.db.save_transaction(&mut txn, &tx).unwrap();
        txn.commit().unwrap();
        (tx, reveal)
    }

    pub fn reveal(
        &self,
        reveal: &bitcoin::Transaction,
        height: u64,
    ) -> Result<db::SubnetCheckpoint, IpcLibError> {
        let (_, IpcMessage::BatchTransfer(mut batch)) = IpcMessage::from_reveal_inputs(reveal)
            .into_iter()
            .next()
            .unwrap()
        else {
            panic!("batch reveal")
        };
        let mut txn = self.db.write_txn()?;
        let checkpoint = batch.process(
            &self.db,
            &mut txn,
            height,
            BlockHash::from_byte_array([height as u8; 32]),
            reveal.compute_txid(),
        )?;
        txn.commit()?;
        Ok(checkpoint)
    }

    pub fn accept(
        &self,
        msg: &IpcCheckpointSubnetMsg,
        tx: &bitcoin::Transaction,
        height: u64,
    ) -> db::SubnetCheckpoint {
        let mut txn = self.db.write_txn().unwrap();
        let checkpoint = msg
            .save_to_db(
                &self.db,
                &mut txn,
                height,
                BlockHash::from_byte_array([height as u8; 32]),
                tx.compute_txid(),
            )
            .unwrap();
        txn.commit().unwrap();
        checkpoint
    }

    pub fn cutoff(&self, height: u64) -> Vec<crate::epochs::CutoffOutcome> {
        let mut txn = self.db.write_txn().unwrap();
        let outcomes = crate::epochs::process_cutoffs(
            &self.db,
            &mut txn,
            height,
            BlockHash::from_byte_array([height as u8; 32]),
        )
        .unwrap();
        txn.commit().unwrap();
        outcomes
    }
}

/// A rotation checkpoint of the latest configuration of `f`, 30 subnet blocks after the last
/// checkpoint, that carries the configuration number `number`.
pub fn rotation(f: &EpochZeroFixture, number: u64) -> IpcCheckpointSubnetMsg {
    let mut msg = f.message(&[]);
    msg.is_rotation_checkpoint = true;
    let last =
        f.db.get_checkpoint(f.id, f.state().last_checkpoint_number.unwrap())
            .unwrap()
            .unwrap();
    msg.checkpoint_height = last.checkpoint_height + 30;
    msg.checkpoint_hash = bitcoin::hashes::sha256::Hash::hash(&msg.checkpoint_height.to_le_bytes());
    msg.next_committee_configuration_number = number;
    msg.change_address = Some(f.state().latest_committee().multisig_address.clone());
    msg
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
    }
}

const ADDRESSES: [&str; 3] = [
    "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
    "bcrt1qgtzpfqlfkz4nhkpvz2enqtucm2pzhdvlssxnss",
    "bcrt1pm0mnpeqhjsjw46rn2gk859837pv8j42g5zlhqczlqexsqwupflxq2y2hlx",
];

/// One of three fixed regtest addresses, `index` taken modulo three
pub fn address_at(index: usize) -> Address<bitcoin::address::NetworkUnchecked> {
    std::str::FromStr::from_str(ADDRESSES[index % ADDRESSES.len()]).unwrap()
}

pub fn arb_keypair() -> impl Strategy<Value = Keypair> {
    any::<[u8; 32]>().prop_filter_map("a valid secret key", |bytes| {
        SecretKey::from_slice(&bytes)
            .ok()
            .map(|sk| Keypair::from_secret_key(&Secp256k1::new(), &sk))
    })
}

pub fn arb_xonly() -> impl Strategy<Value = XOnlyPublicKey> {
    arb_keypair().prop_map(|k| k.x_only_public_key().0)
}

/// A positive amount of at most the 21 million bitcoin supply
pub fn arb_amount() -> impl Strategy<Value = Amount> {
    (1u64..21_000_000 * 100_000_000).prop_map(Amount::from_sat)
}
