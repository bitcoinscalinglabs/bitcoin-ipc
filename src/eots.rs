//! EOTS nonce derivation, the nonce commitment tree with its audit paths, and vote signatures.

use std::collections::BTreeMap;
use std::ops::Range;

use bitcoin::hashes::{sha256, Hash, HashEngine};
#[cfg(test)]
use bitcoin::secp256k1::{self, Message, XOnlyPublicKey};
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use hmac::{Hmac, Mac};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::elliptic_curve::PrimeField;
use k256::{ProjectivePoint, Scalar};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bitcoin_utils::tagged_sha256;
use crate::ipc_lib::SubnetId;

/// Tag of the vote message digest.
pub const VOTE_TAG: &[u8] = b"IPC/vote";

/// The secret from which every EOTS nonce is derived, kept like the EOTS secret key.
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
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EotsNonceCommitment {
    pub start: u64,
    pub count: u32,
    pub root: [u8; 32],
}

impl EotsNonceCommitment {
    pub fn covers(&self, height: u64) -> bool {
        height >= self.start && height - self.start < u64::from(self.count)
    }

    pub fn end(&self) -> Result<u64, EotsError> {
        self.start
            .checked_add(u64::from(self.count))
            .ok_or(EotsError::RangeEndOverflow {
                start: self.start,
                count: self.count,
            })
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
    #[error("the end of the range [{start}, {start} + {count}) does not fit in u64")]
    RangeEndOverflow { start: u64, count: u32 },
    #[error("an EOTS nonce commitment needs count >= 1")]
    ZeroCount,
    #[error("the subtree roots do not match the commitment tree")]
    SubtreeMismatch,
    #[cfg(test)]
    #[error("invalid EOTS key")]
    InvalidKey,
    #[cfg(test)]
    #[error("invalid signature")]
    InvalidSignature,
    #[cfg(test)]
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

/// The public nonces for the heights `[start, start + count)`, derived in parallel.
pub fn public_nonces(
    seed: &EotsSeed,
    subnet_id: &SubnetId,
    start: u64,
    count: u32,
) -> Result<Vec<[u8; 32]>, EotsError> {
    let Some(offset) = count.checked_sub(1) else {
        return Ok(Vec::new());
    };
    let last = start
        .checked_add(u64::from(offset))
        .ok_or(EotsError::RangeEndOverflow { start, count })?;
    let count = count as usize;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = count.div_ceil(threads);
    let mut nonces = vec![[0u8; 32]; count];
    std::thread::scope(|scope| {
        for (slot, first) in nonces.chunks_mut(chunk).zip((start..=last).step_by(chunk)) {
            scope.spawn(move || {
                for (nonce, height) in slot.iter_mut().zip(first..=last) {
                    *nonce = secret_nonce(seed, subnet_id, height).1;
                }
            });
        }
    });
    Ok(nonces)
}

/// The tree of `commitment` on `subnet_id`, rebuilt from `seed`.
pub fn nonce_tree(
    seed: &EotsSeed,
    subnet_id: &SubnetId,
    commitment: &EotsNonceCommitment,
) -> Result<NonceTree, EotsError> {
    Ok(NonceTree::new(&public_nonces(
        seed,
        subnet_id,
        commitment.start,
        commitment.count,
    )?))
}

/// Leaves per subtree of a commitment tree, as a power of two.
pub const SUBTREE_LOG2: u32 = 16;

/// The roots of the subtrees of `2^log2` leaves of the tree of `seed` over the heights
/// `[start, start + count)` on `subnet_id`, derived one subtree at a time.
pub fn subtree_roots(
    seed: &EotsSeed,
    subnet_id: &SubnetId,
    start: u64,
    count: u32,
    log2: u32,
) -> Result<SubtreeRoots, EotsError> {
    let roots = (0..SubtreeRoots::subtree_count_of(count, log2))
        .map(|j| {
            let (first, n) = subtree_heights(start, count, log2, j)?;
            NonceTree::new(&public_nonces(seed, subnet_id, first, n)?)
                .root()
                .ok_or(EotsError::ZeroCount)
        })
        .collect::<Result<Vec<_>, _>>()?;
    SubtreeRoots::new(log2, count, roots)
}

/// The first height and the number of heights of subtree `j` of a tree over `count` heights
/// from `start`.
fn subtree_heights(start: u64, count: u32, log2: u32, j: usize) -> Result<(u64, u32), EotsError> {
    let from = j << log2;
    let n = (count as usize - from).min(1 << log2) as u32;
    let first = start
        .checked_add(from as u64)
        .ok_or(EotsError::RangeEndOverflow { start, count })?;
    Ok((first, n))
}

/// Subtree `j` of the tree of `commitment` on `subnet_id`, rebuilt from `seed` and checked
/// against its root in `roots`.
pub fn nonce_subtree(
    seed: &EotsSeed,
    subnet_id: &SubnetId,
    commitment: &EotsNonceCommitment,
    roots: &SubtreeRoots,
    j: usize,
) -> Result<NonceTree, EotsError> {
    if roots.len() != commitment.count as usize || j >= roots.subtree_count() {
        return Err(EotsError::SubtreeMismatch);
    }
    let (first, n) = subtree_heights(commitment.start, commitment.count, roots.log2(), j)?;
    let subtree = NonceTree::new(&public_nonces(seed, subnet_id, first, n)?);
    if subtree.root() != roots.subtree_root(j) {
        return Err(EotsError::SubtreeMismatch);
    }
    Ok(subtree)
}

/// The commitment of `seed` to the heights `[start, start + count)` on `subnet_id`, with the
/// roots of the subtrees of its tree.
pub fn commit_nonces(
    seed: &EotsSeed,
    subnet_id: &SubnetId,
    start: u64,
    count: u32,
) -> Result<(EotsNonceCommitment, SubtreeRoots), EotsError> {
    let roots = subtree_roots(seed, subnet_id, start, count, SUBTREE_LOG2)?;
    let root = roots.root().ok_or(EotsError::ZeroCount)?;
    Ok((EotsNonceCommitment { start, count, root }, roots))
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

/// The RFC 6962 Merkle tree of a list of nonces, with the subtree hashes cached.
#[derive(Debug, Clone)]
pub struct NonceTree {
    /// `layers[j][i]` is the hash of leaves `[i · 2^j, (i + 1) · 2^j)`
    layers: Vec<Vec<[u8; 32]>>,
}

impl NonceTree {
    pub fn new(leaves: &[[u8; 32]]) -> Self {
        Self::from_hashes(leaves.iter().map(leaf_hash).collect())
    }

    /// The tree whose lowest layer is `hashes`: leaf hashes, or the roots of aligned subtrees.
    fn from_hashes(hashes: Vec<[u8; 32]>) -> Self {
        let mut layers = vec![hashes];
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

    /// The hash of leaves `[from, to)`, with `from` aligned to the subtree size.
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

/// The roots of the aligned subtrees of `2^log2` leaves of a commitment tree of `count` leaves,
/// the last subtree possibly shorter. As RFC 6962 splits at powers of two, the tree's root is the
/// tree over these roots, and the audit path of a leaf is its path inside its subtree followed by
/// the path of its subtree root over the roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SubtreeRootsFields")]
pub struct SubtreeRoots {
    log2: u32,
    count: u32,
    roots: Vec<[u8; 32]>,
}

/// The fields of `SubtreeRoots` before their check.
#[derive(Deserialize)]
struct SubtreeRootsFields {
    log2: u32,
    count: u32,
    roots: Vec<[u8; 32]>,
}

impl TryFrom<SubtreeRootsFields> for SubtreeRoots {
    type Error = EotsError;

    fn try_from(fields: SubtreeRootsFields) -> Result<Self, EotsError> {
        Self::new(fields.log2, fields.count, fields.roots)
    }
}

impl SubtreeRoots {
    pub fn new(log2: u32, count: u32, roots: Vec<[u8; 32]>) -> Result<Self, EotsError> {
        if log2 >= u32::BITS || roots.len() != Self::subtree_count_of(count, log2) {
            return Err(EotsError::SubtreeMismatch);
        }
        Ok(Self { log2, count, roots })
    }

    fn subtree_count_of(count: u32, log2: u32) -> usize {
        (count as usize).div_ceil(1 << log2)
    }

    pub fn log2(&self) -> u32 {
        self.log2
    }

    /// The number of leaves.
    pub fn len(&self) -> usize {
        self.count as usize
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn subtree_count(&self) -> usize {
        self.roots.len()
    }

    /// The subtree of leaf `index`.
    pub fn subtree_of(&self, index: usize) -> usize {
        index >> self.log2
    }

    /// The leaves of subtree `j`.
    pub fn subtree_range(&self, j: usize) -> Option<Range<usize>> {
        (j < self.roots.len()).then(|| {
            let from = j << self.log2;
            from..(from + (1 << self.log2)).min(self.len())
        })
    }

    pub fn subtree_root(&self, j: usize) -> Option<[u8; 32]> {
        self.roots.get(j).copied()
    }

    /// The leaves of subtree `j`, if `subtree` is that subtree.
    fn checked_range(&self, j: usize, subtree: &NonceTree) -> Option<Range<usize>> {
        let range = self.subtree_range(j)?;
        (subtree.len() == range.len() && subtree.root() == self.subtree_root(j)).then_some(range)
    }

    /// The tree over the subtree roots, inner nodes of the commitment tree.
    fn upper_tree(&self) -> NonceTree {
        NonceTree::from_hashes(self.roots.clone())
    }

    pub fn root(&self) -> Option<[u8; 32]> {
        self.upper_tree().root()
    }

    /// The audit path of leaf `index`, given the full subtree that holds it.
    pub fn audit_path(&self, index: usize, subtree: &NonceTree) -> Option<Vec<[u8; 32]>> {
        let j = self.subtree_of(index);
        let range = self.checked_range(j, subtree)?;
        let mut path = subtree.audit_path(index - range.start)?;
        path.extend(self.upper_tree().audit_path(j)?);
        Some(path)
    }
}

/// A commitment tree held in part: the roots of its subtrees and the full subtrees of some
/// heights.
#[derive(Debug, Clone)]
pub struct CommitmentTree {
    roots: SubtreeRoots,
    subtrees: BTreeMap<usize, NonceTree>,
}

impl CommitmentTree {
    pub fn new(roots: SubtreeRoots) -> Self {
        Self {
            roots,
            subtrees: BTreeMap::new(),
        }
    }

    /// The whole tree of `leaves` in subtrees of `2^log2` leaves, every subtree held.
    pub fn from_leaves(leaves: &[[u8; 32]], log2: u32) -> Self {
        let subtrees: Vec<NonceTree> = leaves.chunks(1 << log2).map(NonceTree::new).collect();
        let roots = subtrees
            .iter()
            .map(|t| t.root().expect("a chunk has a leaf"))
            .collect();
        Self {
            roots: SubtreeRoots::new(log2, leaves.len() as u32, roots)
                .expect("one root per subtree"),
            subtrees: subtrees.into_iter().enumerate().collect(),
        }
    }

    pub fn roots(&self) -> &SubtreeRoots {
        &self.roots
    }

    /// The number of leaves.
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub fn root(&self) -> Option<[u8; 32]> {
        self.roots.root()
    }

    pub fn holds_subtree(&self, j: usize) -> bool {
        self.subtrees.contains_key(&j)
    }

    /// Whether the subtree of leaf `index` is held.
    pub fn holds(&self, index: usize) -> bool {
        self.holds_subtree(self.roots.subtree_of(index))
    }

    /// Holds subtree `j`, which must have the root recorded for it.
    pub fn insert_subtree(&mut self, j: usize, subtree: NonceTree) -> Result<(), EotsError> {
        self.roots
            .checked_range(j, &subtree)
            .ok_or(EotsError::SubtreeMismatch)?;
        self.subtrees.insert(j, subtree);
        Ok(())
    }

    /// Releases every subtree but subtree `j` and the next one.
    pub fn release_other_subtrees(&mut self, j: usize) {
        self.subtrees.retain(|&k, _| k == j || k == j + 1);
    }

    /// The subtrees held.
    #[cfg(test)]
    pub fn held_subtrees(&self) -> Vec<usize> {
        self.subtrees.keys().copied().collect()
    }

    /// The audit path of leaf `index`, if its subtree is held.
    pub fn audit_path(&self, index: usize) -> Option<Vec<[u8; 32]>> {
        let subtree = self.subtrees.get(&self.roots.subtree_of(index))?;
        self.roots.audit_path(index, subtree)
    }
}

/// Checks the RFC 6962 audit path `path` of `leaf` at `index` in a tree of `size` leaves against
/// `root` (RFC 9162, section 2.1.3.2).
#[cfg(test)]
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
#[cfg(test)]
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
    tree: &CommitmentTree,
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
        pk_eots: sk.x_only_public_key(&secp).0.serialize(),
        height,
        block_hash: *block_hash,
        signature: sign_with_nonce(sk, &k, &nonce_x, &msg),
        start: commitment.start,
        proof,
    })
}

/// Checks a vote's proof against `commitment` and its signature under its key.
#[cfg(test)]
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
#[cfg(test)]
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
    use sha2::{Digest, Sha256};

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
        assert_eq!(
            NonceTree::new(&leaves).root().unwrap(),
            recursive_root(&leaves)
        );
    }

    #[test]
    fn single_leaf_root_is_its_leaf_hash() {
        let leaf = [9u8; 32];
        assert_eq!(NonceTree::new(&[leaf]).root().unwrap(), leaf_hash(&leaf));
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
        let nonces = public_nonces(&seed, &subnet, commitment_start, 20).unwrap();
        let tree = CommitmentTree::from_leaves(&nonces, 2);
        let commitment = EotsNonceCommitment {
            start: commitment_start,
            count: 20,
            root: tree.root().unwrap(),
        };
        let block = [0xabu8; 32];
        let vote = sign_vote(&sk, &seed, &subnet, &commitment, &tree, 17, &block).unwrap();
        assert_eq!(vote.pk_eots, sk.x_only_public_key(&secp).0.serialize());
        assert_eq!(vote.signature[0], nonces[12]);
        verify_vote(&subnet, &vote, &commitment).unwrap();
        // The signature verifies as an ordinary BIP340 signature with the bitcoin crate
        let sig = secp256k1::schnorr::Signature::from_slice(&signature_to_bytes(&vote.signature))
            .unwrap();
        let msg = Message::from_digest(vote_message(&subnet, 17, &block));
        secp.verify_schnorr(&sig, &msg, &sk.x_only_public_key(&secp).0)
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
        let subnet = test_subnet();
        for seed in 1u8..12 {
            let sk = SecretKey::from_slice(&[seed; 32]).unwrap();
            let nonce_seed = EotsSeed::from_bytes([seed; 32]);
            let nonces = public_nonces(&nonce_seed, &subnet, 1, 3).unwrap();
            let tree = CommitmentTree::from_leaves(&nonces, 2);
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
        }
    }

    #[test]
    fn two_votes_at_one_height_reveal_the_key() {
        let secp = Secp256k1::new();
        let subnet = test_subnet();
        for seed in 1u8..6 {
            let sk = SecretKey::from_slice(&[seed; 32]).unwrap();
            let nonce_seed = EotsSeed::from_bytes([seed.wrapping_mul(7); 32]);
            let nonces = public_nonces(&nonce_seed, &subnet, 1, 4).unwrap();
            let tree = CommitmentTree::from_leaves(&nonces, 2);
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
            assert_eq!(
                extracted.x_only_public_key(&secp).0,
                sk.x_only_public_key(&secp).0
            );
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
        let again = public_nonces(&seed, &subnet, 1, 300).unwrap();
        assert_eq!(public_nonces(&seed, &subnet, 1, 300).unwrap(), again);
        // Sequential derivation gives the same nonces as the parallel one, in the same order
        let sequential: Vec<_> = (1..=300)
            .map(|h| secret_nonce(&seed, &subnet, h).1)
            .collect();
        assert_eq!(sequential, again);
        // Another seed, another subnet or another height gives other nonces
        let other = EotsSeed::from_bytes([10u8; 32]);
        assert_ne!(public_nonces(&other, &subnet, 1, 300).unwrap(), again);
        let other_subnet = SubnetId::from_txid20(&[77u8; 20]);
        assert_ne!(public_nonces(&seed, &other_subnet, 1, 300).unwrap(), again);
        assert_ne!(
            &public_nonces(&seed, &subnet, 2, 300).unwrap()[..299],
            &again[..299]
        );
        assert_eq!(
            &public_nonces(&seed, &subnet, 2, 299).unwrap()[..],
            &again[1..]
        );
        assert!(public_nonces(&seed, &subnet, 1, 0).unwrap().is_empty());
        // The commitment and the rebuilt tree agree
        let (commitment, tree) = commit_nonces(&seed, &subnet, 1, 300).unwrap();
        assert_eq!(
            nonce_tree(&seed, &subnet, &commitment).unwrap().root(),
            tree.root()
        );
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
    }

    fn range_commitment(start: u64, count: u32) -> EotsNonceCommitment {
        EotsNonceCommitment {
            start,
            count,
            root: std::array::from_fn(|i| i as u8),
        }
    }

    #[test]
    fn commitment_covers_the_half_open_range_from_start() {
        const MAX: u64 = u64::MAX;
        let cases: [(u64, u32, u64, bool); 14] = [
            // first height, last height, end, and below start down to 0
            (129, 17, 129, true),
            (129, 17, 145, true),
            (129, 17, 146, false),
            (129, 17, 128, false),
            (129, 17, 0, false),
            // count 1
            (1, 1, 1, true),
            (1, 1, 0, false),
            (1, 1, 2, false),
            // heights past u32
            (2, u32::MAX, 1 << 32, true),
            (2, u32::MAX, (1 << 32) + 1, false),
            // range ending at the largest height
            (MAX - 1, 1, MAX - 2, false),
            (MAX - 1, 1, MAX - 1, true),
            (MAX - 1, 1, MAX, false),
            (0, 1, 0, true),
        ];
        for (start, count, height, expected) in cases {
            assert_eq!(
                range_commitment(start, count).covers(height),
                expected,
                "start {start}, count {count}, height {height}"
            );
        }
        for (start, count, end) in [
            (129, 17, 146),
            (2, u32::MAX, (1 << 32) + 1),
            (MAX - 1, 1, MAX),
        ] {
            assert_eq!(range_commitment(start, count).end().ok(), Some(end));
        }
    }

    #[test]
    fn end_past_the_largest_height_is_an_error() {
        assert!(matches!(
            range_commitment(u64::MAX, 1).end(),
            Err(EotsError::RangeEndOverflow {
                start: u64::MAX,
                count: 1
            })
        ));
    }

    #[test]
    fn public_nonces_past_the_largest_height_are_an_error() {
        let seed = EotsSeed::from_bytes([5u8; 32]);
        assert!(matches!(
            public_nonces(&seed, &test_subnet(), u64::MAX, 2),
            Err(EotsError::RangeEndOverflow {
                start: u64::MAX,
                count: 2
            })
        ));
    }

    #[test]
    fn public_nonce_at_the_largest_height() {
        let seed = EotsSeed::from_bytes([5u8; 32]);
        let nonces = public_nonces(&seed, &test_subnet(), u64::MAX, 1).unwrap();
        assert_eq!(
            nonces,
            vec![secret_nonce(&seed, &test_subnet(), u64::MAX).1]
        );
    }

    #[test]
    fn commit_nonces_with_count_zero_is_an_error() {
        let seed = EotsSeed::from_bytes([5u8; 32]);
        assert!(matches!(
            commit_nonces(&seed, &test_subnet(), 10, 0),
            Err(EotsError::ZeroCount)
        ));
    }

    #[test]
    fn separate_commitments_do_not_fill_the_gap_between_them() {
        let ranges = [range_commitment(1, 3), range_commitment(5, 2)];
        assert!(!ranges.iter().any(|c| c.covers(4)));
        assert_eq!(ranges.iter().filter(|c| c.covers(5)).count(), 1);
    }

    // encoding.md, EOTS nonce commitment and Postcard encoding
    #[test]
    fn commitment_has_a_literal_leb128_tuple_encoding() {
        let mut expected = vec![0x81, 0x01, 0x80, 0x80, 0x01]; // 129, 16384
        expected.extend(0_u8..32);
        assert_eq!(
            postcard::to_stdvec(&range_commitment(129, 16384)).unwrap(),
            expected
        );
        let decoded: EotsNonceCommitment = postcard::from_bytes(&expected).unwrap();
        assert_eq!(decoded, range_commitment(129, 16384));

        let mut one_byte_each = vec![1, 2];
        one_byte_each.extend(0_u8..32);
        assert_eq!(
            postcard::to_stdvec(&range_commitment(1, 2)).unwrap(),
            one_byte_each
        );
    }

    // encoding.md, Postcard encoding
    #[test]
    fn commitment_integer_widths_are_encoded_without_truncation() {
        let mut expected = vec![0xff; 9];
        expected.push(0x01);
        expected.extend([0xff, 0xff, 0xff, 0xff, 0x0f]);
        expected.extend(0_u8..32);
        assert_eq!(
            postcard::to_stdvec(&range_commitment(u64::MAX, u32::MAX)).unwrap(),
            expected
        );
    }

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

    #[test]
    fn overlapping_commitment_ranges_use_identical_nonces_for_the_same_heights() {
        let seed = EotsSeed::from_bytes([0x91; 32]);
        let subnet = SubnetId::from_txid20(&[0x32; 20]);
        let all = public_nonces(&seed, &subnet, 125, 9).unwrap();
        let mut partitioned = public_nonces(&seed, &subnet, 125, 3).unwrap();
        partitioned.extend(public_nonces(&seed, &subnet, 128, 6).unwrap());
        assert_eq!(partitioned, all);
        assert_eq!(public_nonces(&seed, &subnet, 127, 4).unwrap(), all[2..6]);
    }

    #[test]
    fn a_restored_seed_rebuilds_an_uneven_tree_with_the_rfc6962_root() {
        let subnet = SubnetId::from_txid20(&[0x53; 20]);
        let seed = EotsSeed::from_bytes([0xa3; 32]);
        let leaves = public_nonces(&seed, &subnet, 0x0102, 11).unwrap();
        let (commitment, roots) = commit_nonces(&seed, &subnet, 0x0102, 11).unwrap();
        assert_eq!(commitment.root, reference_root(&leaves));
        let original = whole_tree(&seed, &subnet, &commitment);
        assert_eq!(original.roots(), &roots);
        let restored = EotsSeed::from_bytes(*seed.as_bytes());
        let rebuilt = nonce_tree(&restored, &subnet, &commitment).unwrap();
        assert_eq!(rebuilt.root(), Some(reference_root(&leaves)));
        for i in 0..11 {
            assert_eq!(rebuilt.audit_path(i), original.audit_path(i));
        }
    }

    /// The tree of `commitment` with every subtree held.
    fn whole_tree(
        seed: &EotsSeed,
        subnet: &SubnetId,
        commitment: &EotsNonceCommitment,
    ) -> CommitmentTree {
        let leaves = public_nonces(seed, subnet, commitment.start, commitment.count).unwrap();
        CommitmentTree::from_leaves(&leaves, SUBTREE_LOG2)
    }

    /// Leaves whose bytes differ by index and `salt`.
    fn test_leaves(size: usize, salt: u8) -> Vec<[u8; 32]> {
        (0..size)
            .map(|i| {
                let mut leaf = [salt; 32];
                leaf[..8].copy_from_slice(&(i as u64).to_le_bytes());
                leaf
            })
            .collect()
    }

    /// Tree sizes around the subtree boundaries of `2^log2` leaves: 1, powers of two, multiples of
    /// the subtree size and one more or one less.
    fn tree_size(log2: u32) -> impl Strategy<Value = usize> {
        let subtree = 1usize << log2;
        prop_oneof![
            1usize..70,
            (0u32..7).prop_map(|p| 1usize << p),
            (1usize..6).prop_map(move |m| m * subtree),
            (1usize..6).prop_map(move |m| m * subtree + 1),
            (1usize..6).prop_map(move |m| (m * subtree - 1).max(1)),
        ]
    }

    proptest! {
        #[test]
        fn subtree_roots_give_the_root_and_audit_paths_of_the_whole_tree(
            (log2, size, index) in (0u32..5).prop_flat_map(|log2| {
                (Just(log2), tree_size(log2)).prop_flat_map(|(log2, size)| {
                    (Just(log2), Just(size), 0..size)
                })
            }),
            salt in any::<u8>(),
        ) {
            let leaves = test_leaves(size, salt);
            let whole = NonceTree::new(&leaves);
            let split = CommitmentTree::from_leaves(&leaves, log2);
            prop_assert_eq!(split.root(), whole.root());
            prop_assert_eq!(split.len(), size);
            for i in 0..size {
                prop_assert_eq!(split.audit_path(i), whole.audit_path(i));
            }
            // The roots and the one subtree of a height give its path
            let roots = split.roots().clone();
            let j = roots.subtree_of(index);
            let range = roots.subtree_range(j).unwrap();
            prop_assert!(range.contains(&index));
            let mut partial = CommitmentTree::new(roots);
            prop_assert_eq!(partial.audit_path(index), None);
            partial.insert_subtree(j, NonceTree::new(&leaves[range])).unwrap();
            prop_assert_eq!(partial.audit_path(index), whole.audit_path(index));
            let path = partial.audit_path(index).unwrap();
            prop_assert!(verify_audit_path(
                &leaves[index],
                index as u64,
                size as u64,
                &path,
                &whole.root().unwrap(),
            ));
        }
    }

    /// Time and memory of the whole tree against the subtree roots and one subtree, for
    /// 1,000,000 heights. `EOTS_TIMING=whole` or `EOTS_TIMING=split`, under `/usr/bin/time -v`:
    /// `cargo test --release --lib eots_tree_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn eots_tree_timing() {
        let subnet = SubnetId::from_txid20(&[0x71; 20]);
        let seed = EotsSeed::from_bytes([0x17; 32]);
        let count = 1_000_000;
        let started = std::time::Instant::now();
        if std::env::var("EOTS_TIMING").as_deref() == Ok("whole") {
            let tree = NonceTree::new(&public_nonces(&seed, &subnet, 1, count).unwrap());
            println!(
                "whole tree of {count} heights (join, and start): {:.1} s, {} layers",
                started.elapsed().as_secs_f64(),
                tree.layers.len()
            );
            return;
        }
        let (commitment, roots) = commit_nonces(&seed, &subnet, 1, count).unwrap();
        println!(
            "subtree roots of {count} heights (join): {:.1} s, {} roots",
            started.elapsed().as_secs_f64(),
            roots.subtree_count()
        );
        let started = std::time::Instant::now();
        let subtree = nonce_subtree(&seed, &subnet, &commitment, &roots, 7).unwrap();
        println!(
            "one subtree of {} heights (start): {:.1} s",
            subtree.len(),
            started.elapsed().as_secs_f64()
        );
    }

    #[test]
    fn a_subtree_with_another_root_is_refused() {
        let leaves = test_leaves(10, 1);
        let mut tree = CommitmentTree::new(CommitmentTree::from_leaves(&leaves, 2).roots().clone());
        assert_eq!(tree.roots().subtree_count(), 3);
        assert!(matches!(
            tree.insert_subtree(1, NonceTree::new(&test_leaves(4, 2))),
            Err(EotsError::SubtreeMismatch)
        ));
        assert!(matches!(
            tree.insert_subtree(2, NonceTree::new(&leaves[4..8])),
            Err(EotsError::SubtreeMismatch)
        ));
        assert!(matches!(
            tree.insert_subtree(3, NonceTree::new(&leaves[8..])),
            Err(EotsError::SubtreeMismatch)
        ));
        tree.insert_subtree(2, NonceTree::new(&leaves[8..]))
            .unwrap();
        tree.insert_subtree(1, NonceTree::new(&leaves[4..8]))
            .unwrap();
        assert!(tree.holds(5) && tree.holds(9) && !tree.holds(3));
        tree.release_other_subtrees(0);
        assert!(tree.holds(5) && !tree.holds(9));
        tree.insert_subtree(2, NonceTree::new(&leaves[8..]))
            .unwrap();
        tree.release_other_subtrees(2);
        assert!(!tree.holds(5) && tree.holds(9));
        assert!(matches!(
            SubtreeRoots::new(2, 10, vec![[0; 32]; 2]),
            Err(EotsError::SubtreeMismatch)
        ));
    }

    #[test]
    fn subtrees_derived_from_the_seed_match_the_whole_tree() {
        let subnet = SubnetId::from_txid20(&[0x54; 20]);
        let seed = EotsSeed::from_bytes([0xa5; 32]);
        let (start, count) = (0x0100, 11);
        let leaves = public_nonces(&seed, &subnet, start, count).unwrap();
        let whole = CommitmentTree::from_leaves(&leaves, 2);
        let roots = subtree_roots(&seed, &subnet, start, count, 2).unwrap();
        assert_eq!(&roots, whole.roots());
        assert_eq!(roots.root(), Some(reference_root(&leaves)));
        let commitment = EotsNonceCommitment {
            start,
            count,
            root: roots.root().unwrap(),
        };
        let mut rebuilt = CommitmentTree::new(roots.clone());
        for j in 0..roots.subtree_count() {
            let subtree = nonce_subtree(&seed, &subnet, &commitment, &roots, j).unwrap();
            assert_eq!(subtree.len(), roots.subtree_range(j).unwrap().len());
            rebuilt.insert_subtree(j, subtree).unwrap();
        }
        for i in 0..count as usize {
            assert_eq!(rebuilt.audit_path(i), whole.audit_path(i));
        }
        assert!(matches!(
            nonce_subtree(&seed, &subnet, &commitment, &roots, 3),
            Err(EotsError::SubtreeMismatch)
        ));
        let other = EotsSeed::from_bytes([0xa6; 32]);
        assert!(matches!(
            nonce_subtree(&other, &subnet, &commitment, &roots, 0),
            Err(EotsError::SubtreeMismatch)
        ));
        let (committed, committed_roots) = commit_nonces(&seed, &subnet, start, count).unwrap();
        assert_eq!(committed, commitment);
        assert_eq!(committed_roots.subtree_count(), 1);
        assert_eq!(committed_roots.root(), Some(commitment.root));
    }

    #[test]
    fn changing_only_the_nonce_seed_invalidates_the_original_commitment() {
        let subnet = SubnetId::from_txid20(&[0x53; 20]);
        let seed = EotsSeed::from_bytes([0xa3; 32]);
        let other = EotsSeed::from_bytes([0xa4; 32]);
        let (commitment, _) = commit_nonces(&seed, &subnet, 17, 5).unwrap();
        let tree = whole_tree(&seed, &subnet, &commitment);
        let sk = SecretKey::from_slice(&[7; 32]).unwrap();
        let vote = sign_vote(&sk, &other, &subnet, &commitment, &tree, 19, &[0x29; 32]).unwrap();
        assert_ne!(
            vote.signature[0],
            public_nonces(&seed, &subnet, 19, 1).unwrap()[0]
        );
        assert!(verify_vote(&subnet, &vote, &commitment).is_err());
    }

    #[test]
    fn one_seed_nonce_signs_under_distinct_keys_with_independent_bip340_verification() {
        let subnet = SubnetId::from_txid20(&[0x42; 20]);
        let height = 0x0102_0304_0506_0708;
        let block = [0x87; 32];
        let seed = EotsSeed::from_bytes([0x73; 32]);
        let (commitment, _) = commit_nonces(&seed, &subnet, height, 3).unwrap();
        let tree = whole_tree(&seed, &subnet, &commitment);
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
