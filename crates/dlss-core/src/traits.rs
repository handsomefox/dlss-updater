use crate::*;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlatformCapabilities {
    pub game_discovery: bool,
    pub dll_versions: bool,
    pub authenticode: bool,
    pub atomic_replace: bool,
    pub elevation: bool,
    pub system_tools: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported capability: {0}")]
    Unsupported(&'static str),
    #[error("stale operation plan")]
    StalePlan,
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("access was denied; elevation is required")]
    PermissionDenied,
    #[error("operation was cancelled")]
    Cancelled,
}

impl CoreError {
    /// Whether this error indicates the operation failed for lack of privileges
    /// and could succeed after elevation. Detected structurally rather than by
    /// matching localized OS error strings.
    pub fn is_permission_denied(&self) -> bool {
        match self {
            CoreError::PermissionDenied => true,
            CoreError::Io(error) => error.kind() == std::io::ErrorKind::PermissionDenied,
            _ => false,
        }
    }
}

pub trait GameLocator: Send + Sync {
    fn discover(&self) -> Result<Vec<GameInstall>, CoreError>;
}
pub trait DllInspector: Send + Sync {
    fn inspect(&self, path: &Path) -> Result<DllMetadata, CoreError>;
}
pub trait TrustVerifier: Send + Sync {
    fn verify(&self, path: &Path) -> Result<SignatureStatus, CoreError>;
}
pub trait KnownDirectories: Send + Sync {
    fn local_app_data(&self) -> Result<PathBuf, CoreError>;
    fn program_data(&self) -> Result<PathBuf, CoreError>;
}
pub trait AtomicFileReplacer: Send + Sync {
    fn replace(
        &self,
        target: &Path,
        source: &Path,
        expected_source_hash: [u8; 32],
    ) -> Result<(), CoreError>;
}
pub trait PrivilegeBroker: Send + Sync {
    fn run_elevated(&self, plan: &Path) -> Result<(), CoreError>;
}
pub trait SystemToolProvider: Send + Sync {
    fn capabilities(&self) -> PlatformCapabilities;
    fn definitions(&self) -> Vec<SystemToolDefinition>;
    fn read(&self, id: &SystemToolId) -> Result<SystemToolState, CoreError>;
    fn apply(&self, plan: &ToolChangePlan) -> Result<ToolChangeResult, CoreError>;
    fn restore(
        &self,
        point: &ToolRestorePoint,
        expected_current_hash: [u8; 32],
        allow_stale: bool,
    ) -> Result<ToolChangeResult, CoreError>;
}
