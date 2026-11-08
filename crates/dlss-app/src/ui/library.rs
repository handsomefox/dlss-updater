//! Library view: the filterable, sortable game table and its empty states.

use super::table;
use super::table::{RowStatus, row_status, selection_range};
use super::theme::{self, icons};
use super::toast::plural;
use super::widgets::{self, TriState};
use crate::ui::review::ReviewIntent;
use crate::{
    Action, AppWindow, DlssApp, GameActivity, GameFilter, GameRow, GameSort, ReleaseDownload,
    SortKey, StoreFilter,
};
use eframe::egui;

const ROW_HEIGHT: f32 = 40.0;

impl DlssApp {
    pub(crate) fn library_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let actions = self.render_release_setup(ui);
        self.apply_all(&ctx, actions);
        let rows = self.filtered_game_rows();
        if rows.is_empty() {
            let actions = self.render_game_empty_state(ui);
            self.apply_all(&ctx, actions);
            return;
        }
        let actions = self.render_game_rows(ui, &rows);
        self.apply_all(&ctx, actions);
    }

    /// The rows a click on a row checkbox changes: that row, or with Shift
    /// the visible rows from the anchor game to it. As indices into
    /// `self.games`, resolved against the rows this frame drew.
    fn toggled_rows(&self, rows: &[usize], position: usize, extend: bool) -> Vec<usize> {
        let range = if extend {
            selection_range(rows, &self.games, self.selection_anchor.as_ref(), position)
        } else {
            None
        };
        rows[range.unwrap_or(position..=position)].to_vec()
    }

    /// Shown until an official release is downloaded, since nothing can be
    /// compared or updated before then. It keeps one place and one height
    /// through checking, downloading, and failing, so the table below does
    /// not jump as the state changes.
    fn render_release_setup(&self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let has_ready_release = self
            .releases
            .iter()
            .any(|release| release.state == dlss_core::ReleaseState::Ready);
        if has_ready_release {
            return actions;
        }
        let latest = self.latest_release_meta();
        widgets::card(ui, |ui| {
            ui.set_min_height(52.0);
            ui.horizontal(|ui| {
                ui.label(widgets::icon(icons::DOWNLOAD_SIMPLE, 24.0, theme::ACCENT));
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    // Leave the buttons their room, or the text runs under them.
                    ui.set_max_width((ui.available_width() - 340.0).max(240.0));
                    ui.strong("Download the latest DLSS release to check your games");
                    ui.label(
                        egui::RichText::new(
                            "Versions are compared against NVIDIA's official Streamline SDK \
                             release. It is downloaded once, and its signatures are checked \
                             before any DLL from it is used.",
                        )
                        .color(theme::TEXT_MUTED),
                    );
                });
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| match &latest {
                        _ if self.release_download.is_some() => {
                            let (state, received, total) = self.release_progress_now();
                            ui.scope(|ui| {
                                ui.set_max_width(280.0);
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| widgets::download_progress(ui, state, received, total),
                                );
                            });
                        }
                        Some(release) => {
                            // Right to left: the primary button goes first so
                            // it sits rightmost, as in every dialog.
                            let failed = self.release_errors.get(&release.metadata.id);
                            let label = if failed.is_some() {
                                "Try again".to_owned()
                            } else {
                                format!("Download {}", release.metadata.tag)
                            };
                            if ui
                                .add(widgets::primary_icon_button(icons::DOWNLOAD_SIMPLE, &label))
                                .clicked()
                            {
                                actions.push(Action::DownloadRelease(release.metadata.id.clone()));
                            }
                            if ui.button("Other releases…").clicked() {
                                actions.push(Action::OpenWindow(AppWindow::Releases));
                            }
                            if let Some(error) = failed {
                                widgets::status_text(
                                    ui,
                                    icons::WARNING_CIRCLE,
                                    "Download failed",
                                    theme::DANGER,
                                )
                                .on_hover_text(error);
                            }
                        }
                        None if self.runtime.catalog_loading => {
                            ui.label(
                                egui::RichText::new("Checking GitHub…").color(theme::TEXT_MUTED),
                            );
                            ui.spinner();
                        }
                        None => {
                            if ui
                                .button(widgets::icon_text(icons::ARROW_CLOCKWISE, "Try again"))
                                .clicked()
                            {
                                actions.push(Action::RefreshCatalog);
                            }
                            ui.label(
                                egui::RichText::new("Could not reach GitHub").color(theme::DANGER),
                            )
                            .on_hover_text(
                                self.catalog_error.as_deref().unwrap_or(
                                    "GitHub returned no stable Streamline release archives",
                                ),
                            );
                        }
                    },
                );
            });
        });
        ui.add_space(10.0);
        actions
    }

    /// The state, bytes received, and total size of the release download in
    /// progress, for a progress bar.
    pub(crate) fn release_progress_now(&self) -> (dlss_core::ReleaseState, u64, Option<u64>) {
        let Some(ReleaseDownload {
            id,
            progress: Some((received, total)),
        }) = &self.release_download
        else {
            return (dlss_core::ReleaseState::Downloading, 0, None);
        };
        let state = self
            .releases
            .iter()
            .find(|release| &release.metadata.id == id)
            .map_or(dlss_core::ReleaseState::Downloading, |release| {
                release.state
            });
        (state, *received, *total)
    }

    /// Indices into `self.games` of the rows the table shows, sorted.
    pub(crate) fn filtered_game_rows(&self) -> Vec<usize> {
        let search = self.filter.to_lowercase();
        let staged = self.staged_games();
        let mut rows: Vec<usize> = self
            .games
            .iter()
            .enumerate()
            .filter(|(_, game)| {
                self.matches_search_and_store(game, &search)
                    && matches_tab(game, self.filter_mode, &staged)
            })
            .map(|(index, _)| index)
            .collect();
        table::sort_rows(
            &mut rows,
            &self.games,
            self.game_sort,
            self.latest_release_ready(),
        );
        rows
    }

    /// How many games each tab would show under the current search and store.
    pub(crate) fn tab_counts(&self) -> Vec<(GameFilter, usize)> {
        let search = self.filter.to_lowercase();
        let staged = self.staged_games();
        let base: Vec<_> = self
            .games
            .iter()
            .filter(|game| self.matches_search_and_store(game, &search))
            .collect();
        [
            GameFilter::Updates,
            GameFilter::WithDlls,
            GameFilter::All,
            GameFilter::Staged,
            GameFilter::Problems,
            GameFilter::Recent,
        ]
        .into_iter()
        .map(|tab| {
            let count = base
                .iter()
                .filter(|game| matches_tab(game, tab, &staged))
                .count();
            (tab, count)
        })
        .collect()
    }

    fn matches_search_and_store(&self, game: &GameRow, search: &str) -> bool {
        let text_matches = search.is_empty()
            || game.folded_name.contains(search)
            || game.store.to_lowercase().contains(search);
        let store_matches = match self.store_filter {
            StoreFilter::All => true,
            StoreFilter::Steam => game.store_kind == dlss_core::StoreKind::Steam,
            StoreFilter::Epic => game.store_kind == dlss_core::StoreKind::Epic,
            StoreFilter::Gog => game.store_kind == dlss_core::StoreKind::Gog,
            StoreFilter::Manual => game.store_kind == dlss_core::StoreKind::Manual,
        };
        text_matches && store_matches
    }

    /// Games with at least one staged version change.
    fn staged_games(&self) -> std::collections::HashSet<dlss_core::GameId> {
        let staged: std::collections::HashSet<_> = self
            .persisted
            .target_profile
            .targets
            .iter()
            .filter(|(_, target)| **target != dlss_core::DesiredDll::KeepInstalled)
            .map(|(id, _)| id)
            .collect();
        self.games
            .iter()
            .filter(|game| game.details.iter().any(|dll| staged.contains(&dll.id)))
            .map(|game| game.id.clone())
            .collect()
    }

    fn render_game_rows(&self, ui: &mut egui::Ui, rows: &[usize]) -> Vec<Action> {
        let mut actions = Vec::new();
        let latest_ready = self.latest_release_ready();
        let latest_dlss = self.latest_dlss_version();
        let busy = self.busy();
        let can_update = self.catalog_release.is_some() && !busy;
        let selectable = rows
            .iter()
            .filter(|&&index| self.games[index].dlls > 0)
            .count();
        let checked = rows
            .iter()
            .filter(|&&index| self.games[index].selected)
            .count();
        let all_state = TriState::of(checked, selectable);
        egui_extras::TableBuilder::new(ui)
            // A new id, so column widths saved by the old resizable table
            // do not carry over into this layout.
            .id_salt("library_table_v2")
            .striped(true)
            .resizable(false)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(egui_extras::Column::exact(34.0))
            .column(egui_extras::Column::remainder().at_least(220.0).clip(true))
            .column(egui_extras::Column::exact(92.0))
            .column(egui_extras::Column::exact(220.0))
            .column(egui_extras::Column::exact(64.0))
            .column(egui_extras::Column::exact(210.0))
            .header(36.0, |mut header| {
                header_row(
                    &mut header,
                    all_state,
                    selectable,
                    self.game_sort,
                    rows,
                    &mut actions,
                );
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                    let position = row.index();
                    let game = &self.games[rows[position]];
                    row.set_selected(game.selected);
                    row.col(|ui| {
                        if game.dlls > 0 {
                            let mut selected = game.selected;
                            if widgets::checkbox(ui, &mut selected, "").clicked() {
                                actions.push(Action::Select {
                                    rows: self.toggled_rows(rows, position, click_with_shift(ui)),
                                    selected,
                                    anchor: Some(game.id.clone()),
                                });
                            }
                        }
                    });
                    row.col(|ui| game_name_cell(ui, game));
                    row.col(|ui| {
                        ui.label(egui::RichText::new(game.store).color(theme::TEXT_MUTED));
                    });
                    row.col(|ui| dlss_version_cell(ui, game, latest_dlss));
                    row.col(|ui| {
                        if game.dlls > 0 {
                            ui.label(game.dlls.to_string());
                        } else {
                            widgets::empty_cell(ui);
                        }
                    });
                    row.col(|ui| {
                        let activity = self.game_activity(&game.id);
                        let status = row_status(game, latest_ready);
                        if status_cell(ui, game, status, activity, can_update, busy) {
                            actions.push(Action::Review(ReviewIntent::QuickDlss(vec![
                                game.id.clone(),
                            ])));
                        }
                    });
                    let response = row.response();
                    if response.clicked() {
                        actions.push(Action::ShowGame(game.id.clone()));
                    }
                    response.on_hover_cursor(egui::CursorIcon::PointingHand);
                });
            });
        actions
    }

    /// The newest DLSS Super Resolution build in the downloaded latest
    /// release, which is what "DLSS version" is compared against.
    fn latest_dlss_version(&self) -> Option<dlss_core::DllVersion> {
        self.latest_release()?
            .dlls
            .iter()
            .filter(|dll| {
                dlss_core::DllKind::classify(&dll.file_name)
                    == Some(dlss_core::DllKind::DlssSuperResolution)
            })
            .map(|dll| dll.version)
            .max()
    }

    fn render_game_empty_state(&self, ui: &mut egui::Ui) -> Vec<Action> {
        if self.games.is_empty() {
            return self.render_no_games(ui);
        }
        let (icon, title, detail, action) = self.empty_filter_message();
        let mut actions = Vec::new();
        ui.vertical_centered(|ui| {
            ui.add_space(64.0);
            ui.label(widgets::icon(icon, 44.0, theme::TEXT_FAINT));
            ui.add_space(6.0);
            ui.heading(title);
            ui.label(egui::RichText::new(detail).color(theme::TEXT_MUTED));
            if let Some(action) = action {
                ui.add_space(8.0);
                if ui.button(action).clicked() {
                    actions.push(Action::WidenFilter);
                }
            }
        });
        actions
    }

    /// What an empty table says, and the button that widens it again.
    fn empty_filter_message(&self) -> (&'static str, String, String, Option<&'static str>) {
        if !self.filter.is_empty() {
            return (
                icons::MAGNIFYING_GLASS,
                format!("No games match \"{}\"", self.filter),
                "Search looks at game and store names.".into(),
                Some("Clear search"),
            );
        }
        if self.store_filter != StoreFilter::All {
            return (
                icons::GAME_CONTROLLER,
                "No games from this store here".into(),
                "Try another tab, or show every store.".into(),
                Some("Show all stores"),
            );
        }
        let latest_ready = self.latest_release_ready();
        match self.filter_mode {
            GameFilter::Updates if latest_ready => (
                icons::CHECK_CIRCLE,
                "Every game is up to date".into(),
                "Each NVIDIA DLL matches the latest official release.".into(),
                None,
            ),
            GameFilter::Updates => (
                icons::DOWNLOAD_SIMPLE,
                "Updates are not checked yet".into(),
                "Download the latest release above to compare versions.".into(),
                None,
            ),
            GameFilter::WithDlls => (
                icons::GAME_CONTROLLER,
                "None of your games ship NVIDIA DLLs".into(),
                "Games without DLSS, Streamline, or Reflex files have nothing to update.".into(),
                Some("Show all games"),
            ),
            GameFilter::Staged => (
                icons::LIST_CHECKS,
                "No staged changes".into(),
                "Open a game and choose a version for any DLL to stage it.".into(),
                None,
            ),
            GameFilter::Problems => (
                icons::CHECK_CIRCLE,
                "No problems found".into(),
                "Every DLL was read and has a version.".into(),
                None,
            ),
            GameFilter::Recent => (
                icons::CLOCK,
                "Nothing changed yet".into(),
                "Games you update or restore during this session appear here.".into(),
                None,
            ),
            GameFilter::All => (
                icons::GAME_CONTROLLER,
                "No games".into(),
                String::new(),
                None,
            ),
        }
    }

    fn render_no_games(&self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        ui.vertical_centered(|ui| {
            ui.add_space(64.0);
            if self.runtime.scanning {
                ui.add(egui::Spinner::new().size(32.0).color(theme::ACCENT));
                ui.add_space(10.0);
                ui.heading("Looking for your games");
                ui.label(
                    egui::RichText::new(
                        "Checking Steam, Epic Games, GOG, and the folders you added.",
                    )
                    .color(theme::TEXT_MUTED),
                );
                return;
            }
            ui.label(widgets::icon(
                icons::GAME_CONTROLLER,
                48.0,
                theme::TEXT_FAINT,
            ));
            ui.add_space(6.0);
            ui.heading("No games found");
            ui.label(
                egui::RichText::new(
                    "No store reported an installed game. Add a game's install folder \
                     to manage it by hand.",
                )
                .color(theme::TEXT_MUTED),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                // Center the two buttons under the text.
                let width = 300.0;
                ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
                if ui
                    .add(widgets::primary_icon_button(
                        icons::FOLDER_PLUS,
                        "Add game folder…",
                    ))
                    .clicked()
                {
                    actions.push(Action::AddFolder);
                }
                if ui
                    .button(widgets::icon_text(icons::FOLDER_SIMPLE, "Store details"))
                    .clicked()
                {
                    actions.push(Action::OpenWindow(AppWindow::Roots));
                }
            });
        });
        actions
    }
}

/// Whether the click this frame was a shift-click. Reads the modifiers
/// recorded on the button event itself, because a quick shift-click can
/// press and release Shift within one frame, leaving the frame's final
/// modifier state without it.
fn click_with_shift(ui: &egui::Ui) -> bool {
    ui.input(|input| {
        input.modifiers.shift
            || input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::PointerButton {
                        button: egui::PointerButton::Primary,
                        modifiers,
                        ..
                    } if modifiers.shift
                )
            })
    })
}

/// The select-all box and the sortable column titles.
fn header_row(
    header: &mut egui_extras::TableRow<'_, '_>,
    all_state: TriState,
    selectable: usize,
    sort: GameSort,
    rows: &[usize],
    actions: &mut Vec<Action>,
) {
    header.col(|ui| {
        widgets::table_header_background(ui);
        if selectable > 0
            && widgets::tri_checkbox(ui, all_state, "", "Select every game shown (Ctrl+A)")
        {
            actions.push(Action::Select {
                rows: rows.to_vec(),
                selected: all_state != TriState::All,
                anchor: None,
            });
        }
    });
    for (label, key) in [
        ("Game", SortKey::Name),
        ("Store", SortKey::Store),
        ("DLSS version", SortKey::DlssVersion),
        ("DLLs", SortKey::Dlls),
        ("Status", SortKey::Status),
    ] {
        header.col(|ui| {
            if let Some(sort) = table::sort_header(ui, label, key, sort) {
                actions.push(Action::Sort(sort));
            }
        });
    }
}

fn matches_tab(
    game: &GameRow,
    tab: GameFilter,
    staged: &std::collections::HashSet<dlss_core::GameId>,
) -> bool {
    match tab {
        GameFilter::All => true,
        // A game whose files could not be read may well have DLLs; keep it
        // in view rather than filing it with the games that have none.
        GameFilter::WithDlls => game.dlls > 0 || game.inspection_errors > 0,
        GameFilter::Updates => game.upgrades > 0,
        GameFilter::Staged => staged.contains(&game.id),
        GameFilter::Problems => game.inspection_errors > 0 || game.has_unknown,
        GameFilter::Recent => game.last_operation.is_some(),
    }
}

fn game_name_cell(ui: &mut egui::Ui, game: &GameRow) {
    let name = ui.add(
        egui::Label::new(egui::RichText::new(&game.name).strong())
            .truncate()
            .sense(egui::Sense::hover()),
    );
    if let Some(operation) = &game.last_operation {
        name.on_hover_text(operation);
    }
    if let Some(risk_name) = game.known_risk {
        ui.label(widgets::icon(icons::WARNING, 14.0, theme::WARNING))
            .on_hover_text(format!(
                "Known risk: {risk_name}. {}",
                dlss_core::KNOWN_GAME_RISK_WARNING
            ));
    }
}

/// The installed DLSS version, and the newer one on offer when there is
/// one: the single fact most people open this app to learn.
fn dlss_version_cell(ui: &mut egui::Ui, game: &GameRow, latest: Option<dlss_core::DllVersion>) {
    let Some(installed) = game.dlss_version else {
        widgets::empty_cell(ui);
        return;
    };
    ui.spacing_mut().item_spacing.x = 6.0;
    let newer = latest.filter(|latest| *latest > installed && game.dlss_upgrades > 0);
    ui.label(
        egui::RichText::new(installed.to_string())
            .monospace()
            .color(if newer.is_some() {
                theme::TEXT_MUTED
            } else {
                theme::TEXT
            }),
    );
    if let Some(newer) = newer {
        ui.label(widgets::icon(icons::ARROW_RIGHT, 12.0, theme::ACCENT));
        ui.label(
            egui::RichText::new(newer.to_string())
                .monospace()
                .color(theme::ACCENT),
        );
    }
}

/// Why a DLSS 1 Super Resolution DLL is never updated, wherever the app
/// shows one.
pub(crate) const DLSS1_EXPLANATION: &str = "This game uses DLSS 1. DLSS 2 and later work \
     differently and turn DLSS off in a DLSS 1 game, so its DLSS DLL stays as it is.";

/// Draws the status column. Returns true when the Update button was clicked.
fn status_cell(
    ui: &mut egui::Ui,
    game: &GameRow,
    status: RowStatus,
    activity: Option<GameActivity>,
    can_update: bool,
    busy: bool,
) -> bool {
    match activity {
        Some(GameActivity::Running) => {
            ui.spinner();
            ui.label("Updating…");
            return false;
        }
        Some(GameActivity::Queued) => {
            widgets::status_text(ui, icons::HOURGLASS, "Queued", theme::TEXT_MUTED);
            return false;
        }
        None => {}
    }
    let disabled_reason = if busy {
        "Wait for the current update to finish"
    } else {
        "The release catalog has not loaded yet"
    };
    match status {
        RowStatus::UpdateAvailable => ui
            .add_enabled(can_update, widgets::accent_button(icons::SPARKLE, "Update"))
            .on_hover_text(format!(
                "Review {} for this game",
                plural(game.dlss_upgrades, "DLSS update")
            ))
            .on_disabled_hover_text(disabled_reason)
            .clicked(),
        RowStatus::NotChecked => ui
            .add_enabled(
                can_update,
                egui::Button::new(widgets::icon_text(icons::SPARKLE, "Update")),
            )
            .on_hover_text("Download the latest release, then review this game's DLSS updates")
            .on_disabled_hover_text(disabled_reason)
            .clicked(),
        RowStatus::Problem => {
            widgets::status_text(
                ui,
                icons::WARNING_CIRCLE,
                &format!("{} unreadable", plural(game.inspection_errors, "file")),
                theme::DANGER,
            )
            .on_hover_text(
                "Some files in this game's folder could not be inspected. \
                 The log folder, under About, has the details.",
            );
            false
        }
        RowStatus::Dlss1 => {
            widgets::status_text(ui, icons::INFO, "DLSS 1, can't update", theme::TEXT_MUTED)
                .on_hover_text(DLSS1_EXPLANATION);
            false
        }
        RowStatus::OptionalUpdates => {
            ui.label(widgets::status_with_count(
                icons::CHECK,
                "DLSS up to date",
                theme::SUCCESS,
                &format!("+{}", game.upgrades),
                ui.spacing().item_spacing.x,
            ))
            .on_hover_text(format!(
                "{} for Streamline or Reflex. These are optional; open the game to review them.",
                plural(game.upgrades, "update")
            ));
            false
        }
        RowStatus::VersionUnknown => {
            widgets::status_text(ui, icons::QUESTION, "Version unknown", theme::WARNING)
                .on_hover_text(
                    "A DLL in this game has no readable version, so it is never updated",
                );
            false
        }
        RowStatus::UpToDate => {
            widgets::status_text(ui, icons::CHECK, "Up to date", theme::SUCCESS);
            false
        }
        RowStatus::NoDlls => {
            ui.label(egui::RichText::new("No NVIDIA DLLs").color(theme::TEXT_FAINT));
            false
        }
    }
}

/// One store's discovery result: what was found, or why nothing was.
pub(crate) fn discovery_report_row(ui: &mut egui::Ui, report: &dlss_core::StoreDiscoveryReport) {
    let (icon, color, status) = match report.status {
        dlss_core::DiscoveryStatus::Found => (
            icons::CHECK_CIRCLE,
            theme::SUCCESS,
            plural(report.games_found, "game"),
        ),
        dlss_core::DiscoveryStatus::NotDetected => {
            (icons::MINUS, theme::TEXT_MUTED, "Not installed".into())
        }
        dlss_core::DiscoveryStatus::Error => (
            icons::WARNING_CIRCLE,
            theme::DANGER,
            "Could not read".into(),
        ),
    };
    ui.horizontal(|ui| {
        ui.label(widgets::icon(icon, 15.0, color));
        ui.strong(&report.store);
        ui.label(egui::RichText::new(status).color(color));
    });
    if let Some(detail) = &report.detail {
        ui.indent(("discovery_detail", &report.store), |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(detail)
                        .color(theme::TEXT_MUTED)
                        .size(12.0),
                )
                .wrap()
                .selectable(true),
            );
        });
    }
}

/// True when a store could not be read, which the toolbar flags on the
/// Game folders button.
pub(crate) fn has_discovery_errors(reports: &[dlss_core::StoreDiscoveryReport]) -> bool {
    reports
        .iter()
        .any(|report| report.status == dlss_core::DiscoveryStatus::Error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::table::tests::row;

    #[test]
    fn tabs_split_the_library_by_what_needs_doing() {
        let mut update = row("Update", "Steam", 2, None, 1);
        update.dlss_upgrades = 1;
        let mut broken = row("Broken", "Steam", 1, None, 0);
        broken.has_unknown = true;
        let mut changed = row("Changed", "Steam", 1, None, 0);
        changed.last_operation = Some("Updated 1 DLL".into());
        let empty = row("Empty", "Steam", 0, None, 0);
        let staged: std::collections::HashSet<_> = [changed.id.clone()].into();
        let games = [update, broken, changed, empty];

        let names = |tab| -> Vec<&str> {
            games
                .iter()
                .filter(|game| matches_tab(game, tab, &staged))
                .map(|game| game.name.as_str())
                .collect()
        };
        assert_eq!(names(GameFilter::Updates), ["Update"]);
        assert_eq!(names(GameFilter::WithDlls), ["Update", "Broken", "Changed"]);
        let mut unreadable = row("Unreadable", "Steam", 0, None, 0);
        unreadable.inspection_errors = 2;
        assert!(matches_tab(&unreadable, GameFilter::WithDlls, &staged));
        assert_eq!(names(GameFilter::All).len(), 4);
        assert_eq!(names(GameFilter::Problems), ["Broken"]);
        assert_eq!(names(GameFilter::Staged), ["Changed"]);
        assert_eq!(names(GameFilter::Recent), ["Changed"]);
    }

    #[test]
    fn only_unreadable_stores_raise_the_folder_warning() {
        let report = |status| dlss_core::StoreDiscoveryReport {
            store: "Steam".into(),
            status,
            games_found: 0,
            detail: None,
        };
        // A store that is simply not installed is normal, not a warning.
        assert!(!has_discovery_errors(&[report(
            dlss_core::DiscoveryStatus::NotDetected
        )]));
        assert!(has_discovery_errors(&[
            report(dlss_core::DiscoveryStatus::Found),
            report(dlss_core::DiscoveryStatus::Error),
        ]));
    }
}
