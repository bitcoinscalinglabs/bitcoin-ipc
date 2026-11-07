//! EOTS: BIP340 signatures whose nonces are committed in advance, so that two signatures at
//! one subnet height reveal the secret key. Nonce derivation from the validator's nonce seed,
//! the RFC 6962 commitment tree with its audit paths, the vote message and the vote signature.

use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1::{self, Message, Secp256k1, SecretKey, XOnlyPublicKey};
use hmac::{Hmac, Mac};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::elliptic_curve::PrimeField;
use k256::{ProjectivePoint, Scalar};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ipc_lib::{tagged_sha256, SubnetId};

/// Tag of the vote message digest.
pub const VOTE_TAG: &[u8] = b"IPC/vote";

/// The secret from which a validator derives every EOTS nonce: a random value kept like the
/// EOTS secret key, since one nonce and its vote give the key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct EotsSeed([u8; 32]);

impl EotsSeed {
    pub fn new<R: rand::RngCore + rand::CryptoRng>(rng: &mut R) -> Self {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for EotsSeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EotsSeed(..)")
    }
}

/// A commitment of an EOTS key to one public nonce per subnet height in `[start, start + count)`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct EotsNonceCommitment {
    pub start: u64,
    pub count: u32,
    pub root: [u8; 32],
}

impl EotsNonceCommitment {
    pub fn covers(&self, height: u64) -> bool {
        height >= self.start && height - self.start < u64::from(self.count)
    }

    pub fn end(&self) -> u64 {
        self.start + u64::from(self.count)
    }
}

/// A 64-byte signature as postcard encodes it: 64 raw bytes.
pub type Signature64 = [[u8; 32]; 2];

pub fn signature_to_bytes(sig: &Signature64) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&sig[0]);
    out[32..].copy_from_slice(&sig[1]);
    out
}

pub fn signature_from_bytes(bytes: &[u8; 64]) -> Signature64 {
    let mut sig = [[0u8; 32]; 2];
    sig[0].copy_from_slice(&bytes[..32]);
    sig[1].copy_from_slice(&bytes[32..]);
    sig
}

/// An EOTS vote: the key, the height and block hash voted for, the signature, and the proof
/// of the nonce against the commitment whose range holds the height.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EotsVote {
    pub pk_eots: [u8; 32],
    pub height: u64,
    pub block_hash: [u8; 32],
    pub signature: Signature64,
    pub start: u64,
    pub proof: Vec<[u8; 32]>,
}

#[derive(Debug, Error)]
pub enum EotsError {
    #[error("height {0} is outside the committed range")]
    HeightNotCommitted(u64),
    #[error("invalid EOTS key")]
    InvalidKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("invalid audit path")]
    InvalidProof,
}

/// The message an EOTS vote signs: `tagged_sha256("IPC/vote", txid20 ‖ h ‖ b)`.
pub fn vote_message(subnet_id: &SubnetId, height: u64, block_hash: &[u8; 32]) -> [u8; 32] {
    let mut data = Vec::with_capacity(60);
    data.extend_from_slice(&subnet_id.txid20());
    data.extend_from_slice(&height.to_le_bytes());
    data.extend_from_slice(block_hash);
    tagged_sha256(VOTE_TAG, &data)
}

fn scalar_from_secret(sk: &SecretKey) -> Scalar {
    Scalar::from_repr(sk.secret_bytes().into())
        .expect("a secp256k1 secret key is a non-zero scalar below the order")
}

/// The x coordinate and the y parity (`true` when odd) of a point.
fn point_x_and_parity(point: &ProjectivePoint) -> ([u8; 32], bool) {
    let affine = point.to_affine();
    let encoded = affine.to_encoded_point(false);
    let x: [u8; 32] = encoded
        .x()
        .expect("not the identity")
        .as_slice()
        .try_into()
        .unwrap();
    let odd = encoded.y().expect("not the identity")[31] & 1 == 1;
    (x, odd)
}

/// The secret nonce of `seed` at `height` on `subnet_id`, with its public nonce `R_h`: derived
/// with HMAC-SHA256, adjusted so that `R_h` has an even y coordinate.
pub fn secret_nonce(seed: &EotsSeed, subnet_id: &SubnetId, height: u64) -> (Scalar, [u8; 32]) {
    let mut counter: u8 = 0;
    loop {
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(seed.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(&subnet_id.txid20());
        mac.update(&height.to_le_bytes());
        mac.update(&[counter]);
        let bytes: [u8; 32] = mac.finalize().into_bytes().into();
        if let Some(k) = Option::<Scalar>::from(Scalar::from_repr(bytes.into())) {
            if k != Scalar::ZERO {
                let (x, odd) = point_x_and_parity(&(ProjectivePoint::GENERATOR * k));
                return (if odd { -k } else { k }, x);
            }
        }
        counter = counter
            .checked_add(1)
            .expect("a valid nonce within 256 tries");
    }
}

/// The public nonces `R_start, …, R_{start+count-1}`, derived on every core: a nonce is one HMAC
/// and one point multiplication, and a commitment holds up to millions of them.
pub fn public_nonces(
    seed: &EotsSeed,
    subnet_id: &SubnetId,
    start: u64,
    count: u32,
) -> Vec<[u8; 32]> {
    let count = count as usize;
    if count == 0 {
        return Vec::new();
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = count.div_ceil(threads);
    let mut nonces = vec![[0u8; 32]; count];
    std::thread::scope(|scope| {
        for (c, slot) in nonces.chunks_mut(chunk).enumerate() {
            scope.spawn(move || {
                for (i, nonce) in slot.iter_mut().enumerate() {
                    *nonce = secret_nonce(seed, subnet_id, start + (c * chunk + i) as u64).1;
                }
            });
        }
    });
    nonces
}

/// The tree of `commitment` on `subnet_id`, rebuilt from `seed`.
pub fn nonce_tree(
    seed: &EotsSeed,
    subnet_id: &SubnetId,
    commitment: &EotsNonceCommitment,
) -> NonceTree {
    NonceTree::new(&public_nonces(
        seed,
        subnet_id,
        commitment.start,
        commitment.count,
    ))
}

/// The commitment of `seed` to the heights `[start, start + count)` on `subnet_id`, with its
/// tree.
pub fn commit_nonces(
    seed: &EotsSeed,
    subnet_id: &SubnetId,
    start: u64,
    count: u32,
) -> (EotsNonceCommitment, NonceTree) {
    let tree = NonceTree::new(&public_nonces(seed, subnet_id, start, count));
    let root = tree.root().expect("count >= 1");
    (EotsNonceCommitment { start, count, root }, tree)
}

fn leaf_hash(leaf: &[u8; 32]) -> [u8; 32] {
    let mut engine = sha256::Hash::engine();
    engine.input(&[0u8]);
    engine.input(leaf);
    sha256::Hash::from_engine(engine).to_byte_array()
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut engine = sha256::Hash::engine();
    engine.input(&[1u8]);
    engine.input(left);
    engine.input(right);
    sha256::Hash::from_engine(engine).to_byte_array()
}

/// The largest power of two smaller than `n`, for `n > 1`.
fn split_point(n: usize) -> usize {
    1 << (usize::BITS - 1 - (n - 1).leading_zeros())
}

/// The RFC 6962 Merkle tree of a list of nonces, with the hashes of every perfect aligned
/// subtree kept so that an audit path costs `O(log² n)`.
#[derive(Debug, Clone)]
pub struct NonceTree {
    /// `layers[j][i]` is the hash of leaves `[i · 2^j, (i + 1) · 2^j)`
    layers: Vec<Vec<[u8; 32]>>,
}

impl NonceTree {
    pub fn new(leaves: &[[u8; 32]]) -> Self {
        let mut layers = vec![leaves.iter().map(leaf_hash).collect::<Vec<_>>()];
        while layers.last().map_or(false, |l| l.len() > 1) {
            let prev = layers.last().unwrap();
            let next = prev
                .chunks_exact(2)
                .map(|pair| node_hash(&pair[0], &pair[1]))
                .collect();
            layers.push(next);
        }
        Self { layers }
    }

    pub fn len(&self) -> usize {
        self.layers.first().map_or(0, |l| l.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The hash of leaves `[from, to)`, where `from` is a multiple of the largest power of two
    /// below `to - from`, as every subtree of the RFC 6962 recursion is.
    fn range_hash(&self, from: usize, to: usize) -> [u8; 32] {
        let n = to - from;
        if n == 1 {
            return self.layers[0][from];
        }
        if n.is_power_of_two() {
            let j = n.trailing_zeros() as usize;
            return self.layers[j][from / n];
        }
        let k = split_point(n);
        node_hash(
            &self.range_hash(from, from + k),
            &self.range_hash(from + k, to),
        )
    }

    pub fn root(&self) -> Option<[u8; 32]> {
        (!self.is_empty()).then(|| self.range_hash(0, self.len()))
    }

    /// The audit path of leaf `index`, from the leaf up.
    pub fn audit_path(&self, index: usize) -> Option<Vec<[u8; 32]>> {
        if index >= self.len() {
            return None;
        }
        let mut path = Vec::new();
        self.audit_path_in(0, self.len(), index, &mut path);
        Some(path)
    }

    fn audit_path_in(&self, from: usize, to: usize, index: usize, path: &mut Vec<[u8; 32]>) {
        let n = to - from;
        if n == 1 {
            return;
        }
        let k = split_point(n);
        if index < from + k {
            self.audit_path_in(from, from + k, index, path);
            path.push(self.range_hash(from + k, to));
        } else {
            self.audit_path_in(from + k, to, index, path);
            path.push(self.range_hash(from, from + k));
        }
    }
}

/// The RFC 6962 root of `leaves`.
pub fn merkle_root(leaves: &[[u8; 32]]) -> Option<[u8; 32]> {
    NonceTree::new(leaves).root()
}

/// Checks the RFC 6962 audit path `path` of `leaf` at `index` in a tree of `size` leaves against
/// `root` (RFC 9162, section 2.1.3.2).
pub fn verify_audit_path(
    leaf: &[u8; 32],
    index: u64,
    size: u64,
    path: &[[u8; 32]],
    root: &[u8; 32],
) -> bool {
    if size == 0 || index >= size {
        return false;
    }
    let mut fn_ = index;
    let mut sn = size - 1;
    let mut r = leaf_hash(leaf);
    for p in path {
        if sn == 0 {
            return false;
        }
        if fn_ & 1 == 1 || fn_ == sn {
            r = node_hash(p, &r);
            while fn_ & 1 == 0 && fn_ != 0 {
                fn_ >>= 1;
                sn >>= 1;
            }
        } else {
            r = node_hash(&r, p);
        }
        fn_ >>= 1;
        sn >>= 1;
    }
    sn == 0 && r == *root
}

/// The x-only public key of `sk`.
pub fn public_key(secp: &Secp256k1<secp256k1::All>, sk: &SecretKey) -> XOnlyPublicKey {
    sk.x_only_public_key(secp).0
}

/// The BIP340 signature `R ‖ s` on `msg` under `sk` with the secret nonce `k`, whose public
/// nonce has x coordinate `nonce_x` and an even y coordinate.
pub fn sign_with_nonce(
    sk: &SecretKey,
    k: &Scalar,
    nonce_x: &[u8; 32],
    msg: &[u8; 32],
) -> Signature64 {
    let mut d = scalar_from_secret(sk);
    let (px, odd) = point_x_and_parity(&(ProjectivePoint::GENERATOR * d));
    if odd {
        d = -d;
    }
    let e = challenge(nonce_x, &px, msg);
    let s = *k + e * d;
    [*nonce_x, s.to_bytes().into()]
}

/// The BIP340 challenge `e` of `R`, `P` and `m`, reduced modulo the group order.
fn challenge(rx: &[u8; 32], px: &[u8; 32], msg: &[u8; 32]) -> Scalar {
    let mut data = Vec::with_capacity(96);
    data.extend_from_slice(rx);
    data.extend_from_slice(px);
    data.extend_from_slice(msg);
    let e = tagged_sha256(b"BIP0340/challenge", &data);
    <Scalar as k256::elliptic_curve::ops::Reduce<k256::U256>>::reduce_bytes(&e.into())
}

/// Verifies `signature` on `msg` under `pk_eots` as an ordinary BIP340 signature.
pub fn verify(
    pk_eots: &[u8; 32],
    msg: &[u8; 32],
    signature: &Signature64,
) -> Result<(), EotsError> {
    let pk = XOnlyPublicKey::from_slice(pk_eots).map_err(|_| EotsError::InvalidKey)?;
    let sig = secp256k1::schnorr::Signature::from_slice(&signature_to_bytes(signature))
        .map_err(|_| EotsError::InvalidSignature)?;
    Secp256k1::verification_only()
        .verify_schnorr(&sig, &Message::from_digest(*msg), &pk)
        .map_err(|_| EotsError::InvalidSignature)
}

/// Signs the vote for `(height, block_hash)` on `subnet_id` under `sk` with the nonce of `seed`
/// committed in `commitment`, whose tree is `tree`.
pub fn sign_vote(
    sk: &SecretKey,
    seed: &EotsSeed,
    subnet_id: &SubnetId,
    commitment: &EotsNonceCommitment,
    tree: &NonceTree,
    height: u64,
    block_hash: &[u8; 32],
) -> Result<EotsVote, EotsError> {
    if !commitment.covers(height) || tree.len() != commitment.count as usize {
        return Err(EotsError::HeightNotCommitted(height));
    }
    let index = (height - commitment.start) as usize;
    let (k, nonce_x) = secret_nonce(seed, subnet_id, height);
    let proof = tree
        .audit_path(index)
        .ok_or(EotsError::HeightNotCommitted(height))?;
    let msg = vote_message(subnet_id, height, block_hash);
    let secp = Secp256k1::new();
    Ok(EotsVote {
        pk_eots: public_key(&secp, sk).serialize(),
        height,
        block_hash: *block_hash,
        signature: sign_with_nonce(sk, &k, &nonce_x, &msg),
        start: commitment.start,
        proof,
    })
}

/// Checks a vote's proof against `commitment` and its signature under its key.
pub fn verify_vote(
    subnet_id: &SubnetId,
    vote: &EotsVote,
    commitment: &EotsNonceCommitment,
) -> Result<(), EotsError> {
    if vote.start != commitment.start || !commitment.covers(vote.height) {
        return Err(EotsError::HeightNotCommitted(vote.height));
    }
    let ok = verify_audit_path(
        &vote.signature[0],
        vote.height - commitment.start,
        u64::from(commitment.count),
        &vote.proof,
        &commitment.root,
    );
    if !ok {
        return Err(EotsError::InvalidProof);
    }
    let msg = vote_message(subnet_id, vote.height, &vote.block_hash);
    verify(&vote.pk_eots, &msg, &vote.signature)
}

/// The secret key `d` from two votes of one key at one height for two different blocks:
/// `d = (s − s') · (e − e')⁻¹`. Returns `None` when the votes do not share a nonce or sign the
/// same message.
pub fn extract_secret_key(
    subnet_id: &SubnetId,
    pk_eots: &[u8; 32],
    a: &EotsVote,
    b: &EotsVote,
) -> Option<SecretKey> {
    if a.signature[0] != b.signature[0] || a.height != b.height || a.block_hash == b.block_hash {
        return None;
    }
    let ea = challenge(
        &a.signature[0],
        pk_eots,
        &vote_message(subnet_id, a.height, &a.block_hash),
    );
    let eb = challenge(
        &b.signature[0],
        pk_eots,
        &vote_message(subnet_id, b.height, &b.block_hash),
    );
    let sa = Option::<Scalar>::from(Scalar::from_repr(a.signature[1].into()))?;
    let sb = Option::<Scalar>::from(Scalar::from_repr(b.signature[1].into()))?;
    let inv = Option::<Scalar>::from((ea - eb).invert())?;
    let d = (sa - sb) * inv;
    let bytes: [u8; 32] = d.to_bytes().into();
    let sk = SecretKey::from_slice(&bytes).ok()?;
    // `d` is the key as BIP340 uses it: negated when the public key has an odd y
    let secp = Secp256k1::new();
    if sk.x_only_public_key(&secp).0.serialize() == *pk_eots {
        Some(sk)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn test_subnet() -> SubnetId {
        SubnetId::from_txid20(&[7u8; 20])
    }

    fn recursive_root(leaves: &[[u8; 32]]) -> [u8; 32] {
        if leaves.len() == 1 {
            return leaf_hash(&leaves[0]);
        }
        let k = split_point(leaves.len());
        node_hash(&recursive_root(&leaves[..k]), &recursive_root(&leaves[k..]))
    }

    #[test]
    fn rfc6962_root_of_seven_leaves_matches_the_recursion() {
        let leaves: Vec<[u8; 32]> = (0u8..7).map(|i| [i; 32]).collect();
        assert_eq!(merkle_root(&leaves).unwrap(), recursive_root(&leaves));
    }

    #[test]
    fn single_leaf_root_is_its_leaf_hash() {
        let leaf = [9u8; 32];
        assert_eq!(merkle_root(&[leaf]).unwrap(), leaf_hash(&leaf));
        assert!(verify_audit_path(&leaf, 0, 1, &[], &leaf_hash(&leaf)));
        assert!(!verify_audit_path(
            &leaf,
            0,
            1,
            &[[0u8; 32]],
            &leaf_hash(&leaf)
        ));
    }

    proptest! {
        #[test]
        fn audit_paths_verify_and_reject_tampering(size in 1usize..70, index_seed in 0usize..70, flip in 0usize..70) {
            let index = index_seed % size;
            let leaves: Vec<[u8; 32]> = (0..size).map(|i| [i as u8; 32]).collect();
            let tree = NonceTree::new(&leaves);
            let root = tree.root().unwrap();
            prop_assert_eq!(root, recursive_root(&leaves));
            let path = tree.audit_path(index).unwrap();
            prop_assert!(verify_audit_path(&leaves[index], index as u64, size as u64, &path, &root));
            prop_assert!(!verify_audit_path(&[200u8; 32], index as u64, size as u64, &path, &root));
            if size > 1 {
                let other = (index + 1 + flip % (size - 1)) % size;
                prop_assert!(!verify_audit_path(&leaves[other], index as u64, size as u64, &path, &root));
                let mut short = path.clone();
                short.pop();
                prop_assert!(!verify_audit_path(&leaves[index], index as u64, size as u64, &short, &root));
            }
        }
    }

    #[test]
    fn vote_is_a_valid_bip340_signature_and_the_nonce_is_committed() {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[3u8; 32]).unwrap();
        let seed = EotsSeed::from_bytes([30u8; 32]);
        let subnet = test_subnet();
        let commitment_start = 5;
        let nonces = public_nonces(&seed, &subnet, commitment_start, 20);
        let tree = NonceTree::new(&nonces);
        let commitment = EotsNonceCommitment {
            start: commitment_start,
            count: 20,
            root: tree.root().unwrap(),
        };
        let block = [0xabu8; 32];
        let vote = sign_vote(&sk, &seed, &subnet, &commitment, &tree, 17, &block).unwrap();
        assert_eq!(vote.pk_eots, public_key(&secp, &sk).serialize());
        assert_eq!(vote.signature[0], nonces[12]);
        verify_vote(&subnet, &vote, &commitment).unwrap();
        // The signature verifies as an ordinary BIP340 signature with the bitcoin crate
        let sig = secp256k1::schnorr::Signature::from_slice(&signature_to_bytes(&vote.signature))
            .unwrap();
        let msg = Message::from_digest(vote_message(&subnet, 17, &block));
        secp.verify_schnorr(&sig, &msg, &public_key(&secp, &sk))
            .unwrap();
        // Outside the range, or with a wrong start
        assert!(matches!(
            sign_vote(&sk, &seed, &subnet, &commitment, &tree, 25, &block),
            Err(EotsError::HeightNotCommitted(25))
        ));
        let mut wrong_start = vote.clone();
        wrong_start.start = 4;
        assert!(verify_vote(&subnet, &wrong_start, &commitment).is_err());
        let mut wrong_block = vote.clone();
        wrong_block.block_hash = [1u8; 32];
        assert!(matches!(
            verify_vote(&subnet, &wrong_block, &commitment),
            Err(EotsError::InvalidSignature)
        ));
    }

    #[test]
    fn odd_y_keys_sign_valid_votes() {
        // Keys of both parities must produce verifiable signatures
        let secp = Secp256k1::new();
        let subnet = test_subnet();
        for seed in 1u8..12 {
            let sk = SecretKey::from_slice(&[seed; 32]).unwrap();
            let nonce_seed = EotsSeed::from_bytes([seed; 32]);
            let nonces = public_nonces(&nonce_seed, &subnet, 1, 3);
            let tree = NonceTree::new(&nonces);
            let commitment = EotsNonceCommitment {
                start: 1,
                count: 3,
                root: tree.root().unwrap(),
            };
            let vote = sign_vote(
                &sk,
                &nonce_seed,
                &subnet,
                &commitment,
                &tree,
                2,
                &[seed; 32],
            )
            .unwrap();
            verify_vote(&subnet, &vote, &commitment).unwrap();
            let _ = &secp;
        }
    }

    #[test]
    fn two_votes_at_one_height_reveal_the_key() {
        let secp = Secp256k1::new();
        let subnet = test_subnet();
        for seed in 1u8..6 {
            let sk = SecretKey::from_slice(&[seed; 32]).unwrap();
            let nonce_seed = EotsSeed::from_bytes([seed.wrapping_mul(7); 32]);
            let nonces = public_nonces(&nonce_seed, &subnet, 1, 4);
            let tree = NonceTree::new(&nonces);
            let commitment = EotsNonceCommitment {
                start: 1,
                count: 4,
                root: tree.root().unwrap(),
            };
            let a =
                sign_vote(&sk, &nonce_seed, &subnet, &commitment, &tree, 3, &[1u8; 32]).unwrap();
            let b =
                sign_vote(&sk, &nonce_seed, &subnet, &commitment, &tree, 3, &[2u8; 32]).unwrap();
            let extracted = extract_secret_key(&subnet, &a.pk_eots, &a, &b).unwrap();
            assert_eq!(public_key(&secp, &extracted), public_key(&secp, &sk));
            assert!(extract_secret_key(&subnet, &a.pk_eots, &a, &a).is_none());
        }
    }

    #[test]
    fn public_nonces_have_even_y() {
        let seed = EotsSeed::from_bytes([5u8; 32]);
        for h in 1..50 {
            let (k, x) = secret_nonce(&seed, &test_subnet(), h);
            let (px, odd) = point_x_and_parity(&(ProjectivePoint::GENERATOR * k));
            assert_eq!(px, x);
            assert!(!odd);
        }
    }

    #[test]
    fn nonces_come_from_the_seed_alone() {
        let subnet = test_subnet();
        let seed = EotsSeed::from_bytes([9u8; 32]);
        let again = public_nonces(&seed, &subnet, 1, 300);
        assert_eq!(public_nonces(&seed, &subnet, 1, 300), again);
        // Sequential derivation gives the same nonces as the parallel one, in the same order
        let sequential: Vec<_> = (1..=300).map(|h| secret_nonce(&seed, &subnet, h).1).collect();
        assert_eq!(sequential, again);
        // Another seed, another subnet or another height gives other nonces
        let other = EotsSeed::from_bytes([10u8; 32]);
        assert_ne!(public_nonces(&other, &subnet, 1, 300), again);
        let other_subnet = SubnetId::from_txid20(&[77u8; 20]);
        assert_ne!(public_nonces(&seed, &other_subnet, 1, 300), again);
        assert_ne!(&public_nonces(&seed, &subnet, 2, 300)[..299], &again[..299]);
        assert_eq!(&public_nonces(&seed, &subnet, 2, 299)[..], &again[1..]);
        assert!(public_nonces(&seed, &subnet, 1, 0).is_empty());
        // The commitment and the rebuilt tree agree
        let (commitment, tree) = commit_nonces(&seed, &subnet, 1, 300);
        assert_eq!(nonce_tree(&seed, &subnet, &commitment).root(), tree.root());
    }

    #[test]
    fn vote_message_is_the_tagged_hash_of_txid20_height_and_hash() {
        let subnet = test_subnet();
        let mut data = subnet.txid20().to_vec();
        data.extend_from_slice(&3u64.to_le_bytes());
        data.extend_from_slice(&[4u8; 32]);
        assert_eq!(
            vote_message(&subnet, 3, &[4u8; 32]),
            tagged_sha256(b"IPC/vote", &data)
        );
    }

    #[test]
    fn vote_postcard_encoding_is_the_spec_tuple() {
        let vote = EotsVote {
            pk_eots: [1; 32],
            height: 300,
            block_hash: [2; 32],
            signature: [[3; 32], [4; 32]],
            start: 1,
            proof: vec![[5; 32], [6; 32]],
        };
        let bytes = postcard::to_stdvec(&vote).unwrap();
        let mut expected = vec![1u8; 32];
        expected.extend_from_slice(&[0xac, 0x02]); // 300 as a varint
        expected.extend_from_slice(&[2; 32]);
        expected.extend_from_slice(&[3; 32]);
        expected.extend_from_slice(&[4; 32]);
        expected.push(1);
        expected.push(2);
        expected.extend_from_slice(&[5; 32]);
        expected.extend_from_slice(&[6; 32]);
        assert_eq!(bytes, expected);
        let commitment = EotsNonceCommitment {
            start: 1,
            count: 2,
            root: [7; 32],
        };
        let mut expected = vec![1u8, 2];
        expected.extend_from_slice(&[7; 32]);
        assert_eq!(postcard::to_stdvec(&commitment).unwrap(), expected);
    }
}
#[cfg(test)]
#[path = "eots_spec_tests.rs"]
mod spec_tests;
