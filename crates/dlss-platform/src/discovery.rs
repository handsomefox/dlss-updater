use dlss_core::{
    CoreError, DllInspector, DllInstallation, DllInstallationId, GameId, GameInstall, StoreKind,
};
use serde::Deserialize;
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Instant,
};
use walkdir::WalkDir;

#[must_use]
pub fn is_managed_dll(name: &OsStr) -> bool {
    dlss_core::DllKind::classify(name).is_some()
}

/// Walks a game folder and identifies every managed DLL without reading any
/// file contents. Traversal failures are retained so a partially readable
/// install is still reported as such.
#[must_use]
pub fn managed_dll_entries(
    game_id: &GameId,
    root: &Path,
) -> Vec<Result<(DllInstallationId, PathBuf), CoreError>> {
    WalkDir::new(root)
        .follow_links(false)
        .max_depth(12)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() && is_managed_dll(entry.file_name()) => {
                Some(Ok(entry))
            }
            Ok(_) => None,
            Err(error) => Some(Err(CoreError::Validation(format!(
                "filesystem traversal failed: {error}"
            )))),
        })
        .map(|entry| {
            let path = entry?.into_path();
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let id = DllInstallationId(format!("{}:{}", game_id.0, path_key(relative)));
            Ok((id, path))
        })
        .collect()
}

pub fn scan_game(
    game_id: &GameId,
    root: &Path,
    inspector: &dyn DllInspector,
) -> Vec<Result<DllInstallation, CoreError>> {
    managed_dll_entries(game_id, root)
        .into_iter()
        .map(|entry| inspect_entry(entry, game_id, inspector))
        .collect()
}

/// Fills in the managed DLL installations for every discovered game.
///
/// Games are independent, and both phases of the work — walking a large
/// install tree and reading DLL contents — are dominated by waiting on the
/// filesystem, so the games are spread across a small pool of threads and
/// handed out one at a time. Dynamic hand-out matters because install sizes
/// differ by orders of magnitude; a fixed split would leave one thread walking
/// a 200 GB game while the rest idle.
pub fn inspect_games(games: &mut [GameInstall], inspector: &dyn DllInspector) {
    let started = Instant::now();
    let inputs: Vec<(GameId, PathBuf)> = games
        .iter()
        .map(|game| (game.id.clone(), game.root.clone()))
        .collect();
    let next = AtomicUsize::new(0);
    let walk_nanos = AtomicU64::new(0);
    let inspect_nanos = AtomicU64::new(0);
    let outcomes = Mutex::new(Vec::with_capacity(inputs.len()));
    let workers = worker_count(inputs.len());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((game_id, root)) = inputs.get(index) else {
                        break;
                    };
                    let walk_started = Instant::now();
                    let entries = managed_dll_entries(game_id, root);
                    walk_nanos.fetch_add(elapsed_nanos(walk_started), Ordering::Relaxed);

                    let inspect_started = Instant::now();
                    let inspected: Vec<_> = entries
                        .into_iter()
                        .map(|entry| inspect_entry(entry, game_id, inspector))
                        .collect();
                    inspect_nanos.fetch_add(elapsed_nanos(inspect_started), Ordering::Relaxed);
                    outcomes
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((index, inspected));
                }
            });
        }
    });
    // A worker that panicked mid-push poisons the lock; keep every result that
    // did land rather than losing the whole scan.
    let outcomes = outcomes
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    let mut dlls = 0;
    let mut errors = 0;
    for (index, inspected) in outcomes {
        let game = &mut games[index];
        game.inspection_errors = inspected.iter().filter(|result| result.is_err()).count();
        game.dlls = inspected.into_iter().filter_map(Result::ok).collect();
        dlls += game.dlls.len();
        errors += game.inspection_errors;
    }
    tracing::info!(
        games = games.len(),
        dlls,
        errors,
        workers,
        elapsed_ms = started.elapsed().as_millis(),
        walk_thread_ms = walk_nanos.load(Ordering::Relaxed) / 1_000_000,
        inspect_thread_ms = inspect_nanos.load(Ordering::Relaxed) / 1_000_000,
        "library inspection completed"
    );
}

fn inspect_entry(
    entry: Result<(DllInstallationId, PathBuf), CoreError>,
    game_id: &GameId,
    inspector: &dyn DllInspector,
) -> Result<DllInstallation, CoreError> {
    let (id, path) = entry?;
    let metadata = inspector.inspect(&path)?;
    Ok(DllInstallation {
        id,
        game_id: game_id.clone(),
        file_name: path.file_name().unwrap_or_default().to_os_string(),
        path,
        metadata,
    })
}

/// Threads to spread discovery across. Capped well below a big machine's core
/// count: past a handful of concurrent readers the storage device, not the
/// CPU, sets the pace, and extra threads only add seek contention.
fn worker_count(games: usize) -> usize {
    const MAX_WORKERS: usize = 8;
    thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .min(MAX_WORKERS)
        .min(games)
        .max(1)
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Parses legacy and modern Steam `KeyValues` library formats.
#[must_use]
pub fn steam_library_paths(contents: &str) -> Vec<PathBuf> {
    let tokens = quoted_tokens(contents);
    let mut paths = Vec::new();
    for pair in tokens.windows(2) {
        let key = pair[0].as_str();
        let value = pair[1].as_str();
        let looks_like_path = value.contains('/') || value.contains('\\');
        if key.eq_ignore_ascii_case("path")
            || (key.bytes().all(|byte| byte.is_ascii_digit()) && looks_like_path)
        {
            paths.push(PathBuf::from(value.replace("\\\\", "\\")));
        }
    }
    deduplicate_roots(paths)
}

#[must_use]
pub fn steam_steamapps_dirs(steam_root: &Path) -> (Vec<PathBuf>, Option<String>) {
    let candidates = [
        steam_root.join("config/libraryfolders.vdf"),
        steam_root.join("steamapps/libraryfolders.vdf"),
    ];
    let mut roots = vec![steam_root.join("steamapps")];
    let mut found = 0;
    let mut errors = Vec::new();
    for candidate in candidates {
        if !candidate.exists() {
            tracing::info!(path = %candidate.display(), "Steam library file not found");
            continue;
        }
        found += 1;
        tracing::info!(path = %candidate.display(), "Steam library file found");
        match fs::read_to_string(&candidate) {
            Ok(contents) => {
                let libraries = steam_library_paths(&contents);
                tracing::info!(path = %candidate.display(), libraries = libraries.len(), "Steam library file parsed");
                roots.extend(libraries.into_iter().map(|path| path.join("steamapps")));
            }
            Err(error) => {
                tracing::warn!(path = %candidate.display(), %error, "Steam library file could not be read");
                errors.push(format!("{}: {error}", candidate.display()));
            }
        }
    }
    let detail = if !errors.is_empty() {
        Some(errors.join("; "))
    } else if found == 0 {
        Some("libraryfolders.vdf was not found in config or steamapps".into())
    } else {
        None
    };
    (deduplicate_steam_roots(roots), detail)
}

#[must_use]
pub fn steam_manifests(steamapps: &Path) -> Vec<(String, String, PathBuf)> {
    steam_manifests_with_errors(steamapps).items
}

pub struct ManifestScan<T> {
    pub items: Vec<T>,
    pub errors: Vec<String>,
}

#[must_use]
pub fn steam_manifests_with_errors(steamapps: &Path) -> ManifestScan<(String, String, PathBuf)> {
    let entries = match fs::read_dir(steamapps) {
        Ok(entries) => entries,
        Err(error) => {
            return ManifestScan {
                items: Vec::new(),
                errors: vec![format!("{}: {error}", steamapps.display())],
            };
        }
    };
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("{}: {error}", steamapps.display()));
                continue;
            }
        };
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let tokens = quoted_tokens(&content);
        let Some((app_id, title, install)) = manifest_identity(&tokens) else {
            errors.push(format!("{}: required fields are missing", path.display()));
            continue;
        };
        // Steamworks Common Redistributables is shared runtime infrastructure,
        // not a user-launchable game installation.
        if app_id == "228980" {
            tracing::info!(path = %path.display(), "ignoring Steamworks Shared manifest");
            continue;
        }
        items.push((
            app_id.to_owned(),
            title.to_owned(),
            steamapps.join("common").join(install),
        ));
    }
    ManifestScan { items, errors }
}

fn manifest_identity(tokens: &[String]) -> Option<(&str, &str, &str)> {
    Some((
        token_value(tokens, "appid")?,
        token_value(tokens, "name")?,
        token_value(tokens, "installdir")?,
    ))
}

fn token_value<'a>(tokens: &'a [String], key: &str) -> Option<&'a str> {
    tokens
        .windows(2)
        .find(|pair| pair[0].eq_ignore_ascii_case(key))
        .map(|pair| pair[1].as_str())
}

fn quoted_tokens(contents: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = contents.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '"' {
            continue;
        }
        let mut token = String::new();
        while let Some(character) = chars.next() {
            match character {
                '"' => break,
                '\\' if chars.peek() == Some(&'"') => {
                    chars.next();
                    token.push('"');
                }
                other => token.push(other),
            }
        }
        tokens.push(token);
    }
    tokens
}

pub fn deduplicate_roots(roots: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn deduplicate_steam_roots(roots: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter(|path| {
            let key = path
                .to_string_lossy()
                .replace('/', "\\")
                .to_ascii_lowercase();
            seen.insert(key)
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EpicManifest {
    display_name: String,
    install_location: PathBuf,
    #[serde(default)]
    catalog_item_id: String,
    #[serde(default)]
    app_name: String,
}

/// Reads Epic `.item` manifests. A malformed entry never hides valid siblings.
#[must_use]
pub fn epic_manifests(directory: &Path) -> Vec<GameInstall> {
    epic_manifests_with_errors(directory).items
}

#[must_use]
pub fn epic_manifests_with_errors(directory: &Path) -> ManifestScan<GameInstall> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            return ManifestScan {
                items: Vec::new(),
                errors: vec![format!("{}: {error}", directory.display())],
            };
        }
    };
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("{}: {error}", directory.display()));
                continue;
            }
        };
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("item"))
        {
            continue;
        }
        match read_epic_manifest(&path) {
            Ok(game) => items.push(game),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    ManifestScan { items, errors }
}

fn read_epic_manifest(path: &Path) -> Result<GameInstall, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let manifest: EpicManifest =
        serde_json::from_reader(file).map_err(|error| error.to_string())?;
    if manifest.install_location.as_os_str().is_empty() {
        return Err("install location is missing".into());
    }
    let stable = if manifest.catalog_item_id.is_empty() {
        manifest.app_name
    } else {
        manifest.catalog_item_id
    };
    if stable.is_empty() {
        return Err("catalog and app identifiers are missing".into());
    }
    Ok(GameInstall {
        id: GameId(format!("epic:{stable}")),
        name: manifest.display_name,
        store: StoreKind::Epic,
        root: manifest.install_location,
        dlls: Vec::new(),
        inspection_errors: 0,
    })
}

/// Creates a stable manual-game record from a canonical directory.
///
/// # Errors
/// Returns an error when the directory cannot be canonicalized.
pub fn manual_install(root: &Path) -> Result<GameInstall, CoreError> {
    let canonical = root.canonicalize()?;
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Manual game".into());
    Ok(GameInstall {
        id: GameId(format!("manual:{}", path_key(&canonical))),
        name,
        store: StoreKind::Manual,
        root: canonical,
        dlls: Vec::new(),
        inspection_errors: 0,
    })
}

#[cfg(unix)]
fn path_key(path: &Path) -> String {
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(windows)]
fn path_key(path: &Path) -> String {
    use std::fmt::Write as _;
    use std::os::windows::ffi::OsStrExt;
    let mut encoded = String::new();
    for unit in path.as_os_str().encode_wide() {
        write!(encoded, "{unit:04x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(not(any(unix, windows)))]
fn path_key(path: &Path) -> String {
    path.as_os_str().to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modern_and_legacy_steam_libraries() {
        let text = r#""libraryfolders" { "0" "C:\\Steam" "1" { "path" "D:\\Games" "apps" { "123" "42" } } }"#;
        assert_eq!(
            steam_library_paths(text),
            [PathBuf::from(r"C:\Steam"), PathBuf::from(r"D:\Games")]
        );
    }

    #[test]
    fn resolves_both_steam_libraryfolder_locations() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("config")).unwrap();
        fs::create_dir_all(directory.path().join("steamapps")).unwrap();
        fs::write(
            directory.path().join("config/libraryfolders.vdf"),
            r#""path" "D:\\Games""#,
        )
        .unwrap();
        fs::write(
            directory.path().join("steamapps/libraryfolders.vdf"),
            r#""path" "E:\\Games""#,
        )
        .unwrap();
        let (roots, detail) = steam_steamapps_dirs(directory.path());
        assert!(detail.is_none());
        assert!(roots.contains(&directory.path().join("steamapps")));
        assert!(roots.contains(&PathBuf::from(r"D:\Games").join("steamapps")));
        assert!(roots.contains(&PathBuf::from(r"E:\Games").join("steamapps")));
    }

    #[test]
    fn resolves_config_only_steam_libraries() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("config")).unwrap();
        fs::write(
            directory.path().join("config/libraryfolders.vdf"),
            r#""path" "D:\\Games""#,
        )
        .unwrap();
        let (roots, detail) = steam_steamapps_dirs(directory.path());
        assert!(detail.is_none());
        assert!(roots.contains(&PathBuf::from(r"D:\Games").join("steamapps")));
    }

    #[test]
    fn resolves_legacy_only_steam_libraries() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("steamapps")).unwrap();
        fs::write(
            directory.path().join("steamapps/libraryfolders.vdf"),
            r#""path" "E:\\Games""#,
        )
        .unwrap();
        let (roots, detail) = steam_steamapps_dirs(directory.path());
        assert!(detail.is_none());
        assert!(roots.contains(&PathBuf::from(r"E:\Games").join("steamapps")));
    }

    #[test]
    fn reports_when_steam_libraryfolders_are_missing() {
        let directory = tempfile::tempdir().unwrap();
        let (roots, detail) = steam_steamapps_dirs(directory.path());
        assert_eq!(roots, vec![directory.path().join("steamapps")]);
        assert!(detail.is_some());
    }

    #[test]
    fn recognizes_only_supported_names() {
        assert!(is_managed_dll(OsStr::new("nvngx_dlss.dll")));
        assert!(is_managed_dll(OsStr::new("sl.interposer.dll")));
        assert!(is_managed_dll(OsStr::new("NvLowLatencyVk.dll")));
        assert!(!is_managed_dll(OsStr::new("dxgi.dll")));
    }

    #[test]
    fn epic_manifests_skip_malformed_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("bad.item"), "not json").unwrap();
        fs::write(
            directory.path().join("game.item"),
            r#"{"DisplayName":"Unicode 游戏","InstallLocation":"D:\\Epic\\Game","CatalogItemId":"catalog-id","AppName":"game"}"#,
        )
        .unwrap();
        let games = epic_manifests(directory.path());
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].id.0, "epic:catalog-id");
        assert_eq!(games[0].name, "Unicode 游戏");
        let diagnostic = epic_manifests_with_errors(directory.path());
        assert_eq!(diagnostic.items.len(), 1);
        assert_eq!(diagnostic.errors.len(), 1);
    }

    #[test]
    fn steam_manifest_failures_are_reported() {
        let directory = tempfile::tempdir().unwrap();
        let missing = steam_manifests_with_errors(&directory.path().join("missing"));
        assert!(missing.items.is_empty());
        assert_eq!(missing.errors.len(), 1);

        fs::write(directory.path().join("appmanifest_1.acf"), "invalid").unwrap();
        let malformed = steam_manifests_with_errors(directory.path());
        assert!(malformed.items.is_empty());
        assert_eq!(malformed.errors.len(), 1);
    }

    #[test]
    fn parses_realistic_steam_appstate_manifest() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("appmanifest_123456.acf"),
            r#""AppState"
            {
                "appid" "123456"
                "name" "Example Game"
                "installdir" "ExampleGame"
            }"#,
        )
        .unwrap();
        let scan = steam_manifests_with_errors(directory.path());
        assert!(scan.errors.is_empty());
        assert_eq!(scan.items.len(), 1);
        assert_eq!(scan.items[0].0, "123456");
        assert_eq!(scan.items[0].1, "Example Game");
        assert_eq!(scan.items[0].2, directory.path().join("common/ExampleGame"));
    }

    #[test]
    fn ignores_steamworks_shared_redistributables() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("appmanifest_228980.acf"),
            r#""AppState"
            {
                "appid" "228980"
                "name" "Steamworks Common Redistributables"
                "installdir" "Steamworks Shared"
            }"#,
        )
        .unwrap();
        let scan = steam_manifests_with_errors(directory.path());
        assert!(scan.items.is_empty());
        assert!(scan.errors.is_empty());
    }

    #[test]
    fn steam_roots_deduplicate_case_and_separator_variants() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("config")).unwrap();
        fs::write(
            directory.path().join("config/libraryfolders.vdf"),
            r#""path" "C:\\Program Files (x86)\\Steam""#,
        )
        .unwrap();
        let root = Path::new("c:/program files (x86)/steam");
        let (roots, _) = steam_steamapps_dirs(root);
        assert_eq!(roots.len(), 1);
    }

    #[test]
    fn manual_id_uses_canonical_path() {
        let directory = tempfile::tempdir().unwrap();
        let game = manual_install(directory.path()).unwrap();
        assert!(game.id.0.starts_with("manual:"));
        assert_eq!(game.root, directory.path().canonicalize().unwrap());
    }

    /// Reports the first byte of the file as its hash, so a test can tell which
    /// game's DLL an inspection result came from.
    struct MarkerInspector;
    impl DllInspector for MarkerInspector {
        fn inspect(&self, path: &Path) -> Result<dlss_core::DllMetadata, CoreError> {
            let bytes = fs::read(path)?;
            let mut sha256 = [0_u8; 32];
            sha256[0] = *bytes.first().unwrap_or(&0);
            Ok(dlss_core::DllMetadata {
                version: None,
                sha256,
                signature: dlss_core::SignatureStatus::Unavailable,
                x86_64: true,
            })
        }
    }

    #[test]
    fn parallel_inspection_keeps_every_result_with_its_own_game() {
        let directory = tempfile::tempdir().unwrap();
        let mut games: Vec<GameInstall> = (0..24_u8)
            .map(|index| {
                let root = directory.path().join(format!("game-{index}"));
                fs::create_dir_all(root.join("bin")).unwrap();
                fs::write(root.join("bin/nvngx_dlss.dll"), [index]).unwrap();
                GameInstall {
                    id: GameId(format!("test:{index}")),
                    name: format!("Game {index}"),
                    store: StoreKind::Manual,
                    root,
                    dlls: Vec::new(),
                    inspection_errors: 0,
                }
            })
            .collect();

        inspect_games(&mut games, &MarkerInspector);

        for (index, game) in games.iter().enumerate() {
            let index = u8::try_from(index).unwrap();
            assert_eq!(game.inspection_errors, 0);
            assert_eq!(game.dlls.len(), 1, "game {index} lost its DLL");
            let dll = &game.dlls[0];
            assert_eq!(dll.game_id, game.id);
            assert!(dll.path.starts_with(&game.root));
            assert!(dll.id.0.starts_with(&format!("{}:", game.id.0)));
            assert_eq!(
                dll.metadata.sha256[0], index,
                "game {index} received another game's inspection"
            );
        }
    }

    #[test]
    fn scan_retains_root_traversal_failures() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        let results = scan_game(
            &GameId("manual:test".into()),
            &missing,
            &crate::PortablePeInspector,
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
    }

    #[cfg(unix)]
    #[test]
    fn manual_ids_do_not_collapse_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;
        let directory = tempfile::tempdir().unwrap();
        let first = directory
            .path()
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        let second = directory
            .path()
            .join(std::ffi::OsString::from_vec(vec![0xfe]));
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        assert_ne!(
            manual_install(&first).unwrap().id,
            manual_install(&second).unwrap().id
        );
    }
}
