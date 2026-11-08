//! The diagnostics log, and opening the app's folders in the file manager.

use std::path::{Path, PathBuf};

pub(crate) fn init() {
    app_diagnostics::init(log_directory().as_deref(), "dlss-updater", "info");
}

/// The folder this run appends its diagnostics log to, so the About dialog can
/// point the user at the file to attach when reporting a problem.
pub(crate) fn log_directory() -> Option<PathBuf> {
    dlss_platform::data_directory().map(|base| base.join("logs"))
}

/// Reveals a folder in the system file manager, creating it first so the
/// shell never opens on a missing path.
pub(crate) fn reveal(directory: &Path) {
    if let Err(error) = app_folders::reveal(directory) {
        tracing::warn!(path = %directory.display(), %error, "could not open folder");
    }
}

/// Opens a folder that belongs to someone else, such as a game's install
/// folder, in the system file manager. Unlike [`reveal`], never creates it.
pub(crate) fn open_existing(directory: &Path) {
    if let Err(error) = app_folders::open(directory) {
        tracing::warn!(path = %directory.display(), %error, "could not open folder");
    }
}
