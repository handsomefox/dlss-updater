//! Windows implementations. All registry paths are compile-time constants and
//! elevated plans can select only the allowlisted tool ID.

use dlss_core::{
    AtomicFileReplacer, BackupStore, BatchResult, CoreError, DLSS_INDICATOR_TOOL_ID,
    DiscoveryOutcome, DiscoveryStatus, DllInspector, DllMetadata, DllVersion, ElevatedFilePlan,
    ElevatedHelperPlan, GameId, GameInstall, GameLocator, KnownDirectories, NativeTrustFailure,
    OperationPlan, PlannedSwap, PlatformCapabilities, PrivilegeBroker, RegistryValueSnapshot,
    RegistryView, RevocationStatus, SignatureStatus, StoreDiscoveryReport, StoreKind,
    SystemToolDefinition, SystemToolId, SystemToolProvider, SystemToolScope, SystemToolState,
    ToolChangePlan, ToolChangeResult, ToolRestorePoint, TrustPolicy, TrustReport, TrustVerifier,
    execute_plan, hash_file, indicator_state, now_unix, read_versioned_json_bytes,
    write_versioned_json,
};
use object::Object;
use sha2::{Digest, Sha256};
use std::{
    ffi::c_void,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{
            CERT_E_REVOCATION_FAILURE, CRYPT_E_REVOCATION_OFFLINE, CloseHandle, HANDLE, HWND,
            TRUST_E_NOSIGNATURE, WAIT_FAILED,
        },
        Security::Cryptography::{CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW},
        Security::WinTrust::{
            WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
            WINTRUST_DATA_PROVIDER_FLAGS, WINTRUST_FILE_INFO, WTD_CHOICE_FILE,
            WTD_REVOCATION_CHECK_CHAIN, WTD_REVOKE_NONE, WTD_REVOKE_WHOLECHAIN,
            WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
            WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain,
            WTHelperProvDataFromStateData, WinVerifyTrust,
        },
        Storage::FileSystem::{
            GetFileVersionInfoSizeW, GetFileVersionInfoW, MOVE_FILE_FLAGS,
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, VS_FIXEDFILEINFO,
            VerQueryValueW,
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
    core::{HRESULT, PCWSTR},
};
use windows_registry::{CURRENT_USER, LOCAL_MACHINE, Type};

const NGX_KEY: &str = r"SOFTWARE\NVIDIA Corporation\Global\NGXCore";
const INDICATOR_VALUE: &str = "ShowDlssIndicator";
const KEY_WOW64_64KEY: u32 = 0x0100;
const KEY_WOW64_32KEY: u32 = 0x0200;

#[must_use]
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

fn hex_hash(hash: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(64);
    for byte in hash {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn windows_error(error: &windows::core::Error) -> CoreError {
    CoreError::Validation(error.to_string())
}

/// Like `windows_error`, but classifies "access denied" structurally so the app
/// can offer elevation instead of matching localized error strings.
fn replace_error(error: &windows::core::Error) -> CoreError {
    // HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED) == E_ACCESSDENIED (0x80070005).
    const E_ACCESSDENIED: i32 = 0x8007_0005u32.cast_signed();
    if error.code().0 == E_ACCESSDENIED {
        CoreError::PermissionDenied
    } else {
        windows_error(error)
    }
}

fn shell_execute_error(error: &windows::core::Error) -> CoreError {
    // HRESULT_FROM_WIN32(ERROR_CANCELLED), returned when the user dismisses UAC.
    const HRESULT_CANCELLED: i32 = 0x8007_04c7u32.cast_signed();
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
            signature: WindowsTrustVerifier
                .verify(path, TrustPolicy::Strict)?
                .signature,
            x86_64,
        })
    }
}

pub struct WindowsGameLocator;

struct StoreScan {
    games: Vec<GameInstall>,
    empty_status: DiscoveryStatus,
    detail: Option<String>,
}

impl StoreScan {
    fn report(&self, store: &str) -> StoreDiscoveryReport {
        StoreDiscoveryReport {
            store: store.into(),
            status: if self.games.is_empty() {
                self.empty_status
            } else {
                DiscoveryStatus::Found
            },
            games_found: self.games.len(),
            detail: self.detail.clone(),
        }
    }
}

impl GameLocator for WindowsGameLocator {
    fn discover(&self) -> Result<DiscoveryOutcome, CoreError> {
        let inspector = WindowsDllInspector;
        let steam = discover_steam();
        let epic = discover_epic();
        let gog = discover_gog();
        let reports = vec![
            steam.report("Steam"),
            epic.report("Epic"),
            gog.report("GOG"),
        ];
        let mut games = Vec::new();
        games.extend(steam.games);
        games.extend(epic.games);
        games.extend(gog.games);
        for game in &mut games {
            let inspected = crate::scan_game(&game.id, &game.root, &inspector);
            game.inspection_errors = inspected.iter().filter(|result| result.is_err()).count();
            game.dlls = inspected.into_iter().filter_map(Result::ok).collect();
        }
        games.sort_by_key(|game| game.name.to_lowercase());
        games.dedup_by(|right, left| right.id == left.id);
        for report in &reports {
            tracing::info!(store = %report.store, status = ?report.status, games = report.games_found, detail = ?report.detail, "store discovery completed");
        }
        Ok(DiscoveryOutcome { games, reports })
    }
}

fn discover_steam() -> StoreScan {
    let mut registry_errors = Vec::new();
    let steam_path = match CURRENT_USER
        .open(r"Software\Valve\Steam")
        .and_then(|key| key.get_string("SteamPath"))
    {
        Ok(path) => {
            tracing::info!(hive = "HKCU", "Steam registry path found");
            Ok(path)
        }
        Err(error) => {
            tracing::info!(hive = "HKCU", %error, "Steam registry path not found");
            if !is_missing_registry_error(error.code().0) {
                registry_errors.push(format!("HKCU: {error}"));
            }
            let fallback = LOCAL_MACHINE
                .options()
                .read()
                .access(KEY_WOW64_32KEY)
                .open(r"SOFTWARE\Valve\Steam")
                .and_then(|key| key.get_string("InstallPath"))
                .inspect(|_| tracing::info!(hive = "HKLM", "Steam registry path found"))
                .inspect_err(|error| {
                    tracing::info!(hive = "HKLM", %error, "Steam registry path not found");
                });
            if let Err(error) = &fallback
                && !is_missing_registry_error(error.code().0)
            {
                registry_errors.push(format!("HKLM: {error}"));
            }
            fallback
        }
    };
    let Ok(steam_path) = steam_path else {
        tracing::warn!("Steam registry key missing in HKCU and HKLM");
        let had_errors = !registry_errors.is_empty();
        return StoreScan {
            games: Vec::new(),
            empty_status: if had_errors {
                DiscoveryStatus::Error
            } else {
                DiscoveryStatus::NotDetected
            },
            detail: Some(if had_errors {
                registry_errors.join("; ")
            } else {
                "registry key missing (HKCU and HKLM)".into()
            }),
        };
    };
    let steam_root = PathBuf::from(steam_path);
    tracing::info!(path = %steam_root.display(), "Steam root resolved");
    let (steamapps, detail) = crate::steam_steamapps_dirs(&steam_root);
    let mut games = Vec::new();
    let mut errors = Vec::new();
    for library in steamapps {
        let manifests = crate::discovery::steam_manifests_with_errors(&library);
        tracing::info!(path = %library.display(), manifests = manifests.items.len(), errors = manifests.errors.len(), "Steam library scanned");
        for error in &manifests.errors {
            tracing::warn!(%error, "Steam manifest could not be read");
        }
        errors.extend(manifests.errors);
        for (app_id, name, root) in manifests.items {
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
    if let Some(detail) = detail {
        errors.push(detail);
    }
    let detail = (!errors.is_empty()).then(|| errors.join("; "));
    StoreScan {
        games,
        empty_status: if detail.is_some() {
            DiscoveryStatus::Error
        } else {
            DiscoveryStatus::NotDetected
        },
        detail,
    }
}

fn discover_epic() -> StoreScan {
    let directory = match WindowsKnownDirectories.program_data() {
        Ok(root) => root.join(r"Epic\EpicGamesLauncher\Data\Manifests"),
        Err(error) => {
            tracing::warn!(%error, "Epic ProgramData directory could not be resolved");
            return StoreScan {
                games: Vec::new(),
                empty_status: DiscoveryStatus::Error,
                detail: Some(error.to_string()),
            };
        }
    };
    if !directory.exists() {
        tracing::info!(path = %directory.display(), "Epic manifest directory not found");
        return StoreScan {
            games: Vec::new(),
            empty_status: DiscoveryStatus::NotDetected,
            detail: Some(format!(
                "manifest directory not found: {}",
                directory.display()
            )),
        };
    }
    let scan = crate::discovery::epic_manifests_with_errors(&directory);
    for error in &scan.errors {
        tracing::warn!(%error, "Epic manifest could not be read");
    }
    StoreScan {
        games: scan.items,
        empty_status: if scan.errors.is_empty() {
            DiscoveryStatus::NotDetected
        } else {
            DiscoveryStatus::Error
        },
        detail: (!scan.errors.is_empty()).then(|| scan.errors.join("; ")),
    }
}

fn discover_gog() -> StoreScan {
    const GOG_GAMES: &str = r"SOFTWARE\GOG.com\Games";
    let mut games = Vec::new();
    let mut errors = Vec::new();
    let mut found_registry = false;
    for (view, kind) in [
        (KEY_WOW64_64KEY, RegistryView::View64),
        (KEY_WOW64_32KEY, RegistryView::View32),
    ] {
        let parent = match LOCAL_MACHINE.options().read().access(view).open(GOG_GAMES) {
            Ok(parent) => {
                found_registry = true;
                tracing::info!(?kind, "GOG registry view found");
                parent
            }
            Err(error) => {
                tracing::info!(?kind, %error, "GOG registry view not found");
                if !is_missing_registry_error(error.code().0) {
                    errors.push(format!("{kind:?}: {error}"));
                }
                continue;
            }
        };
        let keys = match parent.keys() {
            Ok(keys) => keys,
            Err(error) => {
                tracing::warn!(?kind, %error, "GOG registry games could not be enumerated");
                errors.push(format!("{kind:?}: {error}"));
                continue;
            }
        };
        for key_name in keys {
            let key = match parent.open(&key_name) {
                Ok(key) => key,
                Err(error) => {
                    errors.push(format!("{kind:?}/{key_name}: {error}"));
                    continue;
                }
            };
            let path = match key.get_string("path") {
                Ok(path) => path,
                Err(error) => {
                    errors.push(format!("{kind:?}/{key_name}/path: {error}"));
                    continue;
                }
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
    let detail = if !errors.is_empty() {
        Some(errors.join("; "))
    } else if !found_registry {
        Some("registry key missing in 32-bit and 64-bit views".into())
    } else {
        None
    };
    StoreScan {
        games,
        empty_status: if errors.is_empty() {
            DiscoveryStatus::NotDetected
        } else {
            DiscoveryStatus::Error
        },
        detail,
    }
}

fn is_missing_registry_error(code: i32) -> bool {
    const FILE_NOT_FOUND: i32 = 0x8007_0002u32.cast_signed();
    const PATH_NOT_FOUND: i32 = 0x8007_0003u32.cast_signed();
    matches!(code, FILE_NOT_FOUND | PATH_NOT_FOUND)
}

fn file_version(path: &Path) -> Result<Option<DllVersion>, CoreError> {
    let path = wide(path);
    // SAFETY: `path` is NUL-terminated and all output buffers live across each call.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None);
        if size == 0 {
            return Ok(None);
        }
        let data_len = usize::try_from(size)
            .map_err(|_| CoreError::Validation("version resource is too large".into()))?;
        let mut data = vec![0_u8; data_len];
        GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, data.as_mut_ptr().cast())
            .map_err(|error| windows_error(&error))?;
        let mut info: *mut c_void = std::ptr::null_mut();
        let mut length = 0_u32;
        if !VerQueryValueW(
            data.as_ptr().cast(),
            windows::core::w!("\\"),
            &raw mut info,
            &raw mut length,
        )
        .as_bool()
            || info.is_null()
            || length
                < u32::try_from(size_of::<VS_FIXEDFILEINFO>())
                    .map_err(|_| CoreError::Validation("version structure is too large".into()))?
        {
            return Ok(None);
        }
        let fixed = &*info.cast::<VS_FIXEDFILEINFO>();
        Ok(Some(DllVersion::new(
            u16::try_from(fixed.dwFileVersionMS >> 16).unwrap_or_default(),
            u16::try_from(fixed.dwFileVersionMS & 0xffff).unwrap_or_default(),
            u16::try_from(fixed.dwFileVersionLS >> 16).unwrap_or_default(),
            u16::try_from(fixed.dwFileVersionLS & 0xffff).unwrap_or_default(),
        )))
    }
}

pub struct WindowsTrustVerifier;

impl TrustVerifier for WindowsTrustVerifier {
    fn verify(&self, path: &Path, policy: TrustPolicy) -> Result<TrustReport, CoreError> {
        let strict = Self::verify_once(path, true)?;
        let fallback = if policy == TrustPolicy::OfficialNvidiaCatalog
            && revocation_unavailable(strict.status)
        {
            Some(Self::verify_once(path, false)?)
        } else {
            None
        };
        Ok(resolve_trust(policy, strict, fallback))
    }
}

struct NativeTrustResult {
    status: i32,
    signer: Option<String>,
}

impl NativeTrustResult {
    fn into_report(
        self,
        revocation: RevocationStatus,
        native_failure: Option<NativeTrustFailure>,
    ) -> TrustReport {
        TrustReport {
            signature: signature_status(self.status),
            signer: self.signer,
            revocation,
            native_failure,
        }
    }
}

impl WindowsTrustVerifier {
    fn verify_once(path: &Path, check_revocation: bool) -> Result<NativeTrustResult, CoreError> {
        let path = wide(path);
        let file_size = u32::try_from(size_of::<WINTRUST_FILE_INFO>())
            .map_err(|_| CoreError::Validation("trust file structure is too large".into()))?;
        let data_size = u32::try_from(size_of::<WINTRUST_DATA>())
            .map_err(|_| CoreError::Validation("trust data structure is too large".into()))?;
        let mut file = WINTRUST_FILE_INFO {
            cbStruct: file_size,
            pcwszFilePath: PCWSTR(path.as_ptr()),
            hFile: HANDLE::default(),
            pgKnownSubject: std::ptr::null_mut(),
        };
        let mut data = WINTRUST_DATA {
            cbStruct: data_size,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: if check_revocation {
                WTD_REVOKE_WHOLECHAIN
            } else {
                WTD_REVOKE_NONE
            },
            dwUnionChoice: WTD_CHOICE_FILE,
            Anonymous: WINTRUST_DATA_0 {
                pFile: &raw mut file,
            },
            dwStateAction: WTD_STATEACTION_VERIFY,
            dwProvFlags: if check_revocation {
                WTD_REVOCATION_CHECK_CHAIN
            } else {
                WINTRUST_DATA_PROVIDER_FLAGS::default()
            },
            ..Default::default()
        };
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: structures and path storage remain alive for both calls. Provider
        // state is always closed after verification, including failed verification.
        let (status, signer) = unsafe {
            let status = WinVerifyTrust(HWND::default(), &raw mut action, (&raw mut data).cast());
            let signer = signer_subject_from_state(data.hWVTStateData);
            data.dwStateAction = WTD_STATEACTION_CLOSE;
            let _ = WinVerifyTrust(HWND::default(), &raw mut action, (&raw mut data).cast());
            (status, signer)
        };
        Ok(NativeTrustResult { status, signer })
    }
}

fn signature_status(status: i32) -> SignatureStatus {
    if status == 0 {
        SignatureStatus::Trusted
    } else if status == TRUST_E_NOSIGNATURE.0 {
        SignatureStatus::Unsigned
    } else {
        SignatureStatus::Untrusted
    }
}

fn revocation_unavailable(status: i32) -> bool {
    status == CRYPT_E_REVOCATION_OFFLINE.0 || status == CERT_E_REVOCATION_FAILURE.0
}

fn native_failure(status: i32) -> NativeTrustFailure {
    NativeTrustFailure {
        status,
        reason: windows::core::Error::from_hresult(HRESULT(status)).to_string(),
    }
}

fn resolve_trust(
    policy: TrustPolicy,
    strict: NativeTrustResult,
    fallback: Option<NativeTrustResult>,
) -> TrustReport {
    if strict.status == 0 {
        return strict.into_report(RevocationStatus::Verified, None);
    }
    let strict_status = strict.status;
    if policy == TrustPolicy::OfficialNvidiaCatalog
        && revocation_unavailable(strict_status)
        && let Some(fallback) = fallback
    {
        if fallback.status == 0
            && fallback
                .signer
                .as_deref()
                .is_some_and(dlss_core::is_nvidia_signer)
        {
            return fallback.into_report(
                RevocationStatus::UnavailableFallback,
                Some(native_failure(strict_status)),
            );
        }
        return fallback.into_report(
            RevocationStatus::NotVerified,
            Some(native_failure(strict_status)),
        );
    }
    strict.into_report(
        RevocationStatus::NotVerified,
        Some(native_failure(strict_status)),
    )
}

unsafe fn signer_subject_from_state(state: HANDLE) -> Option<String> {
    // SAFETY: the successful WinVerifyTrust provider state remains open until
    // this function returns.
    let provider = unsafe { WTHelperProvDataFromStateData(state) };
    if provider.is_null() {
        return None;
    }
    let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, false, 0) };
    if signer.is_null() {
        return None;
    }
    let certificate = unsafe { WTHelperGetProvCertFromChain(signer, 0) };
    if certificate.is_null() {
        return None;
    }
    let context = unsafe { (*certificate).pCert };
    if context.is_null() {
        return None;
    }
    let length =
        unsafe { CertGetNameStringW(context, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, None) };
    if length <= 1 {
        return None;
    }
    let mut buffer = vec![0_u16; usize::try_from(length).ok()?];
    let written = unsafe {
        CertGetNameStringW(
            context,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            None,
            Some(&mut buffer),
        )
    };
    if written <= 1 {
        return None;
    }
    buffer.truncate(usize::try_from(written - 1).ok()?);
    String::from_utf16(&buffer).ok()
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
        let value = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG::default(), None)
            .map_err(|error| windows_error(&error))?;
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
        if hash_file(source)? != expected_source_hash {
            return Err(CoreError::Validation("cached source hash changed".into()));
        }
        let stage = target.with_extension(format!(
            "dll.dlss-stage-{}-{:032x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        let result = (|| {
            let mut input = File::open(source)?;
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&stage)?;
            io::copy(&mut input, &mut output)?;
            output.flush()?;
            output.sync_all()?;
            if hash_file(&stage)? != expected_source_hash {
                return Err(CoreError::Validation("staged source hash mismatch".into()));
            }
            let target_wide = wide(target);
            let stage_wide = wide(&stage);
            // SAFETY: both path buffers are NUL-terminated and remain alive for the call.
            unsafe {
                MoveFileExW(
                    PCWSTR(stage_wide.as_ptr()),
                    PCWSTR(target_wide.as_ptr()),
                    MOVE_FILE_FLAGS(MOVEFILE_REPLACE_EXISTING.0 | MOVEFILE_WRITE_THROUGH.0),
                )
                .map_err(|error| replace_error(&error))
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
        let plan_hash = hex_hash(hash_file(plan)?);
        let parameters = format!("--elevated-helper \"{}\" {plan_hash}", plan.display());
        let parameters: Vec<u16> = parameters.encode_utf16().chain(Some(0)).collect();
        let execute_size = u32::try_from(size_of::<SHELLEXECUTEINFOW>())
            .map_err(|_| CoreError::Validation("shell structure is too large".into()))?;
        let mut execute = SHELLEXECUTEINFOW {
            cbSize: execute_size,
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
            ShellExecuteExW(&raw mut execute).map_err(|error| shell_execute_error(&error))?;
            if execute.hProcess.is_invalid() {
                return Err(CoreError::Cancelled);
            }
            let wait = WaitForSingleObject(execute.hProcess, INFINITE);
            let close = CloseHandle(execute.hProcess);
            if wait == WAIT_FAILED {
                return Err(CoreError::Validation("elevated helper wait failed".into()));
            }
            close.map_err(|error| windows_error(&error))?;
        }
        Ok(())
    }
}

pub struct NvidiaSystemTools;

impl NvidiaSystemTools {
    /// Captures the current registry value without interpreting its type.
    ///
    /// # Errors
    /// Returns an error when an existing value cannot be decoded.
    pub fn current_snapshot(&self) -> Result<RegistryValueSnapshot, CoreError> {
        let captured_unix = now_unix();
        let Ok(key) = LOCAL_MACHINE
            .options()
            .read()
            .access(KEY_WOW64_64KEY)
            .open(NGX_KEY)
        else {
            return Ok(RegistryValueSnapshot {
                existed: false,
                registry_view: RegistryView::View64,
                registry_type: None,
                raw: Vec::new(),
                captured_unix,
            });
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

    fn writable_key() -> Result<windows_registry::Key, CoreError> {
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
        Self::writable_key()?
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
        let key = Self::writable_key()?;
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

#[must_use]
pub fn snapshot_hash(snapshot: &RegistryValueSnapshot) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([u8::from(snapshot.existed)]);
    hasher.update([match snapshot.registry_view {
        RegistryView::View32 => 32,
        RegistryView::View64 => 64,
    }]);
    match snapshot.registry_type {
        Some(registry_type) => {
            hasher.update([1]);
            hasher.update(registry_type.to_le_bytes());
        }
        None => hasher.update([0]),
    }
    hasher.update(
        u64::try_from(snapshot.raw.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(&snapshot.raw);
    hasher.finalize().into()
}

/// Elevated entry point. The caller supplies only a plan file; registry paths
/// are resolved above from the allowlisted tool ID.
///
/// # Errors
/// Returns an error when helper paths, the serialized plan, independent plan
/// validation, execution, or result persistence fails.
pub fn run_elevated_helper(
    plan_path: &Path,
    expected_plan_hash: [u8; 32],
) -> Result<(), CoreError> {
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
    let plan_bytes = fs::read(&canonical_plan)?;
    if <[u8; 32]>::from(Sha256::digest(&plan_bytes)) != expected_plan_hash {
        return Err(CoreError::Validation(
            "elevated helper plan digest mismatch".into(),
        ));
    }
    let plan: ElevatedHelperPlan = read_versioned_json_bytes(&plan_bytes, 1)?;
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
            write_versioned_json(&plan.result_path, 1, &outcome)
        }
        ElevatedHelperPlan::FileSwap(plan) => {
            validate_helper_paths(&plan.nonce, &plan.result_path, &result_directory)?;
            let outcome: Result<BatchResult, String> = (|| {
                if plan.operation.nonce != plan.nonce {
                    return Err("file plan nonce mismatch".into());
                }
                let staged = validate_file_plan(&plan, &base).map_err(|error| error.to_string())?;
                Ok(execute_plan(
                    &staged.operation,
                    &WindowsDllInspector,
                    &WindowsAtomicFileReplacer,
                    &BackupStore::new(base.join("backups")),
                    now_unix(),
                ))
            })();
            write_versioned_json(&plan.result_path, 1, &outcome)
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

struct StagedFilePlan {
    operation: OperationPlan,
    sources: Vec<PathBuf>,
}

impl Drop for StagedFilePlan {
    fn drop(&mut self) {
        for source in &self.sources {
            if let Err(error) = fs::remove_file(source)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(path = %source.display(), %error, "could not remove elevated source staging file");
            }
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the elevated boundary keeps all plan validation visible in one audit path"
)]
fn validate_file_plan(plan: &ElevatedFilePlan, base: &Path) -> Result<StagedFilePlan, CoreError> {
    let game_root = plan.game_root.canonicalize()?;
    let discovered_game = WindowsGameLocator
        .discover()?
        .games
        .into_iter()
        .find(|game| {
            game.id == plan.game_id && game.root.canonicalize().ok() == Some(game_root.clone())
        });
    let manual_game = (plan.game_id.0.starts_with("manual:"))
        .then(|| crate::manual_install(&game_root))
        .transpose()?
        .filter(|game| game.id == plan.game_id);
    if discovered_game.is_none() && manual_game.is_none() {
        return Err(CoreError::Validation(
            "file plan does not belong to the declared game".into(),
        ));
    }
    let installations = crate::scan_game(&plan.game_id, &game_root, &WindowsDllInspector)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    let releases = base.join("cache/releases").canonicalize()?;
    let backups = base.join("backups/objects");
    let backups = backups.canonicalize().ok();
    let imports = base.join("imports/objects");
    let imports = imports.canonicalize().ok();
    let mut staged_sources = Vec::with_capacity(plan.operation.swaps.len());
    let mut swaps = Vec::with_capacity(plan.operation.swaps.len());
    for (index, swap) in plan.operation.swaps.iter().enumerate() {
        if swap.game != plan.game_id {
            return Err(CoreError::Validation("file plan game ID mismatch".into()));
        }
        let target = swap.target_path.canonicalize()?;
        if !target.starts_with(&game_root) {
            return Err(CoreError::Validation(
                "target is outside the game root".into(),
            ));
        }
        if target
            .file_name()
            .and_then(dlss_core::DllKind::classify)
            .is_none()
        {
            return Err(CoreError::Validation(
                "target is not a managed NVIDIA DLL".into(),
            ));
        }
        let installation = installations
            .iter()
            .find(|installation| installation.id == swap.installation)
            .ok_or_else(|| {
                CoreError::Validation("target is not a discovered DLL installation".into())
            })?;
        if installation.game_id != plan.game_id
            || installation.path.canonicalize()? != target
            || installation.metadata.sha256 != swap.expected_sha256
        {
            return Err(CoreError::Validation(
                "target does not match the discovered DLL installation".into(),
            ));
        }
        let source = swap.source_path.canonicalize()?;
        let imported_source = imports
            .as_ref()
            .is_some_and(|root| source.starts_with(root));
        let backup_source = backups
            .as_ref()
            .is_some_and(|root| source.starts_with(root));
        let release_source = source.starts_with(&releases);
        if (!release_source && !backup_source && !imported_source)
            || hash_file(&source)? != swap.source_sha256
        {
            return Err(CoreError::Validation(
                "source is outside the validated cache".into(),
            ));
        }
        let policy = if release_source {
            TrustPolicy::OfficialNvidiaCatalog
        } else {
            TrustPolicy::Strict
        };
        validate_nvidia_dll(&source, policy)?;
        let staged = stage_elevated_source(
            &source,
            &target,
            swap.source_sha256,
            &plan.nonce,
            index,
            policy,
        )?;
        staged_sources.push(staged.clone());
        let mut staged_swap: PlannedSwap = swap.clone();
        staged_swap.target_path = target;
        staged_swap.source_path = staged;
        swaps.push(staged_swap);
    }
    Ok(StagedFilePlan {
        operation: OperationPlan {
            nonce: plan.operation.nonce.clone(),
            swaps,
        },
        sources: staged_sources,
    })
}

fn validate_nvidia_dll(path: &Path, policy: TrustPolicy) -> Result<(), CoreError> {
    let metadata = WindowsDllInspector.inspect(path)?;
    let trust = WindowsTrustVerifier.verify(path, policy)?;
    if !metadata.x86_64
        || metadata.version.is_none()
        || trust.signature != SignatureStatus::Trusted
        || !trust
            .signer
            .as_deref()
            .is_some_and(dlss_core::is_nvidia_signer)
    {
        return Err(CoreError::Validation(
            "source failed elevated NVIDIA trust validation".into(),
        ));
    }
    Ok(())
}

fn stage_elevated_source(
    source: &Path,
    target: &Path,
    expected_hash: [u8; 32],
    nonce: &str,
    index: usize,
    policy: TrustPolicy,
) -> Result<PathBuf, CoreError> {
    let parent = target
        .parent()
        .ok_or_else(|| CoreError::Validation("target has no parent directory".into()))?;
    let staged = parent.join(format!(".dlss-source-{nonce}-{index}.dll"));
    let result = (|| {
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)?;
        io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.sync_all()?;
        drop(output);
        if hash_file(&staged)? != expected_hash {
            return Err(CoreError::Validation(
                "elevated source staging hash mismatch".into(),
            ));
        }
        validate_nvidia_dll(&staged, policy)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(status: i32, signer: Option<&str>) -> NativeTrustResult {
        NativeTrustResult {
            status,
            signer: signer.map(str::to_owned),
        }
    }

    #[test]
    fn strict_trust_requires_revocation_success() {
        let report = resolve_trust(
            TrustPolicy::Strict,
            native(0, Some("NVIDIA Corporation")),
            None,
        );
        assert_eq!(report.signature, SignatureStatus::Trusted);
        assert_eq!(report.revocation, RevocationStatus::Verified);
    }

    #[test]
    fn only_offline_revocation_for_official_nvidia_can_fallback() {
        let offline = CRYPT_E_REVOCATION_OFFLINE.0;
        let accepted = resolve_trust(
            TrustPolicy::OfficialNvidiaCatalog,
            native(offline, None),
            Some(native(0, Some("NVIDIA Corporation"))),
        );
        assert_eq!(accepted.signature, SignatureStatus::Trusted);
        assert_eq!(accepted.revocation, RevocationStatus::UnavailableFallback);
        assert_eq!(accepted.native_failure.unwrap().status, offline);

        let wrong_signer = resolve_trust(
            TrustPolicy::OfficialNvidiaCatalog,
            native(offline, None),
            Some(native(0, Some("Example Publisher"))),
        );
        assert_ne!(
            wrong_signer.revocation,
            RevocationStatus::UnavailableFallback
        );

        let strict = resolve_trust(
            TrustPolicy::Strict,
            native(offline, None),
            Some(native(0, Some("NVIDIA Corporation"))),
        );
        assert_eq!(strict.signature, SignatureStatus::Untrusted);
    }

    #[test]
    fn non_revocation_trust_failures_never_fallback() {
        for status in [
            0x8009_6010_u32.cast_signed(), // TRUST_E_BAD_DIGEST
            0x800B_0101_u32.cast_signed(), // CERT_E_EXPIRED
            0x800B_0109_u32.cast_signed(), // CERT_E_UNTRUSTEDROOT
            0x800B_0111_u32.cast_signed(), // TRUST_E_EXPLICIT_DISTRUST
        ] {
            let report = resolve_trust(
                TrustPolicy::OfficialNvidiaCatalog,
                native(status, Some("NVIDIA Corporation")),
                Some(native(0, Some("NVIDIA Corporation"))),
            );
            assert_eq!(report.signature, SignatureStatus::Untrusted);
            assert_eq!(report.revocation, RevocationStatus::NotVerified);
            assert_eq!(report.native_failure.unwrap().status, status);
        }
    }

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
