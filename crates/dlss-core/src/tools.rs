use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SystemToolId(pub String);
pub const DLSS_INDICATOR_TOOL_ID: &str = "nvidia.dlss_indicator";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SystemToolScope {
    Global,
    PerGame,
    PerPlatform,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SystemToolDefinition {
    pub id: SystemToolId,
    pub display_name: String,
    pub warning: String,
    pub scope: SystemToolScope,
    pub requires_elevation: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SystemToolState {
    NotConfigured,
    Off,
    DlssIndicatorDebug,
    DlssIndicatorProduction,
    CustomDword(u32),
    UnexpectedType { registry_type: u32, raw: Vec<u8> },
    Unavailable(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryValueSnapshot {
    pub existed: bool,
    pub registry_view: RegistryView,
    pub registry_type: Option<u32>,
    pub raw: Vec<u8>,
    pub captured_unix: i64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RegistryView {
    View32,
    View64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolChangePlan {
    pub tool_id: SystemToolId,
    pub desired: SystemToolState,
    #[serde(default)]
    pub restore_point: Option<ToolRestorePoint>,
    pub expected_current_hash: [u8; 32],
    pub nonce: String,
    pub result_path: std::path::PathBuf,
    #[serde(default)]
    pub allow_stale_restore: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolRestorePoint {
    pub tool_id: SystemToolId,
    pub snapshot: RegistryValueSnapshot,
    pub state_after_hash: [u8; 32],
    pub app_version: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolChangeResult {
    pub tool_id: SystemToolId,
    pub state: SystemToolState,
    pub restore_point: Option<ToolRestorePoint>,
}

pub fn indicator_state(value: Option<(u32, &[u8])>) -> SystemToolState {
    let Some((ty, raw)) = value else {
        return SystemToolState::NotConfigured;
    };
    if ty != 4 || raw.len() != 4 {
        return SystemToolState::UnexpectedType {
            registry_type: ty,
            raw: raw.to_vec(),
        };
    }
    match u32::from_le_bytes(raw.try_into().expect("length checked")) {
        0 => SystemToolState::Off,
        1 => SystemToolState::DlssIndicatorDebug,
        1024 => SystemToolState::DlssIndicatorProduction,
        other => SystemToolState::CustomDword(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indicator_values_are_typed_without_destroying_custom_state() {
        assert_eq!(indicator_state(None), SystemToolState::NotConfigured);
        assert_eq!(
            indicator_state(Some((4, &0u32.to_le_bytes()))),
            SystemToolState::Off
        );
        assert_eq!(
            indicator_state(Some((4, &1u32.to_le_bytes()))),
            SystemToolState::DlssIndicatorDebug
        );
        assert_eq!(
            indicator_state(Some((4, &1024u32.to_le_bytes()))),
            SystemToolState::DlssIndicatorProduction
        );
        assert_eq!(
            indicator_state(Some((4, &7u32.to_le_bytes()))),
            SystemToolState::CustomDword(7)
        );
        assert!(matches!(
            indicator_state(Some((1, b"text"))),
            SystemToolState::UnexpectedType {
                registry_type: 1,
                ..
            }
        ));
    }
}
