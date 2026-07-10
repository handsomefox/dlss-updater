use dlss_core::{
    CoreError, DllInspector, DllInstallation, DllInstallationId, GameId, GameInstall, StoreKind,
};
use serde::Deserialize;
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

#[must_use]
pub fn is_managed_dll(name: &OsStr) -> bool {
    dlss_core::DllKind::classify(name).is_some()
}

pub fn scan_game(
    game_id: &GameId,
    root: &Path,
    inspector: &dyn DllInspector,
) -> Vec<Result<DllInstallation, CoreError>> {
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
            let entry = entry?;
            let path = entry.into_path();
            let metadata = inspector.inspect(&path)?;
            let relative = path.strip_prefix(root).unwrap_or(&path);
            Ok(DllInstallation {
                id: DllInstallationId(format!("{}:{}", game_id.0, path_key(relative))),
                game_id: game_id.clone(),
                file_name: path.file_name().unwrap_or_default().to_os_string(),
                path,
                metadata,
            })
        })
        .collect()
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
