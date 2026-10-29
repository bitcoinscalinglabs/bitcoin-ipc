//! Property tests of the monitor's parsers: arbitrary bytes and transactions never panic
//! and are never accepted when malformed, and encoding then decoding gives the same message.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::str::FromStr;

use bitcoin::{
    absolute::LockTime, hashes::Hash, key::Keypair, secp256k1::Secp256k1, transaction, Amount,
    OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid, Witness, XOnlyPublicKey,
};
use proptest::prelude::*;

use crate::bitcoin_utils::{self, make_push_data_script, reveal_input, unsigned_commit_reveal};
use crate::collateral::{check_join_ue, unsigned_join_ue_txs};
use crate::db::DatabaseCore;
use crate::ipc_lib::{
    IpcBatchTransferMsg, IpcCheckpointSubnetMsg, IpcCreateSubnetMsg, IpcCrossSubnetTransfer,
    IpcFundSubnetMsg, IpcJoinSubnetMsg, IpcJoinUeMsg, IpcKillSubnetMsg, IpcMessage,
    IpcPrefundSubnetMsg, IpcStakeCollateralMsg, IpcTag, IpcUnstakeCollateralMsg, IpcWithdrawal,
    IPC_TAG_LENGTH,
};
use crate::test_utils::{create_test_db, generate_subnet};
use crate::SubnetId;

const CASES: u32 = 64;

const ADDRESSES: [&str; 3] = [
    "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
    "bcrt1qgtzpfqlfkz4nhkpvz2enqtucm2pzhdvlssxnss",
    "bcrt1pm0mnpeqhjsjw46rn2gk859837pv8j42g5zlhqczlqexsqwupflxq2y2hlx",
];

fn address(index: usize) -> bitcoin::Address<bitcoin::address::NetworkUnchecked> {
    bitcoin::Address::from_str(ADDRESSES[index % ADDRESSES.len()]).unwrap()
}

fn keypair() -> impl Strategy<Value = Keypair> {
    any::<[u8; 32]>().prop_filter_map("a valid secret key", |bytes| {
        bitcoin::secp256k1::SecretKey::from_slice(&bytes)
            .ok()
            .map(|sk| Keypair::from_secret_key(&Secp256k1::new(), &sk))
    })
}

fn xonly() -> impl Strategy<Value = XOnlyPublicKey> {
    keypair().prop_map(|k| k.x_only_public_key().0)
}

fn subnet_id() -> impl Strategy<Value = SubnetId> {
    any::<[u8; 32]>().prop_map(|b| SubnetId::from_txid(&Txid::from_byte_array(b)))
}

fn socket_addr() -> impl Strategy<Value = SocketAddr> {
    (any::<u32>(), any::<u16>())
        .prop_map(|(ip, port)| SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), port)))
}

fn amount() -> impl Strategy<Value = Amount> {
    (1u64..21_000_000 * 100_000_000).prop_map(Amount::from_sat)
}

fn create_msg() -> impl Strategy<Value = IpcCreateSubnetMsg> {
    (
        amount(),
        any::<u16>(),
        any::<u64>(),
        any::<u16>(),
        amount(),
        prop::collection::vec(xonly(), 0..4),
    )
        .prop_map(
            |(min_validator_stake, min_validators, period, limit, fee, whitelist)| {
                IpcCreateSubnetMsg {
                    min_validator_stake,
                    min_validators,
                    bottomup_check_period: period,
                    active_validators_limit: limit,
                    min_cross_msg_fee: fee,
                    whitelist,
                }
            },
        )
}

fn join_msg() -> impl Strategy<Value = IpcJoinSubnetMsg> {
    (subnet_id(), socket_addr(), 0..3usize, xonly()).prop_map(|(subnet_id, ip, a, pubkey)| {
        IpcJoinSubnetMsg {
            subnet_id,
            collateral: Amount::ZERO,
            ip,
            backup_address: address(a),
            pubkey,
        }
    })
}

/// A reveal with one input whose witness is `elements`, and one output
fn reveal_with_witness(elements: &[Vec<u8>]) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::all_zeros(), 0),
            witness: Witness::from_slice(elements),
            ..Default::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(500),
            script_pubkey: ScriptBuf::new_op_return([2]),
        }],
    }
}

fn data_reveal(data: &[u8]) -> Transaction {
    reveal_with_witness(&[make_push_data_script(data).to_bytes(), vec![0xc0u8; 33]])
}

/// `data` with the tag `tag_index` in front: one of the message tags, or none
fn tagged(tag_index: usize, data: &[u8]) -> Vec<u8> {
    let tags = [
        IpcTag::CreateSubnet,
        IpcTag::JoinSubnet,
        IpcTag::JoinUe,
        IpcTag::BatchTransfer,
        IpcTag::CheckpointSubnet,
        IpcTag::PrefundSubnet,
        IpcTag::FundSubnet,
        IpcTag::StakeCollateral,
        IpcTag::UnstakeCollateral,
        IpcTag::KillSubnet,
    ];
    match tags.get(tag_index) {
        Some(tag) => [tag.as_str().as_bytes(), data].concat(),
        None => data.to_vec(),
    }
}

fn op_return_tx(payload: &[u8], extra_outputs: Vec<TxOut>) -> Transaction {
    let push: &bitcoin::script::PushBytes = payload.try_into().unwrap();
    let mut output = vec![TxOut {
        value: Amount::ZERO,
        script_pubkey: bitcoin_utils::make_op_return_script(push),
    }];
    output.extend(extra_outputs);
    Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::all_zeros(), 1),
            ..Default::default()
        }],
        output,
    }
}

fn setup() {
    crate::eth_utils::set_fvm_network();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn reveal_input_never_panics(elements in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..600), 0..5)) {
        let tx = reveal_with_witness(&elements);
        let _ = reveal_input(&tx.input[0]);
    }

    #[test]
    fn reveal_input_reads_back_any_data(data in prop::collection::vec(any::<u8>(), 0..1500)) {
        let tx = data_reveal(&data);
        prop_assert_eq!(reveal_input(&tx.input[0]).unwrap().data, data);
    }

    #[test]
    fn from_reveal_input_never_panics_and_is_idempotent(data in prop::collection::vec(any::<u8>(), 0..300), tag_index in 0..11usize) {
        setup();
        let data = tagged(tag_index, &data);
        let tx = data_reveal(&data);
        match IpcMessage::from_reveal_input(&tx, 0, &data) {
            Ok(IpcMessage::JoinUe { .. }) => prop_assert!(false, "random bytes carry no valid signature"),
            Ok(IpcMessage::CreateSubnet(msg)) => {
                let again = IpcCreateSubnetMsg::from_bytes(&msg.to_bytes().unwrap()).unwrap();
                prop_assert_eq!(format!("{again:?}"), format!("{msg:?}"));
            }
            Ok(IpcMessage::JoinSubnet(msg)) => {
                let again = IpcJoinSubnetMsg::from_bytes(&msg.to_bytes().unwrap()).unwrap();
                prop_assert_eq!(format!("{again:?}"), format!("{msg:?}"));
            }
            Ok(_) | Err(_) => {}
        }
        // Each message parser on its own
        let _ = IpcCreateSubnetMsg::from_bytes(&data);
        let _ = IpcJoinSubnetMsg::from_bytes(&data);
        let _ = IpcJoinUeMsg::from_bytes(&data);
        let _ = IpcBatchTransferMsg::witness_to_wire_data(&data);
    }

    #[test]
    fn create_round_trips_and_needs_one_input_and_one_output(msg in create_msg()) {
        setup();
        let data = msg.to_bytes().unwrap();
        let parsed = IpcCreateSubnetMsg::from_bytes(&data).unwrap();
        prop_assert_eq!(format!("{parsed:?}"), format!("{msg:?}"));
        let tx = data_reveal(&data);
        prop_assert!(matches!(IpcMessage::from_reveal_input(&tx, 0, &data), Ok(IpcMessage::CreateSubnet(_))));
        let mut two_inputs = tx.clone();
        two_inputs.input.push(tx.input[0].clone());
        prop_assert!(IpcMessage::from_reveal_input(&two_inputs, 0, &data).is_err());
        let mut two_outputs = tx.clone();
        two_outputs.output.push(tx.output[0].clone());
        prop_assert!(IpcMessage::from_reveal_input(&two_outputs, 0, &data).is_err());
    }

    #[test]
    fn join_round_trips(msg in join_msg()) {
        setup();
        let data = msg.to_bytes().unwrap();
        let parsed = IpcJoinSubnetMsg::from_bytes(&data).unwrap();
        prop_assert_eq!(parsed.subnet_id, msg.subnet_id);
        prop_assert_eq!(parsed.ip, msg.ip);
        prop_assert_eq!(parsed.backup_address, msg.backup_address);
        prop_assert_eq!(parsed.pubkey, msg.pubkey);
    }

    #[test]
    fn join_ue_round_trips_and_any_mutation_is_rejected(
        keypair in keypair(),
        join in join_msg(),
        timeout in 102u32..500_000_000,
        collateral in amount(),
        flip in (any::<prop::sample::Index>(), 1u8..=255),
        growth in -3i8..=3,
    ) {
        setup();
        let secp = Secp256k1::new();
        let (pubkey, _) = keypair.x_only_public_key();
        let join = IpcJoinSubnetMsg { pubkey, ..join };
        let msg = IpcJoinUeMsg::new_signed(&secp, join, timeout, &keypair.secret_key());
        let data = msg.to_bytes().unwrap();
        let parsed = IpcJoinUeMsg::from_bytes(&data).unwrap();
        prop_assert_eq!(parsed.timeout, timeout);
        prop_assert_eq!(parsed.join.pubkey, pubkey);
        prop_assert_eq!(parsed.signature, msg.signature);

        // The pair as the provider builds it is accepted below the timeout
        let (commit, reveal, _) = unsigned_join_ue_txs(&secp, &msg, collateral).unwrap();
        let block_height = u64::from(timeout) - 2;
        prop_assert!(check_join_ue(&secp, &reveal, 0, &commit, block_height).is_ok());
        prop_assert!(check_join_ue(&secp, &reveal, 0, &commit, u64::from(timeout) - 1).is_err());

        // One byte flipped, or bytes added or removed: rejected
        let mut mutated = data.clone();
        let at = flip.0.index(mutated.len());
        mutated[at] ^= flip.1;
        let collateral_output = commit.output[1].clone();
        let (commit_m, reveal_m) =
            unsigned_commit_reveal(&secp, &mutated, vec![collateral_output.clone()]).unwrap();
        prop_assert!(check_join_ue(&secp, &reveal_m, 0, &commit_m, block_height).is_err());
        prop_assert!(IpcJoinUeMsg::from_bytes(&mutated).is_err());
        let mut resized = data.clone();
        if growth >= 0 {
            resized.extend(std::iter::repeat(0u8).take(growth as usize + 1));
        } else {
            resized.truncate(resized.len() - (-growth) as usize);
        }
        let (commit_r, reveal_r) =
            unsigned_commit_reveal(&secp, &resized, vec![collateral_output]).unwrap();
        prop_assert!(check_join_ue(&secp, &reveal_r, 0, &commit_r, block_height).is_err());
    }

    #[test]
    fn op_return_parsers_never_panic(
        payload in prop::collection::vec(any::<u8>(), 0..100),
        tag_index in 0..11usize,
        extra in prop::collection::vec((1u64..100_000, 0..3usize), 0..3),
    ) {
        setup();
        let db = create_test_db();
        let payload = tagged(tag_index, &payload);
        let outputs = extra
            .into_iter()
            .map(|(sats, a)| TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: address(a).assume_checked().script_pubkey(),
            })
            .collect();
        let tx = op_return_tx(&payload, outputs);
        let _ = IpcPrefundSubnetMsg::from_tx(&tx);
        let _ = IpcFundSubnetMsg::from_tx(&tx);
        let _ = IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &tx);
        let _ = IpcStakeCollateralMsg::from_tx(&db, &tx);
        let _ = IpcUnstakeCollateralMsg::from_tx(&db, &tx);
        let _ = IpcKillSubnetMsg::from_tx(&db, &tx);
        let _ = IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&tx.output[0]);
        let _ = IpcMessage::from_reveal_inputs(&tx);
    }

    #[test]
    fn checkpoint_metadata_round_trips(
        hash in any::<[u8; 32]>(),
        height in any::<u64>(),
        configuration in any::<u64>(),
        withdrawals in 0..4usize,
        transfers in 0..4usize,
        kill in any::<bool>(),
    ) {
        setup();
        let db = create_test_db();
        let subnet = generate_subnet(2);
        {
            let mut wtxn = db.write_txn().unwrap();
            db.save_subnet_state(&mut wtxn, subnet.id, &subnet).unwrap();
            wtxn.commit().unwrap();
        }
        let destination = generate_subnet(2);
        let msg = IpcCheckpointSubnetMsg {
            subnet_id: subnet.id,
            checkpoint_hash: bitcoin::hashes::sha256::Hash::from_byte_array(hash),
            checkpoint_height: height,
            next_committee_configuration_number: configuration,
            withdrawals: (0..withdrawals)
                .map(|i| IpcWithdrawal { amount: Amount::from_sat(1_000 + i as u64), address: address(i) })
                .collect(),
            transfers: (0..transfers)
                .map(|i| IpcCrossSubnetTransfer {
                    amount: Amount::from_sat(2_000 + i as u64),
                    destination_subnet_id: destination.id,
                    subnet_multisig_address: Some(destination.committee.multisig_address.clone()),
                    subnet_user_address: alloy_primitives::Address::repeat_byte(i as u8 + 1),
                })
                .collect(),
            token_registrations: vec![],
            token_supply_adjustments: vec![],
            token_transfers: vec![],
            unstakes: vec![],
            change_address: Some(subnet.committee.multisig_address.clone()),
            is_kill_checkpoint: kill,
        };
        let fee_rate = bitcoin::FeeRate::from_sat_per_vb(10).unwrap();
        let metadata = msg.make_metadata_tx_out(fee_rate);
        let (w, t, u, killed) =
            IpcCheckpointSubnetMsg::extract_markers_from_metadata_tx_out(&metadata).unwrap();
        // One batch per destination subnet: every transfer here goes to `destination`
        prop_assert_eq!((w as usize, t as usize, u as usize, killed), (withdrawals, usize::from(transfers > 0), 0, kill));

        // A checkpoint transaction with only the metadata output parses when it has no payments
        if withdrawals == 0 && transfers == 0 {
            let tx = Transaction {
                version: transaction::Version::TWO,
                lock_time: LockTime::ZERO,
                input: vec![],
                output: vec![metadata],
            };
            let parsed = IpcCheckpointSubnetMsg::from_checkpoint_tx(&db, &tx).unwrap();
            prop_assert_eq!(parsed.subnet_id, subnet.id);
            prop_assert_eq!(parsed.checkpoint_hash, msg.checkpoint_hash);
            prop_assert_eq!(parsed.checkpoint_height, height);
            prop_assert_eq!(parsed.next_committee_configuration_number, configuration);
            prop_assert_eq!(parsed.is_kill_checkpoint, kill);
        }

        // The batch data decodes to the same bytes
        if transfers > 0 {
            let data = msg.make_batched_transfer_data().unwrap();
            let wire = IpcBatchTransferMsg::witness_to_wire_data(&data).unwrap();
            prop_assert_eq!(postcard::to_stdvec(&wire).unwrap(), data[IPC_TAG_LENGTH..].to_vec());
        }
    }
}
