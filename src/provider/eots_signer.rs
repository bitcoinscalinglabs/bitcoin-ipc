//! The validator's EOTS signer: one vote per subnet height, the votes above the latest
//! checkpointed height kept in a database next to the key file.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::io::Write;
use std::ops::{Bound, RangeBounds};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::JoinHandle;

use bitcoin::secp256k1::{Secp256k1, SecretKey, XOnlyPublicKey};
use heed::types::{SerdeBincode, Str};
use heed::{Database, Env, EnvOpenOptions};
use log::warn;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::eots::{
    self, CommitmentTree, EotsError, EotsNonceCommitment, EotsSeed, EotsVote, NonceTree,
    SubtreeRoots,
};
use crate::ipc_lib::SubnetId;

/// Address space reserved for the vote database, the file only holds the votes written.
const MAP_SIZE: usize = 128 * 1024 * 1024 * 1024;
const VOTES_DB: &str = "votes";

/// The database key of a vote, ordered by height within a subnet.
fn vote_key(subnet_id: SubnetId, height: u64) -> String {
    format!("{subnet_id}:{height:020}")
}

/// The keys of one subnet between two heights, as the standard ranges cannot bound `str` keys.
struct KeyRange {
    first: String,
    last: String,
}

impl RangeBounds<str> for KeyRange {
    fn start_bound(&self) -> Bound<&str> {
        Bound::Included(self.first.as_str())
    }

    fn end_bound(&self) -> Bound<&str> {
        Bound::Included(self.last.as_str())
    }
}

/// The subtree roots of one commitment, as kept in a file next to the key file.
#[derive(Serialize, Deserialize)]
struct SubtreeRootsFile {
    subnet_id: SubnetId,
    commitment: EotsNonceCommitment,
    roots: SubtreeRoots,
}

pub struct EotsSigner {
    secret_key: SecretKey,
    seed: EotsSeed,
    pk_eots: XOnlyPublicKey,
    env: Env,
    votes: Database<Str, SerdeBincode<EotsVote>>,
    trees: Trees,
}

/// The commitment trees of the signer's seed, held in part: the roots of their subtrees, from the
/// files next to the key file, the subtree of the height being signed and the next one, held or
/// building.
struct Trees {
    seed: EotsSeed,
    key_path: PathBuf,
    /// Leaves per subtree of a commitment tree, as a power of two
    subtree_log2: u32,
    /// By subnet and commitment
    held: BTreeMap<(SubnetId, EotsNonceCommitment), CommitmentTree>,
    /// Subtrees under construction, by subnet, commitment and subtree
    builds:
        BTreeMap<(SubnetId, EotsNonceCommitment, usize), JoinHandle<Result<NonceTree, EotsError>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum EotsSignerError {
    #[error("height {height} of subnet {subnet_id} is already signed for another block")]
    DoubleVote { subnet_id: SubnetId, height: u64 },
    #[error("no EOTS nonce commitment of this validator covers height {0}")]
    NotCommitted(u64),
    #[error("vote database directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("vote database: {0}")]
    Db(#[from] heed::Error),
    #[error(transparent)]
    Eots(#[from] EotsError),
    #[error("subtree roots file: {0}")]
    RootsFile(String),
    #[error("the build of a nonce subtree failed")]
    SubtreeBuild,
}

impl EotsSigner {
    /// Opens the vote database next to the key file, `<key file>.votes.db`.
    pub fn load(
        secret_key: SecretKey,
        seed: EotsSeed,
        key_path: &Path,
    ) -> Result<Self, EotsSignerError> {
        let db_path = PathBuf::from(format!("{}.votes.db", key_path.display()));
        std::fs::create_dir_all(&db_path)?;
        let env = unsafe {
            EnvOpenOptions::new()
                .max_dbs(1)
                .map_size(MAP_SIZE)
                .open(&db_path)?
        };
        let mut txn = env.write_txn()?;
        let votes = env.create_database(&mut txn, Some(VOTES_DB))?;
        txn.commit()?;
        let secp = Secp256k1::new();
        Ok(Self {
            pk_eots: secret_key.x_only_public_key(&secp).0,
            secret_key,
            seed,
            env,
            votes,
            trees: Trees {
                seed,
                key_path: key_path.to_path_buf(),
                subtree_log2: eots::SUBTREE_LOG2,
                held: BTreeMap::new(),
                builds: BTreeMap::new(),
            },
        })
    }

    pub fn key_path(&self) -> &Path {
        &self.trees.key_path
    }

    /// The file of the subtree roots of `commitment`: `<key file>.subtree-roots.<txid20>.<root>`.
    fn roots_path(
        key_path: &Path,
        subnet_id: SubnetId,
        commitment: &EotsNonceCommitment,
    ) -> PathBuf {
        PathBuf::from(format!(
            "{}.subtree-roots.{}.{}",
            key_path.display(),
            hex::encode(subnet_id.txid20()),
            hex::encode(commitment.root)
        ))
    }

    fn read_subtree_roots(
        path: &Path,
        subnet_id: SubnetId,
        commitment: &EotsNonceCommitment,
        log2: u32,
    ) -> Result<SubtreeRoots, EotsSignerError> {
        let file = |e: &dyn std::fmt::Display| EotsSignerError::RootsFile(e.to_string());
        let bytes = std::fs::read(path).map_err(|e| file(&e))?;
        let record: SubtreeRootsFile = serde_json::from_slice(&bytes).map_err(|e| file(&e))?;
        let roots = record.roots;
        if record.subnet_id != subnet_id
            || record.commitment != *commitment
            || roots.log2() != log2
            || roots.len() != commitment.count as usize
            || roots.root() != Some(commitment.root)
        {
            return Err(file(&"the roots are not those of the commitment"));
        }
        Ok(roots)
    }

    fn write_subtree_roots(
        path: &Path,
        subnet_id: SubnetId,
        commitment: &EotsNonceCommitment,
        roots: &SubtreeRoots,
    ) -> Result<(), EotsSignerError> {
        let file = |e: &dyn std::fmt::Display| EotsSignerError::RootsFile(e.to_string());
        let record = SubtreeRootsFile {
            subnet_id,
            commitment: *commitment,
            roots: roots.clone(),
        };
        let bytes = serde_json::to_vec(&record).map_err(|e| file(&e))?;
        // A new file of this write alone, next to `path`, removed unless renamed onto it
        let mut partial =
            NamedTempFile::new_in(path.parent().unwrap_or(Path::new("."))).map_err(|e| file(&e))?;
        partial.write_all(&bytes).map_err(|e| file(&e))?;
        partial.into_temp_path().persist(path).map_err(|e| file(&e))
    }

    /// The subtree roots of `commitment` from their file next to `key_path`; when the file is
    /// missing or wrong, derived from `seed` and written there, used even if the write fails.
    pub fn subtree_roots(
        key_path: &Path,
        seed: &EotsSeed,
        subnet_id: SubnetId,
        commitment: &EotsNonceCommitment,
        log2: u32,
    ) -> Result<SubtreeRoots, EotsSignerError> {
        let path = Self::roots_path(key_path, subnet_id, commitment);
        match Self::read_subtree_roots(&path, subnet_id, commitment, log2) {
            Ok(roots) => return Ok(roots),
            Err(e) => warn!(
                "EOTS subtree roots of subnet {subnet_id} not read from {}: {e}; deriving every nonce",
                path.display()
            ),
        }
        let roots =
            eots::subtree_roots(seed, &subnet_id, commitment.start, commitment.count, log2)?;
        if roots.root() != Some(commitment.root) {
            return Err(EotsError::SubtreeMismatch.into());
        }
        if let Err(e) = Self::write_subtree_roots(&path, subnet_id, commitment, &roots) {
            warn!(
                "EOTS subtree roots of subnet {subnet_id} not written to {}: {e}",
                path.display()
            );
        }
        Ok(roots)
    }

    /// Keeps the subtree roots of a new commitment of this signer's seed, in their file and for
    /// signing.
    pub fn keep_subtree_roots(
        &mut self,
        subnet_id: SubnetId,
        commitment: &EotsNonceCommitment,
        roots: SubtreeRoots,
    ) -> Result<(), EotsSignerError> {
        let path = Self::roots_path(&self.trees.key_path, subnet_id, commitment);
        Self::write_subtree_roots(&path, subnet_id, commitment, &roots)?;
        self.insert_tree(subnet_id, commitment, CommitmentTree::new(roots));
        Ok(())
    }

    /// Holds the tree of `commitment` from a start: the subtree roots read or derived outside the
    /// lock, with the subtree of the height after the last signed vote of `subnet_id`, else of the
    /// commitment's first height.
    pub fn hold_tree(
        signer: &Mutex<Self>,
        subnet_id: SubnetId,
        commitment: &EotsNonceCommitment,
    ) -> Result<(), EotsSignerError> {
        let lock = || signer.lock().expect("EOTS signer lock");
        let (seed, key_path, log2) = {
            let signer = lock();
            (
                signer.seed,
                signer.trees.key_path.clone(),
                signer.trees.subtree_log2,
            )
        };
        let roots = Self::subtree_roots(&key_path, &seed, subnet_id, commitment, log2)?;
        let next = lock().next_height(subnet_id, commitment.start)?;
        let mut tree = CommitmentTree::new(roots);
        if let Some(next) = next.filter(|h| commitment.covers(*h)) {
            let j = tree.roots().subtree_of((next - commitment.start) as usize);
            let subtree = eots::nonce_subtree(&seed, &subnet_id, commitment, tree.roots(), j)?;
            tree.insert_subtree(j, subtree)?;
        }
        lock().insert_tree(subnet_id, commitment, tree);
        Ok(())
    }

    /// The height after the last signed vote of `subnet_id`, `first` without a vote, none after a
    /// vote at the largest height.
    pub fn next_height(
        &self,
        subnet_id: SubnetId,
        first: u64,
    ) -> Result<Option<u64>, EotsSignerError> {
        let range = KeyRange {
            first: vote_key(subnet_id, 0),
            last: vote_key(subnet_id, u64::MAX),
        };
        let txn = self.env.read_txn()?;
        let last = self.votes.rev_range(&txn, &range)?.next().transpose()?;
        Ok(match last {
            Some((_, vote)) => vote.height.checked_add(1),
            None => Some(first),
        })
    }

    pub fn pk_eots(&self) -> XOnlyPublicKey {
        self.pk_eots
    }

    pub fn seed(&self) -> EotsSeed {
        self.seed
    }

    pub fn secret_key(&self) -> SecretKey {
        self.secret_key
    }

    pub fn has_tree(&self, subnet_id: SubnetId, commitment: &EotsNonceCommitment) -> bool {
        self.trees.held.contains_key(&(subnet_id, *commitment))
    }

    /// Keeps a tree of `commitment` from this signer's seed, unless one is held.
    fn insert_tree(
        &mut self,
        subnet_id: SubnetId,
        commitment: &EotsNonceCommitment,
        tree: CommitmentTree,
    ) {
        self.trees
            .held
            .entry((subnet_id, *commitment))
            .or_insert(tree);
    }

    pub fn signed(
        &self,
        subnet_id: SubnetId,
        height: u64,
    ) -> Result<Option<EotsVote>, EotsSignerError> {
        let txn = self.env.read_txn()?;
        Ok(self.votes.get(&txn, &vote_key(subnet_id, height))?)
    }

    /// Deletes the votes of `subnet_id` at or below `height`, where no vote is signed.
    pub fn delete_up_to(&self, subnet_id: SubnetId, height: u64) -> Result<(), EotsSignerError> {
        let range = KeyRange {
            first: vote_key(subnet_id, 0),
            last: vote_key(subnet_id, height),
        };
        let mut txn = self.env.write_txn()?;
        self.votes.delete_range(&mut txn, &range)?;
        txn.commit()?;
        Ok(())
    }

    /// The votes of `subnet_id` from `from_height` on, in height order, at most `limit`.
    #[cfg(test)]
    pub fn signed_from(
        &self,
        subnet_id: SubnetId,
        from_height: u64,
        limit: usize,
    ) -> Result<Vec<EotsVote>, EotsSignerError> {
        let txn = self.env.read_txn()?;
        let range = KeyRange {
            first: vote_key(subnet_id, from_height),
            last: vote_key(subnet_id, u64::MAX),
        };
        let votes = self
            .votes
            .range(&txn, &range)?
            .take(limit)
            .map(|entry| entry.map(|(_, vote)| vote))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(votes)
    }

    /// Signs the vote for `(height, block_hash)`. A height already signed for the same block
    /// gives the stored vote, for another block an error.
    pub fn sign(
        &mut self,
        subnet_id: SubnetId,
        commitments: &[EotsNonceCommitment],
        height: u64,
        block_hash: &[u8; 32],
    ) -> Result<EotsVote, EotsSignerError> {
        let key = vote_key(subnet_id, height);
        // One transaction for the check and the write
        let mut txn = self.env.write_txn()?;
        if let Some(vote) = self.votes.get(&txn, &key)? {
            if vote.block_hash == *block_hash {
                return Ok(vote);
            }
            return Err(EotsSignerError::DoubleVote { subnet_id, height });
        }
        let commitment = commitments
            .iter()
            .find(|c| c.covers(height))
            .ok_or(EotsSignerError::NotCommitted(height))?;
        let index = (height - commitment.start) as usize;
        let tree = self.trees.hold_subtree(subnet_id, commitment, index)?;
        let vote = eots::sign_vote(
            &self.secret_key,
            &self.seed,
            &subnet_id,
            commitment,
            tree,
            height,
            block_hash,
        )?;
        self.votes.put(&mut txn, &key, &vote)?;
        txn.commit()?;
        Ok(vote)
    }
}

impl Trees {
    /// The tree of `commitment` with the subtree of leaf `index` held: the roots read or derived,
    /// the subtree taken from its background build or built now, the other subtrees and builds of
    /// the commitment released but the next subtree, held or building in the background.
    fn hold_subtree(
        &mut self,
        subnet_id: SubnetId,
        commitment: &EotsNonceCommitment,
        index: usize,
    ) -> Result<&CommitmentTree, EotsSignerError> {
        let key = (subnet_id, *commitment);
        if !self.held.contains_key(&key) {
            let roots = EotsSigner::subtree_roots(
                &self.key_path,
                &self.seed,
                subnet_id,
                commitment,
                self.subtree_log2,
            )?;
            self.held.insert(key, CommitmentTree::new(roots));
        }
        let tree = self.held.get_mut(&key).expect("inserted above");
        let j = tree.roots().subtree_of(index);
        let next = j + 1;
        let build = self.builds.remove(&(subnet_id, *commitment, j));
        // A dropped build runs to its end detached, its subtree then released
        self.builds
            .retain(|&(s, c, k), _| (s, c) != key || k == next);
        if !tree.holds_subtree(j) {
            let subtree = match build {
                Some(build) => build.join().map_err(|_| EotsSignerError::SubtreeBuild)??,
                None => eots::nonce_subtree(&self.seed, &subnet_id, commitment, tree.roots(), j)?,
            };
            tree.insert_subtree(j, subtree)?;
        }
        tree.release_other_subtrees(j);
        if next < tree.roots().subtree_count() && !tree.holds_subtree(next) {
            if let Entry::Vacant(slot) = self.builds.entry((subnet_id, *commitment, next)) {
                let (seed, commitment, roots) = (self.seed, *commitment, tree.roots().clone());
                slot.insert(std::thread::spawn(move || {
                    eots::nonce_subtree(&seed, &subnet_id, &commitment, &roots, next)
                }));
            }
        }
        Ok(tree)
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    const CRASH_DIR_VAR: &str = "EOTS_SIGNER_CRASH_DIR";

    fn secret_key() -> SecretKey {
        SecretKey::from_slice(&[4u8; 32]).unwrap()
    }

    fn seed() -> EotsSeed {
        EotsSeed::from_bytes([40u8; 32])
    }

    fn signer_in(dir: &Path) -> EotsSigner {
        // Vote records are keyed by SubnetId's string form, which depends on this process-global.
        crate::eth_utils::set_fvm_network();
        EotsSigner::load(secret_key(), seed(), &dir.join("eots.sk")).unwrap()
    }

    #[test]
    fn loads_a_vote_written_on_the_configured_network() {
        // The database holds subnet ids as written on the configured network (`t410...` on
        // regtest and testnet): loading them needs the FVM network set first,
        // before `load`. A vote at a height must survive the restart.
        crate::eth_utils::set_fvm_network();
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[7u8; 20]);
        assert!(
            subnet.to_string().contains("/t410"),
            "regtest subnet ids are testnet addresses: {subnet}"
        );
        let mut signer = signer_in(dir.path());
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 1, 5).unwrap();
        let vote = signer.sign(subnet, &[commitment], 3, &[1u8; 32]).unwrap();
        assert!(dir.path().join("eots.sk.votes.db").is_dir());

        drop(signer);
        let reloaded = signer_in(dir.path());
        assert_eq!(reloaded.signed(subnet, 3).unwrap(), Some(vote));
    }

    #[test]
    fn signs_once_per_height_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let mut signer = signer_in(dir.path());
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 10, 5).unwrap();
        let vote = signer.sign(subnet, &[commitment], 12, &[1u8; 32]).unwrap();
        eots::verify_vote(&subnet, &vote, &commitment).unwrap();
        // The same block again gives the same vote, another block is refused
        assert_eq!(
            signer.sign(subnet, &[commitment], 12, &[1u8; 32]).unwrap(),
            vote
        );
        assert!(matches!(
            signer.sign(subnet, &[commitment], 12, &[2u8; 32]),
            Err(EotsSignerError::DoubleVote { height: 12, .. })
        ));
        // The first and the last committed height
        let first = signer.sign(subnet, &[commitment], 10, &[3u8; 32]).unwrap();
        let last = signer.sign(subnet, &[commitment], 14, &[4u8; 32]).unwrap();
        // Outside the committed range
        for height in [9, 15] {
            assert!(matches!(
                signer.sign(subnet, &[commitment], height, &[1u8; 32]),
                Err(EotsSignerError::NotCommitted(h)) if h == height
            ));
        }
        // Another subnet at the same height is independent
        let other = SubnetId::from_txid20(&[5u8; 20]);
        let (other_commitment, _) = eots::commit_nonces(&signer.seed, &other, 1, 20).unwrap();
        signer
            .sign(other, &[other_commitment], 12, &[9u8; 32])
            .unwrap();
        assert_eq!(signer.signed(other, 13).unwrap(), None);
        let record = vec![first.clone(), vote.clone(), last.clone()];
        assert_eq!(signer.signed_from(subnet, 0, usize::MAX).unwrap(), record);

        // After a restart the database still refuses another block at every signed height
        drop(signer);
        let mut reloaded = signer_in(dir.path());
        for signed in &record {
            assert_eq!(
                reloaded.signed(subnet, signed.height).unwrap(),
                Some(signed.clone())
            );
            assert!(matches!(
                reloaded.sign(subnet, &[commitment], signed.height, &[2u8; 32]),
                Err(EotsSignerError::DoubleVote { subnet_id, height })
                    if subnet_id == subnet && height == signed.height
            ));
            // The stored proof suffices when a retry supplies no commitment
            assert_eq!(
                reloaded
                    .sign(subnet, &[commitment], signed.height, &signed.block_hash)
                    .unwrap(),
                *signed
            );
            assert_eq!(
                reloaded
                    .sign(subnet, &[], signed.height, &signed.block_hash)
                    .unwrap(),
                *signed
            );
        }
        // Heights outside the committed range stay refused
        for height in [9, 15] {
            assert!(matches!(
                reloaded.sign(subnet, &[commitment], height, &[2u8; 32]),
                Err(EotsSignerError::NotCommitted(h)) if h == height
            ));
            assert!(reloaded.signed(subnet, height).unwrap().is_none());
        }
        assert_eq!(reloaded.signed_from(subnet, 0, usize::MAX).unwrap(), record);

        // A new height after the restart keeps the earlier votes
        let new = reloaded
            .sign(subnet, &[commitment], 13, &[5u8; 32])
            .unwrap();
        let extended = vec![first, vote, new, last];
        assert_eq!(
            reloaded.signed_from(subnet, 0, usize::MAX).unwrap(),
            extended
        );
        drop(reloaded);
        assert_eq!(
            signer_in(dir.path())
                .signed_from(subnet, 0, usize::MAX)
                .unwrap(),
            extended
        );
    }

    #[test]
    fn lists_votes_from_a_height_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let other = SubnetId::from_txid20(&[4u8; 20]);
        let mut signer = signer_in(dir.path());
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 5, 20).unwrap();
        let (other_commitment, _) = eots::commit_nonces(&signer.seed, &other, 5, 20).unwrap();
        for height in [9, 7, 12, 8, 20] {
            signer
                .sign(subnet, &[commitment], height, &[height as u8; 32])
                .unwrap();
        }
        signer
            .sign(other, &[other_commitment], 10, &[1u8; 32])
            .unwrap();
        let heights = |votes: Vec<EotsVote>| votes.iter().map(|v| v.height).collect::<Vec<_>>();
        assert_eq!(
            heights(signer.signed_from(subnet, 8, usize::MAX).unwrap()),
            vec![8, 9, 12, 20]
        );
        assert_eq!(
            heights(signer.signed_from(subnet, 0, 2).unwrap()),
            vec![7, 8]
        );
        assert_eq!(
            heights(signer.signed_from(subnet, 10, 1).unwrap()),
            vec![12]
        );
        assert_eq!(
            heights(signer.signed_from(subnet, 20, 1).unwrap()),
            vec![20]
        );
        assert_eq!(
            heights(signer.signed_from(subnet, 21, 10).unwrap()),
            Vec::<u64>::new()
        );
        assert!(signer.signed_from(subnet, 0, 0).unwrap().is_empty());
        assert_eq!(heights(signer.signed_from(other, 0, 10).unwrap()), vec![10]);

        // The listed votes are the complete stored votes, also after a restart
        let listed = signer.signed_from(subnet, 0, usize::MAX).unwrap();
        for vote in &listed {
            eots::verify_vote(&subnet, vote, &commitment).unwrap();
        }
        drop(signer);
        assert_eq!(
            signer_in(dir.path())
                .signed_from(subnet, 0, usize::MAX)
                .unwrap(),
            listed
        );
    }

    #[test]
    fn delete_up_to_removes_votes_at_or_below_the_height_and_keeps_other_subnets() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let other = SubnetId::from_txid20(&[4u8; 20]);
        let mut signer = signer_in(dir.path());
        sign_history(&mut signer, subnet, 10);
        sign_history(&mut signer, other, 10);
        signer.delete_up_to(subnet, 6).unwrap();
        let heights = |votes: Vec<EotsVote>| votes.iter().map(|v| v.height).collect::<Vec<_>>();
        assert_eq!(
            heights(signer.signed_from(subnet, 0, usize::MAX).unwrap()),
            vec![7, 8, 9, 10]
        );
        assert_eq!(
            heights(signer.signed_from(other, 0, usize::MAX).unwrap()),
            (1..=10).collect::<Vec<u64>>()
        );
    }

    #[test]
    fn restart_reconstructs_the_commitment_tree_for_a_previously_unsigned_height() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let mut signer = signer_in(dir.path());
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 11, 4).unwrap();
        signer.sign(subnet, &[commitment], 11, &[1; 32]).unwrap();
        let expected_tree = eots::nonce_tree(&signer.seed, &subnet, &commitment).unwrap();
        drop(signer);
        let mut restarted = signer_in(dir.path());
        let vote = restarted.sign(subnet, &[commitment], 13, &[3; 32]).unwrap();
        assert_eq!(
            vote.signature[0],
            eots::public_nonces(&restarted.seed, &subnet, 13, 1).unwrap()[0]
        );
        assert_eq!(vote.proof, expected_tree.audit_path(2).unwrap());
        eots::verify_vote(&subnet, &vote, &commitment).unwrap();
        assert!(restarted.has_tree(subnet, &commitment));
    }

    #[test]
    fn separate_key_stores_rebuild_the_same_seed_commitment_without_sharing_vote_records() {
        let dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let mut a = signer_in(dir.path());
        let mut b = EotsSigner::load(
            SecretKey::from_slice(&[8; 32]).unwrap(),
            seed(),
            &other_dir.path().join("other.sk"),
        )
        .unwrap();
        let (commitment, _) = eots::commit_nonces(&a.seed, &subnet, 11, 4).unwrap();
        let first = a.sign(subnet, &[commitment], 12, &[5; 32]).unwrap();
        let second = b.sign(subnet, &[commitment], 12, &[6; 32]).unwrap();
        assert_eq!(first.signature[0], second.signature[0]);
        assert_eq!(first.proof, second.proof);
        assert_ne!(first.pk_eots, second.pk_eots);
        eots::verify_vote(&subnet, &first, &commitment).unwrap();
        eots::verify_vote(&subnet, &second, &commitment).unwrap();
        assert_eq!(a.signed_from(subnet, 0, 10).unwrap(), vec![first]);
        assert_eq!(b.signed_from(subnet, 0, 10).unwrap(), vec![second]);
    }

    /// The child of `a_crash_between_writes_loses_no_committed_vote`: signs the heights given
    /// in the environment and aborts without any cleanup.
    #[test]
    fn crash_child() {
        let Ok(dir) = std::env::var(CRASH_DIR_VAR) else {
            return;
        };
        // The key of a vote is the subnet id as displayed on the configured network: the child
        // and the parent must agree on it, set before `load`
        crate::eth_utils::set_fvm_network();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let mut signer = signer_in(Path::new(&dir));
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 1, 100).unwrap();
        for height in 1..=50 {
            let vote = signer
                .sign(subnet, &[commitment], height, &[height as u8; 32])
                .unwrap();
            // The vote is on disk once `sign` returns: tell the parent, then crash after some
            println!("signed {height} {}", hex::encode(vote.signature[1]));
            if height == 50 {
                std::process::abort();
            }
        }
    }

    #[test]
    fn a_crash_between_writes_loses_no_committed_vote() {
        let dir = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "provider::eots_signer::tests::crash_child",
                "--nocapture",
            ])
            .env(CRASH_DIR_VAR, dir.path())
            .output()
            .unwrap();
        assert!(!output.status.success(), "the child must crash");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let signed: Vec<(u64, String)> = stdout
            .lines()
            .filter_map(|l| l.strip_prefix("signed "))
            .map(|l| {
                let (h, s) = l.split_once(' ').unwrap();
                (h.parse().unwrap(), s.to_string())
            })
            .collect();
        assert_eq!(signed.len(), 50, "child output: {stdout}");

        // Every vote the child reported is readable and unchanged, the next height is free, and
        // a second block at a signed height is refused
        crate::eth_utils::set_fvm_network();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let mut signer = signer_in(dir.path());
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 1, 100).unwrap();
        for (height, s) in &signed {
            let vote = signer
                .signed(subnet, *height)
                .unwrap()
                .unwrap_or_else(|| panic!("height {height} lost, child output: {stdout}"));
            assert_eq!(&hex::encode(vote.signature[1]), s);
            assert_eq!(vote.block_hash, [*height as u8; 32]);
            eots::verify_vote(&subnet, &vote, &commitment).unwrap();
            assert!(matches!(
                signer.sign(subnet, &[commitment], *height, &[0xffu8; 32]),
                Err(EotsSignerError::DoubleVote { .. })
            ));
        }
        assert_eq!(signer.signed(subnet, 51).unwrap(), None);
        signer.sign(subnet, &[commitment], 51, &[51u8; 32]).unwrap();
    }

    fn sign_history(signer: &mut EotsSigner, subnet: SubnetId, count: u64) {
        const RANGE: u32 = 1000;
        let mut start = 1;
        while start <= count {
            let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, start, RANGE).unwrap();
            for height in start..(start + u64::from(RANGE)).min(count + 1) {
                signer
                    .sign(
                        subnet,
                        &[commitment],
                        height,
                        &height.to_le_bytes().repeat(4).try_into().unwrap(),
                    )
                    .unwrap();
            }
            start += u64::from(RANGE);
        }
    }

    fn check_history(dir: &Path, subnet: SubnetId, count: u64) {
        let mut signer = signer_in(dir);
        for height in [1, count / 2, count] {
            let vote = signer.signed(subnet, height).unwrap().unwrap();
            assert_eq!(vote.height, height);
            assert!(matches!(
                signer.sign(subnet, &[], height, &[0xffu8; 32]),
                Err(EotsSignerError::DoubleVote { .. })
            ));
        }
        assert_eq!(signer.signed(subnet, count + 1).unwrap(), None);
        assert_eq!(
            signer.signed_from(subnet, 0, usize::MAX).unwrap().len() as u64,
            count
        );
        assert_eq!(signer.signed_from(subnet, count - 2, 10).unwrap().len(), 3);
    }

    #[test]
    fn a_long_history_stays_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let count = 3000;
        sign_history(&mut signer_in(dir.path()), subnet, count);
        check_history(dir.path(), subnet, count);
    }

    /// A commitment of the signer's seed with the signer's subtree size.
    fn commitment_of(
        signer: &EotsSigner,
        subnet: SubnetId,
        start: u64,
        count: u32,
    ) -> (EotsNonceCommitment, SubtreeRoots) {
        let roots = eots::subtree_roots(
            &signer.seed,
            &subnet,
            start,
            count,
            signer.trees.subtree_log2,
        )
        .unwrap();
        let root = roots.root().unwrap();
        (EotsNonceCommitment { start, count, root }, roots)
    }

    #[test]
    fn subtree_roots_file_is_written_at_the_join_and_read_at_a_start() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[5u8; 20]);
        let mut signer = signer_in(dir.path());
        signer.trees.subtree_log2 = 2;
        let (commitment, roots) = commitment_of(&signer, subnet, 1, 10);
        signer
            .keep_subtree_roots(subnet, &commitment, roots.clone())
            .unwrap();
        assert!(signer.has_tree(subnet, &commitment));
        let path = EotsSigner::roots_path(signer.key_path(), subnet, &commitment);
        assert!(path.is_file());
        assert_eq!(path.parent(), Some(dir.path()));
        // Read, not derived: another seed does not give the commitment root
        let other = EotsSeed::from_bytes([41u8; 32]);
        assert_eq!(
            EotsSigner::subtree_roots(signer.key_path(), &other, subnet, &commitment, 2).unwrap(),
            roots
        );
        // Another subtree size is a wrong file: derived again
        assert!(matches!(
            EotsSigner::subtree_roots(signer.key_path(), &other, subnet, &commitment, 3),
            Err(EotsSignerError::Eots(EotsError::SubtreeMismatch))
        ));
        let wider =
            EotsSigner::subtree_roots(signer.key_path(), &signer.seed, subnet, &commitment, 3)
                .unwrap();
        assert_eq!(wider.root(), Some(commitment.root));
        assert_eq!(wider.subtree_count(), 2);
    }

    #[test]
    fn a_missing_or_wrong_subtree_roots_file_is_derived_again() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[6u8; 20]);
        let signer = signer_in(dir.path());
        let log2 = signer.trees.subtree_log2;
        let (commitment, roots) = commitment_of(&signer, subnet, 7, 9);
        let path = EotsSigner::roots_path(signer.key_path(), subnet, &commitment);
        assert!(!path.exists());
        let read = || {
            EotsSigner::subtree_roots(signer.key_path(), &signer.seed, subnet, &commitment, log2)
                .unwrap()
        };
        assert_eq!(read(), roots);
        assert!(path.is_file());
        let written = std::fs::read(&path).unwrap();
        let (other, other_roots) = commitment_of(&signer, subnet, 8, 9);
        EotsSigner::write_subtree_roots(&path, subnet, &other, &other_roots).unwrap();
        for wrong in [
            b"not json".to_vec(),
            written[..written.len() / 2].to_vec(),
            std::fs::read(&path).unwrap(),
        ] {
            std::fs::write(&path, wrong).unwrap();
            assert_eq!(read(), roots);
            assert_eq!(std::fs::read(&path).unwrap(), written);
        }
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read(), roots);
        assert_eq!(std::fs::read(&path).unwrap(), written);
    }

    #[test]
    fn a_roots_file_with_too_few_roots_is_derived_again() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[14u8; 20]);
        let mut signer = signer_in(dir.path());
        signer.trees.subtree_log2 = 2;
        let (commitment, roots) = commitment_of(&signer, subnet, 1, 5);
        let path = EotsSigner::roots_path(signer.key_path(), subnet, &commitment);
        // Two subtrees, but the commitment root alone gives the commitment root too
        let mut record = serde_json::to_value(SubtreeRootsFile {
            subnet_id: subnet,
            commitment,
            roots: roots.clone(),
        })
        .unwrap();
        record["roots"]["roots"] = serde_json::json!([commitment.root]);
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(EotsSigner::read_subtree_roots(&path, subnet, &commitment, 2).is_err());
        assert_eq!(
            EotsSigner::subtree_roots(signer.key_path(), &signer.seed, subnet, &commitment, 2)
                .unwrap(),
            roots
        );
        assert_eq!(
            EotsSigner::read_subtree_roots(&path, subnet, &commitment, 2).unwrap(),
            roots
        );
    }

    #[test]
    fn signing_crosses_subtree_boundaries_with_the_next_subtree_built_ahead() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[7u8; 20]);
        let mut signer = signer_in(dir.path());
        signer.trees.subtree_log2 = 2;
        let (commitment, roots) = commitment_of(&signer, subnet, 1, 12);
        signer
            .keep_subtree_roots(subnet, &commitment, roots)
            .unwrap();
        assert_eq!(signer.next_height(subnet, 1).unwrap(), Some(1));
        let key = (subnet, commitment);
        let whole = {
            let leaves = eots::public_nonces(&signer.seed, &subnet, 1, 12).unwrap();
            CommitmentTree::from_leaves(&leaves, 2)
        };
        for height in std::iter::once(1).chain(3..=10u64).chain([2]) {
            let vote = signer
                .sign(subnet, &[commitment], height, &[height as u8; 32])
                .unwrap();
            eots::verify_vote(&subnet, &vote, &commitment).unwrap();
            assert_eq!(vote.proof, whole.audit_path((height - 1) as usize).unwrap());
            check_held(&signer, subnet, &commitment, (height - 1) as usize / 4);
        }
        assert_eq!(signer.next_height(subnet, 1).unwrap(), Some(11));
        drop(signer);
        let mut restarted = signer_in(dir.path());
        restarted.trees.subtree_log2 = 2;
        let vote = restarted
            .sign(subnet, &[commitment], 12, &[12; 32])
            .unwrap();
        eots::verify_vote(&subnet, &vote, &commitment).unwrap();
        assert_eq!(vote.proof, whole.audit_path(11).unwrap());
        let tree = &restarted.trees.held[&key];
        assert!(tree.holds_subtree(2) && !tree.holds_subtree(0) && !tree.holds_subtree(1));
    }

    /// Of the tree of `commitment`, subtree `j` and the next one are held or building, no other.
    fn check_held(
        signer: &EotsSigner,
        subnet: SubnetId,
        commitment: &EotsNonceCommitment,
        j: usize,
    ) {
        let tree = &signer.trees.held[&(subnet, *commitment)];
        assert!(tree.holds_subtree(j));
        let mut kept = tree.held_subtrees();
        kept.extend(
            signer
                .trees
                .builds
                .keys()
                .filter(|&&(s, c, _)| (s, c) == (subnet, *commitment))
                .map(|&(_, _, k)| k),
        );
        kept.sort();
        let next = (j + 1 < tree.roots().subtree_count()).then_some(j + 1);
        assert_eq!(
            kept,
            [Some(j), next].into_iter().flatten().collect::<Vec<_>>()
        );
    }

    /// Waits until every subtree build of `signer` has ended.
    fn wait_for_builds(signer: &EotsSigner) {
        while !signer.trees.builds.values().all(|b| b.is_finished()) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn signing_skipped_subtrees_keeps_only_the_current_and_next_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[8u8; 20]);
        let mut signer = signer_in(dir.path());
        signer.trees.subtree_log2 = 2;
        let (commitment, roots) = commitment_of(&signer, subnet, 1, 40);
        signer
            .keep_subtree_roots(subnet, &commitment, roots)
            .unwrap();
        // One height of every second subtree, each after the build of the next subtree ended
        for height in (1..=40u64).step_by(8) {
            wait_for_builds(&signer);
            let vote = signer
                .sign(subnet, &[commitment], height, &[height as u8; 32])
                .unwrap();
            eots::verify_vote(&subnet, &vote, &commitment).unwrap();
            check_held(&signer, subnet, &commitment, (height - 1) as usize / 4);
        }
    }

    #[test]
    fn a_join_with_another_range_from_the_same_start_leaves_the_signing_tree() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[9u8; 20]);
        let mut signer = signer_in(dir.path());
        signer.trees.subtree_log2 = 2;
        let (recorded, recorded_roots) = commitment_of(&signer, subnet, 1, 8);
        let (refused, refused_roots) = commitment_of(&signer, subnet, 1, 40);
        let whole = {
            let leaves = eots::public_nonces(&signer.seed, &subnet, 1, 8).unwrap();
            CommitmentTree::from_leaves(&leaves, 2)
        };
        // The refused join before and after the recorded one, and between its votes
        signer
            .keep_subtree_roots(subnet, &refused, refused_roots.clone())
            .unwrap();
        signer
            .keep_subtree_roots(subnet, &recorded, recorded_roots)
            .unwrap();
        for height in 1..=8u64 {
            let vote = signer
                .sign(subnet, &[recorded], height, &[height as u8; 32])
                .unwrap();
            eots::verify_vote(&subnet, &vote, &recorded).unwrap();
            assert_eq!(vote.proof, whole.audit_path((height - 1) as usize).unwrap());
            check_held(&signer, subnet, &recorded, (height - 1) as usize / 4);
            signer
                .keep_subtree_roots(subnet, &refused, refused_roots.clone())
                .unwrap();
        }
        assert!(signer.trees.held[&(subnet, refused)]
            .held_subtrees()
            .is_empty());
    }

    #[test]
    fn concurrent_derivations_of_subtree_roots_leave_whole_files() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[10u8; 20]);
        let mut signer = signer_in(dir.path());
        signer.trees.subtree_log2 = 2;
        let key_path = signer.key_path().to_path_buf();
        let commitments = [
            commitment_of(&signer, subnet, 1, 40),
            commitment_of(&signer, subnet, 1, 44),
        ];
        let signer = &Mutex::new(signer);
        let barrier = &std::sync::Barrier::new(8);
        // Per commitment, three starts and a vote, from no roots file
        std::thread::scope(|s| {
            for (i, (commitment, _)) in commitments.iter().enumerate() {
                for _ in 0..3 {
                    s.spawn(move || {
                        barrier.wait();
                        EotsSigner::hold_tree(&signer, subnet, commitment).unwrap();
                    });
                }
                s.spawn(move || {
                    barrier.wait();
                    let height = i as u64 + 1;
                    let vote = signer
                        .lock()
                        .unwrap()
                        .sign(subnet, &[*commitment], height, &[height as u8; 32])
                        .unwrap();
                    eots::verify_vote(&subnet, &vote, commitment).unwrap();
                });
            }
        });
        for (commitment, roots) in &commitments {
            let path = EotsSigner::roots_path(&key_path, subnet, commitment);
            assert_eq!(
                &EotsSigner::read_subtree_roots(&path, subnet, commitment, 2).unwrap(),
                roots
            );
        }
        // The vote database and the two roots files, no file of a write left
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn verified_subtree_roots_sign_when_their_file_cannot_be_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys");
        std::fs::create_dir(&keys).unwrap();
        let subnet = SubnetId::from_txid20(&[11u8; 20]);
        let mut signer = signer_in(&keys);
        signer.trees.subtree_log2 = 2;
        let (recorded, _) = commitment_of(&signer, subnet, 1, 10);
        let (joined, joined_roots) = commitment_of(&signer, subnet, 11, 10);
        let set_mode =
            |mode| std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(mode)).unwrap();
        // The key directory read-only, the vote database in it writable
        set_mode(0o555);
        if NamedTempFile::new_in(&keys).is_ok() {
            set_mode(0o755);
            eprintln!("skipped: the key directory stays writable for this user");
            return;
        }
        let vote = signer.sign(subnet, &[recorded], 5, &[5; 32]);
        let join = signer.keep_subtree_roots(subnet, &joined, joined_roots);
        set_mode(0o755);
        eots::verify_vote(&subnet, &vote.unwrap(), &recorded).unwrap();
        assert!(!EotsSigner::roots_path(signer.key_path(), subnet, &recorded).exists());
        assert!(matches!(join, Err(EotsSignerError::RootsFile(_))));
        assert!(!signer.has_tree(subnet, &joined));
    }

    #[test]
    fn a_vote_at_the_last_height_leaves_no_subtree_to_hold_at_a_start() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[12u8; 20]);
        let other = SubnetId::from_txid20(&[13u8; 20]);
        let mut signer = signer_in(dir.path());
        signer.trees.subtree_log2 = 2;
        let (largest, _) = commitment_of(&signer, subnet, u64::MAX, 1);
        let (ended, _) = commitment_of(&signer, other, 1, 8);
        signer.sign(subnet, &[largest], u64::MAX, &[1; 32]).unwrap();
        signer.sign(other, &[ended], 8, &[8; 32]).unwrap();
        assert_eq!(signer.next_height(subnet, largest.start).unwrap(), None);
        assert_eq!(signer.next_height(other, ended.start).unwrap(), Some(9));
        drop(signer);
        let mut restarted = signer_in(dir.path());
        restarted.trees.subtree_log2 = 2;
        let restarted = Mutex::new(restarted);
        EotsSigner::hold_tree(&restarted, subnet, &largest).unwrap();
        EotsSigner::hold_tree(&restarted, other, &ended).unwrap();
        let restarted = restarted.into_inner().unwrap();
        for (subnet, commitment) in [(subnet, largest), (other, ended)] {
            assert!(restarted.trees.held[&(subnet, commitment)]
                .held_subtrees()
                .is_empty());
        }
    }
}
