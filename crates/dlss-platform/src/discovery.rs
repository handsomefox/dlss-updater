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

#[allow(clippy::case_sensitive_file_extension_comparisons)]
#[must_use]
pub fn is_managed_dll(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    (lower.starts_with("nvngx_") && lower.ends_with(".dll"))
        || (lower.starts_with("sl.") && lower.ends_with(".dll"))
        || lower == "nvlowlatencyvk.dll"
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
pub fn steam_manifests(steamapps: &Path) -> Vec<(String, String, PathBuf)> {
    let Ok(entries) = fs::read_dir(steamapps) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                return None;
            }
            let content = fs::read_to_string(entry.path()).ok()?;
            let pairs = quoted_pairs(&content);
            let app_id = value_for(&pairs, "appid")?.to_owned();
            let title = value_for(&pairs, "name")?.to_owned();
            let install = value_for(&pairs, "installdir")?.to_owned();
            Some((app_id, title, steamapps.join("common").join(install)))
        })
        .collect()
}

fn value_for<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
}

fn quoted_pairs(contents: &str) -> Vec<(String, String)> {
    quoted_tokens(contents)
        .chunks_exact(2)
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect()
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
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("item"))
        })
        .filter_map(|entry| {
            let manifest: EpicManifest =
                serde_json::from_reader(fs::File::open(entry.path()).ok()?).ok()?;
            if manifest.install_location.as_os_str().is_empty() {
                return None;
            }
            let stable = if manifest.catalog_item_id.is_empty() {
                manifest.app_name
            } else {
                manifest.catalog_item_id
            };
            (!stable.is_empty()).then(|| GameInstall {
                id: GameId(format!("epic:{stable}")),
                name: manifest.display_name,
                store: StoreKind::Epic,
                root: manifest.install_location,
                dlls: Vec::new(),
                inspection_errors: 0,
            })
        })
        .collect()
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
