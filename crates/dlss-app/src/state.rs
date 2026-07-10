use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PersistedState {
    pub custom_roots: Vec<std::path::PathBuf>,
    pub target_profile: dlss_core::TargetProfile,
    pub tool_restore_points: Vec<dlss_core::ToolRestorePoint>,
    pub activity: Vec<dlss_core::ActivityRecord>,
}
