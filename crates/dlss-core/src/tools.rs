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

#[must_use]
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
    let mut bytes = [0_u8; 4];
    bytes.copy_from_slice(raw);
    match u32::from_le_bytes(bytes) {
        0 => SystemToolState::Off,
        1 => SystemToolState::DlssIndicatorDebug,
        1024 => SystemToolState::DlssIndicatorProduction,
        other => SystemToolState::CustomDword(other),
    }
}

/// The `REG_DWORD` registry type.
const REG_DWORD: u32 = 4;

/// Checks a restore point's snapshot before the elevated helper writes it
/// back to `ShowDlssIndicator`.
///
/// The restore point arrives in a plan the helper cannot trust, so the helper
/// writes only what the app itself could set: a `REG_DWORD` of 0, 1, or 1024.
/// A snapshot of a missing value is allowed too, because restoring it removes
/// the value.
///
/// # Errors
/// Returns an error for any other registry type or value.
pub fn validate_indicator_restore(
    snapshot: &RegistryValueSnapshot,
) -> Result<(), crate::CoreError> {
    if !snapshot.existed {
        return Ok(());
    }
    let allowed = snapshot.registry_type == Some(REG_DWORD)
        && matches!(
            indicator_state(Some((REG_DWORD, &snapshot.raw))),
            SystemToolState::Off
                | SystemToolState::DlssIndicatorDebug
                | SystemToolState::DlssIndicatorProduction
        );
    if allowed {
        Ok(())
    } else {
        Err(crate::CoreError::Validation(
            "the saved indicator value is not one DLSS Updater sets (a DWORD of 0, 1, or 1024), \
             so it was not restored"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(existed: bool, registry_type: Option<u32>, raw: &[u8]) -> RegistryValueSnapshot {
        RegistryValueSnapshot {
            existed,
            registry_view: RegistryView::View64,
            registry_type,
            raw: raw.to_vec(),
            captured_unix: 0,
        }
    }

    #[test]
    fn indicator_restore_accepts_only_the_values_the_app_sets() {
        for value in [0_u32, 1, 1024] {
            assert!(
                validate_indicator_restore(&snapshot(true, Some(4), &value.to_le_bytes())).is_ok()
            );
        }
        assert!(validate_indicator_restore(&snapshot(false, None, &[])).is_ok());
        for rejected in [
            snapshot(true, Some(4), &7_u32.to_le_bytes()),
            snapshot(true, Some(4), &[1, 0]),
            snapshot(true, Some(11), &1_u64.to_le_bytes()),
            snapshot(true, Some(1), b"1\0"),
            snapshot(true, None, &1_u32.to_le_bytes()),
        ] {
            assert!(
                validate_indicator_restore(&rejected).is_err(),
                "{rejected:?}"
            );
        }
    }
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
