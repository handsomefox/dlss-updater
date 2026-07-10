use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedState {
    pub schema_version: u32,
    pub custom_roots: Vec<std::path::PathBuf>,
    pub show_advanced: bool,
    pub target_profile: dlss_core::TargetProfile,
    pub tool_restore_points: Vec<dlss_core::ToolRestorePoint>,
    pub activity: Vec<dlss_core::ActivityRecord>,
}
impl Default for PersistedState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            custom_roots: Vec::new(),
            show_advanced: false,
            target_profile: dlss_core::TargetProfile::default(),
            tool_restore_points: Vec::new(),
            activity: Vec::new(),
        }
    }
}
