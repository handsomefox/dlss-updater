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
use ui::{
    inspector::desired_label,
    windows::{format_timestamp, progress_label, state_label},
};
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
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([850.0, 520.0]),
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
        if let Err(error) = dlss_platform::windows::run_elevated_helper(std::path::Path::new(&plan))
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

struct DlssApp {
    persisted: PersistedState,
    games: Vec<GameRow>,
    filter: String,
    filter_mode: GameFilter,
    game_sort: GameSort,
    store_filter: StoreFilter,
    selected: Option<usize>,
    selected_dlls: std::collections::HashSet<dlss_core::DllInstallationId>,
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
    backups: Vec<dlss_core::BackupRecord>,
    inspecting_release: Option<dlss_core::ReleaseId>,
    upgrading: Option<dlss_core::GameId>,
    toast: Option<String>,
    toast_identity: Option<String>,
    toast_started: Option<Instant>,
    undo_game: Option<dlss_core::GameId>,
    review: Option<ReviewKind>,
    profiles_applying: std::collections::HashSet<dlss_core::GameId>,
    #[cfg(windows)]
    tool_runtime: WindowsToolRuntime,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum AppWindow {
    Tools,
    Releases,
    Activity,
    Roots,
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

#[derive(Clone, Copy, PartialEq)]
enum GameSort {
    Name,
    DllsAscending,
    DllsDescending,
}

#[derive(Clone, Copy, PartialEq)]
enum StoreFilter {
    All,
    Steam,
    Epic,
    Gog,
    Manual,
}

enum ReviewKind {
    BulkLatest(Vec<dlss_core::GameId>),
    Profiles(Vec<dlss_core::GameId>),
}

struct GameRow {
    id: dlss_core::GameId,
    selected: bool,
    name: String,
    store: &'static str,
    dlls: usize,
    upgrades: usize,
    state: String,
    last_operation: String,
    details: Vec<dlss_core::DllInstallation>,
    inspection_errors: usize,
}

impl GameRow {
    fn from_install(game: dlss_core::GameInstall) -> Self {
        let dll_count = game.dlls.len();
        let inspection_errors = game.inspection_errors;
        let has_unknown = game.dlls.iter().any(|dll| dll.metadata.version.is_none());
        Self {
            id: game.id,
            selected: false,
            name: game.name,
            store: match game.store {
                dlss_core::StoreKind::Steam => "Steam",
                dlss_core::StoreKind::Epic => "Epic",
                dlss_core::StoreKind::Gog => "GOG",
                dlss_core::StoreKind::Manual => "Manual",
            },
            dlls: dll_count,
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
        }
    }
}

impl DlssApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            cc.egui_ctx.style_mut_of(theme, |style| {
                style.spacing.item_spacing = egui::vec2(8.0, 6.0);
                style.spacing.button_padding = egui::vec2(10.0, 5.0);
                style.spacing.interact_size.y = 28.0;
            });
        }
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
                dlss_core::BackupStore::new(base.join("DLSS Updater/backups")).load_index()
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
            filter: String::new(),
            filter_mode: GameFilter::All,
            game_sort: GameSort::Name,
            store_filter: StoreFilter::All,
            selected: None,
            selected_dlls: std::collections::HashSet::new(),
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
            backups,
            inspecting_release: None,
            upgrading: None,
            toast: None,
            toast_identity: None,
            toast_started: None,
            undo_game: None,
            review: None,
            profiles_applying: std::collections::HashSet::new(),
            #[cfg(windows)]
            tool_runtime: WindowsToolRuntime::default(),
        };
        let _ = app.worker.commands.send(Command::Scan);
        let _ = app.worker.commands.send(Command::RefreshCatalog);
        app
    }

    fn game_table(&mut self, ui: &mut egui::Ui) {
        let rows = self.filtered_game_rows();
        let (requested_sort, requested_upgrade) = self.render_game_rows(ui, &rows);
        if let Some(sort) = requested_sort {
            self.game_sort = sort;
        }
        if let Some(game_id) = requested_upgrade {
            self.upgrading = Some(game_id.clone());
            self.toast = Some("Preparing official release…".into());
            let _ = self.worker.commands.send(Command::UpgradeLatest(game_id));
        }
        self.render_game_empty_state(ui, rows.is_empty());
    }

    fn filtered_game_rows(&self) -> Vec<usize> {
        let filter = self.filter.to_ascii_lowercase();
        let mut rows: Vec<usize> = self
            .games
            .iter()
            .enumerate()
            .filter(|(_, game)| self.game_matches_filters(game, &filter))
            .map(|(index, _)| index)
            .collect();
        ui::table::sort_rows(&mut rows, &self.games, self.game_sort);
        rows
    }

    fn game_matches_filters(&self, game: &GameRow, filter: &str) -> bool {
        let text_matches = filter.is_empty()
            || game.name.to_ascii_lowercase().contains(filter)
            || game.store.to_ascii_lowercase().contains(filter);
        let store_matches = match self.store_filter {
            StoreFilter::All => true,
            StoreFilter::Steam => game.store == "Steam",
            StoreFilter::Epic => game.store == "Epic",
            StoreFilter::Gog => game.store == "GOG",
            StoreFilter::Manual => game.store == "Manual",
        };
        let ids: std::collections::HashSet<_> = game.details.iter().map(|dll| &dll.id).collect();
        let custom = self
            .persisted
            .target_profile
            .targets
            .iter()
            .any(|(id, target)| {
                ids.contains(id) && *target != dlss_core::DesiredDll::KeepInstalled
            });
        let mode_matches = match self.filter_mode {
            GameFilter::All => true,
            GameFilter::HasDlls => game.dlls > 0,
            GameFilter::Upgrades => game.upgrades > 0,
            GameFilter::Custom => custom,
            GameFilter::Errors => game.inspection_errors > 0 || game.state == "Unknown",
            GameFilter::Recent => game.last_operation != "Never",
        };
        text_matches && store_matches && mode_matches
    }

    fn render_game_rows(
        &mut self,
        ui: &mut egui::Ui,
        rows: &[usize],
    ) -> (Option<GameSort>, Option<dlss_core::GameId>) {
        let mut requested_upgrade = None;
        let mut requested_sort = None;
        egui_extras::TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .column(egui_extras::Column::exact(28.0))
            .column(egui_extras::Column::remainder().at_least(180.0))
            .column(egui_extras::Column::initial(90.0))
            .column(egui_extras::Column::initial(90.0))
            .column(egui_extras::Column::initial(110.0))
            .column(egui_extras::Column::initial(120.0))
            .column(egui_extras::Column::initial(130.0))
            .header(30.0, |mut h| {
                h.col(|_| {});
                h.col(|ui| {
                    ui.strong("Game");
                });
                h.col(|ui| {
                    ui.strong("Store");
                });
                h.col(|ui| {
                    let label = match self.game_sort {
                        GameSort::DllsAscending => "DLLs ↑",
                        GameSort::DllsDescending => "DLLs ↓",
                        GameSort::Name => "DLLs ↕",
                    };
                    if ui.strong(label).clicked() {
                        requested_sort = Some(match self.game_sort {
                            GameSort::DllsDescending => GameSort::DllsAscending,
                            _ => GameSort::DllsDescending,
                        });
                    }
                });
                h.col(|ui| {
                    ui.strong("Upgrades");
                });
                h.col(|ui| {
                    ui.strong("State");
                });
                h.col(|ui| {
                    ui.strong("Action");
                });
            })
            .body(|body| {
                body.rows(30.0, rows.len(), |mut row| {
                    let index = rows[row.index()];
                    let game = &mut self.games[index];
                    row.col(|ui| {
                        ui.checkbox(&mut game.selected, "");
                    });
                    row.col(|ui| {
                        if ui
                            .selectable_label(self.selected == Some(index), &game.name)
                            .clicked()
                        {
                            self.selected = Some(index);
                        }
                    });
                    row.col(|ui| {
                        ui.label(game.store);
                    });
                    row.col(|ui| {
                        ui.label(game.dlls.to_string());
                    });
                    row.col(|ui| {
                        ui.label(game.upgrades.to_string());
                    });
                    row.col(|ui| {
                        ui.label(&game.state).on_hover_text(&game.last_operation);
                    });
                    row.col(|ui| {
                        let available = self.catalog_release.is_some()
                            && game.dlls > 0
                            && self.upgrading.is_none();
                        if ui
                            .add_enabled(available, egui::Button::new("Upgrade latest"))
                            .clicked()
                        {
                            requested_upgrade = Some(game.id.clone());
                        }
                    });
                });
            });
        (requested_sort, requested_upgrade)
    }

    fn render_game_empty_state(&mut self, ui: &mut egui::Ui, rows_empty: bool) {
        if rows_empty && !self.games.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.weak("No games match the current search and filters.");
            });
        }
        if self.games.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(90.0);
                ui.heading("No games discovered yet");
                ui.label("Add a game root now; automatic store discovery is enabled by platform adapters.");
                if let Some(error) = &self.last_error {
                    ui.colored_label(egui::Color32::RED, error);
                }
                ui.add_space(8.0);
                if ui.button("Add game folder…").clicked()
                    && let Some(root) = rfd::FileDialog::new().pick_folder()
                {
                    self.add_custom_root(&root);
                }
            });
        }
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
        result: Result<Vec<dlss_core::GameInstall>, worker::WorkerError>,
    ) {
        self.runtime.scanning = false;
        let Ok(games) = result else {
            self.last_error = result.err().map(|error| error.to_string());
            return;
        };
        let selected_id = self
            .selected
            .and_then(|index| self.games.get(index))
            .map(|game| game.id.clone());
        self.games = games.into_iter().map(GameRow::from_install).collect();
        self.selected = selected_id.and_then(|id| self.games.iter().position(|game| game.id == id));
        let known_dlls: std::collections::HashSet<_> = self
            .games
            .iter()
            .flat_map(|game| game.details.iter().map(|dll| dll.id.clone()))
            .collect();
        self.selected_dlls.retain(|id| known_dlls.contains(id));
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
        self.inspecting_release = None;
        match result {
            Ok(release) => {
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
            Err(error) => self.last_error = Some(format!("Release validation: {error}")),
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
        applying_profile: bool,
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
        if applying_profile {
            self.clear_game_profile(game_id);
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
        egui::Window::new("Global Tools").open(&mut open).default_width(520.0).show(ctx, |ui| {
            ui.colored_label(egui::Color32::YELLOW, "Global setting — affects all compatible games on this PC");
            ui.add_space(8.0); ui.heading("DLSS on-screen indicator");
            ui.label("Controls NVIDIA's global NGX indicator. It is never changed during scanning or DLL replacement.");
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
                });
                if let Some(error) = &self.last_error {
                    ui.colored_label(egui::Color32::RED, error);
                }
                ui.small("If another program changes this value, confirmation is required before apply or restore.");
            });
        });
        self.set_window_open(AppWindow::Tools, open);
    }

    fn releases_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Releases);
        egui::Window::new("Official Streamline releases")
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label("Older releases remain metadata-only until requested. Validated production DLLs are then available in per-DLL selectors.");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.runtime.catalog_loading, egui::Button::new("Refresh catalog"))
                        .clicked()
                    {
                        let _ = self.worker.commands.send(Command::RefreshCatalog);
                    }
                    if self.runtime.catalog_loading {
                        ui.spinner();
                        ui.label("Loading official releases…");
                    }
                });
                ui.separator();
                if let Some(error) = &self.catalog_error {
                    ui.colored_label(egui::Color32::RED, format!("Catalog request failed: {error}"));
                    ui.label("Check network access and try Refresh catalog. Previously cached releases remain usable when available.");
                } else if !self.runtime.catalog_loading && self.releases.is_empty() {
                    ui.weak("GitHub returned no matching stable Streamline SDK release archives.");
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for release in &self.releases {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong(&release.metadata.tag);
                                ui.label(format!("{:?}", release.state));
                                if release.metadata.published_unix > 0 {
                                    ui.weak(format_timestamp(release.metadata.published_unix));
                                }
                                let busy = self.inspecting_release.is_some();
                                if release.state != dlss_core::ReleaseState::Ready
                                    && ui
                                        .add_enabled(!busy, egui::Button::new("Download and inspect"))
                                        .clicked()
                                {
                                    self.inspecting_release = Some(release.metadata.id.clone());
                                    let _ = self.worker.commands.send(Command::InspectRelease(
                                        release.metadata.id.clone(),
                                    ));
                                }
                            });
                            for dll in &release.dlls {
                                ui.small(format!(
                                    "{}  {}",
                                    dll.file_name.to_string_lossy(),
                                    dll.version
                                ));
                            }
                        });
                    }
                });
            });
        self.set_window_open(AppWindow::Releases, open);
    }

    fn activity_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Activity);
        egui::Window::new("Activity history")
            .open(&mut open)
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
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label("Manually added roots are scanned in addition to supported stores.");
                ui.separator();
                if self.persisted.custom_roots.is_empty() {
                    ui.weak("No manual roots configured.");
                }
                for root in &self.persisted.custom_roots {
                    ui.monospace(root.display().to_string());
                }
                ui.add_space(8.0);
                if ui.button("Add game folder…").clicked()
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
            game.upgrades = game
                .details
                .iter()
                .filter(|installed| {
                    let Some(installed_version) = installed.metadata.version else {
                        return false;
                    };
                    latest.iter().any(|candidate| {
                        candidate
                            .file_name
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&installed.file_name.to_string_lossy())
                            && candidate.version > installed_version
                    })
                })
                .count();
        }
    }

    fn refresh_backups(&mut self) {
        #[cfg(windows)]
        match dlss_platform::windows::WindowsKnownDirectories
            .local_app_data()
            .and_then(|base| {
                dlss_core::BackupStore::new(base.join("DLSS Updater/backups")).load_index()
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

    fn review_window(&mut self, ctx: &egui::Context) {
        let Some(review) = self.review.take() else {
            return;
        };
        let mut keep_open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("Review changes")
            .collapsible(false)
            .resizable(false)
            .open(&mut keep_open)
            .show(ctx, |ui| {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Each DLL is re-inspected, backed up, replaced independently, and verified.",
                );
                ui.add_space(8.0);
                self.render_review_summary(ui, &review);
                ui.weak("If Windows denies access to a target, the app will request elevation only for the denied replacements.");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    apply = ui.button("Apply").clicked();
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            keep_open = false;
        }
        if apply {
            self.queue_review(&review);
            self.toast = Some("Operation queued…".into());
            keep_open = false;
        }
        if keep_open {
            self.review = Some(review);
        }
    }

    fn render_review_summary(&self, ui: &mut egui::Ui, review: &ReviewKind) {
        match review {
            ReviewKind::BulkLatest(ids) => {
                let dlls: usize = self
                    .games
                    .iter()
                    .filter(|game| ids.contains(&game.id))
                    .map(|game| game.dlls)
                    .sum();
                ui.heading(format!("{} games · {dlls} candidate DLLs", ids.len()));
                let latest = self.latest_catalog();
                let upgrades: usize = self
                    .games
                    .iter()
                    .filter(|game| ids.contains(&game.id))
                    .map(|game| {
                        dlss_core::plan_strict_upgrades("preview", &game.details, &latest)
                            .swaps
                            .len()
                    })
                    .sum();
                ui.label(format!(
                    "{upgrades} confirmed upgrades · {} download requirement",
                    usize::from(latest.is_empty())
                ));
                ui.label("Only strictly newer, same-named official DLLs will change. Unknown, equal, newer, and different-build DLLs are preserved.");
            }
            ReviewKind::Profiles(game_ids) => self.render_profile_review(ui, game_ids),
        }
    }

    fn render_profile_review(&self, ui: &mut egui::Ui, game_ids: &[dlss_core::GameId]) {
        let count: usize = game_ids
            .iter()
            .map(|id| {
                self.profile_for_game(id)
                    .targets
                    .values()
                    .filter(|target| **target != dlss_core::DesiredDll::KeepInstalled)
                    .count()
            })
            .sum();
        ui.heading(format!("{count} staged DLL targets"));
        for game_id in game_ids {
            match self.preview_profile(game_id) {
                Ok(plan) => {
                    let summary = plan.summary();
                    ui.label(format!(
                        "{} upgrades · {} downgrades · {} other changes",
                        summary.upgrades,
                        summary.downgrades,
                        summary
                            .dlls
                            .saturating_sub(summary.upgrades + summary.downgrades)
                    ));
                    for swap in plan.swaps {
                        ui.small(format!(
                            "{:?}  {}",
                            swap.comparison,
                            swap.target_path.display()
                        ));
                    }
                }
                Err(error) => {
                    ui.label(format!("1 download/validation requirement · {error}"));
                }
            }
        }
        ui.label(
            "Advanced targets may upgrade, downgrade, restore, or install a different official build.",
        );
    }

    fn queue_review(&mut self, review: &ReviewKind) {
        match review {
            ReviewKind::BulkLatest(ids) => {
                for id in ids {
                    let _ = self
                        .worker
                        .commands
                        .send(Command::UpgradeLatest(id.clone()));
                }
            }
            ReviewKind::Profiles(game_ids) => {
                for game_id in game_ids {
                    let profile = self.profile_for_game(game_id);
                    self.profiles_applying.insert(game_id.clone());
                    let _ = self
                        .worker
                        .commands
                        .send(Command::ApplyProfile(game_id.clone(), profile));
                }
            }
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

    fn latest_catalog(&self) -> Vec<dlss_core::CatalogDll> {
        self.catalog_release
            .as_ref()
            .and_then(|tag| {
                self.releases.iter().find(|release| {
                    release.state == dlss_core::ReleaseState::Ready && &release.metadata.tag == tag
                })
            })
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
        let cached: Vec<_> = self
            .releases
            .iter()
            .flat_map(|release| release.dlls.iter().cloned())
            .collect();
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
        egui::Panel::top("toolbar").show(root, |ui| {
            ui.add_space(5.0);
            self.toolbar(ui);
            ui.add_space(5.0);
        });
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(360.0)
            .show(root, |ui| {
                ui.heading("Inspector");
                ui.separator();
                if let Some(index) = self.selected {
                    let game_id = self.games[index].id.clone();
                    let game_name = self.games[index].name.clone();
                    let dll_count = self.games[index].dlls;
                    let details = self.games[index].details.clone();
                    ui.heading(game_name);
                    ui.label(format!("{dll_count} managed DLLs"));
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for dll in &details {
                            let cached_options: Vec<_> = self
                                .releases
                                .iter()
                                .filter(|release| release.state == dlss_core::ReleaseState::Ready)
                                .flat_map(|release| {
                                    release
                                        .dlls
                                        .iter()
                                .filter(|candidate| {
                                    dlss_core::same_file_name(
                                        &candidate.file_name,
                                        &dll.file_name,
                                    )
                                })
                                        .map(|candidate| {
                                            (
                                                dlss_core::DesiredDll::Cached {
                                                    release: release.metadata.id.clone(),
                                                    sha256: candidate.sha256,
                                                },
                                                format!(
                                                    "{} · {}",
                                                    candidate.version, release.metadata.tag
                                                ),
                                            )
                                        })
                                })
                                .collect();
                            let restore_options: Vec<_> = self
                                .backups
                                .iter()
                                .filter(|backup| backup.original_path == dll.path)
                                .map(|backup| {
                                    (
                                        dlss_core::DesiredDll::Restore {
                                            backup_sha256: backup.sha256,
                                        },
                                        format!(
                                            "Restore {} · {}",
                                            backup.version.map_or_else(
                                                || "Unknown".into(),
                                                |version| version.to_string(),
                                            ),
                                            format_timestamp(backup.created_unix)
                                        ),
                                    )
                                })
                                .collect();
                        ui.group(|ui| {
                            ui.set_min_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    let mut selected = self.selected_dlls.contains(&dll.id);
                                    if ui.checkbox(&mut selected, "").changed() {
                                        if selected {
                                            self.selected_dlls.insert(dll.id.clone());
                                        } else {
                                            self.selected_dlls.remove(&dll.id);
                                        }
                                    }
                                    ui.strong(dll.file_name.to_string_lossy());
                                });
                                ui.small(dll.path.display().to_string());
                                ui.horizontal(|ui| {
                                    ui.label("Installed:");
                                    ui.monospace(
                                        dll.metadata.version.map_or_else(
                                            || "Unknown".into(),
                                            |version| version.to_string(),
                                        ),
                                    );
                                });
                                ui.label(format!("Signature: {:?}", dll.metadata.signature));
                                let comparison = self
                                    .latest_catalog()
                                    .into_iter()
                                    .filter(|candidate| {
                                        dlss_core::same_file_name(
                                            &candidate.file_name,
                                            &dll.file_name,
                                        )
                                    })
                                    .max_by_key(|candidate| (candidate.version, candidate.sha256))
                                    .map_or(dlss_core::Comparison::Unavailable, |target| {
                                        dlss_core::compare_dll(Some(&dll.metadata), Some(&target))
                                    });
                                ui.label(format!("Latest comparison: {comparison:?}"));
                                ui.horizontal(|ui| {
                                    ui.label("Desired:");
                                    // Render from a local value and only write back on an actual
                                    // change, so merely viewing a game never persists a profile
                                    // entry for every DLL (which would leak entries forever).
                                    let mut desired = self
                                        .persisted
                                        .target_profile
                                        .targets
                                        .get(&dll.id)
                                        .cloned()
                                        .unwrap_or(dlss_core::DesiredDll::KeepInstalled);
                                    let before = desired.clone();
                                    egui::ComboBox::from_id_salt(("desired", &dll.id.0))
                                        .width(ui.available_width().max(140.0))
                                        .selected_text(desired_label(&desired))
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut desired,
                                                dlss_core::DesiredDll::KeepInstalled,
                                                "Keep installed",
                                            );
                                            ui.selectable_value(
                                                &mut desired,
                                                dlss_core::DesiredDll::LatestOfficial,
                                                "Latest official",
                                            );
                                            for (target, label) in &cached_options {
                                                ui.selectable_value(
                                                    &mut desired,
                                                    target.clone(),
                                                    label,
                                                );
                                            }
                                            for (target, label) in &restore_options {
                                                ui.selectable_value(
                                                    &mut desired,
                                                    target.clone(),
                                                    label,
                                                );
                                            }
                                        });
                                    if desired != before {
                                        if desired == dlss_core::DesiredDll::KeepInstalled {
                                            self.persisted
                                                .target_profile
                                                .targets
                                                .remove(&dll.id);
                                        } else {
                                            self.persisted
                                                .target_profile
                                                .targets
                                                .insert(dll.id.clone(), desired);
                                        }
                                    }
                                });
                            });
                            ui.add_space(4.0);
                        }
                    });
                    let staged = self
                        .profile_for_game(&game_id)
                        .targets
                        .values()
                        .any(|target| *target != dlss_core::DesiredDll::KeepInstalled);
                    if ui
                        .add_enabled(
                            staged,
                            egui::Button::new("Review staged profile")
                                .min_size(egui::vec2(ui.available_width(), 32.0)),
                        )
                        .clicked()
                    {
                        self.review = Some(ReviewKind::Profiles(vec![game_id]));
                    }
                } else {
                    ui.weak("Select a game to inspect each DLL location, version, signature, target, and restore history.");
                }
            });
        egui::CentralPanel::default().show(root, |ui| {
            if let Some(error) = self.last_error.clone() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.colored_label(egui::Color32::RED, error);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Dismiss").clicked() {
                                self.last_error = None;
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
            self.game_table(ui);
        });
        let selected = self.games.iter().filter(|g| g.selected).count();
        let selected_dlls = self.selected_dlls.len();
        if selected > 0 || selected_dlls > 0 {
            egui::Panel::bottom("bulk").show(root, |ui| {
                ui.horizontal(|ui| {
                    let dlls: usize = self
                        .games
                        .iter()
                        .filter(|game| game.selected)
                        .map(|game| game.dlls)
                        .sum();
                    ui.strong(format!(
                        "{selected} games · {selected_dlls} DLL rows · {dlls} game DLL candidates"
                    ));
                    if ui
                        .add_enabled(selected > 0, egui::Button::new("Upgrade games to latest"))
                        .clicked()
                    {
                        self.review = Some(ReviewKind::BulkLatest(
                            self.games
                                .iter()
                                .filter(|game| game.selected)
                                .map(|game| game.id.clone())
                                .collect(),
                        ));
                    }
                    if ui
                        .add_enabled(selected_dlls > 0, egui::Button::new("Set DLLs to latest"))
                        .clicked()
                    {
                        for id in &self.selected_dlls {
                            self.persisted
                                .target_profile
                                .targets
                                .insert(id.clone(), dlss_core::DesiredDll::LatestOfficial);
                        }
                    }
                    if ui
                        .add_enabled(selected_dlls > 0, egui::Button::new("Review / Apply DLLs"))
                        .clicked()
                    {
                        let game_ids = self
                            .games
                            .iter()
                            .filter(|game| {
                                game.details
                                    .iter()
                                    .any(|dll| self.selected_dlls.contains(&dll.id))
                            })
                            .map(|game| game.id.clone())
                            .collect();
                        self.review = Some(ReviewKind::Profiles(game_ids));
                    }
                    if ui.button("Clear proposed / selection").clicked() {
                        for game in &mut self.games {
                            game.selected = false;
                        }
                        for id in self.selected_dlls.drain() {
                            self.persisted.target_profile.targets.remove(&id);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak("Bulk operations always require review");
                    });
                });
            });
        }
        if self.open_windows.contains(&AppWindow::Tools) {
            self.tools_window(root.ctx());
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
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        if let Some(message) = &self.toast {
                            ui.label(message);
                        } else {
                            ui.label("The last DLL change can still be undone.");
                        }
                        if let Some(game_id) = &self.undo_game
                            && ui.button("Undo").clicked()
                        {
                            undo_requested = Some(game_id.clone());
                        }
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
