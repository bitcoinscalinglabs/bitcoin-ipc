use super::EotsNonceCommitment;

fn commitment(start: u64, count: u32) -> EotsNonceCommitment {
    EotsNonceCommitment {
        start,
        count,
        root: std::array::from_fn(|i| i as u8),
    }
}

// encoding.md, EOTS nonce commitment: the first height belongs to the half-open range.
#[test]
fn commitment_includes_its_start() {
    assert!(commitment(129, 17).covers(129));
}

// encoding.md, EOTS nonce commitment: the last committed height is start + count - 1.
#[test]
fn commitment_includes_its_last_height() {
    assert!(commitment(129, 17).covers(145));
}

// encoding.md, EOTS nonce commitment: start + count is excluded.
#[test]
fn commitment_excludes_its_end() {
    assert!(!commitment(129, 17).covers(146));
    assert_eq!(commitment(129, 17).end(), 146);
}

// encoding.md, EOTS nonce commitment: no height below start belongs to the range.
#[test]
fn commitment_excludes_heights_before_start_without_underflow() {
    let c = commitment(129, 17);
    assert!(!c.covers(128));
    assert!(!c.covers(0));
}

// encoding.md, EOTS nonce commitment: count = 1 covers exactly one height.
#[test]
fn singleton_commitment_has_no_adjacent_height() {
    let c = commitment(1, 1);
    assert!(c.covers(1));
    assert!(!c.covers(0));
    assert!(!c.covers(2));
}

// encoding.md, EOTS nonce commitment: u32 counts do not narrow the subnet height to u32.
#[test]
fn maximum_count_extends_past_the_u32_height_boundary() {
    let c = commitment(2, u32::MAX);
    assert!(c.covers(1_u64 << 32));
    assert!(!c.covers((1_u64 << 32) + 1));
    assert_eq!(c.end(), (1_u64 << 32) + 1);
}

// encoding.md, EOTS nonce commitment: subtraction and endpoint comparisons use u64 heights.
#[test]
fn commitment_can_end_at_the_largest_representable_height() {
    let c = commitment(u64::MAX - 1, 1);
    assert!(!c.covers(u64::MAX - 2));
    assert!(c.covers(u64::MAX - 1));
    assert!(!c.covers(u64::MAX));
    assert_eq!(c.end(), u64::MAX);
}

// slashing.md, EOTS key and EOTS nonce commitments: gaps are missing coverage, not implicit nonces.
#[test]
fn separate_commitments_do_not_fill_the_gap_between_them() {
    let ranges = [commitment(1, 3), commitment(5, 2)];
    assert!(!ranges.iter().any(|c| c.covers(4)));
    assert_eq!(ranges.iter().filter(|c| c.covers(5)).count(), 1);
}

// encoding.md, EOTS nonce commitment and Postcard encoding: (u64, u32, raw 32 bytes), no list prefix.
#[test]
fn commitment_has_a_literal_leb128_tuple_encoding() {
    let mut expected = vec![0x81, 0x01, 0x80, 0x80, 0x01]; // 129, 16384
    expected.extend(0_u8..32);
    assert_eq!(
        postcard::to_stdvec(&commitment(129, 16384)).unwrap(),
        expected
    );
    let decoded: EotsNonceCommitment = postcard::from_bytes(&expected).unwrap();
    assert_eq!(decoded, commitment(129, 16384));
}

// encoding.md, Postcard encoding: u64 and u32 have different maximum-width unsigned LEB128 forms.
#[test]
fn commitment_integer_widths_are_encoded_without_truncation() {
    // This checks the wire types only; it makes no claim about an overflowing range's validity.
    let mut expected = vec![0xff; 9];
    expected.push(0x01);
    expected.extend([0xff, 0xff, 0xff, 0xff, 0x0f]);
    expected.extend(0_u8..32);
    assert_eq!(
        postcard::to_stdvec(&commitment(u64::MAX, u32::MAX)).unwrap(),
        expected
    );
}

mod seed_commitments {
    use super::super::*;
    use sha2::{Digest, Sha256};

    fn reference_root(leaves: &[[u8; 32]]) -> [u8; 32] {
        if leaves.len() == 1 {
            return Sha256::digest([&[0u8][..], &leaves[0]].concat()).into();
        }
        let split = 1usize << ((usize::BITS - 1) - (leaves.len() - 1).leading_zeros());
        Sha256::digest(
            [
                &[1u8][..],
                &reference_root(&leaves[..split]),
                &reference_root(&leaves[split..]),
            ]
            .concat(),
        )
        .into()
    }

    // slashing.md, EOTS key and EOTS nonce commitments: committed nonces are stable under range repartitioning.
    #[test]
    fn overlapping_commitment_ranges_use_identical_nonces_for_the_same_heights() {
        let seed = EotsSeed::from_bytes([0x91; 32]);
        let subnet = SubnetId::from_txid20(&[0x32; 20]);
        let all = public_nonces(&seed, &subnet, 125, 9);
        let mut partitioned = public_nonces(&seed, &subnet, 125, 3);
        partitioned.extend(public_nonces(&seed, &subnet, 128, 6));
        assert_eq!(partitioned, all);
        assert_eq!(public_nonces(&seed, &subnet, 127, 4), all[2..6]);
    }

    // slashing.md, EOTS key and EOTS nonce commitments: restoring only the saved seed reconstructs every commitment proof.
    #[test]
    fn a_restored_seed_rebuilds_an_uneven_tree_with_the_rfc6962_root() {
        let subnet = SubnetId::from_txid20(&[0x53; 20]);
        let seed = EotsSeed::from_bytes([0xa3; 32]);
        let leaves = public_nonces(&seed, &subnet, 0x0102, 11);
        let (commitment, original) = commit_nonces(&seed, &subnet, 0x0102, 11);
        assert_eq!(commitment.root, reference_root(&leaves));
        let restored = EotsSeed::from_bytes(*seed.as_bytes());
        let rebuilt = nonce_tree(&restored, &subnet, &commitment);
        assert_eq!(rebuilt.root(), Some(reference_root(&leaves)));
        for i in 0..11 {
            assert_eq!(rebuilt.audit_path(i), original.audit_path(i));
        }
    }

    // slashing.md, EOTS key and EOTS nonce commitments: changing the seed changes the committed nonces even with the same EOTS key.
    #[test]
    fn changing_only_the_nonce_seed_invalidates_the_original_commitment() {
        let subnet = SubnetId::from_txid20(&[0x53; 20]);
        let seed = EotsSeed::from_bytes([0xa3; 32]);
        let other = EotsSeed::from_bytes([0xa4; 32]);
        let (commitment, tree) = commit_nonces(&seed, &subnet, 17, 5);
        let sk = SecretKey::from_slice(&[7; 32]).unwrap();
        let vote = sign_vote(&sk, &other, &subnet, &commitment, &tree, 19, &[0x29; 32]).unwrap();
        assert_ne!(vote.signature[0], public_nonces(&seed, &subnet, 19, 1)[0]);
        assert!(verify_vote(&subnet, &vote, &commitment).is_err());
    }

    // slashing.md, The vote / BIP340: a fixed seed and height give one R across keys, with independently valid signatures.
    #[test]
    fn one_seed_nonce_signs_under_distinct_keys_with_independent_bip340_verification() {
        let subnet = SubnetId::from_txid20(&[0x42; 20]);
        let height = 0x0102_0304_0506_0708;
        let block = [0x87; 32];
        let seed = EotsSeed::from_bytes([0x73; 32]);
        let (commitment, tree) = commit_nonces(&seed, &subnet, height, 3);
        let mut payload = vec![0x42; 20];
        payload.extend(height.to_le_bytes());
        payload.extend(block);
        let tag = Sha256::digest(b"IPC/vote");
        let digest: [u8; 32] = Sha256::digest([&tag[..], &tag[..], &payload].concat()).into();
        let secp = Secp256k1::new();
        let mut nonces = vec![];
        for key_bytes in [[7; 32], [8; 32]] {
            let sk = SecretKey::from_slice(&key_bytes).unwrap();
            let vote = sign_vote(&sk, &seed, &subnet, &commitment, &tree, height, &block).unwrap();
            let pk = sk.x_only_public_key(&secp).0;
            let signature = secp256k1::schnorr::Signature::from_slice(
                &[vote.signature[0], vote.signature[1]].concat(),
            )
            .unwrap();
            secp.verify_schnorr(&signature, &Message::from_digest(digest), &pk)
                .unwrap();
            let wrong = SecretKey::from_slice(&[9; 32])
                .unwrap()
                .x_only_public_key(&secp)
                .0;
            assert!(secp
                .verify_schnorr(&signature, &Message::from_digest(digest), &wrong)
                .is_err());
            assert_eq!(vote.proof, tree.audit_path(0).unwrap());
            nonces.push(vote.signature[0]);
        }
        assert_eq!(nonces[0], nonces[1]);
    }
}
