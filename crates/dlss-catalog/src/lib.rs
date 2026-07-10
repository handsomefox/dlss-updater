//! Validated, constrained Streamline archive ingestion.

use dlss_core::{CatalogDll, DllInspector, ReleaseId, SignatureStatus, TrustVerifier};
use sha2::{Digest, Sha256};

mod github;
pub use github::*;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub const CATALOG_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct CatalogCacheIndex {
    pub etag: Option<String>,
    pub assets: Vec<OfficialAsset>,
    pub releases: Vec<dlss_core::CachedRelease>,
}

impl CatalogCacheIndex {
    pub fn load(path: &Path) -> Result<Self, dlss_core::CoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        dlss_core::read_versioned_json(path, CATALOG_SCHEMA_VERSION)
    }

    pub fn save(&self, path: &Path) -> Result<(), dlss_core::CoreError> {
        dlss_core::write_versioned_json(path, CATALOG_SCHEMA_VERSION, self)
    }

    pub fn upsert_release(&mut self, release: dlss_core::CachedRelease) {
        if let Some(existing) = self
            .releases
            .iter_mut()
            .find(|existing| existing.metadata.id == release.metadata.id)
        {
            *existing = release;
        } else {
            self.releases.push(release);
        }
    }
}

pub const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;
pub const MAX_DLL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid ZIP: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("archive exceeds safety limit")]
    TooLarge,
    #[error("archive contains too many entries")]
    TooManyEntries,
    #[error("unsafe archive path: {0}")]
    UnsafePath(String),
    #[error("duplicate archive entry: {0}")]
    DuplicateEntry(String),
    #[error("candidate is not a valid x86-64 PE DLL: {0}")]
    InvalidPe(String),
    #[error("candidate is not Authenticode trusted: {0}")]
    Untrusted(String),
    #[error("inspection failed for {0}: {1}")]
    Inspection(String, String),
    #[error("archive contains no immediate production x86-64 DLLs")]
    NoProductionDlls,
}

/// Extracts only immediate production DLLs in `bin/x64`, never development DLLs.
pub fn validate_and_extract(
    archive: &Path,
    destination: &Path,
    release: ReleaseId,
    inspector: &dyn DllInspector,
    trust: &dyn TrustVerifier,
) -> Result<Vec<CatalogDll>, CatalogError> {
    if archive.metadata()?.len() > MAX_ARCHIVE_BYTES {
        return Err(CatalogError::TooLarge);
    }
    let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
    if zip.len() > MAX_ENTRIES {
        return Err(CatalogError::TooManyEntries);
    }
    fs::create_dir_all(destination)?;
    let mut names = HashSet::new();
    let mut extracted = Vec::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let raw_name = entry.name().replace('\\', "/");
        let Some(safe) = entry.enclosed_name() else {
            return Err(CatalogError::UnsafePath(raw_name));
        };
        let normalized = safe.to_string_lossy().replace('\\', "/");
        let folded_path = normalized.to_ascii_lowercase();
        if !names.insert(folded_path) {
            return Err(CatalogError::DuplicateEntry(normalized));
        }
        let parts: Vec<_> = normalized
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        if parts.len() != 3
            || !parts[0].eq_ignore_ascii_case("bin")
            || !parts[1].eq_ignore_ascii_case("x64")
            || !parts[2].to_ascii_lowercase().ends_with(".dll")
        {
            continue;
        }
        if entry.size() > MAX_DLL_BYTES {
            return Err(CatalogError::TooLarge);
        }
        let output = destination.join(parts[2]);
        let temporary = output.with_extension("dll.partial");
        let mut file = File::create(&temporary)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut written = 0_u64;
        loop {
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            written += count as u64;
            if written > MAX_DLL_BYTES {
                let _ = fs::remove_file(&temporary);
                return Err(CatalogError::TooLarge);
            }
            hasher.update(&buffer[..count]);
            file.write_all(&buffer[..count])?;
        }
        file.flush()?;
        file.sync_all()?;
        let metadata = inspector
            .inspect(&temporary)
            .map_err(|e| CatalogError::Inspection(parts[2].into(), e.to_string()))?;
        if !metadata.x86_64 {
            let _ = fs::remove_file(&temporary);
            return Err(CatalogError::InvalidPe(parts[2].into()));
        }
        if trust
            .verify(&temporary)
            .map_err(|e| CatalogError::Inspection(parts[2].into(), e.to_string()))?
            != SignatureStatus::Trusted
        {
            let _ = fs::remove_file(&temporary);
            return Err(CatalogError::Untrusted(parts[2].into()));
        }
        let Some(version) = metadata.version else {
            let _ = fs::remove_file(&temporary);
            return Err(CatalogError::InvalidPe(parts[2].into()));
        };
        if output.exists() {
            fs::remove_file(&output)?;
        }
        fs::rename(&temporary, &output)?;
        extracted.push(CatalogDll {
            file_name: parts[2].into(),
            version,
            sha256: hasher.finalize().into(),
            source: output,
            release: release.clone(),
        });
    }
    if extracted.is_empty() {
        return Err(CatalogError::NoProductionDlls);
    }
    Ok(extracted)
}

pub fn sha256_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    Ok(hash.finalize().into())
}

pub fn cache_release_dir(cache: &Path, release: &ReleaseId) -> PathBuf {
    cache.join("releases").join(&release.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dlss_core::{CoreError, DllMetadata, DllVersion};
    use std::io::Write;
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    struct Inspector;
    impl DllInspector for Inspector {
        fn inspect(&self, path: &Path) -> Result<DllMetadata, CoreError> {
            let bytes = fs::read(path)?;
            Ok(DllMetadata {
                version: Some(DllVersion::new(2, 12, 0, bytes.len() as u16)),
                sha256: Sha256::digest(&bytes).into(),
                signature: SignatureStatus::Trusted,
                x86_64: !bytes.starts_with(b"x86"),
            })
        }
    }

    struct Trust(bool);
    impl TrustVerifier for Trust {
        fn verify(&self, _path: &Path) -> Result<SignatureStatus, CoreError> {
            Ok(if self.0 {
                SignatureStatus::Trusted
            } else {
                SignatureStatus::Untrusted
            })
        }
    }

    fn archive(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
        let directory = tempdir().unwrap();
        let path = directory.path().join("release.zip");
        let file = File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
        (directory, path)
    }

    #[test]
    fn extracts_only_immediate_production_dlls() {
        let (directory, path) = archive(&[
            ("bin/x64/sl.common.dll", b"pe64"),
            ("bin/x64/development/sl.common.dll", b"debug"),
            ("bin/x64/readme.txt", b"text"),
            ("bin/x86/sl.common.dll", b"x86"),
        ]);
        let output = directory.path().join("output");
        let dlls = validate_and_extract(
            &path,
            &output,
            ReleaseId("v2.12.0".into()),
            &Inspector,
            &Trust(true),
        )
        .unwrap();
        assert_eq!(dlls.len(), 1);
        assert_eq!(dlls[0].file_name, "sl.common.dll");
        assert_eq!(fs::read(output.join("sl.common.dll")).unwrap(), b"pe64");
    }

    #[test]
    fn rejects_traversal_even_when_entry_would_not_be_extracted() {
        let (directory, path) = archive(&[("../outside.txt", b"bad")]);
        let result = validate_and_extract(
            &path,
            &directory.path().join("output"),
            ReleaseId("r".into()),
            &Inspector,
            &Trust(true),
        );
        assert!(matches!(result, Err(CatalogError::UnsafePath(_))));
    }

    #[test]
    fn rejects_case_insensitive_duplicate_dll_names() {
        let (directory, path) = archive(&[
            ("bin/x64/sl.common.dll", b"one"),
            ("BIN/X64/SL.COMMON.DLL", b"two"),
        ]);
        let result = validate_and_extract(
            &path,
            &directory.path().join("output"),
            ReleaseId("r".into()),
            &Inspector,
            &Trust(true),
        );
        assert!(matches!(result, Err(CatalogError::DuplicateEntry(_))));
    }

    #[test]
    fn rejects_case_insensitive_duplicates_outside_production_tree() {
        let (directory, path) =
            archive(&[("docs/readme.txt", b"one"), ("DOCS/README.TXT", b"two")]);
        let result = validate_and_extract(
            &path,
            &directory.path().join("output"),
            ReleaseId("r".into()),
            &Inspector,
            &Trust(true),
        );
        assert!(matches!(result, Err(CatalogError::DuplicateEntry(_))));
    }

    #[test]
    fn rejects_untrusted_and_non_x64_candidates() {
        let (directory, path) = archive(&[("bin/x64/sl.common.dll", b"valid")]);
        let untrusted = validate_and_extract(
            &path,
            &directory.path().join("untrusted"),
            ReleaseId("r".into()),
            &Inspector,
            &Trust(false),
        );
        assert!(matches!(untrusted, Err(CatalogError::Untrusted(_))));

        let (directory, path) = archive(&[("bin/x64/sl.common.dll", b"x86-binary")]);
        let wrong_arch = validate_and_extract(
            &path,
            &directory.path().join("wrong-arch"),
            ReleaseId("r".into()),
            &Inspector,
            &Trust(true),
        );
        assert!(matches!(wrong_arch, Err(CatalogError::InvalidPe(_))));
    }

    #[test]
    fn supplied_v212_archive_has_16_immediate_production_dlls() {
        let archive = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("streamline-sdk-v2.12.0.zip");
        if !archive.exists() {
            return;
        }
        let mut zip = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
        let mut names = Vec::new();
        for index in 0..zip.len() {
            let entry = zip.by_index(index).unwrap();
            let normalized = entry.name().replace('\\', "/");
            let parts: Vec<_> = normalized.split('/').collect();
            if parts.len() == 3
                && parts[0].eq_ignore_ascii_case("bin")
                && parts[1].eq_ignore_ascii_case("x64")
                && parts[2].to_ascii_lowercase().ends_with(".dll")
            {
                names.push(parts[2].to_ascii_lowercase());
            }
        }
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 16, "production entries: {names:?}");
    }

    #[test]
    fn catalog_index_persists_etag_assets_and_ready_releases() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.json");
        let index = CatalogCacheIndex {
            etag: Some("etag-value".into()),
            assets: vec![OfficialAsset {
                release: dlss_core::ReleaseMetadata {
                    id: ReleaseId("v1".into()),
                    tag: "v1".into(),
                    asset_name: "streamline-sdk-v1.zip".into(),
                    published_unix: 1,
                },
                download_url: "https://example.invalid/release.zip".into(),
                size: 42,
                digest: Some([7; 32]),
            }],
            releases: Vec::new(),
        };
        index.save(&path).unwrap();
        let loaded = CatalogCacheIndex::load(&path).unwrap();
        assert_eq!(loaded.etag, index.etag);
        assert_eq!(loaded.assets, index.assets);
    }
}
