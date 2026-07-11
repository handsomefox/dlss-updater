use crate::{
    CatalogDll, CoreError, DllInspector, DllKind, DllVersion, ReleaseId, SignatureStatus,
    TrustPolicy, TrustVerifier, copy_flush_hash, hash_file, read_versioned_json,
    write_versioned_json,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

pub const MAX_IMPORTED_DLL_BYTES: u64 = 256 * 1024 * 1024;
const IMPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportedDllRecord {
    pub file_name: OsString,
    pub version: DllVersion,
    pub sha256: [u8; 32],
    pub signer: String,
    pub imported_unix: i64,
    pub content_path: PathBuf,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportIndex {
    pub records: Vec<ImportedDllRecord>,
}

pub struct ImportStore {
    root: PathBuf,
}

impl ImportStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Validates and stores one NVIDIA-signed x86-64 managed DLL.
    ///
    /// # Errors
    /// Returns an error when any name, size, PE, version, trust, publisher,
    /// hashing, copying, or persistence check fails.
    pub fn import(
        &self,
        source: &Path,
        inspector: &dyn DllInspector,
        verifier: &dyn TrustVerifier,
        imported_unix: i64,
    ) -> Result<ImportedDllRecord, CoreError> {
        let file_name = source
            .file_name()
            .ok_or_else(|| CoreError::Validation("import has no file name".into()))?;
        if DllKind::classify(file_name).is_none() {
            return Err(CoreError::Validation(
                "file name is not a managed NVIDIA DLL".into(),
            ));
        }
        if source.metadata()?.len() > MAX_IMPORTED_DLL_BYTES {
            return Err(CoreError::Validation(
                "import exceeds the 256 MiB limit".into(),
            ));
        }
        let metadata = inspector.inspect(source)?;
        if !metadata.x86_64 {
            return Err(CoreError::Validation(
                "import is not an x86-64 PE DLL".into(),
            ));
        }
        let version = metadata
            .version
            .ok_or_else(|| CoreError::Validation("import has no version resource".into()))?;
        let trust = verifier.verify(source, TrustPolicy::Strict)?;
        if trust.signature != SignatureStatus::Trusted {
            return Err(CoreError::Validation(
                "import signature is not trusted".into(),
            ));
        }
        let signer = trust
            .signer
            .filter(|subject| is_nvidia_signer(subject))
            .ok_or_else(|| {
                CoreError::Validation("import is not signed by NVIDIA Corporation".into())
            })?;
        let sha256 = hash_file(source)?;
        let objects = self.root.join("objects");
        fs::create_dir_all(&objects)?;
        let content_path = objects.join(hex_hash(sha256));
        if !content_path.exists() {
            let temporary = objects.join(format!(".import-{}.partial", hex_hash(sha256)));
            if temporary.exists() {
                fs::remove_file(&temporary)?;
            }
            copy_flush_hash(source, &temporary, sha256)?;
            fs::rename(&temporary, &content_path)?;
        }
        let record = ImportedDllRecord {
            file_name: file_name.to_os_string(),
            version,
            sha256,
            signer,
            imported_unix,
            content_path,
        };
        let mut index = self.load_index()?;
        if let Some(existing) = index.records.iter_mut().find(|item| item.sha256 == sha256) {
            existing.clone_from(&record);
        } else {
            index.records.push(record.clone());
        }
        write_versioned_json(&self.root.join("index.json"), IMPORT_SCHEMA_VERSION, &index)?;
        Ok(record)
    }

    /// Loads the import index or an empty index when absent.
    ///
    /// # Errors
    /// Returns an error when the index cannot be read or validated.
    pub fn load_index(&self) -> Result<ImportIndex, CoreError> {
        let path = self.root.join("index.json");
        if path.exists() {
            read_versioned_json(&path, IMPORT_SCHEMA_VERSION)
        } else {
            Ok(ImportIndex::default())
        }
    }

    /// Removes one imported object and its index entry.
    ///
    /// # Errors
    /// Returns an error when deletion or persistence fails.
    pub fn remove(&self, sha256: [u8; 32]) -> Result<(), CoreError> {
        let mut index = self.load_index()?;
        index.records.retain(|record| record.sha256 != sha256);
        let object = self.root.join("objects").join(hex_hash(sha256));
        if object.exists() {
            fs::remove_file(object)?;
        }
        write_versioned_json(&self.root.join("index.json"), IMPORT_SCHEMA_VERSION, &index)
    }
}

#[must_use]
pub fn is_nvidia_signer(subject: &str) -> bool {
    subject.eq_ignore_ascii_case("NVIDIA Corporation")
}

#[must_use]
pub fn imported_release_id(hash: [u8; 32]) -> ReleaseId {
    ReleaseId(format!("import:{}", hex_hash(hash)))
}

#[must_use]
pub fn is_imported_release(id: &ReleaseId) -> bool {
    id.0.starts_with("import:")
}

#[must_use]
pub fn imported_catalog_dlls(index: &ImportIndex) -> Vec<CatalogDll> {
    index
        .records
        .iter()
        .map(|record| CatalogDll {
            file_name: record.file_name.clone(),
            version: record.version,
            sha256: record.sha256,
            source: record.content_path.clone(),
            release: imported_release_id(record.sha256),
        })
        .collect()
}

fn hex_hash(hash: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(64);
    for byte in hash {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DllMetadata, RevocationStatus, SignatureStatus, TrustReport};
    use sha2::{Digest, Sha256};

    struct Inspector {
        x64: bool,
        version: bool,
    }
    impl DllInspector for Inspector {
        fn inspect(&self, path: &Path) -> Result<DllMetadata, CoreError> {
            let bytes = fs::read(path)?;
            Ok(DllMetadata {
                version: self.version.then(|| DllVersion::new(3, 7, 10, 0)),
                sha256: Sha256::digest(bytes).into(),
                signature: SignatureStatus::Trusted,
                x86_64: self.x64,
            })
        }
    }

    struct Trust {
        status: SignatureStatus,
        subject: Option<String>,
    }
    impl TrustVerifier for Trust {
        fn verify(&self, _path: &Path, _policy: TrustPolicy) -> Result<TrustReport, CoreError> {
            Ok(TrustReport {
                signature: self.status,
                signer: self.subject.clone(),
                revocation: if self.status == SignatureStatus::Trusted {
                    RevocationStatus::Verified
                } else {
                    RevocationStatus::NotVerified
                },
                native_failure: None,
            })
        }
    }

    #[test]
    fn imports_only_trusted_nvidia_x64_versioned_managed_dlls() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("nvngx_dlss.dll");
        fs::write(&source, b"dll bytes").unwrap();
        let store = ImportStore::new(directory.path().join("imports"));
        let inspector = Inspector {
            x64: true,
            version: true,
        };
        let trusted = Trust {
            status: SignatureStatus::Trusted,
            subject: Some("NVIDIA Corporation".into()),
        };
        let first = store.import(&source, &inspector, &trusted, 42).unwrap();
        let second = store.import(&source, &inspector, &trusted, 42).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.load_index().unwrap().records.len(), 1);

        let wrong = Trust {
            status: SignatureStatus::Trusted,
            subject: Some("NVIDIA Evil Corp".into()),
        };
        assert!(store.import(&source, &inspector, &wrong, 42).is_err());
        let missing_subject = Trust {
            status: SignatureStatus::Trusted,
            subject: None,
        };
        assert!(
            store
                .import(&source, &inspector, &missing_subject, 42)
                .is_err()
        );
        let untrusted = Trust {
            status: SignatureStatus::Untrusted,
            subject: Some("NVIDIA Corporation".into()),
        };
        assert!(store.import(&source, &inspector, &untrusted, 42).is_err());
        assert!(
            store
                .import(
                    &source,
                    &Inspector {
                        x64: false,
                        version: true
                    },
                    &trusted,
                    42
                )
                .is_err()
        );
        assert!(
            store
                .import(
                    &source,
                    &Inspector {
                        x64: true,
                        version: false
                    },
                    &trusted,
                    42
                )
                .is_err()
        );
    }

    #[test]
    fn rejects_unmanaged_and_oversize_imports_before_inspection() {
        let directory = tempfile::tempdir().unwrap();
        let store = ImportStore::new(directory.path().join("imports"));
        let inspector = Inspector {
            x64: true,
            version: true,
        };
        let trusted = Trust {
            status: SignatureStatus::Trusted,
            subject: Some("NVIDIA Corporation".into()),
        };
        let unmanaged = directory.path().join("dxgi.dll");
        fs::write(&unmanaged, b"dll").unwrap();
        assert!(store.import(&unmanaged, &inspector, &trusted, 1).is_err());

        let oversize = directory.path().join("nvngx_dlss.dll");
        fs::File::create(&oversize)
            .unwrap()
            .set_len(MAX_IMPORTED_DLL_BYTES + 1)
            .unwrap();
        assert!(store.import(&oversize, &inspector, &trusted, 1).is_err());
    }

    #[test]
    fn import_index_round_trips_and_rejects_other_schemas() {
        let directory = tempfile::tempdir().unwrap();
        let store = ImportStore::new(directory.path().join("imports"));
        let index = ImportIndex {
            records: vec![ImportedDllRecord {
                file_name: "nvngx_dlss.dll".into(),
                version: DllVersion::new(3, 7, 10, 0),
                sha256: [7; 32],
                signer: "NVIDIA Corporation".into(),
                imported_unix: 1,
                content_path: "object".into(),
            }],
        };
        write_versioned_json(&directory.path().join("imports/index.json"), 1, &index).unwrap();
        assert_eq!(store.load_index().unwrap(), index);
        write_versioned_json(&directory.path().join("imports/index.json"), 2, &index).unwrap();
        assert!(store.load_index().is_err());
    }

    #[test]
    fn imported_catalog_entries_resolve_profiles() {
        let record = ImportedDllRecord {
            file_name: "nvngx_dlss.dll".into(),
            version: DllVersion::new(3, 7, 10, 0),
            sha256: [7; 32],
            signer: "NVIDIA Corporation".into(),
            imported_unix: 1,
            content_path: PathBuf::from("object"),
        };
        let catalog = imported_catalog_dlls(&ImportIndex {
            records: vec![record],
        });
        assert!(is_imported_release(&catalog[0].release));
        let installed = crate::DllInstallation {
            id: crate::DllInstallationId("dll".into()),
            game_id: crate::GameId("game".into()),
            path: PathBuf::from("game/nvngx_dlss.dll"),
            file_name: "nvngx_dlss.dll".into(),
            metadata: DllMetadata {
                version: Some(DllVersion::new(3, 5, 0, 0)),
                sha256: [3; 32],
                signature: SignatureStatus::Trusted,
                x86_64: true,
            },
        };
        let mut profile = crate::TargetProfile::default();
        profile.targets.insert(
            installed.id.clone(),
            crate::DesiredDll::Cached {
                release: catalog[0].release.clone(),
                sha256: catalog[0].sha256,
            },
        );
        let plan =
            crate::plan_target_profile("preview", &[installed], &[], &catalog, &[], &profile)
                .unwrap();
        assert_eq!(plan.swaps.len(), 1);
        assert_eq!(plan.swaps[0].source_path, PathBuf::from("object"));
        assert!(is_nvidia_signer("nvidia corporation"));
        assert!(!is_nvidia_signer("NVIDIA Corporation LLC"));
    }
}
