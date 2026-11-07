use super::*;
use bitcoin::secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};

// Independent transcript construction: no production serializer or tagged-hash helper is used.
fn leb128(mut value: u64, bytes: &mut Vec<u8>) {
    loop {
        let next = (value & 127) as u8;
        value >>= 7;
        bytes.push(next | if value == 0 { 0 } else { 128 });
        if value == 0 {
            break;
        }
    }
}

fn reference_digest(tag: &[u8], bytes: &[u8]) -> Message {
    let tag_hash = Sha256::digest(tag);
    let mut digest = Sha256::new();
    digest.update(tag_hash);
    digest.update(tag_hash);
    digest.update(bytes);
    Message::from_digest(digest.finalize().into())
}

fn secret(n: u8) -> SecretKey {
    let mut bytes = [0; 32];
    bytes[31] = n;
    SecretKey::from_slice(&bytes).unwrap()
}

struct JoinTranscript {
    join: IpcJoinSubnetMsg,
    eots: SecretKey,
    validator: SecretKey,
    timeout: u32,
    commitment: EotsNonceCommitment,
}

impl JoinTranscript {
    fn new() -> Self {
        // The signed transcript holds SubnetId's string form, which depends on this process-global.
        crate::eth_utils::set_fvm_network();
        let secp = Secp256k1::new();
        let validator = secret(3);
        let eots = secret(6);
        let pubkey = validator.x_only_public_key(&secp).0;
        Self {
            join: IpcJoinSubnetMsg {
                subnet_id: SubnetId::from_txid20(&std::array::from_fn(|i| i as u8)),
                collateral: Amount::from_sat(200_000),
                ip: "192.0.2.7:16384".parse().unwrap(),
                backup_address: bitcoin::Address::p2tr(&secp, pubkey, None, crate::NETWORK)
                    .into_unchecked(),
                pubkey,
            },
            validator,
            eots,
            timeout: 16384,
            commitment: EotsNonceCommitment {
                start: 129,
                count: 16384,
                root: std::array::from_fn(|i| (31 - i) as u8),
            },
        }
    }

    fn prefix(&self) -> Vec<u8> {
        let mut bytes = b"IPCJUE".to_vec();
        let subnet = self.join.subnet_id.to_string();
        leb128(subnet.len() as u64, &mut bytes);
        bytes.extend_from_slice(subnet.as_bytes());
        match self.join.ip.ip() {
            std::net::IpAddr::V4(ip) => {
                bytes.push(0);
                bytes.extend(ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                bytes.push(1);
                bytes.extend(ip.octets());
            }
        }
        leb128(self.join.ip.port() as u64, &mut bytes);
        let backup = self
            .join
            .backup_address
            .clone()
            .assume_checked()
            .to_string();
        leb128(backup.len() as u64, &mut bytes);
        bytes.extend_from_slice(backup.as_bytes());
        bytes.extend(self.join.pubkey.serialize());
        leb128(self.timeout as u64, &mut bytes);
        bytes.extend(self.eots.x_only_public_key(&Secp256k1::new()).0.serialize());
        leb128(self.commitment.start, &mut bytes);
        leb128(self.commitment.count as u64, &mut bytes);
        bytes.extend(self.commitment.root);
        bytes
    }

    fn nonce_signature(&self, tag: &[u8], prefix: &[u8], signer: &SecretKey) -> [u8; 64] {
        let secp = Secp256k1::new();
        secp.sign_schnorr_no_aux_rand(
            &reference_digest(tag, prefix),
            &Keypair::from_secret_key(&secp, signer),
        )
        .serialize()
    }

    fn finish(&self, nonce_sig: [u8; 64], include_nonce_in_digest: bool) -> Vec<u8> {
        let secp = Secp256k1::new();
        let mut bytes = self.prefix();
        let mut signed = bytes.clone();
        if include_nonce_in_digest {
            signed.extend(nonce_sig);
        }
        bytes.extend(nonce_sig);
        bytes.extend(
            secp.sign_schnorr_no_aux_rand(
                &reference_digest(b"IPCJUE/signature", &signed),
                &Keypair::from_secret_key(&secp, &self.validator),
            )
            .serialize(),
        );
        bytes
    }

    fn encoded(&self) -> Vec<u8> {
        let sig = self.nonce_signature(b"IPCJUE/nonce_commitment", &self.prefix(), &self.eots);
        self.finish(sig, true)
    }

    fn production_message(&self) -> IpcJoinUeMsg {
        IpcJoinUeMsg::new_signed(
            &Secp256k1::new(),
            self.join.clone(),
            self.timeout,
            &self.validator,
            &self.eots,
            self.commitment,
        )
    }
}

fn accepted(bytes: &[u8]) -> bool {
    IpcJoinUeMsg::from_bytes(bytes)
        .map(|m| m.validate().is_ok())
        .unwrap_or(false)
}

// join.md, Transaction format: the nonce proof signs the tag and every preceding encoded field.
#[test]
fn join_signer_uses_the_independently_encoded_nonce_transcript() {
    let f = JoinTranscript::new();
    let msg = f.production_message();
    let bytes = msg.to_bytes().unwrap();
    let prefix = f.prefix();
    assert_eq!(&bytes[..prefix.len()], prefix);
    let sig =
        bitcoin::secp256k1::schnorr::Signature::from_slice(&bytes[prefix.len()..prefix.len() + 64])
            .unwrap();
    Secp256k1::new()
        .verify_schnorr(
            &sig,
            &reference_digest(b"IPCJUE/nonce_commitment", &prefix),
            &msg.pk_eots,
        )
        .unwrap();
}

// join.md, Transaction format: the validator's final signature includes the nonce proof's 64 bytes.
#[test]
fn join_signer_binds_the_nonce_signature_into_the_validator_transcript() {
    let f = JoinTranscript::new();
    let msg = f.production_message();
    let bytes = msg.to_bytes().unwrap();
    let signed = &bytes[..bytes.len() - 64];
    Secp256k1::new()
        .verify_schnorr(
            &msg.signature,
            &reference_digest(b"IPCJUE/signature", signed),
            &f.join.pubkey,
        )
        .unwrap();
}

// join.md, Transaction format and encoding.md, Postcard encoding: IPv6 octets and varint port are signed verbatim.
#[test]
fn join_ipv6_signing_transcript_uses_sixteen_network_order_octets() {
    let mut f = JoinTranscript::new();
    f.join.ip = "[2001:db8:102:304:506:708:90a:b0c]:65535".parse().unwrap();
    let bytes = f.production_message().to_bytes().unwrap();
    let prefix = f.prefix();
    assert_eq!(&bytes[..prefix.len()], prefix);
    assert!(accepted(&f.encoded()));
}

// join.md, Verifying the transaction 2.2: an independently signed commitment is accepted.
#[test]
fn independently_signed_join_commitment_is_accepted() {
    assert!(accepted(&JoinTranscript::new().encoded()));
}

// join.md, Verifying the transaction 2.2: a valid outer signature cannot authorize another EOTS signer's proof.
#[test]
fn validator_cannot_vouch_for_a_nonce_proof_signed_by_a_different_key() {
    let f = JoinTranscript::new();
    let sig = f.nonce_signature(b"IPCJUE/nonce_commitment", &f.prefix(), &secret(7));
    assert!(!accepted(&f.finish(sig, true)));
}

// join.md, Transaction format: the two signature domains are distinct even with a valid outer signature.
#[test]
fn nonce_proof_cannot_use_the_validator_signature_domain() {
    let f = JoinTranscript::new();
    let sig = f.nonce_signature(b"IPCJUE/signature", &f.prefix(), &f.eots);
    assert!(!accepted(&f.finish(sig, true)));
}

// join.md, Transaction format: the IPCJUE tag is included in the nonce proof's message.
#[test]
fn nonce_proof_cannot_omit_the_six_byte_message_tag() {
    let f = JoinTranscript::new();
    let sig = f.nonce_signature(b"IPCJUE/nonce_commitment", &f.prefix()[6..], &f.eots);
    assert!(!accepted(&f.finish(sig, true)));
}

// join.md, Transaction format: a validator signature over only the pre-proof fields is insufficient.
#[test]
fn validator_signature_cannot_omit_the_nonce_signature() {
    let f = JoinTranscript::new();
    let sig = f.nonce_signature(b"IPCJUE/nonce_commitment", &f.prefix(), &f.eots);
    assert!(!accepted(&f.finish(sig, false)));
}

// join.md, Verifying the transaction 2.2: both correct signatures still require distinct keys.
#[test]
fn correct_signatures_do_not_allow_one_key_for_both_roles() {
    let mut f = JoinTranscript::new();
    f.eots = f.validator;
    assert!(!accepted(&f.encoded()));
}

// join.md, Verifying the transaction 2.2: a correctly signed zero start is invalid.
#[test]
fn signed_nonce_commitment_cannot_start_at_genesis_height_zero() {
    let mut f = JoinTranscript::new();
    f.commitment.start = 0;
    assert!(!accepted(&f.encoded()));
}

// join.md, Verifying the transaction 2.2: a correctly signed empty nonce range is invalid.
#[test]
fn signed_nonce_commitment_cannot_have_zero_count() {
    let mut f = JoinTranscript::new();
    f.commitment.count = 0;
    assert!(!accepted(&f.encoded()));
}

// slashing.md, EOTS key and EOTS nonce commitments: the monitor does not require ranges to start at height 1.
#[test]
fn join_accepts_a_non_genesis_singleton_nonce_range() {
    let mut f = JoinTranscript::new();
    f.commitment.start = 1_u64 << 32;
    f.commitment.count = 1;
    assert!(accepted(&f.encoded()));
}

// join.md, Transaction format: collateral is read from output 1 and is not a signed payload field.
#[test]
fn collateral_value_does_not_change_the_signed_join_payload() {
    let mut msg = JoinTranscript::new().production_message();
    let bytes = msg.to_bytes().unwrap();
    msg.join.collateral = Amount::from_sat(9_876_543);
    assert_eq!(msg.to_bytes().unwrap(), bytes);
}

// batch-transfer.md, Transaction format: an IPCTFR payload carries a 128-signature entry after its four other collections.
#[test]
fn batch_wire_decoder_keeps_a_multi_byte_slash_signature_count_in_its_field() {
    let mut bytes = b"IPCTFR".to_vec();
    bytes.extend([0, 0, 0, 0, 1]); // four empty collections; one slash entry
    bytes.extend([0x12; 32]); // internal-order txid
    bytes.extend([0, 0x80, 0x01]); // vout zero; 128 signatures
    for index in 0..128_u8 {
        bytes.push(index);
        bytes.extend([index; 32]);
        bytes.extend([255 - index; 32]);
    }
    let parsed = IpcBatchTransferMsg::witness_to_wire_data(&bytes).unwrap();
    assert!(parsed.transfers.is_empty());
    assert!(parsed.token_registrations.is_empty());
    assert!(parsed.token_supply_adjustments.is_empty());
    assert!(parsed.token_transfers.is_empty());
    assert_eq!(parsed.slash_signatures.len(), 1);
    let entry = &parsed.slash_signatures[0];
    assert_eq!(entry.txid, [0x12; 32]);
    assert_eq!(entry.vout, 0);
    assert_eq!(entry.signatures.len(), 128);
    for (index, (actual_index, signature)) in entry.signatures.iter().enumerate() {
        assert_eq!(*actual_index, index as u16);
        assert_eq!(*signature, [[index as u8; 32], [255 - index as u8; 32]]);
    }
}

mod epoch_parameters {
    use super::super::*;

    fn params() -> IpcCreateSubnetMsg {
        IpcCreateSubnetMsg {
            min_validator_stake: Amount::from_sat(1000),
            power_unit_exponent: 3,
            min_validators: 2,
            bottomup_check_period: 10,
            active_validators_limit: 20,
            min_cross_msg_fee: Amount::from_sat(50),
            whitelist: [
                "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
                "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
            ]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect(),
            epoch_length: 144,
            min_timeout: 1,
            max_timeout: 10,
            min_deposit: Amount::from_sat(330),
            kill_vote_period: 36,
            kill_delay_period: 5,
        }
    }

    fn literal_create() -> Vec<u8> {
        hex::decode(concat!(
            "495043435254e80703020a143202",
            "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
            "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
            "9001010aca022405"
        ))
        .unwrap()
    }

    // create-subnet.md, Transaction format: the published 86-byte example includes all seven epoch parameters.
    #[test]
    fn epoch_create_matches_the_published_literal() {
        let expected = literal_create();
        assert_eq!(expected.len(), 86);
        assert_eq!(params().to_bytes().unwrap(), expected);
        let decoded = IpcCreateSubnetMsg::from_bytes(&expected).unwrap();
        assert_eq!(decoded.power_unit_exponent, 3);
        assert_eq!(decoded.epoch_length, 144);
        assert_eq!((decoded.min_timeout, decoded.max_timeout), (1, 10));
        assert_eq!(decoded.min_deposit.to_sat(), 330);
        assert_eq!(
            (decoded.kill_vote_period, decoded.kill_delay_period),
            (36, 5)
        );
    }

    // encoding.md, Postcard encoding: every prefix missing any epoch field is invalid.
    #[test]
    fn epoch_create_rejects_every_truncated_prefix() {
        let bytes = literal_create();
        for end in 0..bytes.len() {
            assert!(
                IpcCreateSubnetMsg::from_bytes(&bytes[..end]).is_err(),
                "prefix {end}"
            );
        }
    }

    // encoding.md, Postcard encoding: bytes after the final kill-delay field are invalid.
    #[test]
    fn epoch_create_rejects_trailing_bytes() {
        for suffix in [vec![0], vec![1], vec![0x80, 1]] {
            let mut bytes = literal_create();
            bytes.extend(suffix);
            assert!(IpcCreateSubnetMsg::from_bytes(&bytes).is_err());
        }
    }

    // encoding.md, Postcard encoding: an overlong LEB128 in each new integer field is invalid.
    #[test]
    fn epoch_create_rejects_nonminimal_epoch_varints() {
        // Offsets follow the literal example above, independently of the encoder.
        for (start, end) in [(78, 80), (80, 81), (81, 82), (82, 84), (84, 85), (85, 86)] {
            let mut bytes = literal_create();
            bytes[end - 1] |= 0x80;
            bytes.insert(end, 0);
            assert!(
                IpcCreateSubnetMsg::from_bytes(&bytes).is_err(),
                "field at {start}"
            );
        }
    }

    // create-subnet.md, Transaction format: min/max timeout remain u32 and K remains u64.
    #[test]
    fn epoch_create_wire_preserves_wide_integer_fields() {
        let mut p = params();
        p.epoch_length = u64::MAX;
        p.min_timeout = 128;
        p.max_timeout = u32::MAX;
        p.min_deposit = Amount::from_sat(u64::MAX);
        p.kill_vote_period = u64::MAX;
        p.kill_delay_period = 128;
        let mut expected = literal_create()[..78].to_vec();
        expected.extend_from_slice(&[
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 0x80, 1, 0xff, 0xff, 0xff,
            0xff, 0x0f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 0x80, 1,
        ]);
        assert_eq!(p.to_bytes().unwrap(), expected);
        let d = IpcCreateSubnetMsg::from_bytes(&expected).unwrap();
        assert_eq!(d.epoch_length, u64::MAX);
        assert_eq!(d.max_timeout, u32::MAX);
        assert_eq!(d.min_deposit.to_sat(), u64::MAX);
        assert_eq!(d.kill_vote_period, u64::MAX);
    }

    // lifecycle.md, Validity of the parameters: p=15 is allowed, p=16 is rejected.
    #[test]
    fn power_exponent_upper_edge_is_fifteen() {
        let mut p = params();
        p.min_validator_stake = Amount::from_sat(1_000_000_000_000_000);
        p.power_unit_exponent = 15;
        assert!(p.validate().is_ok());
        p.power_unit_exponent = 16;
        assert!(p.validate().is_err());
    }

    // lifecycle.md, Validity of the parameters: minimum stake equals at least one power unit.
    #[test]
    fn minimum_stake_is_checked_against_the_selected_power_unit() {
        let mut p = params();
        assert!(p.validate().is_ok());
        p.min_validator_stake = Amount::from_sat(999);
        assert!(p.validate().is_err());
        p.power_unit_exponent = 0;
        p.min_validator_stake = Amount::from_sat(1);
        assert!(p.validate().is_ok());
    }

    // lifecycle.md, Validity of the parameters: K=21845 reaches 3K=65535 exactly.
    #[test]
    fn epoch_length_accepts_the_exact_relative_timelock_limit() {
        let mut p = params();
        p.epoch_length = 21_845;
        assert!(p.validate().is_ok());
    }

    // lifecycle.md, Validity of the parameters: K=21846 exceeds the relative timelock limit.
    #[test]
    fn epoch_length_rejects_one_above_the_relative_timelock_limit() {
        let mut p = params();
        p.epoch_length = 21_846;
        assert!(p.validate().is_err());
    }

    // lifecycle.md, Validity of the parameters: 3K must not wrap when checking a u64 K.
    #[test]
    fn epoch_length_rejects_large_values_without_wrapping() {
        for k in [u64::MAX, u64::MAX / 3 + 1, 1 << 63] {
            let mut p = params();
            p.epoch_length = k;
            assert!(p.validate().is_err(), "K={k}");
        }
    }

    // lifecycle.md, Validity of the parameters: an epoch has positive length.
    #[test]
    fn epoch_length_zero_is_rejected() {
        let mut p = params();
        p.epoch_length = 0;
        assert!(p.validate().is_err());
    }

    // lifecycle.md, Validity of the parameters: minimum timeout is at least one epoch.
    #[test]
    fn minimum_timeout_zero_is_rejected() {
        let mut p = params();
        p.min_timeout = 0;
        assert!(p.validate().is_err());
    }

    // lifecycle.md, Validity of the parameters: equal timeout bounds are valid.
    #[test]
    fn timeout_range_can_be_a_single_epoch() {
        let mut p = params();
        p.min_timeout = 7;
        p.max_timeout = 7;
        assert!(p.validate().is_ok());
        p.max_timeout = 6;
        assert!(p.validate().is_err());
    }

    // lifecycle.md, Validity of the parameters: the minimum deposit includes the 330-sat dust edge.
    #[test]
    fn minimum_deposit_has_an_inclusive_dust_floor() {
        let mut p = params();
        assert!(p.validate().is_ok());
        for sats in [0, 329] {
            p.min_deposit = Amount::from_sat(sats);
            assert!(p.validate().is_err());
        }
    }

    // lifecycle.md, Validity of the parameters: kill_vote_period must be positive even before the kill procedure exists.
    #[test]
    fn create_rejects_zero_kill_vote_period() {
        let mut p = params();
        p.kill_vote_period = 0;
        assert!(p.validate().is_err());
    }

    // lifecycle.md, Validity of the parameters: no positive lower bound is imposed on kill_delay_period.
    #[test]
    fn create_accepts_zero_kill_delay_period() {
        let mut p = params();
        p.kill_delay_period = 0;
        assert!(p.validate().is_ok());
    }

    // lifecycle.md, Validity of the parameters: 90 genesis validators fit, 91 do not.
    #[test]
    fn first_checkpoint_capacity_limits_bootstrap_to_ninety() {
        let mut p = params();
        let secp = bitcoin::secp256k1::Secp256k1::new();
        p.whitelist = (1u16..=91)
            .map(|n| {
                let mut bytes = [0; 32];
                bytes[30..].copy_from_slice(&n.to_be_bytes());
                bitcoin::secp256k1::SecretKey::from_slice(&bytes)
                    .unwrap()
                    .x_only_public_key(&secp)
                    .0
            })
            .collect();
        p.active_validators_limit = 91;
        p.min_validators = 90;
        assert!(p.validate().is_ok());
        p.min_validators = 91;
        assert!(p.validate().is_err());
    }

    // lifecycle.md, Power and thresholds: power uses power_unit, not min_validator_stake.
    #[test]
    fn collateral_power_keeps_the_remainder_below_a_power_unit() {
        let mut p = params();
        p.min_validator_stake = Amount::from_sat(25_000);
        for (sats, expected) in [(25_000, 25), (25_999, 25), (26_000, 26)] {
            assert_eq!(p.power_of(&Amount::from_sat(sats)).unwrap(), expected);
        }
    }

    // lifecycle.md, Epochs: each cutoff belongs to the epoch that ends there.
    #[test]
    fn epochs_are_open_on_the_left_and_closed_on_the_right() {
        let mut p = params();
        p.epoch_length = 10;
        for (height, epoch) in [
            (999, 0),
            (1000, 0),
            (1001, 1),
            (1009, 1),
            (1010, 1),
            (1011, 2),
            (1020, 2),
            (1021, 3),
        ] {
            assert_eq!(p.epoch_of(1000, height), epoch, "height {height}");
        }
    }

    // lifecycle.md, Epochs: H_e=H_0+eK without truncation at u32::MAX.
    #[test]
    fn cutoff_heights_keep_the_full_bitcoin_height_width() {
        let mut p = params();
        p.epoch_length = 10;
        assert_eq!(p.cutoff_height(4_294_967_290, 0), 4_294_967_290);
        assert_eq!(p.cutoff_height(4_294_967_290, 1), 4_294_967_300);
        assert_eq!(p.cutoff_height(4_294_967_290, 3), 4_294_967_320);
    }

    fn distinct_keys(count: u16) -> Vec<XOnlyPublicKey> {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        (1..=count)
            .map(|n| {
                let mut bytes = [0; 32];
                bytes[30..].copy_from_slice(&n.to_be_bytes());
                bitcoin::secp256k1::SecretKey::from_slice(&bytes)
                    .unwrap()
                    .x_only_public_key(&secp)
                    .0
            })
            .collect()
    }

    // lifecycle.md, Validity of the parameters: the TRUC size bound includes 350 whitelist keys but excludes 351.
    #[test]
    fn whitelist_size_has_an_inclusive_three_hundred_fifty_key_limit() {
        let mut p = params();
        p.whitelist = distinct_keys(350);
        assert!(p.validate().is_ok());
        p.whitelist = distinct_keys(351);
        assert!(p.validate().is_err());
    }

    // lifecycle.md, Validity of the parameters: active_validators_limit has the same 350-member size bound.
    #[test]
    fn configured_validator_limit_may_reach_three_hundred_fifty() {
        let mut p = params();
        p.active_validators_limit = 350;
        assert!(p.validate().is_ok());
        p.active_validators_limit = 351;
        assert!(p.validate().is_err());
    }

    // create-subnet.md, Transaction format / encoding.md, Postcard encoding: timeout fields cannot decode 2^32.
    #[test]
    fn epoch_create_rejects_a_timeout_varint_wider_than_u32() {
        for offset in [80, 81] {
            let mut bytes = literal_create();
            bytes.splice(offset..offset + 1, [0x80, 0x80, 0x80, 0x80, 0x10]);
            assert!(IpcCreateSubnetMsg::from_bytes(&bytes).is_err());
        }
    }

    // create-subnet.md, Transaction format / encoding.md, Postcard encoding: epoch length cannot decode 2^64.
    #[test]
    fn epoch_create_rejects_an_epoch_length_varint_wider_than_u64() {
        let mut bytes = literal_create();
        bytes.splice(
            78..80,
            [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 2],
        );
        assert!(IpcCreateSubnetMsg::from_bytes(&bytes).is_err());
    }
}
