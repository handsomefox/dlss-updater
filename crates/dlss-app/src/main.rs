#![cfg_attr(windows, windows_subsystem = "windows")]

mod diagnostics;
mod state;
mod ui;
mod worker;

#[cfg(windows)]
use dlss_core::SystemToolProvider;
use dlss_core::{PlatformCapabilities, SystemToolState};
use eframe::egui;
use state::PersistedState;
use ui::theme::{self, icons};
use ui::toast::{Batch, BatchKind, Toast, ToastKind};
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

#[cfg_attr(
    windows,
    expect(
        clippy::exit,
        reason = "the privileged helper must report malformed or rejected plans through its process status"
    )
)]
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
    /// A problem with the DLSS indicator, shown in the Tools dialog.
    tool_error: Option<String>,
    catalog_release: Option<String>,
    catalog_error: Option<String>,
    releases: Vec<dlss_core::CachedRelease>,
    release_errors: std::collections::HashMap<dlss_core::ReleaseId, String>,
    release_progress: Option<(dlss_core::ReleaseId, u64, Option<u64>)>,
    imports: Vec<dlss_core::ImportedDllRecord>,
    backups: Vec<dlss_core::BackupRecord>,
    backup_warning: Option<String>,
    backups_loading: bool,
    inspecting_release: Option<dlss_core::ReleaseId>,
    /// The game the worker is changing right now.
    upgrading: Option<dlss_core::GameId>,
    toast: Option<Toast>,
    /// The update or undo the user started, counted until every game in it
    /// reports back, so a bulk run ends in one summary instead of a notice
    /// per game.
    batch: Option<Batch>,
    /// Games whose last change the worker can still undo.
    undoable: std::collections::HashSet<dlss_core::GameId>,
    /// The undoable games from the most recent finished update, which the
    /// footer's Undo button reverts together.
    last_change: Vec<dlss_core::GameId>,
    /// The game last clicked in the library, where a shift-click range
    /// starts. An id rather than a row, because sorting moves rows.
    selection_anchor: Option<dlss_core::GameId>,
    review: Option<ui::review::ReviewState>,
    /// Games with a queued or running update or undo. Updates map to the DLL
    /// installation ids that were sent, so exactly those staged targets can
    /// be cleared when the operation finishes; undos map to an empty list.
    profiles_applying:
        std::collections::HashMap<dlss_core::GameId, Vec<dlss_core::DllInstallationId>>,
    #[cfg(windows)]
    tool_runtime: WindowsToolRuntime,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum AppWindow {
    Tools,
    Releases,
    Activity,
    Roots,
    About,
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

/// The library tabs. Games without NVIDIA DLLs are most of a typical
/// library and there is nothing to do for them, so the default tab hides them.
#[derive(Clone, Copy, Debug, PartialEq)]
enum GameFilter {
    Updates,
    WithDlls,
    All,
    Staged,
    Problems,
    Recent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SortKey {
    Name,
    Store,
    Dlls,
    DlssVersion,
    Status,
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
    /// A DLL in this game has no readable version.
    has_unknown: bool,
    /// What the last update or undo did to this game, this session.
    last_operation: Option<String>,
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
            has_unknown,
            last_operation: None,
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
        let worker = Worker::start(persisted.custom_roots.clone(), cc.egui_ctx.clone());
        let app = Self {
            persisted,
            games: Vec::new(),
            discovery_reports: Vec::new(),
            filter: String::new(),
            filter_mode: GameFilter::WithDlls,
            game_sort: GameSort {
                key: SortKey::Status,
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
                // The scan is queued below; showing "scanning" from the first
                // frame keeps the empty "no games" state from flashing first.
                scanning: true,
                catalog_loading: true,
                worker_connected: true,
            },
            last_error: None,
            tool_error: None,
            catalog_release: None,
            catalog_error: None,
            releases: Vec::new(),
            release_errors: std::collections::HashMap::new(),
            release_progress: None,
            imports: Vec::new(),
            backups: Vec::new(),
            backup_warning: None,
            backups_loading: true,
            inspecting_release: None,
            upgrading: None,
            toast: None,
            batch: None,
            undoable: std::collections::HashSet::new(),
            last_change: Vec::new(),
            selection_anchor: None,
            review: None,
            profiles_applying: std::collections::HashMap::new(),
            #[cfg(windows)]
            tool_runtime: WindowsToolRuntime::default(),
        };
        let _ = app.worker.commands.send(Command::Scan);
        let _ = app.worker.commands.send(Command::RefreshBackups);
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
                        self.batch = None;
                        self.toast = None;
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
            Event::BackupsStarted => self.backups_loading = true,
            Event::BackupsFinished(result) => self.handle_backups_finished(result),
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
            Event::UpgradeStarted(game_id) => self.handle_upgrade_started(game_id),
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
        self.undoable.clear();
        self.last_change.clear();
        self.last_error = None;
    }

    fn handle_backups_finished(
        &mut self,
        result: Result<dlss_core::BackupLoadReport, worker::WorkerError>,
    ) {
        self.backups_loading = false;
        match result {
            Ok(report) => {
                self.backup_warning = backup_warning_message(&report);
                self.backups = report.usable;
            }
            Err(error) => {
                self.backups.clear();
                self.backup_warning = Some(format!("Could not load backup history: {error}"));
            }
        }
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
        let previous = std::mem::take(&mut self.games);
        self.games = carry_over_rows(
            previous,
            outcome
                .games
                .into_iter()
                .map(GameRow::from_install)
                .collect(),
        );
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
                self.notify(
                    ToastKind::Success,
                    format!(
                        "{} is downloaded and verified: {} DLLs",
                        release.metadata.tag,
                        release.dlls.len()
                    ),
                );
                self.refresh_upgrade_counts();
            }
            Err(error) => {
                let message = error.to_string();
                if let Some(id) = requested_id {
                    self.release_errors.insert(id, message.clone());
                }
                self.toast = None;
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
                self.notify(ToastKind::Success, "Removed the downloaded release");
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
                let message = format!(
                    "Imported {} {}",
                    dlss_core::friendly_dll_label(&record.file_name),
                    record.version
                );
                if let Some(existing) = self
                    .imports
                    .iter_mut()
                    .find(|existing| existing.sha256 == record.sha256)
                {
                    existing.clone_from(&record);
                } else {
                    self.imports.push(record);
                }
                self.notify(ToastKind::Success, message);
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
        self.notify(ToastKind::Progress, progress_label(state, received, total));
    }

    fn handle_upgrade_started(&mut self, game_id: dlss_core::GameId) {
        self.undoable.remove(&game_id);
        let name = self.game_name(&game_id);
        let message = match &self.batch {
            Some(batch) if batch.total > 1 => format!(
                "{} {name} ({} of {})…",
                batch.kind.verb_ing(),
                batch.done + 1,
                batch.total
            ),
            Some(batch) => format!("{} {name}…", batch.kind.verb_ing()),
            None => format!("Updating {name}…"),
        };
        self.upgrading = Some(game_id);
        self.notify(ToastKind::Progress, message);
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
            let previous = std::mem::replace(&mut self.games[index], GameRow::from_install(game));
            self.games[index].selected = previous.selected;
            self.games[index].last_operation = previous.last_operation;
        }
        match result {
            Ok(report) => self.handle_upgrade_report(game_id, applying_profile, report),
            Err(error) => {
                let name = self.game_name(game_id);
                self.last_error = Some(format!("{name}: {error}"));
                if let Some(batch) = &mut self.batch {
                    batch.failed_games += 1;
                }
            }
        }
        if let Some(batch) = &mut self.batch {
            batch.done += 1;
        }
        self.refresh_upgrade_counts();
        self.finish_batch_if_done();
        self.refresh_backups();
    }

    fn handle_upgrade_report(
        &mut self,
        game_id: &dlss_core::GameId,
        applied_targets: Option<Vec<dlss_core::DllInstallationId>>,
        report: worker::UpgradeReport,
    ) {
        let undo = report.release == "Undo";
        let name = self.game_name(game_id);
        let summary = ui::toast::operation_summary(undo, report.changed, report.failed);
        if let Some(warning) = report.warning {
            self.last_error = Some(format!("{name}: {warning}"));
        }
        if let Some(row) = self.games.iter_mut().find(|row| &row.id == game_id) {
            row.last_operation = Some(format!(
                "{summary} at {}",
                ui::windows::format_time(dlss_core::now_unix())
            ));
        }
        if report.can_undo {
            self.undoable.insert(game_id.clone());
        }
        if let Some(batch) = &mut self.batch {
            batch.changed += report.changed;
            batch.failed += report.failed;
            if report.changed > 0 {
                batch.games_changed += 1;
            }
            if report.can_undo {
                batch.undoable.push(game_id.clone());
            }
        }
        let source = if undo {
            String::new()
        } else {
            format!(" from {}", report.release)
        };
        self.append_activity(dlss_core::ActivityRecord {
            timestamp_unix: dlss_core::now_unix(),
            kind: if undo { "restore" } else { "dll_swap" }.into(),
            detail: format!("{name}: {}{source}", ui::toast::lower_first(&summary)),
        });
        // Clear exactly the staged targets that were sent; targets the user
        // left unchecked in the review stay staged.
        if let Some(ids) = applied_targets {
            for id in ids {
                self.persisted.target_profile.targets.remove(&id);
            }
        }
    }

    /// Starts counting a run of updates or undos, one per game, so progress
    /// and the final summary cover the whole run.
    fn start_batch(&mut self, kind: BatchKind, games: &[dlss_core::GameId]) {
        let single_name = (games.len() == 1).then(|| self.game_name(&games[0]));
        self.last_change.clear();
        self.batch = Some(Batch::new(kind, games.len(), single_name));
        self.notify(
            ToastKind::Progress,
            match games.len() {
                1 => format!("{} {}…", kind.verb_ing(), self.game_name(&games[0])),
                count => format!("{} {count} games…", kind.verb_ing()),
            },
        );
    }

    fn finish_batch_if_done(&mut self) {
        if self
            .batch
            .as_ref()
            .is_some_and(|batch| batch.done < batch.total)
        {
            return;
        }
        let Some(batch) = self.batch.take() else {
            self.toast = None;
            return;
        };
        let kind = if batch.failed == 0 && batch.failed_games == 0 {
            ToastKind::Success
        } else {
            ToastKind::Info
        };
        self.notify(kind, batch.summary());
        if let Some(toast) = &mut self.toast {
            toast.offers_undo = !batch.undoable.is_empty();
        }
        self.last_change = batch.undoable;
    }

    /// Reverts the last change in each game, through the worker's undo plans.
    fn start_undo(&mut self, games: Vec<dlss_core::GameId>) {
        let games: Vec<_> = games
            .into_iter()
            .filter(|id| self.undoable.contains(id) && !self.profiles_applying.contains_key(id))
            .collect();
        if games.is_empty() {
            return;
        }
        self.start_batch(BatchKind::Undo, &games);
        for game_id in games {
            self.profiles_applying.insert(game_id.clone(), Vec::new());
            let _ = self.worker.commands.send(Command::UndoLast(game_id));
        }
    }

    fn game_name(&self, game_id: &dlss_core::GameId) -> String {
        self.games
            .iter()
            .find(|game| &game.id == game_id)
            .map_or_else(|| "the game".into(), |game| game.name.clone())
    }

    /// True while any update or undo is queued or running. The worker runs
    /// them one at a time, so between two games of a bulk run `upgrading` is
    /// briefly empty; checking the queue too keeps buttons from flickering.
    fn busy(&self) -> bool {
        self.upgrading.is_some() || !self.profiles_applying.is_empty()
    }

    #[cfg(windows)]
    fn handle_indicator_finished(
        &mut self,
        result: Result<dlss_core::ToolChangeResult, worker::WorkerError>,
    ) {
        let change = match result {
            Ok(change) => change,
            Err(error) => {
                let message = format!("Indicator change failed: {error}");
                // The result can land after the dialog was closed; then the
                // app-wide banner is the only place left to report it.
                if !self.open_windows.contains(&AppWindow::Tools) {
                    self.last_error = Some(message.clone());
                }
                self.tool_error = Some(message);
                self.toast = None;
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
        self.notify(
            ToastKind::Success,
            format!("DLSS indicator: {}", state_label(&self.tool_state)),
        );
        self.tool_error = None;
    }

    fn tools_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Tools);
        let mut apply = false;
        let mut restore = false;
        widgets::modal(
            ctx,
            widgets::dialog("tools", icons::WRENCH, "Tools", 520.0),
            &mut open,
            |ui| {
                widgets::section_title(ui, "DLSS on-screen indicator", None);
                ui.label(
                    egui::RichText::new(
                        "Shows the DLSS version and mode in a corner of the screen while a \
                         game runs, to confirm which DLL it loaded. This is NVIDIA's \
                         machine-wide registry setting, so it applies to every game on this PC.",
                    )
                    .color(theme::TEXT_MUTED),
                );
                ui.add_space(8.0);
                if !self.capabilities.system_tools {
                    widgets::status_text(
                        ui,
                        icons::INFO,
                        "Only available in the Windows build.",
                        theme::TEXT_MUTED,
                    );
                    return;
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Now:").color(theme::TEXT_MUTED));
                    ui.strong(state_label(&self.tool_state));
                });
                ui.add_space(4.0);
                ui.radio_value(&mut self.staged_tool_state, SystemToolState::Off, "Off");
                ui.radio_value(
                    &mut self.staged_tool_state,
                    SystemToolState::DlssIndicatorDebug,
                    "Only with debug DLLs",
                );
                ui.radio_value(
                    &mut self.staged_tool_state,
                    SystemToolState::DlssIndicatorProduction,
                    "Always, with any DLSS DLL",
                );
                if let Some(error) = &self.tool_error {
                    ui.add_space(6.0);
                    widgets::banner(ui, theme::WARNING, icons::WARNING, error, false);
                }
                ui.add_space(10.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let changed = self.staged_tool_state != self.tool_state;
                    apply = ui
                        .add_enabled(changed, widgets::primary_button("Apply"))
                        .on_disabled_hover_text("Choose a different setting to apply")
                        .clicked();
                    #[cfg(windows)]
                    let can_restore = !self.persisted.tool_restore_points.is_empty();
                    #[cfg(not(windows))]
                    let can_restore = false;
                    restore = ui
                        .add_enabled(can_restore, egui::Button::new("Restore previous"))
                        .on_hover_text("Put back the setting from before the last change")
                        .on_disabled_hover_text("The app has not changed this setting yet")
                        .clicked();
                });
            },
        );
        if apply {
            self.change_indicator(false);
        }
        if restore {
            self.change_indicator(true);
        }
        self.set_window_open(AppWindow::Tools, open);
    }

    fn activity_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Activity);
        widgets::modal(
            ctx,
            widgets::dialog(
                "activity",
                icons::CLOCK_COUNTER_CLOCKWISE,
                "Activity",
                640.0,
            ),
            &mut open,
            |ui| {
                if self.persisted.activity.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "Nothing yet. Every DLL update, restore, and indicator change \
                             the app makes is listed here.",
                        )
                        .color(theme::TEXT_MUTED),
                    );
                    return;
                }
                egui::Grid::new("activity_grid")
                    .num_columns(3)
                    .spacing([14.0, 8.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for record in self.persisted.activity.iter().rev() {
                            ui.label(
                                egui::RichText::new(format_timestamp(record.timestamp_unix))
                                    .monospace()
                                    .color(theme::TEXT_MUTED),
                            );
                            let (icon, label) = activity_kind(&record.kind);
                            ui.label(widgets::icon_text(icon, label));
                            ui.add(egui::Label::new(&record.detail).wrap());
                            ui.end_row();
                        }
                    });
            },
        );
        self.set_window_open(AppWindow::Activity, open);
    }

    fn roots_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Roots);
        let mut remove = None;
        widgets::modal(
            ctx,
            widgets::dialog("roots", icons::FOLDER_SIMPLE, "Game folders", 640.0),
            &mut open,
            |ui| {
                ui.strong("Stores");
                ui.weak("Steam, Epic Games, and GOG games are found automatically.");
                ui.add_space(4.0);
                for report in &self.discovery_reports {
                    ui::library::discovery_report_row(ui, report);
                }
                if self.runtime.scanning {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("Scanning…");
                    });
                }
                ui.add_space(10.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong("Your folders");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(widgets::icon_text(icons::FOLDER_PLUS, "Add folder…"))
                            .clicked()
                            && let Some(root) = rfd::FileDialog::new().pick_folder()
                        {
                            self.add_custom_root(&root);
                        }
                    });
                });
                ui.weak(
                    "Add a game's install folder when no store reports it. \
                     Removing a folder only stops the app from scanning it.",
                );
                ui.add_space(4.0);
                if self.persisted.custom_roots.is_empty() {
                    ui.label(egui::RichText::new("No folders added.").color(theme::TEXT_FAINT));
                }
                if !self.persisted.custom_roots.is_empty() {
                    widgets::card(ui, |ui| {
                        for (position, root) in self.persisted.custom_roots.iter().enumerate() {
                            if position > 0 {
                                ui.separator();
                            }
                            if folder_row(ui, root) {
                                remove = Some(root.clone());
                            }
                        }
                    });
                }
            },
        );
        if let Some(root) = remove {
            self.remove_custom_root(&root);
        }
        self.set_window_open(AppWindow::Roots, open);
    }

    /// Keyboard shortcuts.
    ///
    /// Skipped entirely while a dialog is up, so Esc closes the dialog rather
    /// than navigating behind it, and so typing in a dialog cannot trigger a
    /// rescan. Ctrl+A and Esc also stand aside while a text field has focus,
    /// so they keep selecting and unfocusing text there.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if !self.open_windows.is_empty()
            || self.review.is_some()
            || !self.persisted.disclaimer_acknowledged
        {
            return;
        }
        let typing = ctx.egui_wants_keyboard_input();
        let (focus_search, rescan, escape, select_all) = ctx.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::COMMAND, egui::Key::F),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F5),
                !typing && input.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                !typing && input.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
            )
        });
        if focus_search {
            if matches!(self.view, View::Game(_)) {
                self.view = View::Library;
            }
            ctx.memory_mut(|memory| memory.request_focus(ui::toolbar::search_field_id()));
        }
        if rescan && !self.runtime.scanning {
            let _ = self.worker.commands.send(Command::Scan);
        }
        match self.view {
            View::Game(_) if escape => self.view = View::Library,
            View::Library if escape => self.set_selection(false, None),
            View::Library if select_all => {
                let rows = self.filtered_game_rows();
                self.set_selection(true, Some(&rows));
            }
            _ => {}
        }
    }

    /// Selects or clears the given library rows, or every game when `rows`
    /// is `None`. Games without DLLs have nothing to update, so they are
    /// never selected.
    pub(crate) fn set_selection(&mut self, selected: bool, rows: Option<&[usize]>) {
        match rows {
            Some(rows) => {
                for &index in rows {
                    let game = &mut self.games[index];
                    game.selected = selected && game.dlls > 0;
                }
            }
            None => {
                for game in &mut self.games {
                    game.selected = selected && game.dlls > 0;
                }
            }
        }
    }

    /// Version, project link, and one-click access to the folders a user needs
    /// when reporting a problem or inspecting what the app has stored.
    fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::About);
        widgets::modal(
            ctx,
            widgets::dialog("about", icons::INFO, "About DLSS Updater", 520.0),
            &mut open,
            |ui| {
                ui.label(
                    egui::RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION")))
                        .color(theme::TEXT_MUTED),
                );
                ui.add_space(6.0);
                ui.label(
                    "Replaces official NVIDIA DLSS and Streamline DLLs in installed games. \
                     Every change is planned against the installed file's hash, backed up, \
                     and verified again after it is written.",
                );
                ui.add_space(6.0);
                ui.hyperlink_to(
                    widgets::icon_text(icons::ARROW_SQUARE_OUT, "Project page"),
                    env!("CARGO_PKG_REPOSITORY"),
                );
                ui.add_space(10.0);
                ui.separator();
                widgets::section_title(ui, "Folders", None);
                for (label, hint, directory) in [
                    (
                        "Open log folder",
                        "Diagnostics logs to attach when reporting a problem",
                        diagnostics::log_directory(),
                    ),
                    (
                        "Open data folder",
                        "Backups, the validated release cache, and imported DLLs",
                        diagnostics::data_directory(),
                    ),
                ] {
                    let Some(directory) = directory else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        if ui
                            .button(widgets::icon_text(icons::FOLDER_SIMPLE, label))
                            .on_hover_text(directory.display().to_string())
                            .clicked()
                        {
                            diagnostics::reveal(&directory);
                        }
                        ui.weak(hint);
                    });
                }
                ui.add_space(10.0);
                ui.separator();
                ui.weak(
                    "Bundled fonts keep their own licenses: Inter and JetBrains Mono \
                     (SIL Open Font License 1.1) · Phosphor Icons (MIT).",
                );
            },
        );
        self.set_window_open(AppWindow::About, open);
    }

    /// Stops scanning a folder the user added. Matches the stored path
    /// exactly rather than canonicalizing it again, because a folder that was
    /// deleted from disk can no longer be canonicalized, and that is the
    /// folder a user most wants to remove.
    fn remove_custom_root(&mut self, root: &std::path::Path) {
        self.persisted
            .custom_roots
            .retain(|existing| existing != root);
        let _ = self
            .worker
            .commands
            .send(Command::RemoveRoot(root.to_path_buf()));
    }

    pub(crate) fn add_custom_root(&mut self, root: &std::path::Path) {
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
                self.tool_error = None;
            }
            (Err(error), _) | (_, Err(error)) => self.tool_error = Some(error.to_string()),
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
        self.tool_error = Some("Registry controls are available only on Windows".into());
    }

    /// Performs the fast stale-hash confirmation on the UI thread, then hands the
    /// slow, blocking elevation to the worker so the window never freezes behind
    /// the UAC prompt. The result arrives via `Event::IndicatorFinished`.
    #[cfg(windows)]
    fn change_indicator(&mut self, restore: bool) {
        let provider = dlss_platform::windows::NvidiaSystemTools;
        let current = match provider.current_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.tool_error = Some(error.to_string());
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
                    self.tool_error = Some(error.to_string());
                    return;
                }
            }
            self.tool_runtime.observed_hash = Some(current_hash);
            self.tool_runtime.stale_confirmed = true;
            self.tool_error = Some("The registry value changed outside DLSS Updater. Review the new state, then click again to confirm overwriting it.".into());
            return;
        }
        let restore_point = if restore {
            let Some(point) = self.persisted.tool_restore_points.last().cloned() else {
                self.tool_error = Some("no restore point is available".into());
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
        self.tool_error = None;
        self.notify(
            ToastKind::Progress,
            if restore {
                "Restoring the previous indicator setting…"
            } else {
                "Changing the DLSS indicator…"
            },
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
        self.backups_loading = true;
        if self.worker.commands.send(Command::RefreshBackups).is_err() {
            self.backups_loading = false;
            self.backup_warning = Some("Background worker stopped unexpectedly".into());
        }
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

fn backup_warning_message(report: &dlss_core::BackupLoadReport) -> Option<String> {
    let mut warnings = Vec::new();
    if !report.rejected.is_empty() {
        let details = report
            .rejected
            .iter()
            .take(3)
            .map(|rejected| {
                format!(
                    "{}: {}",
                    rejected.record.original_path.display(),
                    rejected.reason
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        warnings.push(format!(
            "{} backup{} hidden because verification failed ({details})",
            report.rejected.len(),
            if report.rejected.len() == 1 { "" } else { "s" }
        ));
    }
    if report.offline_revocation_fallbacks > 0 {
        warnings.push(format!(
            "{} NVIDIA-signed backup{} available with offline revocation validation; retry when online to complete the check",
            report.offline_revocation_fallbacks,
            if report.offline_revocation_fallbacks == 1 {
                " is"
            } else {
                "s are"
            }
        ));
    }
    (!warnings.is_empty()).then(|| warnings.join(". "))
}

impl eframe::App for DlssApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_worker_events(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.persisted);
    }
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.persisted.disclaimer_acknowledged {
            root.disable();
        }
        self.handle_shortcuts(root.ctx());
        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_PANEL)
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(root, |ui| self.toolbar(ui));
        self.footer(root);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_APP)
                    .inner_margin(egui::Margin::symmetric(14, 12)),
            )
            .show(root, |ui| {
                self.banners(ui);
                match &self.view {
                    View::Library => self.library_view(ui),
                    View::Game(id) => {
                        let id = id.clone();
                        self.game_detail_view(ui, &id);
                    }
                }
            });
        self.dialogs(root.ctx());
        self.show_toast(root.ctx());
        if !self.persisted.disclaimer_acknowledged {
            self.disclaimer(root.ctx());
        }
    }
}

impl DlssApp {
    /// App-wide warnings above the current view. The error banner stays
    /// until dismissed, so a failure is never only a notice that fades.
    fn banners(&mut self, ui: &mut egui::Ui) {
        if let Some(warning) = self.backup_warning.clone() {
            let label = if self.backups_loading {
                "Checking…"
            } else {
                "Check again"
            };
            if widgets::action_banner(
                ui,
                theme::WARNING,
                icons::WARNING,
                &warning,
                label,
                !self.backups_loading,
            ) {
                self.refresh_backups();
            }
            ui.add_space(8.0);
        }
        if let Some(error) = self.last_error.clone() {
            if widgets::banner(ui, theme::DANGER, icons::WARNING_CIRCLE, &error, true) {
                self.last_error = None;
            }
            ui.add_space(8.0);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.open_windows.contains(&AppWindow::Tools) {
            self.tools_window(ctx);
        }
        if self.open_windows.contains(&AppWindow::Releases) {
            self.releases_window(ctx);
        }
        if self.open_windows.contains(&AppWindow::Activity) {
            self.activity_window(ctx);
        }
        if self.open_windows.contains(&AppWindow::Roots) {
            self.roots_window(ctx);
        }
        if self.open_windows.contains(&AppWindow::About) {
            self.about_window(ctx);
        }
        if self.review.is_some() {
            self.review_window(ctx);
        }
    }

    fn disclaimer(&mut self, ctx: &egui::Context) {
        // Deliberately not dismissible: unlike every other dialog, this one
        // ignores Esc and backdrop clicks, because continuing has to be an
        // explicit acknowledgement.
        egui::Modal::new(egui::Id::new("disclaimer")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.heading("Before you continue");
            ui.add_space(8.0);
            ui.label(
                "DLSS Updater replaces NVIDIA DLLs inside your game folders. \
                 It backs up every file it replaces, so you can undo a change.",
            );
            ui.add_space(8.0);
            widgets::banner(
                ui,
                theme::WARNING,
                icons::WARNING,
                "Games with anti-cheat, online games especially, may treat a replaced DLL \
                 as tampering and ban your account.",
                false,
            );
            ui.add_space(4.0);
            widgets::banner(
                ui,
                theme::WARNING,
                icons::WARNING,
                "A newer DLL is not always better. Some games run worse or crash with \
                 Streamline DLLs they did not ship with.",
                false,
            );
            ui.add_space(10.0);
            if ui.add(widgets::primary_button("I understand")).clicked() {
                self.persisted.disclaimer_acknowledged = true;
            }
        });
    }
}

/// Keeps what the user did to each game across a rescan: which games were
/// selected, and what the last update did. A rescan only re-reads files, so
/// neither should reset.
fn carry_over_rows(previous: Vec<GameRow>, mut fresh: Vec<GameRow>) -> Vec<GameRow> {
    let mut previous: std::collections::HashMap<_, _> = previous
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    for row in &mut fresh {
        if let Some(old) = previous.remove(&row.id) {
            row.selected = old.selected && row.dlls > 0;
            row.last_operation = old.last_operation;
        }
    }
    fresh
}

/// One folder the user added. Returns true when Remove was clicked.
fn folder_row(ui: &mut egui::Ui, root: &std::path::Path) -> bool {
    let mut remove = false;
    ui.horizontal(|ui| {
        let missing = !root.is_dir();
        let path = root.display().to_string();
        ui.scope(|ui| {
            ui.set_max_width((ui.available_width() - 200.0).max(120.0));
            ui.add(egui::Label::new(egui::RichText::new(&path).monospace()).truncate())
                .on_hover_text(&path);
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            remove = ui
                .add(
                    egui::Button::new(widgets::icon_text(icons::FOLDER_MINUS, "Remove"))
                        .frame(false),
                )
                .on_hover_text("Stop scanning this folder. Nothing on disk changes.")
                .clicked();
            if ui
                .add_enabled(
                    !missing,
                    egui::Button::new(widgets::icon_text(icons::FOLDER_OPEN, "Open")).frame(false),
                )
                .on_hover_text("Open in File Explorer")
                .clicked()
            {
                diagnostics::open_existing(root);
            }
            if missing {
                widgets::chip(ui, icons::WARNING, "Not found", theme::WARNING).on_hover_text(
                    "The folder no longer exists. Remove it, or restore the folder.",
                );
            }
        });
    });
    remove
}

fn activity_kind(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "dll_swap" => (icons::ARROW_CIRCLE_UP, "DLL update"),
        "restore" => (icons::ARROW_U_UP_LEFT, "Undo"),
        "tool_change" => (icons::WRENCH, "Indicator"),
        "tool_restore" => (icons::ARROW_U_UP_LEFT, "Indicator restore"),
        _ => (icons::INFO, "Change"),
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

    #[test]
    fn backup_warning_clears_after_clean_retry_report() {
        let warning = backup_warning_message(&dlss_core::BackupLoadReport {
            offline_revocation_fallbacks: 1,
            ..Default::default()
        });
        assert!(warning.is_some());
        assert!(backup_warning_message(&dlss_core::BackupLoadReport::default()).is_none());
    }
}
