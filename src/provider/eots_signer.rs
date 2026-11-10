//! The validator's EOTS signer: one vote per subnet height, the votes above the latest
//! checkpointed height kept in a database next to the key file.

use std::collections::BTreeMap;
use std::ops::{Bound, RangeBounds};
use std::path::{Path, PathBuf};

use bitcoin::secp256k1::{Secp256k1, SecretKey, XOnlyPublicKey};
use heed::types::{SerdeBincode, Str};
use heed::{Database, Env, EnvOpenOptions};

use crate::eots::{self, EotsNonceCommitment, EotsSeed, EotsVote, NonceTree};
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

pub struct EotsSigner {
    secret_key: SecretKey,
    seed: EotsSeed,
    pk_eots: XOnlyPublicKey,
    env: Env,
    votes: Database<Str, SerdeBincode<EotsVote>>,
    /// By subnet and start of the commitment
    trees: BTreeMap<(SubnetId, u64), NonceTree>,
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
    Eots(#[from] eots::EotsError),
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
            trees: BTreeMap::new(),
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
        self.trees.contains_key(&(subnet_id, commitment.start))
    }

    /// Keeps a tree built outside the lock, by `eots::nonce_tree` with this signer's seed.
    pub fn insert_tree(&mut self, subnet_id: SubnetId, start: u64, tree: NonceTree) {
        self.trees.insert((subnet_id, start), tree);
    }

    pub fn signed(
        &self,
        subnet_id: SubnetId,
        height: u64,
    ) -> Result<Option<EotsVote>, EotsSignerError> {
        let txn = self.env.read_txn()?;
        Ok(self.votes.get(&txn, &vote_key(subnet_id, height))?)
    }

    /// Deletes the votes of `subnet_id` at or below `height`, where the provider signs no vote.
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
        let tree = self
            .trees
            .entry((subnet_id, commitment.start))
            .or_insert_with(|| eots::nonce_tree(&self.seed, &subnet_id, commitment));
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
        // The database holds subnet ids as the provider writes them on its network (`t410...` on
        // regtest and testnet): loading them needs the FVM network set first, as the provider
        // binary does before `load`. A vote at a height must survive the restart.
        crate::eth_utils::set_fvm_network();
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[7u8; 20]);
        assert!(
            subnet.to_string().contains("/t410"),
            "regtest subnet ids are testnet addresses: {subnet}"
        );
        let mut signer = signer_in(dir.path());
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 1, 5);
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
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 10, 5);
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
        let (other_commitment, _) = eots::commit_nonces(&signer.seed, &other, 1, 20);
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
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 5, 20);
        let (other_commitment, _) = eots::commit_nonces(&signer.seed, &other, 5, 20);
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
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 11, 4);
        signer.sign(subnet, &[commitment], 11, &[1; 32]).unwrap();
        let expected_tree = eots::nonce_tree(&signer.seed, &subnet, &commitment);
        drop(signer);
        let mut restarted = signer_in(dir.path());
        let vote = restarted.sign(subnet, &[commitment], 13, &[3; 32]).unwrap();
        assert_eq!(
            vote.signature[0],
            eots::public_nonces(&restarted.seed, &subnet, 13, 1)[0]
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
        let (commitment, _) = eots::commit_nonces(&a.seed, &subnet, 11, 4);
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
        // and the parent must agree on it, as the provider sets it before `load`
        crate::eth_utils::set_fvm_network();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let mut signer = signer_in(Path::new(&dir));
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 1, 100);
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
        let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, 1, 100);
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
            let (commitment, _) = eots::commit_nonces(&signer.seed, &subnet, start, RANGE);
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
}
