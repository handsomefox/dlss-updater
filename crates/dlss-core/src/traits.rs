use crate::{
    DiscoveryOutcome, DllMetadata, SystemToolDefinition, SystemToolId, SystemToolState,
    ToolChangePlan, ToolChangeResult, ToolRestorePoint, TrustPolicy, TrustReport,
};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
// These independent flags describe OS facilities, not one compound state.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the independent trust results intentionally remain explicit booleans"
)]
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
    #[must_use]
    pub fn is_permission_denied(&self) -> bool {
        match self {
            CoreError::PermissionDenied => true,
            CoreError::Io(error) => error.kind() == std::io::ErrorKind::PermissionDenied,
            _ => false,
        }
    }
}

pub trait GameLocator: Send + Sync {
    /// Discovers installed games.
    ///
    /// # Errors
    /// Returns an error when a platform discovery source cannot be queried.
    fn discover(&self) -> Result<DiscoveryOutcome, CoreError>;
}
pub trait DllInspector: Send + Sync {
    /// Reads trusted metadata from a DLL candidate.
    ///
    /// # Errors
    /// Returns an error when the file cannot be read or parsed.
    fn inspect(&self, path: &Path) -> Result<DllMetadata, CoreError>;
}
pub trait TrustVerifier: Send + Sync {
    /// Verifies the platform signature policy for a DLL.
    ///
    /// # Errors
    /// Returns an error when trust evaluation cannot be completed.
    fn verify(&self, path: &Path, policy: TrustPolicy) -> Result<TrustReport, CoreError>;
}
pub trait KnownDirectories: Send + Sync {
    /// Returns the per-user application-data directory.
    ///
    /// # Errors
    /// Returns an error when the platform directory cannot be resolved.
    fn local_app_data(&self) -> Result<PathBuf, CoreError>;
    /// Returns the machine-wide application-data directory.
    ///
    /// # Errors
    /// Returns an error when the platform directory cannot be resolved.
    fn program_data(&self) -> Result<PathBuf, CoreError>;
}
pub trait AtomicFileReplacer: Send + Sync {
    /// Replaces `target` with the hash-checked `source`.
    ///
    /// # Errors
    /// Returns an error when validation or replacement fails.
    fn replace(
        &self,
        target: &Path,
        source: &Path,
        expected_source_hash: [u8; 32],
    ) -> Result<(), CoreError>;
}
pub trait PrivilegeBroker: Send + Sync {
    /// Runs an independently validated helper plan with elevated privileges.
    ///
    /// # Errors
    /// Returns an error when elevation is cancelled or the helper cannot start.
    fn run_elevated(&self, plan: &Path) -> Result<(), CoreError>;
}
pub trait SystemToolProvider: Send + Sync {
    fn capabilities(&self) -> PlatformCapabilities;
    fn definitions(&self) -> Vec<SystemToolDefinition>;
    /// Reads the current state for a system tool.
    ///
    /// # Errors
    /// Returns an error when platform state cannot be read.
    fn read(&self, id: &SystemToolId) -> Result<SystemToolState, CoreError>;
    /// Applies a validated system-tool plan.
    ///
    /// # Errors
    /// Returns an error when validation or the platform mutation fails.
    fn apply(&self, plan: &ToolChangePlan) -> Result<ToolChangeResult, CoreError>;
    /// Restores a previously recorded system-tool state.
    ///
    /// # Errors
    /// Returns an error when validation or the platform mutation fails.
    fn restore(
        &self,
        point: &ToolRestorePoint,
        expected_current_hash: [u8; 32],
        allow_stale: bool,
    ) -> Result<ToolChangeResult, CoreError>;
}
