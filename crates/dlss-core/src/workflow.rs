use crate::{
    AtomicFileReplacer, BackupIndex, BackupRecord, BatchResult, CatalogDll, Comparison, CoreError,
    DesiredDll, DllInspector, DllInstallation, OperationPlan, PlannedSwap, SwapResult,
    TargetProfile, compare_target,
};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// Build the direct one-click plan. Unknown, equal-version/different-build, newer,
/// identical, missing, and differently named DLLs are deliberately preserved.
#[must_use]
pub fn plan_strict_upgrades(
    nonce: impl Into<String>,
    installed: &[DllInstallation],
    latest: &[CatalogDll],
) -> OperationPlan {
    plan_filtered_upgrades(nonce, installed, latest, |_| true)
}

#[must_use]
pub fn plan_dlss_only_upgrades(
    nonce: impl Into<String>,
    installed: &[DllInstallation],
    latest: &[CatalogDll],
) -> OperationPlan {
    plan_filtered_upgrades(nonce, installed, latest, |dll| {
        crate::DllKind::classify(&dll.file_name).is_some_and(crate::DllKind::is_dlss_family)
    })
}

fn plan_filtered_upgrades(
    nonce: impl Into<String>,
    installed: &[DllInstallation],
    latest: &[CatalogDll],
    include: impl Fn(&DllInstallation) -> bool,
) -> OperationPlan {
    let latest_by_name: HashMap<_, _> = latest
        .iter()
        .filter_map(|dll| fold_file_name(&dll.file_name).map(|name| (name, dll)))
        .collect();
    let swaps = installed
        .iter()
        .filter(|current| include(current))
        .filter_map(|current| {
            let name = fold_file_name(&current.file_name)?;
            let target = latest_by_name.get(&name)?;
            let current_version = current.metadata.version?;
            (target.version > current_version).then(|| PlannedSwap {
                game: current.game_id.clone(),
                installation: current.id.clone(),
                target_path: current.path.clone(),
                expected_sha256: current.metadata.sha256,
                source_path: target.source.clone(),
                source_sha256: target.sha256,
                comparison: Comparison::Upgrade,
            })
        })
        .collect();
    OperationPlan {
        nonce: nonce.into(),
        swaps,
    }
}

#[must_use]
pub fn plan_touches_streamline(plans: &[OperationPlan]) -> bool {
    plans.iter().any(|plan| {
        plan.swaps.iter().any(|swap| {
            swap.target_path
                .file_name()
                .and_then(crate::DllKind::classify)
                == Some(crate::DllKind::Streamline)
        })
    })
}

/// Resolves an advanced per-installation profile into immutable file swaps.
/// Unlike the one-click planner, this deliberately permits downgrades,
/// different builds with the same numeric version, and restore points.
///
/// # Errors
/// Returns an error when the profile references an unknown installation or an
/// unavailable source.
pub fn plan_target_profile(
    nonce: impl Into<String>,
    installed: &[DllInstallation],
    latest: &[CatalogDll],
    cached: &[CatalogDll],
    backups: &[BackupRecord],
    profile: &TargetProfile,
) -> Result<OperationPlan, CoreError> {
    let installed_by_id: HashMap<_, _> = installed.iter().map(|dll| (&dll.id, dll)).collect();
    let mut swaps = Vec::new();

    for (installation_id, desired) in &profile.targets {
        let current = installed_by_id.get(installation_id).ok_or_else(|| {
            CoreError::Validation(format!(
                "profile references unknown DLL installation {}",
                installation_id.0
            ))
        })?;
        let target = match desired {
            DesiredDll::KeepInstalled => continue,
            DesiredDll::LatestOfficial => latest
                .iter()
                .filter(|candidate| same_file_name(&candidate.file_name, &current.file_name))
                .max_by_key(|candidate| (candidate.version, candidate.sha256))
                .map(|candidate| {
                    (
                        candidate.source.clone(),
                        candidate.sha256,
                        Some(candidate.version),
                    )
                }),
            DesiredDll::Cached { release, sha256 } => cached
                .iter()
                .find(|candidate| {
                    candidate.release == *release
                        && candidate.sha256 == *sha256
                        && same_file_name(&candidate.file_name, &current.file_name)
                })
                .map(|candidate| {
                    (
                        candidate.source.clone(),
                        candidate.sha256,
                        Some(candidate.version),
                    )
                }),
            DesiredDll::Restore { backup_sha256 } => backups
                .iter()
                .find(|backup| {
                    backup.sha256 == *backup_sha256 && backup.original_path == current.path
                })
                .map(|backup| (backup.content_path.clone(), backup.sha256, backup.version)),
        }
        .ok_or_else(|| {
            CoreError::Validation(format!(
                "desired source is unavailable for DLL installation {}",
                installation_id.0
            ))
        })?;

        let comparison = compare_target(
            current.metadata.version,
            current.metadata.sha256,
            target.2,
            target.1,
        );
        if comparison == Comparison::Identical {
            continue;
        }
        swaps.push(PlannedSwap {
            game: current.game_id.clone(),
            installation: current.id.clone(),
            target_path: current.path.clone(),
            expected_sha256: current.metadata.sha256,
            source_path: target.0,
            source_sha256: target.1,
            comparison,
        });
    }

    Ok(OperationPlan {
        nonce: nonce.into(),
        swaps,
    })
}

#[must_use]
pub fn same_file_name(left: &OsStr, right: &OsStr) -> bool {
    matches!((fold_file_name(left), fold_file_name(right)), (Some(left), Some(right)) if left == right)
}

fn fold_file_name(name: &OsStr) -> Option<Vec<u8>> {
    // Official DLL names are ASCII. Non-UTF-8 names never match catalog names
    // or one another.
    name.to_str().map(|value| {
        value
            .bytes()
            .map(|byte| byte.to_ascii_lowercase())
            .collect()
    })
}

pub struct BackupStore {
    root: PathBuf,
}

impl BackupStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Commits an immutable, content-addressed backup and updates its index.
    ///
    /// # Errors
    /// Returns an error when inspection, hashing, copying, or persistence fails.
    pub fn commit(
        &self,
        original: &Path,
        expected_hash: [u8; 32],
        inspector: &dyn DllInspector,
        created_unix: i64,
    ) -> Result<BackupRecord, CoreError> {
        let metadata = inspector.inspect(original)?;
        if metadata.sha256 != expected_hash {
            return Err(CoreError::StalePlan);
        }
        let content_dir = self.root.join("objects");
        fs::create_dir_all(&content_dir)?;
        let content_path = content_dir.join(hex_hash(expected_hash));
        if !content_path.exists() {
            let temporary = content_path.with_extension("partial");
            copy_flush_hash(original, &temporary, expected_hash)?;
            match fs::rename(&temporary, &content_path) {
                Ok(()) => {}
                Err(error) if content_path.exists() => {
                    let _ = fs::remove_file(&temporary);
                    if hash_file(&content_path)? != expected_hash {
                        return Err(CoreError::Validation(
                            "existing backup object has an unexpected hash".into(),
                        ));
                    }
                    let _ = error;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let record = BackupRecord {
            sha256: expected_hash,
            content_path,
            original_path: original.to_path_buf(),
            version: metadata.version,
            created_unix,
        };
        let mut index = self.load_index()?;
        if let Some(existing) = index.records.iter_mut().find(|existing| {
            existing.sha256 == record.sha256 && existing.original_path == record.original_path
        }) {
            *existing = record.clone();
        } else {
            index.records.push(record.clone());
        }
        write_versioned_json(&self.root.join("index.json"), 1, &index)?;
        Ok(record)
    }

    /// Loads the backup index, returning an empty index when none exists.
    ///
    /// # Errors
    /// Returns an error when the index cannot be read or validated.
    pub fn load_index(&self) -> Result<BackupIndex, CoreError> {
        let path = self.root.join("index.json");
        if path.exists() {
            read_versioned_json(&path, 1)
        } else {
            Ok(BackupIndex::default())
        }
    }
}

/// Executes independent per-file transactions and continues after failures.
pub fn execute_plan(
    plan: &OperationPlan,
    inspector: &dyn DllInspector,
    replacer: &dyn AtomicFileReplacer,
    backups: &BackupStore,
    timestamp_unix: i64,
) -> BatchResult {
    let swaps = plan
        .swaps
        .iter()
        .map(|swap| {
            let outcome = execute_swap(swap, inspector, replacer, backups, timestamp_unix);
            let (result, backup, denied) = match outcome {
                Ok((metadata, backup)) => (Ok(metadata), Some(backup), false),
                Err(error) => {
                    let denied = error.is_permission_denied();
                    (Err(error.to_string()), None, denied)
                }
            };
            SwapResult {
                installation: swap.installation.clone(),
                result,
                backup,
                denied,
            }
        })
        .collect();
    BatchResult { swaps }
}

fn execute_swap(
    swap: &PlannedSwap,
    inspector: &dyn DllInspector,
    replacer: &dyn AtomicFileReplacer,
    backups: &BackupStore,
    timestamp_unix: i64,
) -> Result<(crate::DllMetadata, BackupRecord), CoreError> {
    let current = inspector.inspect(&swap.target_path)?;
    if current.sha256 != swap.expected_sha256 {
        return Err(CoreError::StalePlan);
    }
    if hash_file(&swap.source_path)? != swap.source_sha256 {
        return Err(CoreError::Validation("cached source hash changed".into()));
    }
    let backup = backups.commit(
        &swap.target_path,
        swap.expected_sha256,
        inspector,
        timestamp_unix,
    )?;
    replacer.replace(&swap.target_path, &swap.source_path, swap.source_sha256)?;
    let installed = inspector.inspect(&swap.target_path)?;
    if installed.sha256 != swap.source_sha256 {
        let verification = CoreError::Validation("replacement verification hash mismatch".into());
        if let Err(rollback) =
            replacer.replace(&swap.target_path, &backup.content_path, backup.sha256)
        {
            return Err(CoreError::Validation(format!(
                "{verification}; backup rollback failed: {rollback}"
            )));
        }
        return Err(verification);
    }
    Ok((installed, backup))
}

/// Computes a file's SHA-256 digest.
///
/// # Errors
/// Returns an error when the file cannot be read.
pub fn hash_file(path: &Path) -> Result<[u8; 32], CoreError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

pub(crate) fn copy_flush_hash(
    source: &Path,
    destination: &Path,
    expected: [u8; 32],
) -> Result<(), CoreError> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
    }
    output.flush()?;
    output.sync_all()?;
    if <[u8; 32]>::from(hasher.finalize()) != expected {
        let _ = fs::remove_file(destination);
        return Err(CoreError::Validation("copy hash mismatch".into()));
    }
    Ok(())
}

/// Atomically writes a payload in a versioned JSON envelope.
///
/// # Errors
/// Returns an error when serialization or durable persistence fails.
pub fn write_versioned_json<T: Serialize>(
    destination: &Path,
    schema_version: u32,
    payload: &T,
) -> Result<(), CoreError> {
    #[derive(Serialize)]
    struct Envelope<'a, T> {
        schema_version: u32,
        payload: &'a T,
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&Envelope {
        schema_version,
        payload,
    })
    .map_err(|error| CoreError::Validation(error.to_string()))?;
    let mut output = atomic_write_file::AtomicWriteFile::open(destination)?;
    output.write_all(&bytes)?;
    output.flush()?;
    output.commit()?;
    Ok(())
}

/// Reads and validates a payload from a versioned JSON envelope.
///
/// # Errors
/// Returns an error when reading, decoding, or schema validation fails.
pub fn read_versioned_json<T: DeserializeOwned>(
    source: &Path,
    expected_schema: u32,
) -> Result<T, CoreError> {
    #[derive(serde::Deserialize)]
    struct Envelope<T> {
        schema_version: u32,
        payload: T,
    }
    let envelope: Envelope<T> = serde_json::from_reader(File::open(source)?)
        .map_err(|error| CoreError::Validation(error.to_string()))?;
    if envelope.schema_version != expected_schema {
        return Err(CoreError::Validation(format!(
            "unsupported schema version {}",
            envelope.schema_version
        )));
    }
    Ok(envelope.payload)
}

fn hex_hash(hash: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(64);
    for byte in hash {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DllInstallationId, DllMetadata, DllVersion, GameId, ReleaseId, SignatureStatus};
    use std::sync::Mutex;
    use tempfile::tempdir;

    struct BytesInspector;
    impl DllInspector for BytesInspector {
        fn inspect(&self, path: &Path) -> Result<DllMetadata, CoreError> {
            Ok(DllMetadata {
                version: Some(DllVersion::new(1, 0, 0, 0)),
                sha256: hash_file(path)?,
                signature: SignatureStatus::Trusted,
                x86_64: true,
            })
        }
    }

    struct CopyReplacer {
        fail: Mutex<Vec<PathBuf>>,
    }
    impl AtomicFileReplacer for CopyReplacer {
        fn replace(
            &self,
            target: &Path,
            source: &Path,
            _expected_source_hash: [u8; 32],
        ) -> Result<(), CoreError> {
            if self.fail.lock().unwrap().iter().any(|path| path == target) {
                return Err(CoreError::Validation("injected replacement failure".into()));
            }
            fs::copy(source, target)?;
            Ok(())
        }
    }

    struct PermissionDeniedReplacer;
    impl AtomicFileReplacer for PermissionDeniedReplacer {
        fn replace(
            &self,
            _target: &Path,
            _source: &Path,
            _expected_source_hash: [u8; 32],
        ) -> Result<(), CoreError> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into())
        }
    }

    fn installation(path: PathBuf, version: Option<DllVersion>) -> DllInstallation {
        DllInstallation {
            id: DllInstallationId(path.display().to_string()),
            game_id: GameId("g".into()),
            file_name: path.file_name().unwrap().to_os_string(),
            metadata: DllMetadata {
                version,
                sha256: hash_file(&path).unwrap(),
                signature: SignatureStatus::Trusted,
                x86_64: true,
            },
            path,
        }
    }

    #[test]
    fn quick_upgrade_only_selects_strictly_newer_same_named_dlls() {
        let dir = tempdir().unwrap();
        let current_path = dir.path().join("nvngx_dlss.dll");
        let unknown_path = dir.path().join("sl.common.dll");
        fs::write(&current_path, b"old").unwrap();
        fs::write(&unknown_path, b"unknown").unwrap();
        let target_path = dir.path().join("cached.dll");
        fs::write(&target_path, b"new").unwrap();
        let latest = CatalogDll {
            file_name: "NVNGX_DLSS.DLL".into(),
            version: DllVersion::new(2, 0, 0, 0),
            sha256: hash_file(&target_path).unwrap(),
            source: target_path,
            release: ReleaseId("r".into()),
        };
        let plan = plan_strict_upgrades(
            "n",
            &[
                installation(current_path, Some(DllVersion::new(1, 0, 0, 0))),
                installation(unknown_path, None),
            ],
            &[latest],
        );
        assert_eq!(plan.swaps.len(), 1);
        assert_eq!(plan.swaps[0].comparison, Comparison::Upgrade);
    }

    #[test]
    fn dlss_only_planner_skips_streamline_and_reflex() {
        let dir = tempdir().unwrap();
        let names = [
            "nvngx_dlss.dll",
            "nvngx_dlssg.dll",
            "sl.common.dll",
            "nvlowlatencyvk.dll",
        ];
        let mut installed = Vec::new();
        let mut latest = Vec::new();
        for name in names {
            let current = dir.path().join(name);
            let source = dir.path().join(format!("new-{name}"));
            fs::write(&current, b"old").unwrap();
            fs::write(&source, name.as_bytes()).unwrap();
            installed.push(installation(current, Some(DllVersion::new(1, 0, 0, 0))));
            latest.push(CatalogDll {
                file_name: name.into(),
                version: DllVersion::new(2, 0, 0, 0),
                sha256: hash_file(&source).unwrap(),
                source,
                release: ReleaseId("r".into()),
            });
        }
        let plan = plan_dlss_only_upgrades("n", &installed, &latest);
        assert_eq!(plan.swaps.len(), 2);
        assert!(plan.swaps.iter().all(|swap| {
            swap.target_path
                .file_name()
                .and_then(crate::DllKind::classify)
                .is_some_and(crate::DllKind::is_dlss_family)
        }));
        let all = plan_strict_upgrades("n", &installed, &latest);
        assert!(plan_touches_streamline(&[all]));
        assert!(!plan_touches_streamline(&[plan]));
        let streamline_only = plan_dlss_only_upgrades("n", &installed[2..], &latest[2..]);
        assert!(streamline_only.swaps.is_empty());
    }

    #[test]
    fn advanced_profile_supports_mixed_upgrade_downgrade_and_restore() {
        let dir = tempdir().unwrap();
        let installed_paths =
            ["upgrade.dll", "downgrade.dll", "restore.dll"].map(|name| dir.path().join(name));
        for path in &installed_paths {
            fs::write(path, path.file_name().unwrap().as_encoded_bytes()).unwrap();
        }
        let installed = vec![
            installation(
                installed_paths[0].clone(),
                Some(DllVersion::new(2, 0, 0, 0)),
            ),
            installation(
                installed_paths[1].clone(),
                Some(DllVersion::new(3, 0, 0, 0)),
            ),
            installation(
                installed_paths[2].clone(),
                Some(DllVersion::new(1, 0, 0, 0)),
            ),
        ];
        let sources = ["latest.bin", "cached.bin", "backup.bin"].map(|name| dir.path().join(name));
        for (index, source) in sources.iter().enumerate() {
            fs::write(source, [u8::try_from(index).unwrap() + 20]).unwrap();
        }
        let latest = CatalogDll {
            file_name: "upgrade.dll".into(),
            version: DllVersion::new(4, 0, 0, 0),
            sha256: hash_file(&sources[0]).unwrap(),
            source: sources[0].clone(),
            release: ReleaseId("latest".into()),
        };
        let cached = CatalogDll {
            file_name: "downgrade.dll".into(),
            version: DllVersion::new(1, 0, 0, 0),
            sha256: hash_file(&sources[1]).unwrap(),
            source: sources[1].clone(),
            release: ReleaseId("old".into()),
        };
        let backup = BackupRecord {
            sha256: hash_file(&sources[2]).unwrap(),
            content_path: sources[2].clone(),
            original_path: installed_paths[2].clone(),
            version: Some(DllVersion::new(1, 0, 0, 0)),
            created_unix: 1,
        };
        let profile = TargetProfile {
            targets: [
                (installed[0].id.clone(), DesiredDll::LatestOfficial),
                (
                    installed[1].id.clone(),
                    DesiredDll::Cached {
                        release: cached.release.clone(),
                        sha256: cached.sha256,
                    },
                ),
                (
                    installed[2].id.clone(),
                    DesiredDll::Restore {
                        backup_sha256: backup.sha256,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };

        let plan = plan_target_profile(
            "mixed",
            &installed,
            &[latest],
            &[cached],
            &[backup],
            &profile,
        )
        .unwrap();
        assert_eq!(plan.swaps.len(), 3);
        for (installation, expected) in [
            (&installed[0].id, Comparison::Upgrade),
            (&installed[1].id, Comparison::Downgrade),
            (&installed[2].id, Comparison::DifferentBuild),
        ] {
            assert_eq!(
                plan.swaps
                    .iter()
                    .find(|swap| &swap.installation == installation)
                    .unwrap()
                    .comparison,
                expected
            );
        }
    }

    #[test]
    fn advanced_profile_rejects_unavailable_or_wrong_named_sources() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("right.dll");
        let source = dir.path().join("source.dll");
        fs::write(&path, b"installed").unwrap();
        fs::write(&source, b"source").unwrap();
        let installed = installation(path, Some(DllVersion::new(1, 0, 0, 0)));
        let wrong = CatalogDll {
            file_name: "wrong.dll".into(),
            version: DllVersion::new(2, 0, 0, 0),
            sha256: hash_file(&source).unwrap(),
            source,
            release: ReleaseId("r".into()),
        };
        let profile = TargetProfile {
            targets: [(
                installed.id.clone(),
                DesiredDll::Cached {
                    release: wrong.release.clone(),
                    sha256: wrong.sha256,
                },
            )]
            .into_iter()
            .collect(),
        };
        let result = plan_target_profile("n", &[installed], &[], &[wrong], &[], &profile);
        assert!(matches!(result, Err(CoreError::Validation(_))));
    }

    #[test]
    fn stale_target_is_not_backed_up_or_replaced() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target.dll");
        let source = dir.path().join("source.dll");
        fs::write(&target, b"changed").unwrap();
        fs::write(&source, b"new").unwrap();
        let plan = OperationPlan {
            nonce: "n".into(),
            swaps: vec![PlannedSwap {
                game: GameId("g".into()),
                installation: DllInstallationId("d".into()),
                target_path: target.clone(),
                expected_sha256: [0; 32],
                source_path: source.clone(),
                source_sha256: hash_file(&source).unwrap(),
                comparison: Comparison::Upgrade,
            }],
        };
        let result = execute_plan(
            &plan,
            &BytesInspector,
            &CopyReplacer {
                fail: Mutex::new(Vec::new()),
            },
            &BackupStore::new(dir.path().join("backups")),
            0,
        );
        assert!(
            result.swaps[0]
                .result
                .as_ref()
                .unwrap_err()
                .contains("stale")
        );
        assert!(!dir.path().join("backups/objects").exists());
    }

    #[test]
    fn permission_denied_is_structured_on_swap_result() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target.dll");
        let source = dir.path().join("source.dll");
        fs::write(&target, b"old").unwrap();
        fs::write(&source, b"new").unwrap();
        let plan = OperationPlan {
            nonce: "n".into(),
            swaps: vec![PlannedSwap {
                game: GameId("g".into()),
                installation: DllInstallationId("d".into()),
                target_path: target.clone(),
                expected_sha256: hash_file(&target).unwrap(),
                source_path: source.clone(),
                source_sha256: hash_file(&source).unwrap(),
                comparison: Comparison::Upgrade,
            }],
        };
        let result = execute_plan(
            &plan,
            &BytesInspector,
            &PermissionDeniedReplacer,
            &BackupStore::new(dir.path().join("backups")),
            0,
        );
        assert!(result.swaps[0].denied);
        assert!(result.swaps[0].result.is_err());
    }

    #[test]
    fn batch_continues_and_backup_is_content_addressed() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source.dll");
        fs::write(&source, b"new").unwrap();
        let source_hash = hash_file(&source).unwrap();
        let mut swaps = Vec::new();
        for name in ["works.dll", "fails.dll"] {
            let target = dir.path().join(name);
            fs::write(&target, b"same original").unwrap();
            swaps.push(PlannedSwap {
                game: GameId("g".into()),
                installation: DllInstallationId(name.into()),
                target_path: target.clone(),
                expected_sha256: hash_file(&target).unwrap(),
                source_path: source.clone(),
                source_sha256: source_hash,
                comparison: Comparison::Upgrade,
            });
        }
        let replacer = CopyReplacer {
            fail: Mutex::new(vec![dir.path().join("fails.dll")]),
        };
        let backups = BackupStore::new(dir.path().join("backups"));
        let result = execute_plan(
            &OperationPlan {
                nonce: "n".into(),
                swaps,
            },
            &BytesInspector,
            &replacer,
            &backups,
            1,
        );
        assert!(result.swaps[0].result.is_ok());
        assert!(result.swaps[1].result.is_err());
        assert_eq!(
            fs::read_dir(dir.path().join("backups/objects"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            fs::read(dir.path().join("fails.dll")).unwrap(),
            b"same original"
        );
        let index = backups.load_index().unwrap();
        assert_eq!(index.records.len(), 2);

        backups
            .commit(
                &dir.path().join("fails.dll"),
                hash_file(&dir.path().join("fails.dll")).unwrap(),
                &BytesInspector,
                2,
            )
            .unwrap();
        let index = backups.load_index().unwrap();
        assert_eq!(index.records.len(), 2);
        assert_eq!(
            index
                .records
                .iter()
                .find(|record| record.original_path.ends_with("fails.dll"))
                .unwrap()
                .created_unix,
            2
        );
    }

    #[test]
    fn versioned_json_rejects_other_schema() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_versioned_json(&path, 1, &vec!["value"]).unwrap();
        let loaded: Vec<String> = read_versioned_json(&path, 1).unwrap();
        assert_eq!(loaded, ["value"]);
        assert!(read_versioned_json::<Vec<String>>(&path, 2).is_err());
    }
}
