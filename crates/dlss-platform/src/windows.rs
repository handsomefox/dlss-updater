//! Windows implementations. All registry paths are compile-time constants and
//! elevated plans can select only the allowlisted tool ID.

use dlss_core::*;
use object::Object;
use sha2::{Digest, Sha256};
use std::{
    ffi::c_void,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, TRUST_E_NOSIGNATURE, WAIT_FAILED},
        Security::WinTrust::{
            WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
            WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
            WTD_UI_NONE, WinVerifyTrust,
        },
        Storage::FileSystem::{
            GetFileVersionInfoSizeW, GetFileVersionInfoW, REPLACE_FILE_FLAGS, ReplaceFileW,
            VS_FIXEDFILEINFO, VerQueryValueW,
        },
        System::{
            Com::CoTaskMemFree,
            Threading::{INFINITE, WaitForSingleObject},
        },
        UI::Shell::{
            FOLDERID_LocalAppData, FOLDERID_ProgramData, KNOWN_FOLDER_FLAG,
            SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SHGetKnownFolderPath, ShellExecuteExW,
        },
        UI::WindowsAndMessaging::SW_SHOWNORMAL,
    },
    core::PCWSTR,
};
use windows_registry::{CURRENT_USER, LOCAL_MACHINE, Type};

const NGX_KEY: &str = r"SOFTWARE\NVIDIA Corporation\Global\NGXCore";
const INDICATOR_VALUE: &str = "ShowDlssIndicator";
const KEY_WOW64_64KEY: u32 = 0x0100;

pub fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        game_discovery: true,
        dll_versions: true,
        authenticode: true,
        atomic_replace: true,
        elevation: true,
        system_tools: true,
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn windows_error(error: windows::core::Error) -> CoreError {
    CoreError::Validation(error.to_string())
}

/// Like `windows_error`, but classifies "access denied" structurally so the app
/// can offer elevation instead of matching localized error strings.
fn replace_error(error: windows::core::Error) -> CoreError {
    // HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED) == E_ACCESSDENIED (0x80070005).
    const E_ACCESSDENIED: i32 = 0x8007_0005u32 as i32;
    if error.code().0 == E_ACCESSDENIED {
        CoreError::PermissionDenied
    } else {
        windows_error(error)
    }
}

fn shell_execute_error(error: windows::core::Error) -> CoreError {
    // HRESULT_FROM_WIN32(ERROR_CANCELLED), returned when the user dismisses UAC.
    const HRESULT_CANCELLED: i32 = 0x8007_04c7u32 as i32;
    if error.code().0 == HRESULT_CANCELLED {
        CoreError::Cancelled
    } else {
        windows_error(error)
    }
}

pub struct WindowsDllInspector;

impl DllInspector for WindowsDllInspector {
    fn inspect(&self, path: &Path) -> Result<DllMetadata, CoreError> {
        let mut bytes = Vec::new();
        File::open(path)?.read_to_end(&mut bytes)?;
        let object = object::File::parse(&*bytes)
            .map_err(|error| CoreError::Validation(error.to_string()))?;
        let x86_64 = object.format() == object::BinaryFormat::Pe
            && object.architecture() == object::Architecture::X86_64
            && object.kind() == object::ObjectKind::Dynamic;
        Ok(DllMetadata {
            version: file_version(path)?,
            sha256: Sha256::digest(&bytes).into(),
            signature: WindowsTrustVerifier.verify(path)?,
            x86_64,
        })
    }
}

pub struct WindowsGameLocator;

impl GameLocator for WindowsGameLocator {
    fn discover(&self) -> Result<Vec<GameInstall>, CoreError> {
        let inspector = WindowsDllInspector;
        let mut games = Vec::new();
        games.extend(discover_steam());
        let epic_directory = WindowsKnownDirectories
            .program_data()?
            .join(r"Epic\EpicGamesLauncher\Data\Manifests");
        games.extend(crate::epic_manifests(&epic_directory));
        games.extend(discover_gog());
        for game in &mut games {
            let inspected = crate::scan_game(&game.id, &game.root, &inspector);
            game.inspection_errors = inspected.iter().filter(|result| result.is_err()).count();
            game.dlls = inspected.into_iter().filter_map(Result::ok).collect();
        }
        games.sort_by_key(|game| game.name.to_lowercase());
        games.dedup_by(|right, left| right.id == left.id);
        Ok(games)
    }
}

fn discover_steam() -> Vec<GameInstall> {
    let Ok(key) = CURRENT_USER.open(r"Software\Valve\Steam") else {
        return Vec::new();
    };
    let Ok(steam_path) = key.get_string("SteamPath") else {
        return Vec::new();
    };
    let primary = PathBuf::from(steam_path).join("steamapps");
    let mut steamapps = vec![primary.clone()];
    if let Ok(contents) = fs::read_to_string(primary.join("libraryfolders.vdf")) {
        steamapps.extend(
            crate::steam_library_paths(&contents)
                .into_iter()
                .map(|path| path.join("steamapps")),
        );
    }
    let mut games = Vec::new();
    for library in crate::deduplicate_roots(steamapps) {
        for (app_id, name, root) in crate::steam_manifests(&library) {
            let id = GameId(format!("steam:{app_id}"));
            games.push(GameInstall {
                id,
                name,
                store: StoreKind::Steam,
                root,
                dlls: Vec::new(),
                inspection_errors: 0,
            });
        }
    }
    games
}

fn discover_gog() -> Vec<GameInstall> {
    const GOG_GAMES: &str = r"SOFTWARE\GOG.com\Games";
    const KEY_WOW64_32KEY: u32 = 0x0200;
    let mut games = Vec::new();
    for (view, _kind) in [
        (KEY_WOW64_64KEY, RegistryView::View64),
        (KEY_WOW64_32KEY, RegistryView::View32),
    ] {
        let Ok(parent) = LOCAL_MACHINE.options().read().access(view).open(GOG_GAMES) else {
            continue;
        };
        let Ok(keys) = parent.keys() else {
            continue;
        };
        for key_name in keys {
            let Ok(key) = parent.open(&key_name) else {
                continue;
            };
            let Ok(path) = key.get_string("path") else {
                continue;
            };
            let id = key
                .get_string("gameID")
                .unwrap_or_else(|_| key_name.clone());
            let name = key
                .get_string("gameName")
                .unwrap_or_else(|_| key_name.clone());
            games.push(GameInstall {
                id: GameId(format!("gog:{id}")),
                name,
                store: StoreKind::Gog,
                root: PathBuf::from(path),
                dlls: Vec::new(),
                inspection_errors: 0,
            });
        }
    }
    games
}

fn file_version(path: &Path) -> Result<Option<DllVersion>, CoreError> {
    let path = wide(path);
    // SAFETY: `path` is NUL-terminated and all output buffers live across each call.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None);
        if size == 0 {
            return Ok(None);
        }
        let mut data = vec![0_u8; size as usize];
        GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, data.as_mut_ptr().cast())
            .map_err(windows_error)?;
        let mut info: *mut c_void = std::ptr::null_mut();
        let mut length = 0_u32;
        if !VerQueryValueW(
            data.as_ptr().cast(),
            windows::core::w!("\\"),
            &mut info,
            &mut length,
        )
        .as_bool()
            || info.is_null()
            || length < size_of::<VS_FIXEDFILEINFO>() as u32
        {
            return Ok(None);
        }
        let fixed = &*info.cast::<VS_FIXEDFILEINFO>();
        Ok(Some(DllVersion::new(
            (fixed.dwFileVersionMS >> 16) as u16,
            fixed.dwFileVersionMS as u16,
            (fixed.dwFileVersionLS >> 16) as u16,
            fixed.dwFileVersionLS as u16,
        )))
    }
}

pub struct WindowsTrustVerifier;

impl TrustVerifier for WindowsTrustVerifier {
    fn verify(&self, path: &Path) -> Result<SignatureStatus, CoreError> {
        let path = wide(path);
        let mut file = WINTRUST_FILE_INFO {
            cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: PCWSTR(path.as_ptr()),
            hFile: HANDLE::default(),
            pgKnownSubject: std::ptr::null_mut(),
        };
        let mut data = WINTRUST_DATA {
            cbStruct: size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_NONE,
            dwUnionChoice: WTD_CHOICE_FILE,
            Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
            dwStateAction: WTD_STATEACTION_VERIFY,
            ..Default::default()
        };
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: structures and path storage remain alive for both calls. Provider
        // state is always closed after verification, including failed verification.
        let status = unsafe {
            let status = WinVerifyTrust(
                HWND::default(),
                &mut action,
                (&mut data as *mut WINTRUST_DATA).cast(),
            );
            data.dwStateAction = WTD_STATEACTION_CLOSE;
            let _ = WinVerifyTrust(
                HWND::default(),
                &mut action,
                (&mut data as *mut WINTRUST_DATA).cast(),
            );
            status
        };
        Ok(if status == 0 {
            SignatureStatus::Trusted
        } else if status == TRUST_E_NOSIGNATURE.0 {
            SignatureStatus::Unsigned
        } else {
            SignatureStatus::Untrusted
        })
    }
}

pub struct WindowsKnownDirectories;

impl KnownDirectories for WindowsKnownDirectories {
    fn local_app_data(&self) -> Result<PathBuf, CoreError> {
        known_folder(&FOLDERID_LocalAppData)
    }

    fn program_data(&self) -> Result<PathBuf, CoreError> {
        known_folder(&FOLDERID_ProgramData)
    }
}

fn known_folder(id: &windows::core::GUID) -> Result<PathBuf, CoreError> {
    // SAFETY: the returned COM allocation is converted before being freed once.
    unsafe {
        let value =
            SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG::default(), None).map_err(windows_error)?;
        let path = PathBuf::from(
            value
                .to_string()
                .map_err(|error| CoreError::Validation(error.to_string()))?,
        );
        CoTaskMemFree(Some(value.as_ptr().cast()));
        Ok(path)
    }
}

pub struct WindowsAtomicFileReplacer;

impl AtomicFileReplacer for WindowsAtomicFileReplacer {
    fn replace(
        &self,
        target: &Path,
        source: &Path,
        expected_source_hash: [u8; 32],
    ) -> Result<(), CoreError> {
        if dlss_core::hash_file(source)? != expected_source_hash {
            return Err(CoreError::Validation("cached source hash changed".into()));
        }
        let stage = target.with_extension(format!("dll.dlss-stage-{}", std::process::id()));
        let result = (|| {
            let mut input = File::open(source)?;
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&stage)?;
            std::io::copy(&mut input, &mut output)?;
            output.flush()?;
            output.sync_all()?;
            if dlss_core::hash_file(&stage)? != expected_source_hash {
                return Err(CoreError::Validation("staged source hash mismatch".into()));
            }
            let target_wide = wide(target);
            let stage_wide = wide(&stage);
            // SAFETY: both path buffers are NUL-terminated and remain alive for the call.
            unsafe {
                ReplaceFileW(
                    PCWSTR(target_wide.as_ptr()),
                    PCWSTR(stage_wide.as_ptr()),
                    PCWSTR::null(),
                    REPLACE_FILE_FLAGS::default(),
                    None,
                    None,
                )
                .map_err(replace_error)
            }
        })();
        if stage.exists() {
            let _ = fs::remove_file(stage);
        }
        result
    }
}

pub struct WindowsPrivilegeBroker;

impl PrivilegeBroker for WindowsPrivilegeBroker {
    fn run_elevated(&self, plan: &Path) -> Result<(), CoreError> {
        let executable = std::env::current_exe()?;
        let executable = wide(&executable);
        let parameters = format!("--elevated-helper \"{}\"", plan.display());
        let parameters: Vec<u16> = parameters.encode_utf16().chain(Some(0)).collect();
        let mut execute = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: windows::core::w!("runas"),
            lpFile: PCWSTR(executable.as_ptr()),
            lpParameters: PCWSTR(parameters.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: all strings are NUL-terminated and the returned process handle
        // is retained until completion, then closed exactly once.
        unsafe {
            ShellExecuteExW(&mut execute).map_err(shell_execute_error)?;
            if execute.hProcess.is_invalid() {
                return Err(CoreError::Cancelled);
            }
            let wait = WaitForSingleObject(execute.hProcess, INFINITE);
            let close = CloseHandle(execute.hProcess);
            if wait == WAIT_FAILED {
                return Err(CoreError::Validation("elevated helper wait failed".into()));
            }
            close.map_err(windows_error)?;
        }
        Ok(())
    }
}

pub struct NvidiaSystemTools;

impl NvidiaSystemTools {
    pub fn current_snapshot(&self) -> Result<RegistryValueSnapshot, CoreError> {
        let captured_unix = now_unix();
        let key = match LOCAL_MACHINE
            .options()
            .read()
            .access(KEY_WOW64_64KEY)
            .open(NGX_KEY)
        {
            Ok(key) => key,
            Err(_) => {
                return Ok(RegistryValueSnapshot {
                    existed: false,
                    registry_view: RegistryView::View64,
                    registry_type: None,
                    raw: Vec::new(),
                    captured_unix,
                });
            }
        };
        match key.get_value(INDICATOR_VALUE) {
            Ok(value) => Ok(RegistryValueSnapshot {
                existed: true,
                registry_view: RegistryView::View64,
                registry_type: Some(u32::from(value.ty())),
                raw: value.to_vec(),
                captured_unix,
            }),
            Err(_) => Ok(RegistryValueSnapshot {
                existed: false,
                registry_view: RegistryView::View64,
                registry_type: None,
                raw: Vec::new(),
                captured_unix,
            }),
        }
    }

    fn writable_key(&self) -> Result<windows_registry::Key, CoreError> {
        LOCAL_MACHINE
            .options()
            .read()
            .write()
            .create()
            .access(KEY_WOW64_64KEY)
            .open(NGX_KEY)
            .map_err(|error| CoreError::Validation(error.to_string()))
    }
}

impl SystemToolProvider for NvidiaSystemTools {
    fn capabilities(&self) -> PlatformCapabilities {
        capabilities()
    }

    fn definitions(&self) -> Vec<SystemToolDefinition> {
        vec![SystemToolDefinition {
            id: SystemToolId(DLSS_INDICATOR_TOOL_ID.into()),
            display_name: "DLSS on-screen indicator".into(),
            warning: "Global setting that affects every compatible game".into(),
            scope: SystemToolScope::Global,
            requires_elevation: true,
        }]
    }

    fn read(&self, id: &SystemToolId) -> Result<SystemToolState, CoreError> {
        ensure_indicator_id(id)?;
        let snapshot = self.current_snapshot()?;
        Ok(snapshot_state(&snapshot))
    }

    fn apply(&self, plan: &ToolChangePlan) -> Result<ToolChangeResult, CoreError> {
        ensure_indicator_id(&plan.tool_id)?;
        if plan.restore_point.is_some() {
            return Err(CoreError::Validation(
                "apply plan unexpectedly contains a restore point".into(),
            ));
        }
        let before = self.current_snapshot()?;
        if snapshot_hash(&before) != plan.expected_current_hash {
            return Err(CoreError::StalePlan);
        }
        let value = match plan.desired {
            SystemToolState::Off => 0,
            SystemToolState::DlssIndicatorDebug => 1,
            SystemToolState::DlssIndicatorProduction => 1024,
            _ => return Err(CoreError::Validation("unsupported indicator state".into())),
        };
        self.writable_key()?
            .set_u32(INDICATOR_VALUE, value)
            .map_err(|error| CoreError::Validation(error.to_string()))?;
        let after = self.current_snapshot()?;
        let state = snapshot_state(&after);
        Ok(ToolChangeResult {
            tool_id: plan.tool_id.clone(),
            state,
            restore_point: Some(ToolRestorePoint {
                tool_id: plan.tool_id.clone(),
                snapshot: before,
                state_after_hash: snapshot_hash(&after),
                app_version: env!("CARGO_PKG_VERSION").into(),
            }),
        })
    }

    fn restore(
        &self,
        point: &ToolRestorePoint,
        expected_current_hash: [u8; 32],
        allow_stale: bool,
    ) -> Result<ToolChangeResult, CoreError> {
        ensure_indicator_id(&point.tool_id)?;
        let current = self.current_snapshot()?;
        if snapshot_hash(&current) != expected_current_hash
            || (!allow_stale && expected_current_hash != point.state_after_hash)
        {
            return Err(CoreError::StalePlan);
        }
        let key = self.writable_key()?;
        if point.snapshot.existed {
            let registry_type = point
                .snapshot
                .registry_type
                .ok_or_else(|| CoreError::Validation("restore type is missing".into()))?;
            key.set_bytes(
                INDICATOR_VALUE,
                Type::from(registry_type),
                &point.snapshot.raw,
            )
            .map_err(|error| CoreError::Validation(error.to_string()))?;
        } else if key.get_value(INDICATOR_VALUE).is_ok() {
            key.remove_value(INDICATOR_VALUE)
                .map_err(|error| CoreError::Validation(error.to_string()))?;
        }
        let state = snapshot_state(&self.current_snapshot()?);
        Ok(ToolChangeResult {
            tool_id: point.tool_id.clone(),
            state,
            restore_point: None,
        })
    }
}

fn ensure_indicator_id(id: &SystemToolId) -> Result<(), CoreError> {
    if id.0 == DLSS_INDICATOR_TOOL_ID {
        Ok(())
    } else {
        Err(CoreError::Validation("unknown system tool ID".into()))
    }
}

fn snapshot_state(snapshot: &RegistryValueSnapshot) -> SystemToolState {
    if !snapshot.existed {
        return SystemToolState::NotConfigured;
    }
    indicator_state(Some((
        snapshot.registry_type.unwrap_or_default(),
        &snapshot.raw,
    )))
}

pub fn snapshot_hash(snapshot: &RegistryValueSnapshot) -> [u8; 32] {
    let mut stable = snapshot.clone();
    stable.captured_unix = 0;
    let encoded = serde_json::to_vec(&stable).expect("registry snapshot is serializable");
    Sha256::digest(encoded).into()
}

/// Elevated entry point. The caller supplies only a plan file; registry paths
/// are resolved above from the allowlisted tool ID.
pub fn run_elevated_helper(plan_path: &Path) -> Result<(), CoreError> {
    tracing::info!(plan = %plan_path.display(), "elevated helper started");
    let local = WindowsKnownDirectories.local_app_data()?;
    let base = local.join("DLSS Updater");
    let plan_directory = base.join("helper-plans");
    let result_directory = base.join("helper-results");
    fs::create_dir_all(&plan_directory)?;
    fs::create_dir_all(&result_directory)?;
    let canonical_plan = plan_path.canonicalize()?;
    let canonical_plan_directory = plan_directory.canonicalize()?;
    if canonical_plan.parent() != Some(canonical_plan_directory.as_path()) {
        return Err(CoreError::Validation(
            "helper plan is outside the private plan directory".into(),
        ));
    }
    let plan: ElevatedHelperPlan = dlss_core::read_versioned_json(&canonical_plan, 1)?;
    let result = match plan {
        ElevatedHelperPlan::SystemTool(plan) => {
            // Validate the result path before doing anything else so that any
            // subsequent failure can be reported back to the caller through the
            // result file rather than vanishing into a headless process.
            validate_helper_paths(&plan.nonce, &plan.result_path, &result_directory)?;
            let outcome: Result<ToolChangeResult, String> = (|| {
                ensure_indicator_id(&plan.tool_id).map_err(|error| error.to_string())?;
                if let Some(point) = &plan.restore_point {
                    if point.tool_id != plan.tool_id {
                        return Err("restore tool ID mismatch".into());
                    }
                    NvidiaSystemTools
                        .restore(point, plan.expected_current_hash, plan.allow_stale_restore)
                        .map_err(|error| error.to_string())
                } else {
                    NvidiaSystemTools
                        .apply(&plan)
                        .map_err(|error| error.to_string())
                }
            })();
            dlss_core::write_versioned_json(&plan.result_path, 1, &outcome)
        }
        ElevatedHelperPlan::FileSwap(plan) => {
            validate_helper_paths(&plan.nonce, &plan.result_path, &result_directory)?;
            let outcome: Result<dlss_core::BatchResult, String> = (|| {
                if plan.operation.nonce != plan.nonce {
                    return Err("file plan nonce mismatch".into());
                }
                validate_file_plan(&plan, &base).map_err(|error| error.to_string())?;
                Ok(dlss_core::execute_plan(
                    &plan.operation,
                    &WindowsDllInspector,
                    &WindowsAtomicFileReplacer,
                    &dlss_core::BackupStore::new(base.join("backups")),
                    now_unix(),
                ))
            })();
            dlss_core::write_versioned_json(&plan.result_path, 1, &outcome)
        }
    };
    match &result {
        Ok(()) => tracing::info!("elevated helper completed"),
        Err(error) => tracing::error!(%error, "elevated helper failed"),
    }
    result
}

fn validate_helper_paths(
    nonce: &str,
    result_path: &Path,
    result_directory: &Path,
) -> Result<(), CoreError> {
    if nonce.len() < 16
        || nonce.len() > 128
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(CoreError::Validation("invalid helper nonce".into()));
    }
    if result_path != result_directory.join(format!("{nonce}.json")) {
        return Err(CoreError::Validation("invalid helper result path".into()));
    }
    Ok(())
}

fn validate_file_plan(plan: &ElevatedFilePlan, base: &Path) -> Result<(), CoreError> {
    let game_root = plan.game_root.canonicalize()?;
    let known_game = WindowsGameLocator.discover()?.into_iter().any(|game| {
        game.id == plan.game_id && game.root.canonicalize().ok() == Some(game_root.clone())
    });
    let known_manual = plan.game_id.0.starts_with("manual:")
        && crate::manual_install(&game_root).is_ok_and(|game| game.id == plan.game_id);
    if !known_game && !known_manual {
        return Err(CoreError::Validation(
            "file plan does not belong to the declared game".into(),
        ));
    }
    let releases = base.join("cache/releases").canonicalize()?;
    let backups = base.join("backups/objects");
    let backups = backups.canonicalize().ok();
    for swap in &plan.operation.swaps {
        let target = swap.target_path.canonicalize()?;
        if !target.starts_with(&game_root) {
            return Err(CoreError::Validation(
                "target is outside the game root".into(),
            ));
        }
        let source = swap.source_path.canonicalize()?;
        let trusted_source = source.starts_with(&releases)
            || backups
                .as_ref()
                .is_some_and(|root| source.starts_with(root));
        if !trusted_source || dlss_core::hash_file(&source)? != swap.source_sha256 {
            return Err(CoreError::Validation(
                "source is outside the validated cache".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevated_helper_rejects_bad_nonces_and_result_paths() {
        let results = PathBuf::from(r"C:\private\helper-results");
        assert!(validate_helper_paths("short", Path::new("x"), &results).is_err());
        assert!(
            validate_helper_paths(
                "0123456789abcdef",
                Path::new(r"C:\elsewhere\0123456789abcdef.json"),
                &results,
            )
            .is_err()
        );
        assert!(
            validate_helper_paths(
                "0123456789abcdef",
                &results.join("0123456789abcdef.json"),
                &results,
            )
            .is_ok()
        );
    }
}
