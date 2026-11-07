use super::ue_leaf_script;
use bitcoin::{
    absolute,
    consensus::serialize,
    hashes::Hash,
    secp256k1::{Keypair, Message, Secp256k1, SecretKey, XOnlyPublicKey},
    sighash::{Prevouts, SighashCache, TapSighashType},
    taproot::{LeafVersion, TapLeafHash, TaprootBuilder},
    transaction, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};

fn key(n: u8) -> Keypair {
    let mut bytes = [0; 32];
    bytes[31] = n;
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&bytes).unwrap())
}

fn assert_leaf_prefix(timeout: u32, prefix: &[u8]) {
    let pk = key(3).x_only_public_key().0;
    let mut expected = prefix.to_vec();
    expected.extend([0xb1, 0x75, 0x20]); // CLTV DROP PUSHBYTES_32
    expected.extend(pk.serialize());
    expected.push(0xac); // CHECKSIG
    assert_eq!(ue_leaf_script(&pk, timeout).as_bytes(), expected);
}

// encoding.md, Collateral script: script numbers 1 through 16 use their dedicated opcodes.
#[test]
fn exit_leaf_uses_small_integer_opcodes_at_sixteen() {
    assert_leaf_prefix(1, &[0x51]);
    assert_leaf_prefix(16, &[0x60]);
}

// encoding.md, Collateral script: 17 is a one-byte minimally pushed script number.
#[test]
fn exit_leaf_switches_to_a_data_push_at_seventeen() {
    assert_leaf_prefix(17, &[0x01, 0x11]);
}

// encoding.md, Collateral script: positive script numbers need a sign-padding byte at 128.
#[test]
fn exit_leaf_does_not_encode_height_128_as_negative_zero() {
    assert_leaf_prefix(127, &[0x01, 0x7f]);
    assert_leaf_prefix(128, &[0x02, 0x80, 0x00]);
    assert_leaf_prefix(255, &[0x02, 0xff, 0x00]);
}

// encoding.md, Collateral script: the script-number sign boundary recurs at 32768.
#[test]
fn exit_leaf_sign_padding_is_correct_at_the_two_byte_boundary() {
    assert_leaf_prefix(32767, &[0x02, 0xff, 0x7f]);
    assert_leaf_prefix(32768, &[0x03, 0x00, 0x80, 0x00]);
}

// encoding.md, Collateral script: the largest allowed T is a minimally pushed little-endian height.
#[test]
fn exit_leaf_encodes_the_largest_block_height_in_little_endian() {
    assert_leaf_prefix(499_999_999, &[0x04, 0xff, 0x64, 0xcd, 0x1d]);
}

// Only U is under test. A single-leaf BIP341 harness isolates it from the two leaves explicitly
// marked unimplemented in encoding.md; no expected collateral tree or control block is asserted.
fn spend(
    timeout: u32,
    locktime: u32,
    sequence: u32,
    signer: u8,
    second_input: bool,
) -> (Transaction, Vec<TxOut>) {
    let secp = Secp256k1::new();
    let leaf = ue_leaf_script(&key(3).x_only_public_key().0, timeout);
    let nums = XOnlyPublicKey::from_slice(
        &hex::decode("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81799").unwrap(),
    )
    .unwrap();
    let info = TaprootBuilder::new()
        .add_leaf(0, leaf.clone())
        .unwrap()
        .finalize(&secp, nums)
        .unwrap();
    let prevout = TxOut {
        value: Amount::from_sat(100_000),
        script_pubkey: ScriptBuf::new_p2tr_tweaked(info.output_key()),
    };
    let input = TxIn {
        previous_output: OutPoint::new(Txid::from_byte_array([42; 32]), 1),
        script_sig: ScriptBuf::new(),
        sequence: Sequence(sequence),
        witness: Witness::new(),
    };
    let mut tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::from_consensus(locktime),
        input: vec![input],
        output: vec![TxOut {
            value: Amount::from_sat(99_000),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    let mut prevouts = vec![prevout];
    if second_input {
        tx.input.push(TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([43; 32]), 1),
            script_sig: ScriptBuf::new(),
            sequence: Sequence(0xffff_fffd),
            witness: Witness::new(),
        });
        prevouts.push(TxOut {
            value: Amount::from_sat(5_000),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        });
    }
    let hash = SighashCache::new(&tx)
        .taproot_script_spend_signature_hash(
            0,
            &Prevouts::All(&prevouts),
            TapLeafHash::from_script(&leaf, LeafVersion::TapScript),
            TapSighashType::Default,
        )
        .unwrap();
    let sig =
        secp.sign_schnorr_no_aux_rand(&Message::from_digest(hash.to_byte_array()), &key(signer));
    let cb = info
        .control_block(&(leaf.clone(), LeafVersion::TapScript))
        .unwrap();
    tx.input[0].witness =
        Witness::from_slice(&[sig.serialize().to_vec(), leaf.into_bytes(), cb.serialize()]);
    (tx, prevouts)
}

fn verifies(tx: &Transaction, prevouts: &[TxOut]) -> bool {
    // The script buffers and their pointers remain live for the entire consensus call.
    let utxos: Vec<bitcoinconsensus::Utxo> = prevouts
        .iter()
        .map(|p| bitcoinconsensus::Utxo {
            script_pubkey: p.script_pubkey.as_bytes().as_ptr(),
            script_pubkey_len: p.script_pubkey.len() as u32,
            value: p.value.to_sat() as i64,
        })
        .collect();
    bitcoinconsensus::verify(
        prevouts[0].script_pubkey.as_bytes(),
        prevouts[0].value.to_sat(),
        &serialize(tx),
        Some(&utxos),
        0,
    )
    .is_ok()
}

// encoding.md, Collateral script: U accepts a signature of pk with nLockTime equal to T.
#[test]
fn exit_leaf_accepts_the_exact_cltv_locktime() {
    let (tx, prevouts) = spend(128, 128, 0xffff_fffd, 3, false);
    assert!(verifies(&tx, &prevouts));
}

// encoding.md, Collateral script: U requires nLockTime >= T, even with a valid signature.
#[test]
fn exit_leaf_rejects_a_signed_transaction_one_height_early() {
    let (tx, prevouts) = spend(128, 127, 0xffff_fffd, 3, false);
    assert!(!verifies(&tx, &prevouts));
    assert!(verifies(
        &spend(128, 128, 0xffff_fffd, 3, false).0,
        &prevouts
    ));
}

// encoding.md, Collateral script: U uses a lower bound rather than exact equality on nLockTime.
#[test]
fn exit_leaf_allows_a_later_block_height_locktime() {
    let (tx, prevouts) = spend(128, 129, 0xffff_fffd, 3, false);
    assert!(verifies(&tx, &prevouts));
}

// exit.md, Transaction format: a final nSequence cannot bypass U's CHECKLOCKTIMEVERIFY.
#[test]
fn exit_leaf_rejects_final_sequence_even_at_the_timeout() {
    let (tx, prevouts) = spend(128, 128, 0xffff_ffff, 3, false);
    assert!(!verifies(&tx, &prevouts));
}

// encoding.md, Collateral script: a timestamp locktime does not satisfy a block-height CLTV operand.
#[test]
fn exit_leaf_rejects_a_timestamp_locktime_for_a_height_timeout() {
    let (tx, prevouts) = spend(128, 500_000_000, 0xffff_fffd, 3, false);
    assert!(!verifies(&tx, &prevouts));
}

// encoding.md, Collateral script: only pk can spend U, even after the locktime has passed.
#[test]
fn exit_leaf_rejects_another_keys_valid_schnorr_signature() {
    let (tx, prevouts) = spend(128, 129, 0xffff_fffd, 7, false);
    assert!(!verifies(&tx, &prevouts));
    assert!(verifies(
        &spend(128, 129, 0xffff_fffd, 3, false).0,
        &prevouts
    ));
}

// exit.md, Transaction format: SIGHASH_DEFAULT commits to the output amount.
#[test]
fn an_exit_signature_valid_for_another_output_value_is_rejected() {
    let (mut tx, prevouts) = spend(128, 128, 0xffff_fffd, 3, false);
    assert!(verifies(&tx, &prevouts));
    tx.output[0].value = Amount::from_sat(98_999);
    assert!(!verifies(&tx, &prevouts));
}

// exit.md, Transaction format: SIGHASH_DEFAULT commits to the destination script.
#[test]
fn an_exit_signature_valid_for_another_destination_is_rejected() {
    let (mut tx, prevouts) = spend(128, 128, 0xffff_fffd, 3, false);
    assert!(verifies(&tx, &prevouts));
    tx.output[0].script_pubkey = ScriptBuf::from_bytes(vec![0x00]);
    assert!(!verifies(&tx, &prevouts));
}

// exit.md, Transaction format: the script-path signature commits to all prevout values.
#[test]
fn exit_signature_commits_to_the_other_inputs_amount() {
    let (tx, mut prevouts) = spend(128, 128, 0xffff_fffd, 3, true);
    assert!(verifies(&tx, &prevouts));
    prevouts[1].value = Amount::from_sat(5_001);
    assert!(!verifies(&tx, &prevouts));
}

// exit.md, Transaction format: the script-path signature commits to all prevout scriptPubKeys.
#[test]
fn exit_signature_commits_to_the_other_inputs_script() {
    let (tx, mut prevouts) = spend(128, 128, 0xffff_fffd, 3, true);
    assert!(verifies(&tx, &prevouts));
    prevouts[1].script_pubkey = ScriptBuf::from_bytes(vec![0x00]);
    assert!(!verifies(&tx, &prevouts));
}

mod epoch_timeout_edges {
    use super::super::JoinContext;

    fn context() -> JoinContext {
        JoinContext {
            named: vec![],
            epoch: 7,
            h_0: Some(1000),
            epoch_length: 10,
            min_timeout: 2,
            max_timeout: 5,
        }
    }

    // join.md, Verifying 2.6: epoch 12 begins at 1111, not at its preceding cutoff 1110.
    #[test]
    fn timeout_lower_epoch_accepts_its_first_and_last_blocks() {
        let c = context();
        assert!(c.check_timeout(1110, 1070).is_err());
        assert!(c.check_timeout(1111, 1070).is_ok());
        assert!(c.check_timeout(1120, 1070).is_ok());
    }

    // join.md, Verifying 2.6: epoch 15 includes height 1150 and excludes height 1151.
    #[test]
    fn timeout_upper_epoch_includes_its_cutoff_only() {
        let c = context();
        assert!(c.check_timeout(1141, 1070).is_ok());
        assert!(c.check_timeout(1150, 1070).is_ok());
        assert!(c.check_timeout(1151, 1070).is_err());
    }

    // join.md, Verifying 2.6: E_T uses the ceiling of (T-H_0)/K.
    #[test]
    fn timeout_epoch_uses_a_ceiling_at_every_boundary() {
        let c = context();
        for (t, e) in [
            (1000, 0),
            (1001, 1),
            (1010, 1),
            (1011, 2),
            (1110, 11),
            (1111, 12),
            (1120, 12),
        ] {
            assert_eq!(c.timeout_epoch(t, 1070), e);
        }
    }

    // join.md, Verifying 2.6: before activation the inclusion height replaces H_0.
    #[test]
    fn bootstrap_timeout_origin_moves_with_the_inclusion_block() {
        let c = JoinContext {
            epoch: 0,
            h_0: None,
            min_timeout: 1,
            max_timeout: 1,
            ..context()
        };
        assert!(c.check_timeout(140, 100).is_ok());
        assert!(c.check_timeout(140, 109).is_ok());
        assert!(c.check_timeout(140, 110).is_err());
        assert!(c.check_timeout(141, 100).is_err());
        assert!(c.check_timeout(141, 101).is_ok());
    }

    // join.md, Verifying 2.6: after activation inclusion height never replaces the recorded H_0.
    #[test]
    fn active_timeout_origin_is_stable_across_inclusion_heights() {
        let c = context();
        for block in [1070, 1071, 1079] {
            assert_eq!(c.timeout_epoch(1111, block), 12);
            assert!(c.check_timeout(1111, block).is_ok());
            assert!(c.check_timeout(1110, block).is_err());
        }
    }

    // join.md, Verifying 2.6: block_height+1<T remains strict even for an allowed timeout epoch.
    #[test]
    fn one_block_epoch_still_requires_two_blocks_before_timeout() {
        let c = JoinContext {
            epoch: 0,
            h_0: Some(100),
            epoch_length: 1,
            min_timeout: 1,
            max_timeout: 1,
            named: vec![],
        };
        assert!(c.check_timeout(104, 102).is_ok());
        assert!(c.check_timeout(104, 103).is_err());
        assert!(c.check_timeout(104, 104).is_err());
    }

    // join.md, Verifying 2.5: a timestamp cannot be used as a timeout even inside the permitted epoch range.
    #[test]
    fn timeout_height_limit_is_independent_of_its_epoch() {
        let c = JoinContext {
            epoch: 0,
            h_0: Some(499_999_960),
            min_timeout: 1,
            max_timeout: 2,
            ..context()
        };
        assert!(c.check_timeout(499_999_999, 499_999_961).is_ok());
        assert!(c.check_timeout(500_000_000, 499_999_961).is_err());
    }

    // join.md, Verifying 2.6: u32 timeout bounds are widened before adding e+3.
    #[test]
    fn timeout_bound_addition_does_not_wrap_u32() {
        let c = JoinContext {
            epoch: 1,
            h_0: Some(100),
            min_timeout: u32::MAX,
            max_timeout: u32::MAX,
            ..context()
        };
        assert!(c.check_timeout(150, 110).is_err());
    }
}
