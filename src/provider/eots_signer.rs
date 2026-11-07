//! The validator's EOTS signer: signs one vote per subnet height with the committed nonce and
//! keeps every vote it signed in a database next to the key file, so that no height is ever
//! signed twice. A vote is committed to disk before it is returned, and old votes stay on disk:
//! neither memory nor the start time grows with the history.

use std::collections::BTreeMap;
use std::ops::{Bound, RangeBounds};
use std::path::{Path, PathBuf};

use bitcoin::secp256k1::{Secp256k1, SecretKey, XOnlyPublicKey};
use heed::types::{SerdeBincode, Str};
use heed::{Database, Env, EnvOpenOptions};
use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::eots::{self, EotsNonceCommitment, EotsSeed, EotsVote, NonceTree};
use crate::ipc_lib::SubnetId;

/// One line of the sign record file that preceded the database, `<key file>.votes`.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct SignedVoteRecord {
    subnet_id: SubnetId,
    vote: EotsVote,
}

/// Address space reserved for the vote database: the file only holds the votes written, about
/// 60 MB a day at one subnet block per second.
const MAP_SIZE: usize = 128 * 1024 * 1024 * 1024;
const VOTES_DB: &str = "votes";

/// The database key of a vote: string order equals height order within a subnet.
fn vote_key(subnet_id: SubnetId, height: u64) -> String {
    format!("{subnet_id}:{height:020}")
}

/// The keys of one subnet from a height on: the standard ranges cannot bound unsized `str` keys.
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
    /// The seed of every nonce the key commits to
    seed: EotsSeed,
    pk_eots: XOnlyPublicKey,
    env: Env,
    /// Every vote signed, by subnet and height
    votes: Database<Str, SerdeBincode<EotsVote>>,
    /// The nonce trees of the commitments signed so far, by subnet and start
    trees: BTreeMap<(SubnetId, u64), NonceTree>,
}

#[derive(Debug, thiserror::Error)]
pub enum EotsSignerError {
    #[error("height {height} of subnet {subnet_id} is already signed for another block")]
    DoubleVote { subnet_id: SubnetId, height: u64 },
    #[error("no EOTS nonce commitment of this validator covers height {0}")]
    NotCommitted(u64),
    #[error("sign record: {0}")]
    Io(#[from] std::io::Error),
    #[error("vote database: {0}")]
    Db(#[from] heed::Error),
    #[error("sign record line: {0}")]
    Record(String),
    #[error(transparent)]
    Eots(#[from] eots::EotsError),
}

impl EotsSigner {
    /// Opens the vote database next to the key file, `<key file>.votes.db`, and imports the
    /// sign record file `<key file>.votes` of an earlier version if one is present.
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
        let signer = Self {
            pk_eots: secret_key.x_only_public_key(&secp).0,
            secret_key,
            seed,
            env,
            votes,
            trees: BTreeMap::new(),
        };
        signer.import_record_file(&PathBuf::from(format!("{}.votes", key_path.display())))?;
        Ok(signer)
    }

    /// Imports every vote of the record file into the database in one transaction, then renames
    /// the file to `<file>.imported`. A partial last line is a write that a crash cut before the
    /// vote left the provider: it is skipped. A vote that differs from the database at its height
    /// is an error, as is any other malformed line.
    fn import_record_file(&self, path: &Path) -> Result<(), EotsSignerError> {
        if !path.exists() {
            return Ok(());
        }
        let text = std::fs::read_to_string(path)?;
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut txn = self.env.write_txn()?;
        let mut imported = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let record: SignedVoteRecord = match serde_json::from_str(line) {
                Ok(record) => record,
                Err(e) if i + 1 == lines.len() && !text.ends_with('\n') => {
                    warn!(
                        "sign record {}: partial last line skipped: {e}",
                        path.display()
                    );
                    break;
                }
                Err(e) => return Err(EotsSignerError::Record(e.to_string())),
            };
            let key = vote_key(record.subnet_id, record.vote.height);
            match self.votes.get(&txn, &key)? {
                Some(stored) if stored != record.vote => {
                    return Err(EotsSignerError::Record(format!(
                        "height {} of subnet {} differs from the vote database",
                        record.vote.height, record.subnet_id
                    )));
                }
                Some(_) => {}
                None => {
                    self.votes.put(&mut txn, &key, &record.vote)?;
                    imported += 1;
                }
            }
        }
        txn.commit()?;
        std::fs::rename(path, format!("{}.imported", path.display()))?;
        info!(
            "sign record {}: {imported} votes imported into the vote database",
            path.display()
        );
        Ok(())
    }

    pub fn pk_eots(&self) -> XOnlyPublicKey {
        self.pk_eots
    }

    pub fn seed(&self) -> EotsSeed {
        self.seed
    }

    /// Whether the tree of `commitment` on `subnet_id` is built.
    pub fn has_tree(&self, subnet_id: SubnetId, commitment: &EotsNonceCommitment) -> bool {
        self.trees.contains_key(&(subnet_id, commitment.start))
    }

    /// Keeps a tree built outside the lock, by `eots::nonce_tree` with this signer's seed.
    pub fn insert_tree(&mut self, subnet_id: SubnetId, start: u64, tree: NonceTree) {
        self.trees.insert((subnet_id, start), tree);
    }

    /// The vote signed at `height`, if any.
    pub fn signed(
        &self,
        subnet_id: SubnetId,
        height: u64,
    ) -> Result<Option<EotsVote>, EotsSignerError> {
        let txn = self.env.read_txn()?;
        Ok(self.votes.get(&txn, &vote_key(subnet_id, height))?)
    }

    /// The votes of `subnet_id` from `from_height` on, in height order, at most `limit`.
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

    /// Signs the vote for `(height, block_hash)` with the nonce committed in the commitment of
    /// `commitments` whose range holds `height`. A height already signed for the same block
    /// gives the stored vote again, for another block an error. The vote is committed to the
    /// database before it is returned.
    pub fn sign(
        &mut self,
        subnet_id: SubnetId,
        commitments: &[EotsNonceCommitment],
        height: u64,
        block_hash: &[u8; 32],
    ) -> Result<EotsVote, EotsSignerError> {
        let key = vote_key(subnet_id, height);
        // The check and the write share one transaction, so no second vote can slip in between
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
    use std::io::Write;
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

    fn record_line(subnet_id: SubnetId, vote: &EotsVote) -> String {
        serde_json::to_string(&SignedVoteRecord {
            subnet_id,
            vote: vote.clone(),
        })
        .unwrap()
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
        // Outside the committed range
        assert!(matches!(
            signer.sign(subnet, &[commitment], 15, &[1u8; 32]),
            Err(EotsSignerError::NotCommitted(15))
        ));
        // Another subnet at the same height is independent
        let other = SubnetId::from_txid20(&[5u8; 20]);
        let (other_commitment, _) = eots::commit_nonces(&signer.seed, &other, 1, 20);
        signer
            .sign(other, &[other_commitment], 12, &[9u8; 32])
            .unwrap();
        assert_eq!(signer.signed(other, 13).unwrap(), None);

        // After a restart the database still refuses the second block
        drop(signer);
        let mut reloaded = signer_in(dir.path());
        assert_eq!(reloaded.signed(subnet, 12).unwrap(), Some(vote.clone()));
        assert!(matches!(
            reloaded.sign(subnet, &[commitment], 12, &[2u8; 32]),
            Err(EotsSignerError::DoubleVote { height: 12, .. })
        ));
        assert_eq!(
            reloaded
                .sign(subnet, &[commitment], 12, &[1u8; 32])
                .unwrap(),
            vote
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
            heights(signer.signed_from(subnet, 21, 10).unwrap()),
            Vec::<u64>::new()
        );
        assert_eq!(heights(signer.signed_from(other, 0, 10).unwrap()), vec![10]);
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

    #[test]
    fn imports_the_record_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let sk = secret_key();
        let (commitment, _) = eots::commit_nonces(&seed(), &subnet, 1, 10);
        // Votes signed by an earlier version into the record file, through a signer in another
        // directory
        let other_dir = tempfile::tempdir().unwrap();
        let mut old = signer_in(other_dir.path());
        let votes: Vec<EotsVote> = [2u64, 3, 5]
            .iter()
            .map(|h| {
                old.sign(subnet, &[commitment], *h, &[*h as u8; 32])
                    .unwrap()
            })
            .collect();
        let record_path = dir.path().join("eots.sk.votes");
        let mut file = std::fs::File::create(&record_path).unwrap();
        for vote in &votes {
            writeln!(file, "{}", record_line(subnet, vote)).unwrap();
        }
        writeln!(file).unwrap();
        // A line cut by a crash during the last write
        write!(file, "{{\"subnet_id\":\"").unwrap();
        drop(file);

        let mut signer = signer_in(dir.path());
        assert!(
            !record_path.exists(),
            "the record file is renamed after the import"
        );
        assert!(dir.path().join("eots.sk.votes.imported").exists());
        for vote in &votes {
            assert_eq!(
                signer.signed(subnet, vote.height).unwrap().as_ref(),
                Some(vote)
            );
            assert!(matches!(
                signer.sign(subnet, &[commitment], vote.height, &[0xffu8; 32]),
                Err(EotsSignerError::DoubleVote { .. })
            ));
        }
        assert_eq!(signer.signed(subnet, 4).unwrap(), None);
        signer.sign(subnet, &[commitment], 4, &[4u8; 32]).unwrap();

        // A second start finds no record file and keeps everything
        drop(signer);
        let reloaded = signer_in(dir.path());
        assert_eq!(reloaded.signed_from(subnet, 0, 10).unwrap().len(), 4);

        // A record file whose vote differs from the database is refused
        let mut conflicting = old.sign(subnet, &[commitment], 4, &[4u8; 32]).unwrap();
        conflicting.block_hash = [9u8; 32];
        std::fs::write(
            &record_path,
            format!("{}\n", record_line(subnet, &conflicting)),
        )
        .unwrap();
        drop(reloaded);
        assert!(matches!(
            EotsSigner::load(sk, seed(), &dir.path().join("eots.sk")),
            Err(EotsSignerError::Record(_))
        ));
        // A malformed line that is not the last one is refused
        std::fs::write(&record_path, "{\"subnet_id\":\n\n").unwrap();
        assert!(matches!(
            EotsSigner::load(sk, seed(), &dir.path().join("eots.sk")),
            Err(EotsSignerError::Record(_))
        ));
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

    /// A day of votes at one block per second: `cargo test --lib a_day_of_votes -- --ignored`.
    #[test]
    #[ignore]
    fn a_day_of_votes() {
        let dir = tempfile::tempdir().unwrap();
        let subnet = SubnetId::from_txid20(&[3u8; 20]);
        let count = 86_400;
        sign_history(&mut signer_in(dir.path()), subnet, count);
        let started = std::time::Instant::now();
        check_history(dir.path(), subnet, count);
        println!(
            "restart and lookups after {count} votes: {:?}",
            started.elapsed()
        );
    }
}
#[cfg(test)]
#[path = "eots_signer_spec_tests.rs"]
mod spec_tests;
