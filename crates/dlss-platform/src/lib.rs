//! OS adapters and portable discovery helpers.

mod discovery;
mod portable;
pub use discovery::*;
pub use portable::*;

#[cfg(windows)]
pub mod windows;

/// The folder that holds backups, the release cache, imported DLLs, helper
/// staging files, and the logs: `%LOCALAPPDATA%\DLSS Updater` on Windows.
#[must_use]
pub fn data_directory() -> Option<std::path::PathBuf> {
    app_folders::app_data_dir(DATA_FOLDER)
}

/// The data folder's name under `%LOCALAPPDATA%`. Users' backups live in it,
/// so it must never change.
pub const DATA_FOLDER: &str = "DLSS Updater";
