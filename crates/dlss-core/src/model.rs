use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.into())
            }
        }
    };
}
string_id!(GameId);
string_id!(DllInstallationId);
string_id!(ReleaseId);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StoreKind {
    Steam,
    Epic,
    Gog,
    Manual,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GameInstall {
    pub id: GameId,
    pub name: String,
    pub store: StoreKind,
    pub root: PathBuf,
    pub dlls: Vec<DllInstallation>,
    #[serde(default)]
    pub inspection_errors: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DllVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub revision: u16,
}

impl DllVersion {
    pub const fn new(major: u16, minor: u16, build: u16, revision: u16) -> Self {
        Self {
            major,
            minor,
            build,
            revision,
        }
    }
}
impl std::fmt::Display for DllVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.build, self.revision
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SignatureStatus {
    Trusted,
    Untrusted,
    Unsigned,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DllMetadata {
    pub version: Option<DllVersion>,
    pub sha256: [u8; 32],
    pub signature: SignatureStatus,
    pub x86_64: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DllInstallation {
    pub id: DllInstallationId,
    pub game_id: GameId,
    pub path: PathBuf,
    pub file_name: OsString,
    pub metadata: DllMetadata,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReleaseState {
    MetadataOnly,
    Downloading,
    Downloaded,
    Validating,
    Ready,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseMetadata {
    pub id: ReleaseId,
    pub tag: String,
    pub asset_name: String,
    pub published_unix: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogDll {
    pub file_name: OsString,
    pub version: DllVersion,
    pub sha256: [u8; 32],
    pub source: PathBuf,
    pub release: ReleaseId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CachedRelease {
    pub metadata: ReleaseMetadata,
    pub state: ReleaseState,
    pub dlls: Vec<CatalogDll>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Comparison {
    Upgrade,
    Downgrade,
    Identical,
    DifferentBuild,
    Unknown,
    Unavailable,
}

pub fn compare_target(
    installed_version: Option<DllVersion>,
    installed_hash: [u8; 32],
    target_version: Option<DllVersion>,
    target_hash: [u8; 32],
) -> Comparison {
    if installed_hash == target_hash {
        return Comparison::Identical;
    }
    match (installed_version, target_version) {
        (Some(installed), Some(target)) => match target.cmp(&installed) {
            std::cmp::Ordering::Greater => Comparison::Upgrade,
            std::cmp::Ordering::Less => Comparison::Downgrade,
            std::cmp::Ordering::Equal => Comparison::DifferentBuild,
        },
        _ => Comparison::Unknown,
    }
}

pub fn compare_dll(installed: Option<&DllMetadata>, target: Option<&CatalogDll>) -> Comparison {
    match (installed, target) {
        (Some(installed), Some(target)) => compare_target(
            installed.version,
            installed.sha256,
            Some(target.version),
            target.sha256,
        ),
        (_, None) => Comparison::Unavailable,
        (None, Some(_)) => Comparison::Unknown,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DesiredDll {
    KeepInstalled,
    LatestOfficial,
    Cached {
        release: ReleaseId,
        sha256: [u8; 32],
    },
    Restore {
        backup_sha256: [u8; 32],
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TargetProfile {
    pub targets: BTreeMap<DllInstallationId, DesiredDll>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlannedSwap {
    pub game: GameId,
    pub installation: DllInstallationId,
    pub target_path: PathBuf,
    pub expected_sha256: [u8; 32],
    pub source_path: PathBuf,
    pub source_sha256: [u8; 32],
    pub comparison: Comparison,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationPlan {
    pub nonce: String,
    pub swaps: Vec<PlannedSwap>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanSummary {
    pub games: usize,
    pub dlls: usize,
    pub upgrades: usize,
    pub downgrades: usize,
}

impl OperationPlan {
    pub fn summary(&self) -> PlanSummary {
        let games: std::collections::HashSet<_> =
            self.swaps.iter().map(|swap| &swap.game).collect();
        let mut s = PlanSummary {
            games: games.len(),
            dlls: self.swaps.len(),
            ..Default::default()
        };
        for swap in &self.swaps {
            match swap.comparison {
                Comparison::Upgrade => s.upgrades += 1,
                Comparison::Downgrade => s.downgrades += 1,
                _ => {}
            }
        }
        s
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SwapResult {
    pub installation: DllInstallationId,
    pub result: Result<DllMetadata, String>,
    pub backup: Option<BackupRecord>,
    /// True when the swap failed solely because access was denied, signalling
    /// that retrying under elevation may succeed.
    #[serde(default)]
    pub denied: bool,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BatchResult {
    pub swaps: Vec<SwapResult>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackupRecord {
    pub sha256: [u8; 32],
    pub content_path: PathBuf,
    pub original_path: PathBuf,
    pub version: Option<DllVersion>,
    pub created_unix: i64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivityRecord {
    pub timestamp_unix: i64,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackupIndex {
    pub records: Vec<BackupRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ElevatedFilePlan {
    pub game_id: GameId,
    pub game_root: PathBuf,
    pub operation: OperationPlan,
    pub nonce: String,
    pub result_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ElevatedHelperPlan {
    SystemTool(crate::ToolChangePlan),
    FileSwap(ElevatedFilePlan),
}

#[cfg(test)]
mod tests {
    use super::*;
    fn meta(v: Option<DllVersion>, hash: u8) -> DllMetadata {
        DllMetadata {
            version: v,
            sha256: [hash; 32],
            signature: SignatureStatus::Trusted,
            x86_64: true,
        }
    }
    fn dll(v: DllVersion, hash: u8) -> CatalogDll {
        CatalogDll {
            file_name: "x.dll".into(),
            version: v,
            sha256: [hash; 32],
            source: "x".into(),
            release: "r".into(),
        }
    }
    #[test]
    fn comparison_covers_version_and_hash_semantics() {
        let v = DllVersion::new(1, 2, 3, 4);
        assert_eq!(
            compare_dll(Some(&meta(Some(v), 1)), Some(&dll(v, 1))),
            Comparison::Identical
        );
        assert_eq!(
            compare_dll(Some(&meta(Some(v), 1)), Some(&dll(v, 2))),
            Comparison::DifferentBuild
        );
        assert_eq!(
            compare_dll(
                Some(&meta(Some(v), 1)),
                Some(&dll(DllVersion::new(2, 0, 0, 0), 2))
            ),
            Comparison::Upgrade
        );
        assert_eq!(
            compare_dll(Some(&meta(None, 1)), Some(&dll(v, 2))),
            Comparison::Unknown
        );
        assert_eq!(
            compare_dll(Some(&meta(Some(v), 1)), None),
            Comparison::Unavailable
        );
    }
}
