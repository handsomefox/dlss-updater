#![cfg_attr(windows, windows_subsystem = "windows")]

mod diagnostics;
mod state;
mod ui;
mod worker;

#[cfg(windows)]
use dlss_core::{KnownDirectories, SystemToolProvider};
use dlss_core::{PlatformCapabilities, SystemToolState};
use eframe::egui;
use state::PersistedState;
use std::time::{Duration, Instant};
use ui::library::discovery_report_label;
use ui::review::ReviewIntent;
use ui::theme::{self, icons};
use ui::widgets;
use ui::windows::{format_timestamp, progress_label, state_label};
#[cfg(windows)]
use worker::IndicatorRequest;
use worker::{Command, Event, Worker};

fn main() -> eframe::Result {
    diagnostics::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting DLSS Updater");
    if std::env::args_os().any(|arg| arg == "--elevated-helper") {
        elevated_helper();
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([1000.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DLSS Updater",
        options,
        Box::new(|cc| Ok(Box::new(DlssApp::new(cc)))),
    )
}

fn elevated_helper() {
    #[cfg(windows)]
    {
        let mut arguments = std::env::args_os().skip_while(|arg| arg != "--elevated-helper");
        let _mode = arguments.next();
        let Some(plan) = arguments.next() else {
            tracing::error!("missing elevated helper plan");
            std::process::exit(2);
        };
        let Some(plan_hash) = arguments.next().and_then(|value| parse_sha256(&value)) else {
            tracing::error!("missing or invalid elevated helper plan digest");
            std::process::exit(2);
        };
        if let Err(error) =
            dlss_platform::windows::run_elevated_helper(std::path::Path::new(&plan), plan_hash)
        {
            // The plan could not be validated far enough to write a result file,
            // so signal failure through the exit code. When the plan did parse,
            // the outcome (including errors) is written to the result file above.
            tracing::error!(%error, "elevated helper rejected the plan");
            std::process::exit(2);
        }
    }
    #[cfg(not(windows))]
    tracing::warn!("elevated helper is unavailable on this platform");
}

#[cfg(windows)]
fn parse_sha256(value: &std::ffi::OsStr) -> Option<[u8; 32]> {
    let value = value.to_str()?;
    if value.len() != 64 {
        return None;
    }
    let mut hash = [0_u8; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(hash)
}

struct DlssApp {
    persisted: PersistedState,
    games: Vec<GameRow>,
    discovery_reports: Vec<dlss_core::StoreDiscoveryReport>,
    filter: String,
    filter_mode: GameFilter,
    game_sort: GameSort,
    store_filter: StoreFilter,
    view: View,
    open_windows: std::collections::HashSet<AppWindow>,
    tool_state: SystemToolState,
    staged_tool_state: SystemToolState,
    capabilities: PlatformCapabilities,
    worker: Worker,
    runtime: RuntimeStatus,
    last_error: Option<String>,
    catalog_release: Option<String>,
    catalog_error: Option<String>,
    releases: Vec<dlss_core::CachedRelease>,
    release_errors: std::collections::HashMap<dlss_core::ReleaseId, String>,
    release_progress: Option<(dlss_core::ReleaseId, u64, Option<u64>)>,
    imports: Vec<dlss_core::ImportedDllRecord>,
    backups: Vec<dlss_core::BackupRecord>,
    inspecting_release: Option<dlss_core::ReleaseId>,
    upgrading: Option<dlss_core::GameId>,
    toast: Option<String>,
    toast_identity: Option<String>,
    toast_started: Option<Instant>,
    undo_game: Option<dlss_core::GameId>,
    review: Option<ui::review::ReviewState>,
    /// Games with an in-flight profile apply, mapped to the DLL installation
    /// ids that were sent, so exactly those staged targets can be cleared
    /// when the operation finishes.
    profiles_applying:
        std::collections::HashMap<dlss_core::GameId, Vec<dlss_core::DllInstallationId>>,
    #[cfg(windows)]
    tool_runtime: WindowsToolRuntime,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum AppWindow {
    Tools,
    ToolInfo,
    StoreWarnings,
    Releases,
    Activity,
    Roots,
}

/// Which main content the central panel shows.
enum View {
    Library,
    Game(dlss_core::GameId),
}

struct RuntimeStatus {
    scanning: bool,
    catalog_loading: bool,
    worker_connected: bool,
}

#[cfg(windows)]
#[derive(Default)]
struct WindowsToolRuntime {
    observed_hash: Option<[u8; 32]>,
    stale_confirmed: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum GameFilter {
    All,
    HasDlls,
    Upgrades,
    Custom,
    Errors,
    Recent,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SortKey {
    Name,
    Store,
    Dlls,
    DlssVersion,
    Upgrades,
    State,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct GameSort {
    key: SortKey,
    ascending: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum StoreFilter {
    All,
    Steam,
    Epic,
    Gog,
    Manual,
}

struct GameRow {
    id: dlss_core::GameId,
    selected: bool,
    name: String,
    store: &'static str,
    store_kind: dlss_core::StoreKind,
    root: std::path::PathBuf,
    dlls: usize,
    dlss_version: Option<dlss_core::DllVersion>,
    dlss_upgrades: usize,
    upgrades: usize,
    state: String,
    last_operation: String,
    details: Vec<dlss_core::DllInstallation>,
    inspection_errors: usize,
    known_risk: Option<&'static str>,
}

impl GameRow {
    fn from_install(game: dlss_core::GameInstall) -> Self {
        let known_risk = dlss_core::known_game_risk(&game);
        let dll_count = game.dlls.len();
        let inspection_errors = game.inspection_errors;
        let has_unknown = game.dlls.iter().any(|dll| dll.metadata.version.is_none());
        let dlss_version = game
            .dlls
            .iter()
            .filter(|dll| {
                dlss_core::DllKind::classify(&dll.file_name)
                    == Some(dlss_core::DllKind::DlssSuperResolution)
            })
            .filter_map(|dll| dll.metadata.version)
            .max();
        let store_kind = game.store;
        Self {
            id: game.id,
            selected: false,
            name: game.name,
            store: match store_kind {
                dlss_core::StoreKind::Steam => "Steam",
                dlss_core::StoreKind::Epic => "Epic",
                dlss_core::StoreKind::Gog => "GOG",
                dlss_core::StoreKind::Manual => "Manual",
            },
            store_kind,
            root: game.root,
            dlls: dll_count,
            dlss_version,
            dlss_upgrades: 0,
            upgrades: 0,
            state: if inspection_errors > 0 {
                format!("Error ({inspection_errors})")
            } else if dll_count == 0 {
                "No DLLs".into()
            } else if has_unknown {
                "Unknown".into()
            } else {
                "Current".into()
            },
            last_operation: "Never".into(),
            details: game.dlls,
            inspection_errors,
            known_risk,
        }
    }
}

impl DlssApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut persisted: PersistedState = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        if persisted.activity.len() > 500 {
            let excess = persisted.activity.len() - 500;
            persisted.activity.drain(..excess);
        }
        canonicalize_roots(&mut persisted.custom_roots);
        #[cfg(windows)]
        let capabilities = dlss_platform::windows::capabilities();
        #[cfg(not(windows))]
        let capabilities = PlatformCapabilities::default();
        #[cfg(windows)]
        let backup_result = dlss_platform::windows::WindowsKnownDirectories
            .local_app_data()
            .and_then(|base| {
                dlss_core::BackupStore::new(base.join("DLSS Updater/backups")).load_trusted_index(
                    &dlss_platform::windows::WindowsDllInspector,
                    &dlss_platform::windows::WindowsTrustVerifier,
                )
            })
            .map(|index| index.records);
        #[cfg(windows)]
        let (backups, backup_warning) = match backup_result {
            Ok(backups) => (backups, None),
            Err(error) => (
                Vec::new(),
                Some(format!("Could not load backup history: {error}")),
            ),
        };
        #[cfg(not(windows))]
        let (backups, backup_warning) = (Vec::new(), None);
        let worker = Worker::start(persisted.custom_roots.clone(), cc.egui_ctx.clone());
        let app = Self {
            persisted,
            games: Vec::new(),
            discovery_reports: Vec::new(),
            filter: String::new(),
            filter_mode: GameFilter::All,
            game_sort: GameSort {
                key: SortKey::Name,
                ascending: true,
            },
            store_filter: StoreFilter::All,
            view: View::Library,
            open_windows: std::collections::HashSet::new(),
            tool_state: SystemToolState::Unavailable(
                "Windows registry controls are unavailable on this platform".into(),
            ),
            staged_tool_state: SystemToolState::Off,
            capabilities,
            worker,
            runtime: RuntimeStatus {
                scanning: false,
                catalog_loading: true,
                worker_connected: true,
            },
            last_error: backup_warning,
            catalog_release: None,
            catalog_error: None,
            releases: Vec::new(),
            release_errors: std::collections::HashMap::new(),
            release_progress: None,
            imports: Vec::new(),
            backups,
            inspecting_release: None,
            upgrading: None,
            toast: None,
            toast_identity: None,
            toast_started: None,
            undo_game: None,
            review: None,
            profiles_applying: std::collections::HashMap::new(),
            #[cfg(windows)]
            tool_runtime: WindowsToolRuntime::default(),
        };
        let _ = app.worker.commands.send(Command::Scan);
        let _ = app.worker.commands.send(Command::RefreshCatalog);
        app
    }

    fn receive_worker_events(&mut self, ctx: &egui::Context) {
        loop {
            let event = match self.worker.events.try_recv() {
                Ok(event) => event,
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    if self.runtime.worker_connected {
                        self.runtime.worker_connected = false;
                        self.runtime.scanning = false;
                        self.runtime.catalog_loading = false;
                        self.upgrading = None;
                        self.profiles_applying.clear();
                        self.last_error = Some("Background worker stopped unexpectedly".into());
                    }
                    break;
                }
            };
            self.handle_worker_event(event);
            ctx.request_repaint();
        }
    }

    fn handle_worker_event(&mut self, event: Event) {
        match event {
            Event::Warning(warning) => {
                tracing::warn!(%warning, "worker warning");
                self.last_error = Some(warning);
            }
            Event::ScanStarted => self.handle_scan_started(),
            Event::ScanFinished(result) => self.handle_scan_finished(result),
            Event::CatalogStarted => {
                self.runtime.catalog_loading = true;
                self.catalog_error = None;
            }
            Event::CatalogFinished(result) => self.handle_catalog_finished(result),
            Event::ReleaseFinished(result) => self.handle_release_finished(result),
            Event::ReleaseRemoved(result) => self.handle_release_removed(result),
            Event::ImportsLoaded(imports) => self.imports = imports,
            Event::ImportFinished(result) => self.handle_import_finished(result),
            Event::ImportRemoved(result) => self.handle_import_removed(result),
            Event::ReleaseProgress {
                id,
                state,
                received,
                total,
            } => {
                self.handle_release_progress(&id, state, received, total);
            }
            Event::UpgradeStarted(game_id) => {
                self.upgrading = Some(game_id);
                self.undo_game = None;
                self.toast = Some("Downloading and validating the official release…".into());
            }
            Event::UpgradeFinished {
                game_id,
                game,
                result,
            } => {
                self.handle_upgrade_finished(&game_id, game, result);
            }
            #[cfg(windows)]
            Event::IndicatorFinished(result) => self.handle_indicator_finished(result),
        }
    }

    fn handle_scan_started(&mut self) {
        self.runtime.scanning = true;
        self.undo_game = None;
        self.last_error = None;
    }

    fn handle_scan_finished(
        &mut self,
        result: Result<dlss_core::DiscoveryOutcome, worker::WorkerError>,
    ) {
        self.runtime.scanning = false;
        let Ok(outcome) = result else {
            self.last_error = result.err().map(|error| error.to_string());
            return;
        };
        self.discovery_reports = outcome.reports;
        self.games = outcome
            .games
            .into_iter()
            .map(GameRow::from_install)
            .collect();
        if let View::Game(id) = &self.view
            && !self.games.iter().any(|game| &game.id == id)
        {
            self.view = View::Library;
        }
        let known_dlls: std::collections::HashSet<_> = self
            .games
            .iter()
            .flat_map(|game| game.details.iter().map(|dll| dll.id.clone()))
            .collect();
        self.persisted
            .target_profile
            .targets
            .retain(|id, _| known_dlls.contains(id));
        self.refresh_upgrade_counts();
    }

    fn handle_catalog_finished(
        &mut self,
        result: Result<worker::CatalogSnapshot, worker::WorkerError>,
    ) {
        self.runtime.catalog_loading = false;
        match result {
            Ok(snapshot) => {
                self.catalog_release = snapshot.latest;
                self.releases = snapshot.releases;
                self.catalog_error = None;
                self.refresh_upgrade_counts();
            }
            Err(error) => {
                self.catalog_error = Some(error.to_string());
                self.last_error = Some(format!("Catalog: {error}"));
            }
        }
    }

    fn handle_release_finished(
        &mut self,
        result: Result<dlss_core::CachedRelease, worker::WorkerError>,
    ) {
        let requested_id = self.inspecting_release.take();
        self.release_progress = None;
        match result {
            Ok(release) => {
                self.release_errors.remove(&release.metadata.id);
                if let Some(existing) = self
                    .releases
                    .iter_mut()
                    .find(|existing| existing.metadata.id == release.metadata.id)
                {
                    existing.clone_from(&release);
                } else {
                    self.releases.push(release.clone());
                }
                self.toast = Some(format!(
                    "{} ready: {} production DLLs",
                    release.metadata.tag,
                    release.dlls.len()
                ));
                self.refresh_upgrade_counts();
            }
            Err(error) => {
                let message = error.to_string();
                if let Some(id) = requested_id {
                    self.release_errors.insert(id, message.clone());
                }
                self.last_error = Some(format!("Release validation: {message}"));
            }
        }
    }

    fn handle_release_removed(
        &mut self,
        result: Result<dlss_core::ReleaseId, worker::WorkerError>,
    ) {
        match result {
            Ok(id) => {
                if let Some(release) = self
                    .releases
                    .iter_mut()
                    .find(|release| release.metadata.id == id)
                {
                    release.state = dlss_core::ReleaseState::MetadataOnly;
                    release.dlls.clear();
                }
                self.release_errors.remove(&id);
                self.toast = Some("Downloaded release removed".into());
                self.refresh_upgrade_counts();
            }
            Err(error) => self.last_error = Some(format!("Could not remove release: {error}")),
        }
    }

    fn handle_import_finished(
        &mut self,
        result: Result<dlss_core::ImportedDllRecord, worker::WorkerError>,
    ) {
        match result {
            Ok(record) => {
                if let Some(existing) = self
                    .imports
                    .iter_mut()
                    .find(|existing| existing.sha256 == record.sha256)
                {
                    existing.clone_from(&record);
                } else {
                    self.imports.push(record);
                }
                self.toast = Some("NVIDIA-signed DLL imported".into());
            }
            Err(error) => self.last_error = Some(format!("DLL import failed: {error}")),
        }
    }

    fn handle_import_removed(&mut self, result: Result<[u8; 32], worker::WorkerError>) {
        match result {
            Ok(hash) => self.imports.retain(|record| record.sha256 != hash),
            Err(error) => self.last_error = Some(format!("Could not remove import: {error}")),
        }
    }

    fn handle_release_progress(
        &mut self,
        id: &dlss_core::ReleaseId,
        state: dlss_core::ReleaseState,
        received: u64,
        total: Option<u64>,
    ) {
        if let Some(release) = self
            .releases
            .iter_mut()
            .find(|release| &release.metadata.id == id)
        {
            release.state = state;
        }
        self.release_progress = Some((id.clone(), received, total));
        self.toast = Some(progress_label(state, received, total));
    }

    fn handle_upgrade_finished(
        &mut self,
        game_id: &dlss_core::GameId,
        game: Option<dlss_core::GameInstall>,
        result: Result<worker::UpgradeReport, worker::WorkerError>,
    ) {
        self.upgrading = None;
        let applying_profile = self.profiles_applying.remove(game_id);
        if let Some(game) = game
            && let Some(index) = self.games.iter().position(|row| &row.id == game_id)
        {
            let selected = self.games[index].selected;
            self.games[index] = GameRow::from_install(game);
            self.games[index].selected = selected;
        }
        match result {
            Ok(report) => self.handle_upgrade_report(game_id, applying_profile, report),
            Err(error) => {
                self.last_error = Some(error.to_string());
                self.toast = Some(format!("Upgrade failed: {error}"));
            }
        }
        self.refresh_backups();
    }

    fn handle_upgrade_report(
        &mut self,
        game_id: &dlss_core::GameId,
        applied_targets: Option<Vec<dlss_core::DllInstallationId>>,
        report: worker::UpgradeReport,
    ) {
        self.toast = Some(format!(
            "{}: changed {}, failed {}",
            report.release, report.changed, report.failed
        ));
        if let Some(warning) = report.warning {
            self.last_error = Some(warning.clone());
            self.toast = Some(format!(
                "{} · {warning}",
                self.toast.take().unwrap_or_default()
            ));
        }
        if let Some(row) = self.games.iter_mut().find(|row| &row.id == game_id) {
            row.last_operation = self.toast.clone().unwrap_or_default();
        }
        self.undo_game = report.can_undo.then(|| game_id.clone());
        self.append_activity(dlss_core::ActivityRecord {
            timestamp_unix: dlss_core::now_unix(),
            kind: if report.release == "Undo" {
                "restore"
            } else {
                "dll_swap"
            }
            .into(),
            detail: self.toast.clone().unwrap_or_default(),
        });
        // Clear exactly the staged targets that were sent; targets the user
        // left unchecked in the review stay staged.
        if let Some(ids) = applied_targets {
            for id in ids {
                self.persisted.target_profile.targets.remove(&id);
            }
        }
    }

    #[cfg(windows)]
    fn handle_indicator_finished(
        &mut self,
        result: Result<dlss_core::ToolChangeResult, worker::WorkerError>,
    ) {
        let change = match result {
            Ok(change) => change,
            Err(error) => {
                self.last_error = Some(error.to_string());
                self.toast = Some(format!("Indicator change failed: {error}"));
                return;
            }
        };
        let was_apply = change.restore_point.is_some();
        self.tool_state = change.state;
        if let Some(point) = change.restore_point {
            self.persisted.tool_restore_points.push(point);
        } else {
            self.persisted.tool_restore_points.pop();
        }
        if let Ok(after) = dlss_platform::windows::NvidiaSystemTools.current_snapshot() {
            self.tool_runtime.observed_hash = Some(dlss_platform::windows::snapshot_hash(&after));
        }
        self.tool_runtime.stale_confirmed = false;
        self.append_activity(dlss_core::ActivityRecord {
            timestamp_unix: dlss_core::now_unix(),
            kind: if was_apply {
                "tool_change"
            } else {
                "tool_restore"
            }
            .into(),
            detail: format!("DLSS indicator: {}", state_label(&self.tool_state)),
        });
        self.toast = Some(format!("DLSS indicator: {}", state_label(&self.tool_state)));
        self.last_error = None;
    }

    fn tools_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Tools);
        egui::Window::new("Global Tools").open(&mut open).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).default_width(520.0).show(ctx, |ui| {
            widgets::banner(ui, theme::WARNING, icons::WARNING, "Global setting — affects all compatible games on this PC", false);
            ui.add_space(8.0); ui.heading("DLSS on-screen indicator");
            ui.separator(); ui.label(format!("Current state: {}", state_label(&self.tool_state)));
            if !self.capabilities.system_tools { ui.weak("Unavailable: Windows NVIDIA registry controls are not supported on this platform."); }
            ui.add_enabled_ui(self.capabilities.system_tools, |ui| {
                ui.radio_value(&mut self.staged_tool_state, SystemToolState::Off, "Off");
                ui.radio_value(&mut self.staged_tool_state, SystemToolState::DlssIndicatorDebug, "Debug DLLs");
                ui.radio_value(&mut self.staged_tool_state, SystemToolState::DlssIndicatorProduction, "Production and debug DLLs");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        self.change_indicator(false);
                    }
                    #[cfg(windows)]
                    let can_restore = !self.persisted.tool_restore_points.is_empty();
                    #[cfg(not(windows))]
                    let can_restore = false;
                    if ui.add_enabled(can_restore, egui::Button::new("Restore Previous")).clicked() {
                        self.change_indicator(true);
                    }
                    if ui.button(widgets::icon_text(icons::INFO, "About this setting")).clicked() {
                        self.open_windows.insert(AppWindow::ToolInfo);
                    }
                });
                if let Some(error) = &self.last_error {
                    selectable_error(ui, error);
                }
            });
        });
        self.set_window_open(AppWindow::Tools, open);
    }

    fn tool_info_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::ToolInfo);
        egui::Window::new("About the DLSS indicator")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label("This changes NVIDIA's machine-wide DLSS indicator registry setting and affects every compatible game on this PC.");
                ui.add_space(6.0);
                ui.label("If another program changes the setting, DLSS Updater requires confirmation before applying or restoring it.");
            });
        self.set_window_open(AppWindow::ToolInfo, open);
    }

    fn store_warnings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::StoreWarnings);
        egui::Window::new("Store discovery warnings")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                for report in self.discovery_reports.iter().filter(|report| {
                    report.games_found == 0
                        && matches!(
                            report.status,
                            dlss_core::DiscoveryStatus::NotDetected
                                | dlss_core::DiscoveryStatus::Error
                        )
                }) {
                    ui.strong(discovery_report_label(report));
                    ui.label(
                        report
                            .detail
                            .as_deref()
                            .unwrap_or("The store was not detected."),
                    );
                    ui.separator();
                }
            });
        self.set_window_open(AppWindow::StoreWarnings, open);
    }

    fn releases_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Releases);
        let mut remove_import = None;
        let mut action = None;
        egui::Window::new("DLL sources")
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .fixed_size([700.0, 650.0])
            .show(ctx, |ui| {
                remove_import = self.render_imports(ui);
                ui.separator();
                ui.heading("Official releases");
                self.render_catalog_status(ui);
                ui.separator();
                if let Some(error) = &self.catalog_error {
                    selectable_error(ui, &format!("Catalog request failed: {error}"));
                    ui.label("Check network access and try Refresh catalog. Previously cached releases remain usable when available.");
                } else if !self.runtime.catalog_loading && self.releases.is_empty() {
                    ui.weak("GitHub returned no matching stable Streamline SDK release archives.");
                }
                let busy = self.inspecting_release.is_some();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for release in &self.releases {
                        action = release_group(
                            ui,
                            release,
                            busy,
                            self.release_errors.get(&release.metadata.id),
                        )
                        .or(action.take());
                    }
                });
            });
        match action {
            Some(ReleaseAction::Inspect(id)) => {
                self.inspecting_release = Some(id.clone());
                let _ = self.worker.commands.send(Command::InspectRelease(id));
            }
            Some(ReleaseAction::Remove(id)) => {
                let _ = self.worker.commands.send(Command::RemoveRelease(id));
            }
            None => {}
        }
        if let Some(hash) = remove_import {
            let _ = self.worker.commands.send(Command::RemoveImport(hash));
        }
        self.set_window_open(AppWindow::Releases, open);
    }

    fn render_imports(&mut self, ui: &mut egui::Ui) -> Option<[u8; 32]> {
        let mut remove = None;
        ui.horizontal(|ui| {
            ui.heading("Imported DLLs");
            if ui
                .button(widgets::icon_text(icons::FOLDER_PLUS, "Import DLL…"))
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Windows DLL", &["dll"])
                    .pick_file()
            {
                let _ = self.worker.commands.send(Command::ImportDll(path));
            }
        });
        ui.weak("x86-64 NVIDIA-signed DLL files only; ZIP is not supported.");
        for record in &self.imports {
            ui.group(|ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.strong(dlss_core::friendly_dll_label(&record.file_name));
                    ui.label(record.version.to_string());
                    ui.weak(&record.signer);
                    ui.weak(format_timestamp(record.imported_unix));
                    if ui.small_button("Remove").clicked() {
                        remove = Some(record.sha256);
                    }
                });
            });
        }
        remove
    }

    fn render_catalog_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.runtime.catalog_loading,
                    egui::Button::new(widgets::icon_text(
                        icons::ARROW_CLOCKWISE,
                        "Refresh catalog",
                    )),
                )
                .clicked()
            {
                let _ = self.worker.commands.send(Command::RefreshCatalog);
            }
            if self.runtime.catalog_loading {
                ui.spinner();
                ui.label("Loading official releases…");
            }
        });
    }

    fn activity_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Activity);
        egui::Window::new("Activity history")
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(560.0)
            .show(ctx, |ui| {
                if self.persisted.activity.is_empty() {
                    ui.weak("No app-initiated changes have been recorded.");
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for record in self.persisted.activity.iter().rev() {
                        ui.horizontal_wrapped(|ui| {
                            ui.monospace(format_timestamp(record.timestamp_unix));
                            ui.strong(&record.kind);
                            ui.label(&record.detail);
                        });
                        ui.separator();
                    }
                });
            });
        self.set_window_open(AppWindow::Activity, open);
    }

    fn roots_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Roots);
        egui::Window::new("Game folders")
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(620.0)
            .show(ctx, |ui| {
                if self.persisted.custom_roots.is_empty() {
                    ui.weak("No manual roots configured.");
                }
                for report in &self.discovery_reports {
                    ui.add(egui::Label::new(discovery_report_label(report)).selectable(true))
                        .on_hover_text(report.detail.as_deref().unwrap_or("No additional detail"));
                }
                ui.separator();
                for root in &self.persisted.custom_roots {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(root.display().to_string()).monospace(),
                        )
                        .selectable(true),
                    );
                }
                ui.add_space(8.0);
                if ui
                    .button(widgets::icon_text(icons::FOLDER_PLUS, "Add game folder…"))
                    .clicked()
                    && let Some(root) = rfd::FileDialog::new().pick_folder()
                {
                    self.add_custom_root(&root);
                }
            });
        self.set_window_open(AppWindow::Roots, open);
    }

    fn add_custom_root(&mut self, root: &std::path::Path) {
        match root.canonicalize() {
            Ok(root) => {
                if !self.persisted.custom_roots.contains(&root) {
                    self.persisted.custom_roots.push(root.clone());
                }
                let _ = self.worker.commands.send(Command::AddRoot(root));
            }
            Err(error) => {
                self.last_error = Some(format!("Could not add {}: {error}", root.display()));
            }
        }
    }

    fn set_window_open(&mut self, window: AppWindow, open: bool) {
        if open {
            self.open_windows.insert(window);
        } else {
            self.open_windows.remove(&window);
        }
    }

    fn append_activity(&mut self, record: dlss_core::ActivityRecord) {
        const MAX_ACTIVITY: usize = 500;
        self.persisted.activity.push(record);
        if self.persisted.activity.len() > MAX_ACTIVITY {
            let excess = self.persisted.activity.len() - MAX_ACTIVITY;
            self.persisted.activity.drain(..excess);
        }
    }

    fn refresh_tool_state(&mut self) {
        #[cfg(windows)]
        match (
            dlss_platform::windows::NvidiaSystemTools.read(&dlss_core::SystemToolId(
                dlss_core::DLSS_INDICATOR_TOOL_ID.into(),
            )),
            dlss_platform::windows::NvidiaSystemTools.current_snapshot(),
        ) {
            (Ok(state), Ok(snapshot)) => {
                self.tool_state = state;
                self.tool_runtime.observed_hash =
                    Some(dlss_platform::windows::snapshot_hash(&snapshot));
                self.tool_runtime.stale_confirmed = false;
                self.last_error = None;
            }
            (Err(error), _) | (_, Err(error)) => self.last_error = Some(error.to_string()),
        }
        #[cfg(not(windows))]
        {
            self.tool_state = SystemToolState::Unavailable(
                "Windows NVIDIA registry controls are unavailable".into(),
            );
        }
    }

    #[cfg(not(windows))]
    fn change_indicator(&mut self, _restore: bool) {
        self.last_error = Some("Registry controls are available only on Windows".into());
    }

    /// Performs the fast stale-hash confirmation on the UI thread, then hands the
    /// slow, blocking elevation to the worker so the window never freezes behind
    /// the UAC prompt. The result arrives via `Event::IndicatorFinished`.
    #[cfg(windows)]
    fn change_indicator(&mut self, restore: bool) {
        self.undo_game = None;
        let provider = dlss_platform::windows::NvidiaSystemTools;
        let current = match provider.current_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return;
            }
        };
        let current_hash = dlss_platform::windows::snapshot_hash(&current);
        if self
            .tool_runtime
            .observed_hash
            .is_some_and(|observed| observed != current_hash)
            && !self.tool_runtime.stale_confirmed
        {
            match provider.read(&dlss_core::SystemToolId(
                dlss_core::DLSS_INDICATOR_TOOL_ID.into(),
            )) {
                Ok(state) => self.tool_state = state,
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return;
                }
            }
            self.tool_runtime.observed_hash = Some(current_hash);
            self.tool_runtime.stale_confirmed = true;
            self.last_error = Some("The registry value changed outside DLSS Updater. Review the new state, then click again to confirm overwriting it.".into());
            return;
        }
        let restore_point = if restore {
            let Some(point) = self.persisted.tool_restore_points.last().cloned() else {
                self.last_error = Some("no restore point is available".into());
                return;
            };
            Some(point)
        } else {
            None
        };
        let request = IndicatorRequest {
            desired: self.staged_tool_state.clone(),
            restore_point,
            expected_current_hash: current_hash,
            allow_stale_restore: restore && self.tool_runtime.stale_confirmed,
        };
        self.last_error = None;
        self.toast = Some(
            if restore {
                "Restoring the previous indicator setting…"
            } else {
                "Applying the indicator change…"
            }
            .into(),
        );
        let _ = self.worker.commands.send(Command::ChangeIndicator(request));
    }

    fn clear_game_profile(&mut self, game_id: &dlss_core::GameId) {
        let ids: std::collections::HashSet<_> = self
            .games
            .iter()
            .find(|game| &game.id == game_id)
            .map(|game| game.details.iter().map(|dll| dll.id.clone()).collect())
            .unwrap_or_default();
        self.persisted
            .target_profile
            .targets
            .retain(|id, _| !ids.contains(id));
    }

    fn refresh_upgrade_counts(&mut self) {
        let latest = self
            .catalog_release
            .as_ref()
            .and_then(|tag| {
                self.releases.iter().find(|release| {
                    release.state == dlss_core::ReleaseState::Ready && &release.metadata.tag == tag
                })
            })
            .map(|release| release.dlls.clone())
            .unwrap_or_default();
        for game in &mut self.games {
            let is_upgrade = |installed: &&dlss_core::DllInstallation| {
                let Some(installed_version) = installed.metadata.version else {
                    return false;
                };
                latest.iter().any(|candidate| {
                    dlss_core::same_file_name(&candidate.file_name, &installed.file_name)
                        && candidate.version > installed_version
                })
            };
            game.upgrades = game.details.iter().filter(is_upgrade).count();
            game.dlss_upgrades = game
                .details
                .iter()
                .filter(|installed| {
                    dlss_core::DllKind::classify(&installed.file_name)
                        .is_some_and(dlss_core::DllKind::is_dlss_family)
                })
                .filter(is_upgrade)
                .count();
        }
    }

    fn refresh_backups(&mut self) {
        #[cfg(windows)]
        match dlss_platform::windows::WindowsKnownDirectories
            .local_app_data()
            .and_then(|base| {
                dlss_core::BackupStore::new(base.join("DLSS Updater/backups")).load_trusted_index(
                    &dlss_platform::windows::WindowsDllInspector,
                    &dlss_platform::windows::WindowsTrustVerifier,
                )
            }) {
            Ok(index) => self.backups = index.records,
            Err(error) => {
                tracing::warn!(%error, "could not refresh backup history");
                self.last_error = Some(format!("Could not refresh backup history: {error}"));
            }
        }
        #[cfg(not(windows))]
        self.backups.clear();
    }

    fn profile_for_game(&self, game_id: &dlss_core::GameId) -> dlss_core::TargetProfile {
        let Some(game) = self.games.iter().find(|game| &game.id == game_id) else {
            return dlss_core::TargetProfile::default();
        };
        let ids: std::collections::HashSet<_> = game.details.iter().map(|dll| &dll.id).collect();
        dlss_core::TargetProfile {
            targets: self
                .persisted
                .target_profile
                .targets
                .iter()
                .filter(|(id, _)| ids.contains(id))
                .map(|(id, target)| (id.clone(), target.clone()))
                .collect(),
        }
    }

    /// The latest official release, only when downloaded and validated.
    fn latest_release(&self) -> Option<&dlss_core::CachedRelease> {
        self.catalog_release.as_ref().and_then(|tag| {
            self.releases.iter().find(|release| {
                release.state == dlss_core::ReleaseState::Ready && &release.metadata.tag == tag
            })
        })
    }

    /// The latest official release in any state, including metadata-only.
    fn latest_release_meta(&self) -> Option<&dlss_core::CachedRelease> {
        self.catalog_release.as_ref().and_then(|tag| {
            self.releases
                .iter()
                .find(|release| &release.metadata.tag == tag)
        })
    }

    fn latest_release_ready(&self) -> bool {
        self.latest_release().is_some()
    }

    fn latest_catalog(&self) -> Vec<dlss_core::CatalogDll> {
        self.latest_release()
            .map(|release| release.dlls.clone())
            .unwrap_or_default()
    }

    /// Bottom bar: bulk actions over checked games in the library, or the
    /// staged-changes ribbon in the game detail view.
    fn bottom_bar(&mut self, root: &mut egui::Ui) {
        match &self.view {
            View::Library => {
                let selected = self.games.iter().filter(|game| game.selected).count();
                if selected == 0 {
                    return;
                }
                egui::Panel::bottom("bulk").show(root, |ui| {
                    self.bulk_bar(ui, selected);
                });
            }
            View::Game(id) => {
                let id = id.clone();
                if self.staged_targets_for(&id) == 0 {
                    return;
                }
                egui::Panel::bottom("staged").show(root, |ui| {
                    self.staged_ribbon(ui, &id);
                });
            }
        }
    }

    fn bulk_bar(&mut self, ui: &mut egui::Ui, selected: usize) {
        ui.horizontal(|ui| {
            ui.strong(format!(
                "{selected} {} selected",
                if selected == 1 { "game" } else { "games" }
            ));
            let ids: Vec<_> = self
                .games
                .iter()
                .filter(|game| game.selected)
                .map(|game| game.id.clone())
                .collect();
            let available = self.catalog_release.is_some() && self.upgrading.is_none();
            if ui
                .add_enabled(
                    available,
                    egui::Button::new(widgets::icon_text(icons::SPARKLE, "Update DLSS")),
                )
                .clicked()
            {
                self.open_review(ReviewIntent::QuickDlss(ids.clone()));
            }
            if ui
                .add_enabled(
                    available,
                    egui::Button::new(widgets::icon_text(icons::STACK, "All DLLs")),
                )
                .clicked()
            {
                self.open_review(ReviewIntent::AllDlls(ids));
            }
            if ui.button("Clear selection").clicked() {
                for game in &mut self.games {
                    game.selected = false;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak("Bulk operations always require review");
            });
        });
    }

    fn preview_profile(
        &self,
        game_id: &dlss_core::GameId,
    ) -> Result<dlss_core::OperationPlan, String> {
        let game = self
            .games
            .iter()
            .find(|game| &game.id == game_id)
            .ok_or_else(|| "game is no longer in the scan".to_owned())?;
        let latest = self.latest_catalog();
        let mut cached: Vec<_> = self
            .releases
            .iter()
            .flat_map(|release| release.dlls.iter().cloned())
            .collect();
        cached.extend(dlss_core::imported_catalog_dlls(&dlss_core::ImportIndex {
            records: self.imports.clone(),
        }));
        dlss_core::plan_target_profile(
            "preview",
            &game.details,
            &latest,
            &cached,
            &self.backups,
            &self.profile_for_game(game_id),
        )
        .map_err(|error| error.to_string())
    }
}

fn canonicalize_roots(roots: &mut Vec<std::path::PathBuf>) {
    let mut normalized = Vec::with_capacity(roots.len());
    for root in roots.drain(..) {
        let root = root.canonicalize().unwrap_or(root);
        if !normalized.contains(&root) {
            normalized.push(root);
        }
    }
    *roots = normalized;
}

fn selectable_error(ui: &mut egui::Ui, error: &str) {
    ui.add(egui::Label::new(egui::RichText::new(error).color(theme::DANGER)).selectable(true));
}

enum ReleaseAction {
    Inspect(dlss_core::ReleaseId),
    Remove(dlss_core::ReleaseId),
}

/// One release card in the "DLL sources" window; returns the action the user
/// requested, if any.
fn release_group(
    ui: &mut egui::Ui,
    release: &dlss_core::CachedRelease,
    busy: bool,
    error: Option<&String>,
) -> Option<ReleaseAction> {
    let mut action = None;
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.set_max_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.strong(&release.metadata.tag);
            ui.label(release_state_label(release));
            if release.metadata.published_unix > 0 {
                ui.weak(format_timestamp(release.metadata.published_unix));
            }
            if release.state != dlss_core::ReleaseState::Ready
                && ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(widgets::icon_text(
                            icons::DOWNLOAD_SIMPLE,
                            "Download and inspect",
                        )),
                    )
                    .clicked()
            {
                action = Some(ReleaseAction::Inspect(release.metadata.id.clone()));
            }
            if release.state == dlss_core::ReleaseState::Ready
                && ui
                    .button(widgets::icon_text(icons::TRASH_SIMPLE, "Remove download"))
                    .clicked()
            {
                action = Some(ReleaseAction::Remove(release.metadata.id.clone()));
            }
            ui.hyperlink_to(
                widgets::icon_text(icons::ARROW_SQUARE_OUT, "View on GitHub"),
                format!(
                    "https://github.com/NVIDIA-RTX/Streamline/releases/tag/{}",
                    release.metadata.tag
                ),
            );
        });
        if let Some(error) = error {
            ui.add(
                egui::Label::new(egui::RichText::new(error).color(theme::DANGER)).selectable(true),
            );
            if ui
                .button(widgets::icon_text(icons::ARROW_CLOCKWISE, "Retry"))
                .clicked()
            {
                action = Some(ReleaseAction::Inspect(release.metadata.id.clone()));
            }
        }
        if release.state == dlss_core::ReleaseState::Ready
            && release.validation == dlss_core::ReleaseValidation::RevocationUnavailableFallback
        {
            widgets::banner(
                ui,
                theme::WARNING,
                icons::WARNING,
                "Windows could not reach revocation services. Signatures and the NVIDIA publisher were verified without the online revocation result.",
                false,
            );
        }
        ui.collapsing(format!("{} DLLs", release.dlls.len()), |ui| {
            for dll in &release.dlls {
                ui.horizontal_wrapped(|ui| {
                    ui.small(format!(
                        "{}  {}",
                        dll.file_name.to_string_lossy(),
                        dll.version
                    ));
                });
            }
        });
    });
    action
}

fn release_state_label(release: &dlss_core::CachedRelease) -> String {
    match release.state {
        dlss_core::ReleaseState::MetadataOnly => "Not downloaded".into(),
        dlss_core::ReleaseState::Downloading => "Downloading…".into(),
        dlss_core::ReleaseState::Downloaded | dlss_core::ReleaseState::Validating => {
            "Validating…".into()
        }
        dlss_core::ReleaseState::Ready => {
            format!("Downloaded ({} DLLs)", release.dlls.len())
        }
        dlss_core::ReleaseState::Invalid => "Failed".into(),
    }
}

impl eframe::App for DlssApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_worker_events(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.persisted);
    }
    #[expect(
        clippy::too_many_lines,
        reason = "the eframe root method declaratively composes independent panels and overlays"
    )]
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.persisted.disclaimer_acknowledged {
            root.disable();
        }
        egui::Panel::top("toolbar").show(root, |ui| {
            ui.add_space(5.0);
            self.toolbar(ui);
            ui.add_space(5.0);
        });
        self.bottom_bar(root);
        egui::CentralPanel::default().show(root, |ui| {
            if let Some(error) = self.last_error.clone()
                && widgets::banner(ui, theme::DANGER, icons::WARNING_CIRCLE, &error, true)
            {
                self.last_error = None;
            }
            match &self.view {
                View::Library => self.library_view(ui),
                View::Game(id) => {
                    let id = id.clone();
                    self.game_detail_view(ui, &id);
                }
            }
        });
        if self.open_windows.contains(&AppWindow::Tools) {
            self.tools_window(root.ctx());
        }
        if self.open_windows.contains(&AppWindow::ToolInfo) {
            self.tool_info_window(root.ctx());
        }
        if self.open_windows.contains(&AppWindow::StoreWarnings) {
            self.store_warnings_window(root.ctx());
        }
        if self.open_windows.contains(&AppWindow::Releases) {
            self.releases_window(root.ctx());
        }
        if self.open_windows.contains(&AppWindow::Activity) {
            self.activity_window(root.ctx());
        }
        if self.open_windows.contains(&AppWindow::Roots) {
            self.roots_window(root.ctx());
        }
        if self.review.is_some() {
            self.review_window(root.ctx());
        }
        if self.toast != self.toast_identity {
            self.toast_identity = self.toast.clone();
            self.toast_started = self.toast.as_ref().map(|_| Instant::now());
        }
        if self
            .toast_started
            .is_some_and(|started| started.elapsed() >= Duration::from_secs(10))
        {
            self.toast = None;
            self.toast_identity = None;
            self.toast_started = None;
        }
        if self.toast.is_some() || self.undo_game.is_some() {
            let mut undo_requested = None;
            egui::Area::new("operation_toast".into())
                .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -48.0])
                .show(root.ctx(), |ui| {
                    egui::Frame::new()
                        .fill(theme::BG_CARD)
                        .stroke(egui::Stroke::new(1.0, theme::STROKE))
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(10))
                        .shadow(ui.style().visuals.popup_shadow)
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.horizontal(|ui| {
                                ui.label(widgets::icon(icons::INFO, 15.0, theme::ACCENT));
                                if let Some(message) = &self.toast {
                                    ui.add(egui::Label::new(message).wrap());
                                } else {
                                    ui.label("The last DLL change can still be undone.");
                                }
                                if let Some(game_id) = &self.undo_game {
                                    let undo = widgets::primary_icon_button(
                                        icons::ARROW_U_UP_LEFT,
                                        "Undo",
                                    );
                                    if ui.add(undo).clicked() {
                                        undo_requested = Some(game_id.clone());
                                    }
                                }
                            });
                        });
                });
            if self.toast.is_some() {
                root.ctx().request_repaint_after(Duration::from_secs(1));
            }
            if let Some(game_id) = undo_requested {
                self.undo_game = None;
                self.toast = Some("Restoring backed-up DLLs…".into());
                let _ = self.worker.commands.send(Command::UndoLast(game_id));
            }
        }
        if !self.persisted.disclaimer_acknowledged {
            egui::Window::new("Before you continue")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(root.ctx(), |ui| {
                    ui.set_max_width(440.0);
                    widgets::banner(
                        ui,
                        theme::WARNING,
                        icons::WARNING,
                        "Online/anti-cheat games may detect DLL swaps and ban you.",
                        false,
                    );
                    widgets::banner(
                        ui,
                        theme::WARNING,
                        icons::WARNING,
                        "Replacing Streamline DLLs may reduce performance or crash.",
                        false,
                    );
                    ui.add_space(8.0);
                    let acknowledge = widgets::primary_button("I understand");
                    if ui.add(acknowledge).clicked() {
                        self.persisted.disclaimer_acknowledged = true;
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::inspector::{comparison_label, desired_label, signature_label};

    fn installation(name: &str, version: dlss_core::DllVersion) -> dlss_core::DllInstallation {
        dlss_core::DllInstallation {
            id: dlss_core::DllInstallationId(name.into()),
            game_id: dlss_core::GameId("game".into()),
            path: std::path::PathBuf::from(name),
            file_name: name.into(),
            metadata: dlss_core::DllMetadata {
                version: Some(version),
                sha256: [0; 32],
                signature: dlss_core::SignatureStatus::Trusted,
                x86_64: true,
            },
        }
    }

    #[test]
    fn tool_and_desired_labels_preserve_custom_meaning() {
        assert_eq!(
            state_label(&SystemToolState::DlssIndicatorProduction),
            "Production + debug"
        );
        assert_eq!(
            state_label(&SystemToolState::CustomDword(77)),
            "Custom value (77)"
        );
        assert_eq!(
            desired_label(&dlss_core::DesiredDll::Cached {
                release: dlss_core::ReleaseId("v2".into()),
                sha256: [0; 32],
            }),
            "Cached v2"
        );
        assert_eq!(
            desired_label(&dlss_core::DesiredDll::Cached {
                release: dlss_core::ReleaseId(format!("import:{}", "0".repeat(64))),
                sha256: [0; 32],
            }),
            "Imported"
        );
        assert_eq!(
            signature_label(dlss_core::SignatureStatus::Trusted),
            "Signed (trusted)"
        );
        assert_eq!(
            comparison_label(dlss_core::Comparison::Upgrade),
            "Update available"
        );
    }

    #[test]
    fn game_row_uses_only_the_highest_super_resolution_version() {
        let older = dlss_core::DllVersion::new(2, 5, 0, 0);
        let newer = dlss_core::DllVersion::new(3, 7, 10, 0);
        let game = dlss_core::GameInstall {
            id: dlss_core::GameId("game".into()),
            name: "Game".into(),
            store: dlss_core::StoreKind::Manual,
            root: ".".into(),
            dlls: vec![
                installation("nvngx_dlss.dll", older),
                installation("NVNGX_DLSS.DLL", newer),
                installation("nvngx_dlssg.dll", dlss_core::DllVersion::new(9, 0, 0, 0)),
            ],
            inspection_errors: 0,
        };
        assert_eq!(GameRow::from_install(game).dlss_version, Some(newer));
    }

    #[test]
    fn persisted_disclaimer_defaults_to_unacknowledged() {
        assert!(!PersistedState::default().disclaimer_acknowledged);
        let old_state: PersistedState = serde_json::from_str("{}").unwrap();
        assert!(!old_state.disclaimer_acknowledged);
    }
}
