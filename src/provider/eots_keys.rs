//! The EOTS keys a provider holds: one per subnet, generated with its nonce seed at the first
//! join to the subnet and kept next to the validator key file, and the key configured with
//! `EOTS_SK_PATH`, matched to the subnet whose join carries it.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use bitcoin::secp256k1::XOnlyPublicKey;
use bitcoin::{Transaction, Txid};
use log::info;
use serde::{Deserialize, Serialize};

use super::eots_signer::{EotsKeyFiles, EotsSigner, EotsSignerError};
use crate::ipc_lib::SubnetId;

pub type SharedSigner = Arc<Mutex<EotsSigner>>;

/// The commit and reveal transactions of a join.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinTransactions {
    pub commit: Transaction,
    pub reveal: Transaction,
}

/// The last join of this provider to a subnet, kept in `eots.<txid20>.join` next to the
/// validator key file: the EOTS key chosen for the subnet, written when it is chosen, the
/// prefund transaction of the join, written before it is sent and kept until a join to the
/// subnet is accepted, and the transactions built for the join, written before they are sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRecord {
    pub pk_eots: XOnlyPublicKey,
    pub prefund: Option<Transaction>,
    pub transactions: Option<JoinTransactions>,
}

impl JoinRecord {
    /// The txid of the reveal transaction, the join txid, once the transactions are built.
    pub fn txid(&self) -> Option<Txid> {
        self.transactions.as_ref().map(|t| t.reveal.compute_txid())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EotsKeysError {
    #[error(transparent)]
    Signer(#[from] EotsSignerError),
    #[error("key directory {path}: {reason}")]
    Dir { path: PathBuf, reason: String },
    #[error("join record {path}: {reason}")]
    JoinRecord { path: PathBuf, reason: String },
    #[error("recorded EOTS nonce commitments: {0}")]
    Records(String),
}

/// The directory of the validator key file, which holds the files of each subnet: the current
/// directory for a bare file name.
pub fn key_dir(key_file: &Path) -> PathBuf {
    match key_file.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// The file `eots.<txid20>.<ext>` of `subnet_id` in `dir`.
pub fn subnet_file(dir: &Path, subnet_id: SubnetId, ext: &str) -> PathBuf {
    dir.join(format!("eots.{}.{ext}", hex::encode(subnet_id.txid20())))
}

/// Writes `contents` to `path`, complete and on disk before it appears there: through
/// `<path>.tmp`, synced, then renamed, and the directory synced. The file is readable by its
/// owner only.
pub fn write_durably(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = key_dir(path);
    std::fs::create_dir_all(&dir)?;
    let mut name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name"))?
        .to_os_string();
    name.push(".tmp");
    let tmp = dir.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(contents)?;
    sync_all(&file, &dir, SyncStep::File)?;
    std::fs::rename(&tmp, path)?;
    sync_all(&std::fs::File::open(&dir)?, &dir, SyncStep::Directory)?;
    Ok(())
}

/// The two syncs of `write_durably`: the file written, then its directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStep {
    File,
    Directory,
}

/// Syncs `file`, in `dir`, to disk; a test fault for `step` fails it instead.
fn sync_all(file: &std::fs::File, _dir: &Path, _step: SyncStep) -> std::io::Result<()> {
    #[cfg(test)]
    sync_faults::check(_dir, _step)?;
    file.sync_all()
}

/// Test hook: makes a sync step of `write_durably` fail for the files of a directory.
#[cfg(test)]
pub mod sync_faults {
    pub use super::SyncStep;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static FAULTS: Mutex<Vec<(PathBuf, SyncStep)>> = Mutex::new(Vec::new());

    /// Every `step` sync in `dir` fails until `clear`.
    pub fn fail(dir: &Path, step: SyncStep) {
        FAULTS.lock().unwrap().push((dir.to_path_buf(), step));
    }

    pub fn clear(dir: &Path) {
        FAULTS.lock().unwrap().retain(|(d, _)| d != dir);
    }

    pub(super) fn check(dir: &Path, step: SyncStep) -> std::io::Result<()> {
        if FAULTS
            .lock()
            .unwrap()
            .iter()
            .any(|(d, s)| d == dir && *s == step)
        {
            return Err(std::io::Error::other(format!(
                "{step:?} sync failed (test fault)"
            )));
        }
        Ok(())
    }
}

/// The subnets with a file `eots.<txid20>.<ext>` in `dir`.
fn subnets_with_file(dir: &Path, ext: &str) -> Result<Vec<SubnetId>, EotsKeysError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(EotsKeysError::Dir {
                path: dir.to_path_buf(),
                reason: e.to_string(),
            })
        }
    };
    let mut subnets = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| EotsKeysError::Dir {
            path: dir.to_path_buf(),
            reason: e.to_string(),
        })?;
        let name = entry.file_name();
        let Some(txid20) = name
            .to_str()
            .and_then(|n| n.strip_prefix("eots."))
            .and_then(|n| n.strip_suffix(&format!(".{ext}")))
            .and_then(|n| hex::decode(n).ok())
            .and_then(|bytes| <[u8; 20]>::try_from(bytes).ok())
        else {
            continue;
        };
        subnets.push(SubnetId::from_txid20(&txid20));
    }
    subnets.sort();
    Ok(subnets)
}

pub struct EotsKeys {
    /// The directory of the validator key file, which holds the files of each subnet
    dir: PathBuf,
    /// The key of `EOTS_SK_PATH`
    configured: Option<SharedSigner>,
    /// The keys generated for a subnet, loaded at start and at the join
    by_subnet: Mutex<BTreeMap<SubnetId, SharedSigner>>,
    /// Held through a join, from its first check to the sending of its transactions
    joins: Mutex<()>,
}

impl EotsKeys {
    /// Loads the configured key, whose files must exist, and the key of every subnet whose
    /// files are in `dir`.
    pub fn load(dir: PathBuf, configured: Option<&EotsKeyFiles>) -> Result<Self, EotsKeysError> {
        let configured = match configured {
            Some(files) => {
                let signer = files.load()?.ok_or_else(|| EotsSignerError::KeyFile {
                    path: files.key.clone(),
                    reason: "the file does not exist".to_string(),
                })?;
                info!("EOTS key {} from {}", signer.pk_eots(), files.key.display());
                Some(Arc::new(Mutex::new(signer)))
            }
            None => None,
        };
        let mut by_subnet = BTreeMap::new();
        for subnet_id in subnets_with_file(&dir, "sk")? {
            let files = EotsKeyFiles::of_subnet(&dir, subnet_id);
            let signer = files.load()?.expect("the key file was listed");
            info!(
                "EOTS key {} of subnet {subnet_id} from {}",
                signer.pk_eots(),
                files.key.display()
            );
            by_subnet.insert(subnet_id, Arc::new(Mutex::new(signer)));
        }
        Ok(Self {
            dir,
            configured,
            by_subnet: Mutex::new(by_subnet),
            joins: Mutex::new(()),
        })
    }

    /// Every key held, for the nonce trees built at start.
    pub fn signers(&self) -> Vec<SharedSigner> {
        let by_subnet = self.by_subnet.lock().expect("EOTS keys lock");
        self.configured
            .iter()
            .chain(by_subnet.values())
            .cloned()
            .collect()
    }

    /// One join at a time: held from the first check of a join to the sending of its
    /// transactions, so that a second join to the subnet finds the first one recorded.
    pub fn join_guard(&self) -> MutexGuard<'_, ()> {
        self.joins.lock().expect("joins lock")
    }

    /// The key of `subnet_id`: the key generated for it, else the configured key when the subnet
    /// of the join that carries it is `subnet_id` or none. That subnet comes from the recorded
    /// EOTS nonce commitments (`accepted_subnet`, the accepted join) or from the join records.
    pub fn signer_of(
        &self,
        subnet_id: SubnetId,
        accepted_subnet: impl FnOnce(&XOnlyPublicKey) -> Result<Option<SubnetId>, String>,
    ) -> Result<Option<SharedSigner>, EotsKeysError> {
        let by_subnet = self.by_subnet.lock().expect("EOTS keys lock");
        if let Some(signer) = by_subnet.get(&subnet_id) {
            return Ok(Some(signer.clone()));
        }
        let Some(configured) = &self.configured else {
            return Ok(None);
        };
        Ok(match self.configured_subnet(accepted_subnet)? {
            Some(subnet) if subnet != subnet_id => None,
            _ => Some(configured.clone()),
        })
    }

    /// The subnet of the join that carries the configured key, `None` while no join does.
    fn configured_subnet(
        &self,
        accepted_subnet: impl FnOnce(&XOnlyPublicKey) -> Result<Option<SubnetId>, String>,
    ) -> Result<Option<SubnetId>, EotsKeysError> {
        let Some(configured) = &self.configured else {
            return Ok(None);
        };
        let pk_eots = configured.lock().expect("EOTS signer lock").pk_eots();
        if let Some(subnet) = accepted_subnet(&pk_eots).map_err(EotsKeysError::Records)? {
            return Ok(Some(subnet));
        }
        Ok(self
            .join_records()?
            .into_iter()
            .find(|(_, record)| record.pk_eots == pk_eots)
            .map(|(subnet, _)| subnet))
    }

    /// The key of `subnet_id` for a join to it: the key generated for it, else the configured
    /// key, bound to `subnet_id` by a join record when no join carries it yet, else a key
    /// generated with its nonce seed.
    pub fn signer_for_join(
        &self,
        subnet_id: SubnetId,
        accepted_subnet: impl FnOnce(&XOnlyPublicKey) -> Result<Option<SubnetId>, String>,
    ) -> Result<SharedSigner, EotsKeysError> {
        let mut by_subnet = self.by_subnet.lock().expect("EOTS keys lock");
        if let Some(signer) = by_subnet.get(&subnet_id) {
            return Ok(signer.clone());
        }
        if let Some(configured) = &self.configured {
            match self.configured_subnet(accepted_subnet)? {
                Some(subnet) if subnet != subnet_id => {}
                Some(_) => return Ok(configured.clone()),
                None => {
                    let pk_eots = configured.lock().expect("EOTS signer lock").pk_eots();
                    self.record_join(
                        subnet_id,
                        &JoinRecord {
                            pk_eots,
                            prefund: None,
                            transactions: None,
                        },
                    )?;
                    info!(
                        "EOTS key {pk_eots} of {} joins subnet {subnet_id}",
                        self.dir.display()
                    );
                    return Ok(configured.clone());
                }
            }
        }
        let files = EotsKeyFiles::of_subnet(&self.dir, subnet_id);
        let signer = files.load_or_create()?;
        info!(
            "EOTS key {} of subnet {subnet_id} from {}",
            signer.pk_eots(),
            files.key.display()
        );
        let signer = Arc::new(Mutex::new(signer));
        by_subnet.insert(subnet_id, signer.clone());
        Ok(signer)
    }

    /// The record of the last join to `subnet_id`, if any.
    pub fn join_record(&self, subnet_id: SubnetId) -> Result<Option<JoinRecord>, EotsKeysError> {
        let path = subnet_file(&self.dir, subnet_id, "join");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(EotsKeysError::JoinRecord {
                    path,
                    reason: e.to_string(),
                })
            }
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| EotsKeysError::JoinRecord {
                path,
                reason: e.to_string(),
            })
    }

    /// Records the join to `subnet_id` durably, replacing an earlier record.
    pub fn record_join(&self, subnet_id: SubnetId, join: &JoinRecord) -> Result<(), EotsKeysError> {
        let path = subnet_file(&self.dir, subnet_id, "join");
        let text = serde_json::to_string(join).expect("a join record serializes");
        write_durably(&path, text.as_bytes()).map_err(|e| EotsKeysError::JoinRecord {
            path,
            reason: e.to_string(),
        })
    }

    fn join_records(&self) -> Result<Vec<(SubnetId, JoinRecord)>, EotsKeysError> {
        subnets_with_file(&self.dir, "join")?
            .into_iter()
            .filter_map(|subnet_id| {
                self.join_record(subnet_id)
                    .map(|record| record.map(|r| (subnet_id, r)))
                    .transpose()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{pubkey, subnet};

    fn no_accepted_join(_: &XOnlyPublicKey) -> Result<Option<SubnetId>, String> {
        Ok(None)
    }

    fn pk_of(signer: &SharedSigner) -> XOnlyPublicKey {
        signer.lock().unwrap().pk_eots()
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("eots."))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_key_directory_of_a_bare_file_name_is_the_current_directory() {
        assert_eq!(key_dir(Path::new("validator.sk")), PathBuf::from("."));
        assert_eq!(
            key_dir(Path::new("keys/validator.sk")),
            PathBuf::from("keys")
        );
        assert_eq!(
            key_dir(Path::new("/root/.ipc/validator.sk")),
            PathBuf::from("/root/.ipc")
        );
        assert_eq!(subnets_with_file(Path::new(""), "sk").unwrap(), vec![]);
        assert!(subnets_with_file(Path::new("."), "sk").is_ok());
    }

    #[test]
    fn each_subnet_gets_its_own_key_and_a_restart_loads_them() {
        crate::eth_utils::set_fvm_network();
        let dir = tempfile::tempdir().unwrap();
        let keys = EotsKeys::load(dir.path().to_path_buf(), None).unwrap();
        assert!(keys
            .signer_of(subnet(1), no_accepted_join)
            .unwrap()
            .is_none());
        let first = keys.signer_for_join(subnet(1), no_accepted_join).unwrap();
        let second = keys.signer_for_join(subnet(2), no_accepted_join).unwrap();
        assert_ne!(pk_of(&first), pk_of(&second));
        assert_ne!(first.lock().unwrap().seed(), second.lock().unwrap().seed());
        for (n, signer) in [(1, &first), (2, &second)] {
            let files = EotsKeyFiles::of_subnet(dir.path(), subnet(n));
            assert_eq!(
                files.key,
                dir.path()
                    .join(format!("eots.{}.sk", hex::encode([n as u8; 20])))
            );
            assert_eq!(signer.lock().unwrap().key_path(), files.key);
            assert!(files.seed.exists());
        }
        // A second join to a subnet, and a vote, use the subnet's key
        let again = keys.signer_for_join(subnet(1), no_accepted_join).unwrap();
        assert!(Arc::ptr_eq(&again, &first));
        let voter = keys
            .signer_of(subnet(2), no_accepted_join)
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&voter, &second));
        assert_eq!(keys.signers().len(), 2);
        let (pk_first, pk_second) = (pk_of(&first), pk_of(&second));
        // One vote database opens once per process: the keys are dropped before the restart
        drop((first, second, again, voter, keys));

        let restarted = EotsKeys::load(dir.path().to_path_buf(), None).unwrap();
        assert_eq!(restarted.signers().len(), 2);
        let first = restarted
            .signer_of(subnet(1), no_accepted_join)
            .unwrap()
            .unwrap();
        let second = restarted
            .signer_of(subnet(2), no_accepted_join)
            .unwrap()
            .unwrap();
        assert_eq!(pk_of(&first), pk_first);
        assert_eq!(pk_of(&second), pk_second);
        assert!(restarted
            .signer_of(subnet(3), no_accepted_join)
            .unwrap()
            .is_none());
    }

    #[test]
    fn the_configured_key_is_bound_to_the_first_subnet_it_is_chosen_for() {
        crate::eth_utils::set_fvm_network();
        let dir = tempfile::tempdir().unwrap();
        let configured = EotsKeyFiles {
            key: dir.path().join("configured").join("eots.sk"),
            seed: dir.path().join("configured").join("eots.seed"),
        };
        assert!(
            EotsKeys::load(dir.path().to_path_buf(), Some(&configured)).is_err(),
            "a configured key file must exist"
        );
        drop(configured.load_or_create().unwrap());
        let keys = EotsKeys::load(dir.path().to_path_buf(), Some(&configured)).unwrap();
        let pk_configured = pk_of(&keys.signers()[0]);

        // Unbound, it serves any subnet's votes
        let signer = keys
            .signer_of(subnet(1), no_accepted_join)
            .unwrap()
            .unwrap();
        assert_eq!(pk_of(&signer), pk_configured);

        // Chosen for a join to subnet 1, it is bound there before any transaction
        assert_eq!(
            pk_of(&keys.signer_for_join(subnet(1), no_accepted_join).unwrap()),
            pk_configured
        );
        let bound = keys.join_record(subnet(1)).unwrap().unwrap();
        assert_eq!(bound.pk_eots, pk_configured);
        assert_eq!(bound.prefund, None);
        assert_eq!(bound.transactions, None);
        assert_eq!(bound.txid(), None);
        assert!(keys.join_record(subnet(2)).unwrap().is_none());
        assert!(keys
            .signer_of(subnet(2), no_accepted_join)
            .unwrap()
            .is_none());
        let second = keys.signer_for_join(subnet(2), no_accepted_join).unwrap();
        assert_ne!(pk_of(&second), pk_configured);
        assert!(EotsKeyFiles::of_subnet(dir.path(), subnet(2)).key.exists());
        assert!(!EotsKeyFiles::of_subnet(dir.path(), subnet(1)).key.exists());
        assert_eq!(
            pk_of(&keys.signer_for_join(subnet(1), no_accepted_join).unwrap()),
            pk_configured,
            "a later join to subnet 1 keeps the configured key"
        );
        assert_eq!(
            pk_of(
                &keys
                    .signer_of(subnet(1), no_accepted_join)
                    .unwrap()
                    .unwrap()
            ),
            pk_configured
        );
        let (pk_second, records) = (pk_of(&second), file_names(dir.path()));
        drop((signer, second, keys));

        // The binding survives a restart
        let restarted = EotsKeys::load(dir.path().to_path_buf(), Some(&configured)).unwrap();
        assert_eq!(
            pk_of(
                &restarted
                    .signer_for_join(subnet(1), no_accepted_join)
                    .unwrap()
            ),
            pk_configured
        );
        assert_eq!(
            pk_of(
                &restarted
                    .signer_for_join(subnet(2), no_accepted_join)
                    .unwrap()
            ),
            pk_second
        );
        assert!(restarted
            .signer_of(subnet(3), no_accepted_join)
            .unwrap()
            .is_none());
        assert_eq!(file_names(dir.path()), records, "no new file");

        // An accepted join recorded for subnet 3 binds it there instead
        let accepted = |pk: &XOnlyPublicKey| {
            assert_eq!(*pk, pk_configured);
            Ok(Some(subnet(3)))
        };
        assert_eq!(
            pk_of(&restarted.signer_of(subnet(3), accepted).unwrap().unwrap()),
            pk_configured
        );
        assert!(restarted.signer_of(subnet(1), accepted).unwrap().is_none());
        let records_error = restarted.signer_of(subnet(1), |_| Err("db down".to_string()));
        assert!(
            matches!(&records_error, Err(e) if e.to_string().contains("db down")),
            "an error of the records is not an unbound key"
        );

        // A new record replaces the earlier one, through a temporary file
        let later = JoinRecord {
            pk_eots: pubkey(40),
            prefund: None,
            transactions: None,
        };
        restarted.record_join(subnet(1), &later).unwrap();
        assert_eq!(restarted.join_record(subnet(1)).unwrap(), Some(later));
        assert!(!subnet_file(dir.path(), subnet(1), "join.tmp").exists());
    }

    #[test]
    fn an_interrupted_key_generation_does_not_stop_the_start_and_keeps_its_seed() {
        crate::eth_utils::set_fvm_network();
        let dir = tempfile::tempdir().unwrap();
        let complete = format!("{}\n", hex::encode([5u8; 32]));
        // Stopped while writing the seed file
        std::fs::write(subnet_file(dir.path(), subnet(2), "seed.tmp"), "0123").unwrap();
        // Stopped between the seed file and the key file
        std::fs::write(subnet_file(dir.path(), subnet(3), "seed"), &complete).unwrap();
        // Stopped while writing the key file
        std::fs::write(subnet_file(dir.path(), subnet(4), "seed"), &complete).unwrap();
        std::fs::write(subnet_file(dir.path(), subnet(4), "sk.tmp"), "0123").unwrap();

        let keys = EotsKeys::load(dir.path().to_path_buf(), None).unwrap();
        assert!(keys.signers().is_empty(), "no key is complete");
        for n in [2, 3, 4] {
            assert!(keys
                .signer_of(subnet(n), no_accepted_join)
                .unwrap()
                .is_none());
        }
        // The next join to each subnet completes the generation, keeping a complete seed
        let signers: Vec<SharedSigner> = [2, 3, 4]
            .into_iter()
            .map(|n| keys.signer_for_join(subnet(n), no_accepted_join).unwrap())
            .collect();
        assert_ne!(*signers[0].lock().unwrap().seed().as_bytes(), [5u8; 32]);
        assert_eq!(*signers[1].lock().unwrap().seed().as_bytes(), [5u8; 32]);
        assert_eq!(*signers[2].lock().unwrap().seed().as_bytes(), [5u8; 32]);
        assert_ne!(pk_of(&signers[1]), pk_of(&signers[2]));
        assert!(
            !file_names(dir.path()).iter().any(|n| n.ends_with(".tmp")),
            "{:?}",
            file_names(dir.path())
        );
        drop((signers, keys));
        assert_eq!(
            EotsKeys::load(dir.path().to_path_buf(), None)
                .unwrap()
                .signers()
                .len(),
            3
        );

        // A key file without its seed file is damage, not an interrupted generation
        std::fs::write(subnet_file(dir.path(), subnet(5), "sk"), &complete).unwrap();
        let Err(damaged) = EotsKeys::load(dir.path().to_path_buf(), None) else {
            panic!("a key file without its seed file loads");
        };
        assert!(damaged.to_string().contains("seed"), "{damaged}");
    }

    #[test]
    fn a_failed_seed_publication_leaves_no_key_and_the_provider_starts() {
        crate::eth_utils::set_fvm_network();
        let dir = tempfile::tempdir().unwrap();
        let keys = EotsKeys::load(dir.path().to_path_buf(), None).unwrap();
        // The seed cannot be published: a directory holds its temporary file's name
        let blocked = subnet_file(dir.path(), subnet(6), "seed.tmp");
        std::fs::create_dir(&blocked).unwrap();
        let Err(failed) = keys.signer_for_join(subnet(6), no_accepted_join) else {
            panic!("the key was generated without its seed");
        };
        assert!(failed.to_string().contains("seed"), "{failed}");
        assert!(
            !subnet_file(dir.path(), subnet(6), "sk").exists(),
            "no key without its seed"
        );
        assert!(!subnet_file(dir.path(), subnet(6), "seed").exists());
        assert!(keys.signers().is_empty());
        drop(keys);
        let restarted = EotsKeys::load(dir.path().to_path_buf(), None).unwrap();
        assert!(restarted.signers().is_empty());
        // The sync of the seed file fails: the same
        std::fs::remove_dir(&blocked).unwrap();
        sync_faults::fail(dir.path(), sync_faults::SyncStep::File);
        let Err(failed) = restarted.signer_for_join(subnet(6), no_accepted_join) else {
            panic!("the key was generated without a synced seed");
        };
        assert!(failed.to_string().contains("sync failed"), "{failed}");
        sync_faults::clear(dir.path());
        assert!(!subnet_file(dir.path(), subnet(6), "sk").exists());
        assert!(!subnet_file(dir.path(), subnet(6), "seed").exists());
        // Published, the key is complete
        drop(
            restarted
                .signer_for_join(subnet(6), no_accepted_join)
                .unwrap(),
        );
        assert!(subnet_file(dir.path(), subnet(6), "sk").exists());
        assert!(subnet_file(dir.path(), subnet(6), "seed").exists());
    }

    #[test]
    fn other_files_in_the_key_directory_are_ignored() {
        crate::eth_utils::set_fvm_network();
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "validator.sk",
            "eots.sk",
            "eots.zz.sk",
            "eots.0011.sk",
            "eots.00.join",
        ] {
            std::fs::write(dir.path().join(name), "00").unwrap();
        }
        let keys = EotsKeys::load(dir.path().to_path_buf(), None).unwrap();
        assert!(keys.signers().is_empty());
        assert!(keys.join_records().unwrap().is_empty());
        let missing = EotsKeys::load(dir.path().join("none"), None).unwrap();
        assert!(missing.signers().is_empty());
    }
}
