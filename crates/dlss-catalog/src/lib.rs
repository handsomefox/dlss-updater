//! Validated, constrained Streamline archive ingestion.

use dlss_core::{
    CatalogDll, DllInspector, ReleaseId, ReleaseValidation, RevocationStatus, SignatureStatus,
    TrustPolicy, TrustVerifier,
};
use sha2::{Digest, Sha256};

mod github;
pub use github::*;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

static STAGING_NONCE: AtomicU64 = AtomicU64::new(0);

enum CandidateTrust {
    Accepted { revocation_fallback: bool },
    Rejected(String),
}

pub const CATALOG_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct CatalogCacheIndex {
    pub etag: Option<String>,
    pub assets: Vec<OfficialAsset>,
    pub releases: Vec<dlss_core::CachedRelease>,
}

impl CatalogCacheIndex {
    /// Loads the cache index or returns an empty index when it does not exist.
    ///
    /// # Errors
    /// Returns an error when the persisted envelope is unreadable or invalid.
    pub fn load(path: &Path) -> Result<Self, dlss_core::CoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        dlss_core::read_versioned_json(path, CATALOG_SCHEMA_VERSION)
    }

    /// Atomically persists the cache index.
    ///
    /// # Errors
    /// Returns an error when serialization or persistence fails.
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

    pub fn remove_release(&mut self, id: &ReleaseId) {
        self.releases.retain(|release| &release.metadata.id != id);
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
    #[error("production DLL validation failed: {0}")]
    RejectedCandidates(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedRelease {
    pub dlls: Vec<CatalogDll>,
    pub validation: ReleaseValidation,
}

/// Extracts only immediate production DLLs in `bin/x64`, never development DLLs.
///
/// # Errors
/// Returns an error when the archive or any candidate violates path, size, PE,
/// signature, duplication, or persistence constraints.
pub fn validate_and_extract(
    archive: &Path,
    destination: &Path,
    release: &ReleaseId,
    inspector: &dyn DllInspector,
    trust: &dyn TrustVerifier,
) -> Result<ValidatedRelease, CatalogError> {
    let parent = destination.parent().ok_or_else(|| {
        CatalogError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "release destination has no parent",
        ))
    })?;
    fs::create_dir_all(parent)?;
    let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
    let leaf = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("release");
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let unique = format!("{}-{timestamp:032x}-{nonce:016x}", std::process::id());
    let staging = parent.join(format!(".{leaf}.staging-{unique}"));
    let previous = parent.join(format!(".{leaf}.previous-{unique}"));
    let result = extract_to_staging(archive, &staging, release, inspector, trust);
    let mut extracted = match result {
        Ok(extracted) => extracted,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let had_previous = destination.exists();
    if had_previous && let Err(error) = fs::rename(destination, &previous) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(&staging, destination) {
        if had_previous {
            let _ = fs::rename(&previous, destination);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error.into());
    }
    if had_previous && let Err(error) = fs::remove_dir_all(&previous) {
        let _ = fs::rename(destination, &staging);
        let _ = fs::rename(&previous, destination);
        let _ = fs::remove_dir_all(&staging);
        return Err(error.into());
    }
    for dll in &mut extracted.dlls {
        dll.source = destination.join(&dll.file_name);
    }
    Ok(extracted)
}

fn extract_to_staging(
    archive: &Path,
    destination: &Path,
    release: &ReleaseId,
    inspector: &dyn DllInspector,
    trust: &dyn TrustVerifier,
) -> Result<ValidatedRelease, CatalogError> {
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
    let mut rejections = Vec::new();
    let mut used_revocation_fallback = false;
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
            || parts[2].eq_ignore_ascii_case("NvLowLatencyVk.dll")
        {
            continue;
        }
        if entry.size() > MAX_DLL_BYTES {
            return Err(CatalogError::TooLarge);
        }
        let output = destination.join(parts[2]);
        // Keep the final `.dll` extension: Windows Authenticode provider
        // selection can reject an otherwise valid PE when staged as
        // `*.dll.partial`.
        let temporary = destination.join(format!(".partial-{}", parts[2]));
        let sha256 = copy_staged_candidate(&mut entry, &temporary)?;
        let metadata = inspector
            .inspect(&temporary)
            .map_err(|e| CatalogError::Inspection(parts[2].into(), e.to_string()))?;
        if !metadata.x86_64 {
            let _ = fs::remove_file(&temporary);
            return Err(CatalogError::InvalidPe(parts[2].into()));
        }
        match verify_catalog_candidate(&temporary, parts[2], trust)? {
            CandidateTrust::Accepted {
                revocation_fallback,
            } => used_revocation_fallback |= revocation_fallback,
            CandidateTrust::Rejected(reason) => {
                let _ = fs::remove_file(&temporary);
                rejections.push(reason);
                continue;
            }
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
            sha256,
            source: output,
            release: release.clone(),
        });
    }
    if extracted.is_empty() {
        return if rejections.is_empty() {
            Err(CatalogError::NoProductionDlls)
        } else {
            Err(CatalogError::RejectedCandidates(rejections.join("; ")))
        };
    }
    if !rejections.is_empty() {
        tracing::warn!(rejections = %rejections.join("; "), "some production DLL candidates were rejected");
    }
    Ok(ValidatedRelease {
        dlls: extracted,
        validation: if used_revocation_fallback {
            ReleaseValidation::RevocationUnavailableFallback
        } else {
            ReleaseValidation::Full
        },
    })
}

fn copy_staged_candidate(reader: &mut impl Read, path: &Path) -> Result<[u8; 32], CatalogError> {
    let mut file = File::create(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut written = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        written += count as u64;
        if written > MAX_DLL_BYTES {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(CatalogError::TooLarge);
        }
        hasher.update(&buffer[..count]);
        file.write_all(&buffer[..count])?;
    }
    file.flush()?;
    file.sync_all()?;
    drop(file);
    Ok(hasher.finalize().into())
}

fn verify_catalog_candidate(
    path: &Path,
    name: &str,
    trust: &dyn TrustVerifier,
) -> Result<CandidateTrust, CatalogError> {
    let report = trust
        .verify(path, TrustPolicy::OfficialNvidiaCatalog)
        .map_err(|error| CatalogError::Inspection(name.into(), error.to_string()))?;
    if report.signature == SignatureStatus::Trusted
        && report
            .signer
            .as_deref()
            .is_some_and(dlss_core::is_nvidia_signer)
    {
        return Ok(CandidateTrust::Accepted {
            revocation_fallback: report.revocation == RevocationStatus::UnavailableFallback,
        });
    }
    let native = report.native_failure.as_ref().map_or_else(
        || "no native status".into(),
        |failure| {
            format!(
                "0x{:08X}: {}",
                failure.status.cast_unsigned(),
                failure.reason
            )
        },
    );
    Ok(CandidateTrust::Rejected(format!(
        "{name}: signature={:?}, signer={}, {native}",
        report.signature,
        report.signer.as_deref().unwrap_or("unknown")
    )))
}

pub(crate) fn sha256_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    Ok(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dlss_core::{CoreError, DllMetadata, DllVersion, TrustReport};
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    struct Inspector;
    impl DllInspector for Inspector {
        fn inspect(&self, path: &Path) -> Result<DllMetadata, CoreError> {
            let bytes = fs::read(path)?;
            Ok(DllMetadata {
                version: Some(DllVersion::new(
                    2,
                    12,
                    0,
                    u16::try_from(bytes.len()).unwrap(),
                )),
                sha256: Sha256::digest(&bytes).into(),
                signature: SignatureStatus::Trusted,
                x86_64: !bytes.starts_with(b"x86"),
            })
        }
    }

    struct Trust(bool);
    impl TrustVerifier for Trust {
        fn verify(&self, _path: &Path, _policy: TrustPolicy) -> Result<TrustReport, CoreError> {
            let signature = if self.0 {
                SignatureStatus::Trusted
            } else {
                SignatureStatus::Untrusted
            };
            Ok(TrustReport {
                signature,
                signer: self.0.then(|| "NVIDIA Corporation".into()),
                revocation: if self.0 {
                    RevocationStatus::Verified
                } else {
                    RevocationStatus::NotVerified
                },
                native_failure: None,
            })
        }
    }

    struct TrustByContents;
    impl TrustVerifier for TrustByContents {
        fn verify(&self, path: &Path, _policy: TrustPolicy) -> Result<TrustReport, CoreError> {
            let trusted = fs::read(path)? != b"untrusted";
            Ok(TrustReport {
                signature: if trusted {
                    SignatureStatus::Trusted
                } else {
                    SignatureStatus::Untrusted
                },
                signer: trusted.then(|| "NVIDIA Corporation".into()),
                revocation: if trusted {
                    RevocationStatus::Verified
                } else {
                    RevocationStatus::NotVerified
                },
                native_failure: None,
            })
        }
    }

    struct TrustRequiresDllExtension;
    impl TrustVerifier for TrustRequiresDllExtension {
        fn verify(&self, path: &Path, _policy: TrustPolicy) -> Result<TrustReport, CoreError> {
            let trusted = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"));
            Ok(TrustReport {
                signature: if trusted {
                    SignatureStatus::Trusted
                } else {
                    SignatureStatus::Untrusted
                },
                signer: trusted.then(|| "NVIDIA Corporation".into()),
                revocation: RevocationStatus::Verified,
                native_failure: None,
            })
        }
    }

    struct ReportTrust {
        signature: SignatureStatus,
        signer: Option<&'static str>,
        revocation: RevocationStatus,
        native_status: Option<i32>,
    }

    impl TrustVerifier for ReportTrust {
        fn verify(&self, _path: &Path, policy: TrustPolicy) -> Result<TrustReport, CoreError> {
            assert_eq!(policy, TrustPolicy::OfficialNvidiaCatalog);
            Ok(TrustReport {
                signature: self.signature,
                signer: self.signer.map(str::to_owned),
                revocation: self.revocation,
                native_failure: self
                    .native_status
                    .map(|status| dlss_core::NativeTrustFailure {
                        status,
                        reason: "test trust failure".into(),
                    }),
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
            &ReleaseId("v2.12.0".into()),
            &Inspector,
            &Trust(true),
        )
        .unwrap();
        assert_eq!(dlls.dlls.len(), 1);
        assert_eq!(dlls.dlls[0].file_name, "sl.common.dll");
        assert_eq!(fs::read(output.join("sl.common.dll")).unwrap(), b"pe64");
    }

    #[test]
    fn enforces_nvidia_signer_and_aggregates_native_rejections() {
        let (directory, path) = archive(&[
            ("bin/x64/sl.one.dll", b"one"),
            ("bin/x64/sl.two.dll", b"two"),
        ]);
        let result = validate_and_extract(
            &path,
            &directory.path().join("output"),
            &ReleaseId("r".into()),
            &Inspector,
            &ReportTrust {
                signature: SignatureStatus::Untrusted,
                signer: Some("Example Publisher"),
                revocation: RevocationStatus::NotVerified,
                native_status: Some(0x800B_0109_u32.cast_signed()),
            },
        );
        let Err(CatalogError::RejectedCandidates(message)) = result else {
            panic!("unexpected validation result: {result:?}");
        };
        assert!(message.contains("sl.one.dll"));
        assert!(message.contains("sl.two.dll"));
        assert!(message.contains("0x800B0109"));
        assert!(message.contains("Example Publisher"));
    }

    #[test]
    fn records_offline_revocation_fallback_as_a_warning_quality() {
        let (directory, path) = archive(&[("bin/x64/sl.common.dll", b"valid")]);
        let validated = validate_and_extract(
            &path,
            &directory.path().join("output"),
            &ReleaseId("r".into()),
            &Inspector,
            &ReportTrust {
                signature: SignatureStatus::Trusted,
                signer: Some("NVIDIA Corporation"),
                revocation: RevocationStatus::UnavailableFallback,
                native_status: Some(0x8009_2013_u32.cast_signed()),
            },
        )
        .unwrap();
        assert_eq!(
            validated.validation,
            ReleaseValidation::RevocationUnavailableFallback
        );
    }

    #[test]
    fn trusted_non_nvidia_publisher_is_rejected() {
        let (directory, path) = archive(&[("bin/x64/sl.common.dll", b"valid")]);
        let result = validate_and_extract(
            &path,
            &directory.path().join("output"),
            &ReleaseId("r".into()),
            &Inspector,
            &ReportTrust {
                signature: SignatureStatus::Trusted,
                signer: Some("Example Publisher"),
                revocation: RevocationStatus::Verified,
                native_status: None,
            },
        );
        let Err(CatalogError::RejectedCandidates(message)) = result else {
            panic!("unexpected validation result: {result:?}");
        };
        assert!(message.contains("Example Publisher"));
    }

    #[test]
    fn rejects_traversal_even_when_entry_would_not_be_extracted() {
        let (directory, path) = archive(&[("../outside.txt", b"bad")]);
        let result = validate_and_extract(
            &path,
            &directory.path().join("output"),
            &ReleaseId("r".into()),
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
            &ReleaseId("r".into()),
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
            &ReleaseId("r".into()),
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
            &ReleaseId("r".into()),
            &Inspector,
            &Trust(false),
        );
        assert!(matches!(
            untrusted,
            Err(CatalogError::RejectedCandidates(_))
        ));

        let (directory, path) = archive(&[("bin/x64/sl.common.dll", b"x86-binary")]);
        let wrong_arch = validate_and_extract(
            &path,
            &directory.path().join("wrong-arch"),
            &ReleaseId("r".into()),
            &Inspector,
            &Trust(true),
        );
        assert!(matches!(wrong_arch, Err(CatalogError::InvalidPe(_))));
    }

    #[test]
    fn ignores_nv_low_latency_vk_candidate() {
        let (directory, path) = archive(&[
            ("bin/x64/NvLowLatencyVk.DLL", b"x86-binary"),
            ("bin/x64/nvngx_dlss.dll", b"trusted"),
        ]);
        let output = directory.path().join("output");
        let dlls = validate_and_extract(
            &path,
            &output,
            &ReleaseId("r".into()),
            &Inspector,
            &Trust(true),
        )
        .unwrap();
        assert_eq!(dlls.dlls.len(), 1);
        assert_eq!(dlls.dlls[0].file_name, "nvngx_dlss.dll");
        assert!(!output.join("NvLowLatencyVk.DLL").exists());
    }

    #[test]
    fn skips_untrusted_candidate_when_trusted_dlls_remain() {
        let (directory, path) = archive(&[
            ("bin/x64/NvLowLatencyVk.dll", b"untrusted"),
            ("bin/x64/nvngx_dlss.dll", b"trusted"),
        ]);
        let output = directory.path().join("output");
        let dlls = validate_and_extract(
            &path,
            &output,
            &ReleaseId("r".into()),
            &Inspector,
            &TrustByContents,
        )
        .unwrap();
        assert_eq!(dlls.dlls.len(), 1);
        assert_eq!(dlls.dlls[0].file_name, "nvngx_dlss.dll");
        assert!(!output.join("NvLowLatencyVk.dll").exists());
    }

    #[test]
    fn authenticode_validation_keeps_dll_extension_during_staging() {
        let (directory, path) = archive(&[("bin/x64/nvngx_dlss.dll", b"trusted")]);
        let output = directory.path().join("output");
        let dlls = validate_and_extract(
            &path,
            &output,
            &ReleaseId("r".into()),
            &Inspector,
            &TrustRequiresDllExtension,
        )
        .unwrap();
        assert_eq!(dlls.dlls.len(), 1);
        assert!(output.join("nvngx_dlss.dll").exists());
    }

    #[test]
    fn failed_validation_preserves_ready_cache_and_cleans_staging() {
        let (directory, path) = archive(&[
            ("bin/x64/sl.common.dll", b"valid"),
            ("bin/x64/sl.bad.dll", b"x86-binary"),
        ]);
        let output = directory.path().join("release");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("ready.dll"), b"previous").unwrap();

        let result = validate_and_extract(
            &path,
            &output,
            &ReleaseId("r".into()),
            &Inspector,
            &Trust(true),
        );

        assert!(matches!(result, Err(CatalogError::InvalidPe(_))));
        assert_eq!(fs::read(output.join("ready.dll")).unwrap(), b"previous");
        let siblings: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(
            siblings
                .iter()
                .all(|name| !name.to_string_lossy().contains("staging"))
        );
    }

    #[test]
    fn successful_validation_commits_complete_release_at_once() {
        let (directory, path) = archive(&[("bin/x64/sl.common.dll", b"valid")]);
        let output = directory.path().join("release");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("stale.dll"), b"previous").unwrap();

        let dlls = validate_and_extract(
            &path,
            &output,
            &ReleaseId("r".into()),
            &Inspector,
            &Trust(true),
        )
        .unwrap();

        assert!(!output.join("stale.dll").exists());
        assert_eq!(dlls.dlls[0].source, output.join("sl.common.dll"));
        assert!(dlls.dlls[0].source.exists());
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

    #[test]
    fn legacy_ready_release_shape_requires_revalidation() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.json");
        fs::write(
            &path,
            r#"{
                "schema_version": 1,
                "payload": {
                    "etag": null,
                    "assets": [],
                    "releases": [{
                        "metadata": {
                            "id": "v1",
                            "tag": "v1",
                            "asset_name": "streamline-sdk-v1.zip",
                            "published_unix": 0
                        },
                        "state": "Ready",
                        "dlls": []
                    }]
                }
            }"#,
        )
        .unwrap();
        assert!(CatalogCacheIndex::load(&path).is_err());
        assert!(
            path.exists(),
            "schema invalidation must not remove cached artifacts"
        );
    }
}
