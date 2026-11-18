//! What the elevated helper may install, proven from the app's own records.
//!
//! The helper cannot trust the process that launched it. A plan names a
//! source file and its hash, and the helper accepts it only when the store the
//! file lives in lists that hash for the same DLL: the release's manifest, the
//! import index, or the backup index.

use crate::{
    BackupStore, CoreError, ImportStore, ReleaseId, ReleaseValidation, hex_hash,
    read_versioned_json, same_file_name, same_path, write_versioned_json,
};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub const PREPARED_RELEASE_SCHEMA_VERSION: u32 = 1;
/// The file in each prepared release folder that lists its validated DLLs.
pub const PREPARED_MANIFEST: &str = "manifest.json";

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PreparedReleaseEntry {
    pub file_name: String,
    pub version: crate::DllVersion,
    pub sha256: [u8; 32],
    pub source: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PreparedReleaseManifest {
    pub release: ReleaseId,
    pub validation: ReleaseValidation,
    pub dlls: Vec<PreparedReleaseEntry>,
}

/// The file in the data folder that lists the game folders the user added.
pub const CUSTOM_ROOTS_FILE: &str = "custom-roots.json";
const CUSTOM_ROOTS_SCHEMA_VERSION: u32 = 1;

/// Saves the game folders the user added, so the elevated helper can check a
/// manual game against them.
///
/// # Errors
/// Returns an error when the file cannot be written.
pub fn save_custom_roots(base: &Path, roots: &[PathBuf]) -> Result<(), CoreError> {
    write_versioned_json(
        &base.join(CUSTOM_ROOTS_FILE),
        CUSTOM_ROOTS_SCHEMA_VERSION,
        &roots,
    )
}

/// Whether `root`, a canonical path, is one of the game folders the user
/// added. A missing list means the user added none.
///
/// # Errors
/// Returns an error when the list exists but cannot be read.
pub fn is_custom_root(base: &Path, root: &Path) -> Result<bool, CoreError> {
    let path = base.join(CUSTOM_ROOTS_FILE);
    if !path.exists() {
        return Ok(false);
    }
    let roots: Vec<PathBuf> = read_versioned_json(&path, CUSTOM_ROOTS_SCHEMA_VERSION)?;
    Ok(roots
        .iter()
        .any(|saved| saved.canonicalize().ok().as_deref() == Some(root)))
}

/// The stores a helper source may come from, under one data folder.
pub struct HelperStores {
    base: PathBuf,
    releases: Option<PathBuf>,
    imports: Option<PathBuf>,
    backups: Option<PathBuf>,
}

impl HelperStores {
    /// Resolves the stores under `base`. A store that does not exist yet,
    /// such as the release cache before the first download, allows no
    /// sources but does not block the others.
    #[must_use]
    pub fn resolve(base: &Path) -> Self {
        Self {
            base: base.to_path_buf(),
            releases: base.join("cache/releases").canonicalize().ok(),
            imports: base.join("imports/objects").canonicalize().ok(),
            backups: base.join("backups/objects").canonicalize().ok(),
        }
    }
}

/// Where a helper source comes from, which decides how it is checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperSource {
    /// A DLL from a prepared official release.
    Release,
    /// A DLL the user imported.
    Import,
    /// The file a game had before a swap. It may go back only to the path it
    /// came from.
    Backup,
}

/// Finds which store holds `source` and checks that the store's own record
/// lists `source_sha256` for `target`.
///
/// - A release DLL must sit directly in a release folder whose manifest lists
///   that hash under the target's file name.
/// - An import must be the object named by its hash, and the import index
///   must list that hash under the target's file name.
/// - A backup must be the object named by its hash, and the backup index must
///   list that hash with `target` as its original path.
///
/// Both paths must be canonical.
///
/// # Errors
/// Returns an error when the source is outside every store, a record cannot
/// be read, or no record matches.
pub fn identify_helper_source(
    stores: &HelperStores,
    source: &Path,
    source_sha256: [u8; 32],
    target: &Path,
) -> Result<HelperSource, CoreError> {
    let target_name = target
        .file_name()
        .ok_or_else(|| CoreError::Validation("target has no file name".into()))?;
    let source_name = source.file_name().unwrap_or_default();
    let parent = source.parent();
    let is_object = || {
        source_name
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(&hex_hash(source_sha256)))
    };

    if let (Some(releases), Some(release_dir)) = (&stores.releases, parent)
        && release_dir.parent() == Some(releases.as_path())
    {
        let manifest: PreparedReleaseManifest = read_versioned_json(
            &release_dir.join(PREPARED_MANIFEST),
            PREPARED_RELEASE_SCHEMA_VERSION,
        )?;
        let listed = release_dir.file_name() == Some(OsStr::new(&manifest.release.0))
            && manifest.dlls.iter().any(|entry| {
                entry.sha256 == source_sha256
                    && same_file_name(entry.source.as_os_str(), source_name)
                    && same_file_name(OsStr::new(&entry.file_name), target_name)
            });
        return if listed {
            Ok(HelperSource::Release)
        } else {
            Err(mismatch("the release manifest", target_name))
        };
    }

    if stores
        .imports
        .as_deref()
        .is_some_and(|root| parent == Some(root))
    {
        let index = ImportStore::new(stores.base.join("imports")).load_index()?;
        let listed = is_object()
            && index.records.iter().any(|record| {
                record.sha256 == source_sha256 && same_file_name(&record.file_name, target_name)
            });
        return if listed {
            Ok(HelperSource::Import)
        } else {
            Err(mismatch("the import index", target_name))
        };
    }

    if stores
        .backups
        .as_deref()
        .is_some_and(|root| parent == Some(root))
    {
        let index = BackupStore::new(stores.base.join("backups")).load_index()?;
        let listed = is_object()
            && index.records.iter().any(|record| {
                record.sha256 == source_sha256
                    && (same_path(&record.original_path, target)
                        || record.original_path.canonicalize().ok().as_deref() == Some(target))
            });
        return if listed {
            Ok(HelperSource::Backup)
        } else {
            Err(CoreError::Validation(
                "the backup index lists no backup with this hash for this file".into(),
            ))
        };
    }

    Err(CoreError::Validation(
        "source is outside the validated cache".into(),
    ))
}

fn mismatch(store: &str, target_name: &OsStr) -> CoreError {
    CoreError::Validation(format!(
        "{store} lists no {} with this hash",
        target_name.to_string_lossy()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackupIndex, BackupRecord, DllVersion, ImportIndex, ImportedDllRecord};
    use std::fs;

    struct Fixture {
        _dir: tempfile::TempDir,
        base: PathBuf,
        game: PathBuf,
    }

    const RELEASE_HASH: [u8; 32] = [1; 32];
    const IMPORT_HASH: [u8; 32] = [2; 32];
    const BACKUP_HASH: [u8; 32] = [3; 32];

    /// A data folder with one prepared release, one import, and one backup,
    /// each recorded for `nvngx_dlss.dll` in one game.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("data");
        let game = dir.path().join("game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("nvngx_dlss.dll"), b"installed").unwrap();
        fs::write(game.join("nvngx_dlssg.dll"), b"installed").unwrap();

        let release = base.join("cache/releases/v2.12.0");
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join("nvngx_dlss.dll"), b"release").unwrap();
        let entry = |name: &str| PreparedReleaseEntry {
            file_name: name.into(),
            version: DllVersion::new(3, 0, 0, 0),
            sha256: RELEASE_HASH,
            source: name.into(),
        };
        write_versioned_json(
            &release.join(PREPARED_MANIFEST),
            PREPARED_RELEASE_SCHEMA_VERSION,
            &PreparedReleaseManifest {
                release: ReleaseId("v2.12.0".into()),
                validation: ReleaseValidation::Full,
                dlls: vec![entry("nvngx_dlss.dll")],
            },
        )
        .unwrap();

        let imports = base.join("imports/objects");
        fs::create_dir_all(&imports).unwrap();
        fs::write(imports.join(hex_hash(IMPORT_HASH)), b"import").unwrap();
        write_versioned_json(
            &base.join("imports/index.json"),
            1,
            &ImportIndex {
                records: vec![ImportedDllRecord {
                    file_name: "nvngx_dlss.dll".into(),
                    version: DllVersion::new(3, 0, 0, 0),
                    sha256: IMPORT_HASH,
                    signer: "NVIDIA Corporation".into(),
                    imported_unix: 1,
                    content_path: imports.join(hex_hash(IMPORT_HASH)),
                }],
            },
        )
        .unwrap();

        let backups = base.join("backups/objects");
        fs::create_dir_all(&backups).unwrap();
        fs::write(backups.join(hex_hash(BACKUP_HASH)), b"backup").unwrap();
        write_versioned_json(
            &base.join("backups/index.json"),
            1,
            &BackupIndex {
                records: vec![BackupRecord {
                    sha256: BACKUP_HASH,
                    content_path: backups.join(hex_hash(BACKUP_HASH)),
                    original_path: game.join("nvngx_dlss.dll"),
                    version: None,
                    created_unix: 1,
                }],
            },
        )
        .unwrap();
        Fixture {
            _dir: dir,
            base,
            game,
        }
    }

    fn identify(
        fixture: &Fixture,
        source: &str,
        hash: [u8; 32],
        target: &str,
    ) -> Result<HelperSource, CoreError> {
        identify_helper_source(
            &HelperStores::resolve(&fixture.base),
            &fixture.base.join(source).canonicalize().unwrap(),
            hash,
            &fixture.game.join(target).canonicalize().unwrap(),
        )
    }

    #[test]
    fn accepts_each_store_only_for_its_recorded_dll() {
        let fixture = fixture();
        let release = "cache/releases/v2.12.0/nvngx_dlss.dll";
        let import = format!("imports/objects/{}", hex_hash(IMPORT_HASH));
        let backup = format!("backups/objects/{}", hex_hash(BACKUP_HASH));
        assert_eq!(
            identify(&fixture, release, RELEASE_HASH, "nvngx_dlss.dll").unwrap(),
            HelperSource::Release
        );
        assert_eq!(
            identify(&fixture, &import, IMPORT_HASH, "nvngx_dlss.dll").unwrap(),
            HelperSource::Import
        );
        assert_eq!(
            identify(&fixture, &backup, BACKUP_HASH, "nvngx_dlss.dll").unwrap(),
            HelperSource::Backup
        );

        // The right file, but for another DLL.
        for (source, hash) in [
            (release, RELEASE_HASH),
            (import.as_str(), IMPORT_HASH),
            (backup.as_str(), BACKUP_HASH),
        ] {
            assert!(identify(&fixture, source, hash, "nvngx_dlssg.dll").is_err());
        }
        // A hash the store does not list for that file.
        assert!(identify(&fixture, release, [9; 32], "nvngx_dlss.dll").is_err());
        assert!(identify(&fixture, &import, [9; 32], "nvngx_dlss.dll").is_err());
        assert!(identify(&fixture, &backup, IMPORT_HASH, "nvngx_dlss.dll").is_err());
    }

    #[test]
    fn rejects_a_backup_for_another_path_with_the_same_name() {
        let fixture = fixture();
        let other = fixture.game.join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("nvngx_dlss.dll"), b"installed").unwrap();
        let backup = format!("backups/objects/{}", hex_hash(BACKUP_HASH));
        assert!(identify(&fixture, &backup, BACKUP_HASH, "other/nvngx_dlss.dll").is_err());
    }

    #[test]
    fn rejects_sources_outside_or_nested_in_the_stores() {
        let fixture = fixture();
        let nested = fixture.base.join("cache/releases/v2.12.0/sub");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("nvngx_dlss.dll"), b"release").unwrap();
        let loose = fixture.base.join("nvngx_dlss.dll");
        fs::write(&loose, b"release").unwrap();
        for source in [
            "cache/releases/v2.12.0/sub/nvngx_dlss.dll",
            "nvngx_dlss.dll",
            "cache/releases/v2.12.0/manifest.json",
        ] {
            assert!(
                identify(&fixture, source, RELEASE_HASH, "nvngx_dlss.dll").is_err(),
                "{source}"
            );
        }
        // An import object that is not named by its hash.
        let renamed = fixture.base.join("imports/objects/renamed");
        fs::write(&renamed, b"import").unwrap();
        assert!(
            identify(
                &fixture,
                "imports/objects/renamed",
                IMPORT_HASH,
                "nvngx_dlss.dll"
            )
            .is_err()
        );
    }

    #[test]
    fn custom_roots_round_trip_and_match_canonical_paths() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("game");
        fs::create_dir_all(&game).unwrap();
        let canonical = game.canonicalize().unwrap();
        assert!(!is_custom_root(dir.path(), &canonical).unwrap());
        save_custom_roots(dir.path(), &[game.join("..").join("game")]).unwrap();
        assert!(is_custom_root(dir.path(), &canonical).unwrap());
        save_custom_roots(dir.path(), &[]).unwrap();
        assert!(!is_custom_root(dir.path(), &canonical).unwrap());
    }
}
