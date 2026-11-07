use super::{EotsSigner, EotsSignerError};
use crate::eots::{public_nonces, EotsNonceCommitment, EotsSeed, NonceTree};
use crate::ipc_lib::SubnetId;
use bitcoin::secp256k1::SecretKey;

struct Fixture {
    _dir: tempfile::TempDir,
    key_path: std::path::PathBuf,
    key: SecretKey,
    seed: EotsSeed,
    subnet: SubnetId,
    commitment: EotsNonceCommitment,
}

impl Fixture {
    fn new() -> Self {
        // Vote records are keyed by SubnetId's string form, which depends on this process-global.
        crate::eth_utils::set_fvm_network();
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("eots.sk");
        let key = SecretKey::from_slice(&[7; 32]).unwrap();
        let seed = EotsSeed::from_bytes([70; 32]);
        let subnet = SubnetId::from_txid20(&[42; 20]);
        let tree = NonceTree::new(&public_nonces(&seed, &subnet, 11, 4));
        Self {
            _dir: dir,
            key_path,
            key,
            seed,
            subnet,
            commitment: EotsNonceCommitment {
                start: 11,
                count: 4,
                root: tree.root().unwrap(),
            },
        }
    }

    fn load(&self) -> EotsSigner {
        EotsSigner::load(self.key, self.seed, &self.key_path).unwrap()
    }

    fn records(&self) -> Vec<crate::eots::EotsVote> {
        self.load().signed_from(self.subnet, 0, usize::MAX).unwrap()
    }

    fn legacy_vote(&self, height: u64, hash: [u8; 32]) -> crate::eots::EotsVote {
        let tree = NonceTree::new(&public_nonces(&self.seed, &self.subnet, 11, 4));
        crate::eots::sign_vote(
            &self.key,
            &self.seed,
            &self.subnet,
            &self.commitment,
            &tree,
            height,
            &hash,
        )
        .unwrap()
    }

    fn write_legacy(&self, votes: &[crate::eots::EotsVote]) -> std::path::PathBuf {
        let path = self.key_path.with_file_name("eots.sk.votes");
        let mut text = String::new();
        for vote in votes {
            text.push_str(
                &serde_json::to_string(&serde_json::json!({
                    "subnet_id": self.subnet, "vote": vote,
                }))
                .unwrap(),
            );
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();
        path
    }
}

// slashing.md, The vote: republication after restart returns the complete stored vote, with no new nonce.
#[test]
fn retry_after_restart_returns_the_same_vote_without_adding_a_record() {
    let f = Fixture::new();
    let original = f
        .load()
        .sign(f.subnet, &[f.commitment], 12, &[9; 32])
        .unwrap();
    assert!(!original.proof.is_empty());
    let record = f.records();
    for _ in 0..2 {
        let mut restarted = f.load();
        assert_eq!(
            restarted.signed(f.subnet, 12).unwrap(),
            Some(original.clone())
        );
        // The stored proof remains sufficient when no commitment is supplied on a retry.
        assert_eq!(
            restarted.sign(f.subnet, &[], 12, &[9; 32]).unwrap(),
            original
        );
        assert_eq!(
            restarted.signed_from(f.subnet, 0, usize::MAX).unwrap(),
            record
        );
    }
}

// slashing.md, EOTS key and EOTS nonce commitments / Final blocks: no recorded height can sign another block.
#[test]
fn restart_rejects_a_conflicting_vote_at_every_previously_signed_height() {
    let f = Fixture::new();
    let mut signer = f.load();
    let first = signer
        .sign(f.subnet, &[f.commitment], 11, &[1; 32])
        .unwrap();
    let last = signer
        .sign(f.subnet, &[f.commitment], 14, &[2; 32])
        .unwrap();
    drop(signer);
    let record = f.records();
    let mut restarted = f.load();
    for original in [&first, &last] {
        assert!(matches!(
            restarted.sign(f.subnet, &[f.commitment], original.height, &[3; 32]),
            Err(EotsSignerError::DoubleVote { subnet_id, height })
                if subnet_id == f.subnet && height == original.height
        ));
        assert_eq!(
            restarted.signed(f.subnet, original.height).unwrap(),
            Some(original.clone())
        );
        assert_eq!(
            restarted.signed_from(f.subnet, 0, usize::MAX).unwrap(),
            record
        );
    }
    drop(restarted);
    let reloaded = f.load();
    assert_eq!(reloaded.signed(f.subnet, 11).unwrap(), Some(first.clone()));
    assert_eq!(reloaded.signed(f.subnet, 14).unwrap(), Some(last.clone()));
}

// slashing.md, EOTS key and EOTS nonce commitments: new votes after restart preserve every earlier complete vote.
#[test]
fn signing_a_new_height_after_restart_keeps_the_old_record() {
    let f = Fixture::new();
    let first = f
        .load()
        .sign(f.subnet, &[f.commitment], 11, &[1; 32])
        .unwrap();
    let record = f.records();
    let second = f
        .load()
        .sign(f.subnet, &[f.commitment], 12, &[2; 32])
        .unwrap();
    let extended = f.records();
    assert_eq!(record, vec![first.clone()]);
    assert_eq!(extended, vec![first.clone(), second.clone()]);
    let reloaded = f.load();
    assert_eq!(reloaded.signed(f.subnet, 11).unwrap(), Some(first.clone()));
    assert_eq!(reloaded.signed(f.subnet, 12).unwrap(), Some(second.clone()));
}

// slashing.md, Final blocks: restart gives no permission to vote outside a committed nonce range.
#[test]
fn restarted_signer_rejects_uncommitted_heights_without_changing_the_record() {
    let f = Fixture::new();
    f.load()
        .sign(f.subnet, &[f.commitment], 12, &[1; 32])
        .unwrap();
    let record = f.records();
    let mut restarted = f.load();
    for height in [10, 15] {
        assert!(matches!(
            restarted.sign(f.subnet, &[f.commitment], height, &[2; 32]),
            Err(EotsSignerError::NotCommitted(h)) if h == height
        ));
        assert!(restarted.signed(f.subnet, height).unwrap().is_none());
        assert_eq!(
            restarted.signed_from(f.subnet, 0, usize::MAX).unwrap(),
            record
        );
    }
}

// slashing.md, EOTS key and EOTS nonce commitments / storage migration: every complete legacy vote survives import.
#[test]
fn importing_a_legacy_record_keeps_votes_and_double_vote_protection_across_restarts() {
    let f = Fixture::new();
    let first = f.legacy_vote(11, [1; 32]);
    let last = f.legacy_vote(14, [2; 32]);
    let path = f.write_legacy(&[first.clone(), last.clone()]);
    for _ in 0..2 {
        let mut signer = f.load();
        assert_eq!(
            signer.signed_from(f.subnet, 0, 10).unwrap(),
            vec![first.clone(), last.clone()]
        );
        for vote in [&first, &last] {
            assert_eq!(
                signer
                    .sign(f.subnet, &[], vote.height, &vote.block_hash)
                    .unwrap(),
                *vote
            );
            assert!(matches!(
                signer.sign(f.subnet, &[f.commitment], vote.height, &[3; 32]),
                Err(EotsSignerError::DoubleVote { .. })
            ));
        }
    }
    assert!(!path.exists());
    assert!(f
        .key_path
        .with_file_name("eots.sk.votes.imported")
        .is_file());
    assert!(f.key_path.with_file_name("eots.sk.votes.db").is_dir());
}

// slashing.md, The vote: importing history cannot replace a vote that was already published from the database.
#[test]
fn a_conflicting_legacy_record_cannot_overwrite_the_database() {
    let f = Fixture::new();
    let first = f
        .load()
        .sign(f.subnet, &[f.commitment], 11, &[1; 32])
        .unwrap();
    let conflict = f.legacy_vote(11, [2; 32]);
    let legacy = f.write_legacy(&[conflict]);
    assert!(EotsSigner::load(f.key, f.seed, &f.key_path).is_err());
    assert!(legacy.is_file(), "a failed import must retain its source");
    std::fs::remove_file(legacy).unwrap();
    assert_eq!(f.records(), vec![first]);
}

// slashing.md, EOTS key and EOTS nonce commitments: the same complete record is retained through paginated reads.
#[test]
fn the_database_lists_complete_votes_in_height_order_with_inclusive_pagination() {
    let f = Fixture::new();
    let mut signer = f.load();
    let last = signer
        .sign(f.subnet, &[f.commitment], 14, &[2; 32])
        .unwrap();
    let first = signer
        .sign(f.subnet, &[f.commitment], 11, &[1; 32])
        .unwrap();
    assert_eq!(
        signer.signed_from(f.subnet, 0, 1).unwrap(),
        vec![first.clone()]
    );
    assert_eq!(
        signer.signed_from(f.subnet, 12, 1).unwrap(),
        vec![last.clone()]
    );
    assert_eq!(
        signer.signed_from(f.subnet, 14, 1).unwrap(),
        vec![last.clone()]
    );
    assert!(signer.signed_from(f.subnet, 15, 1).unwrap().is_empty());
    assert!(signer.signed_from(f.subnet, 0, 0).unwrap().is_empty());
    drop(signer);
    assert_eq!(f.records(), vec![first, last]);
}

// slashing.md, EOTS key and EOTS nonce commitments: a restarted signer rebuilds proofs for heights never signed before.
#[test]
fn restart_reconstructs_the_commitment_tree_for_a_previously_unsigned_height() {
    let f = Fixture::new();
    f.load()
        .sign(f.subnet, &[f.commitment], 11, &[1; 32])
        .unwrap();
    let expected_tree = crate::eots::nonce_tree(&f.seed, &f.subnet, &f.commitment);
    let mut restarted = f.load();
    let vote = restarted
        .sign(f.subnet, &[f.commitment], 13, &[3; 32])
        .unwrap();
    assert_eq!(
        vote.signature[0],
        crate::eots::public_nonces(&f.seed, &f.subnet, 13, 1)[0]
    );
    assert_eq!(vote.proof, expected_tree.audit_path(2).unwrap());
    crate::eots::verify_vote(&f.subnet, &vote, &f.commitment).unwrap();
    assert!(restarted.has_tree(f.subnet, &f.commitment));
}

// slashing.md, EOTS key and EOTS nonce commitments: rebuilding the same seed's tree is independent of the signing key.
#[test]
fn separate_key_stores_rebuild_the_same_seed_commitment_without_sharing_vote_records() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let mut a = f.load();
    let mut b = EotsSigner::load(
        SecretKey::from_slice(&[8; 32]).unwrap(),
        f.seed,
        &dir.path().join("other.sk"),
    )
    .unwrap();
    let first = a.sign(f.subnet, &[f.commitment], 12, &[5; 32]).unwrap();
    let second = b.sign(f.subnet, &[f.commitment], 12, &[6; 32]).unwrap();
    assert_eq!(first.signature[0], second.signature[0]);
    assert_eq!(first.proof, second.proof);
    assert_ne!(first.pk_eots, second.pk_eots);
    crate::eots::verify_vote(&f.subnet, &first, &f.commitment).unwrap();
    crate::eots::verify_vote(&f.subnet, &second, &f.commitment).unwrap();
    assert_eq!(a.signed_from(f.subnet, 0, 10).unwrap(), vec![first]);
    assert_eq!(b.signed_from(f.subnet, 0, 10).unwrap(), vec![second]);
}
