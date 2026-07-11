//! Library view: the filterable, sortable game table and its empty states.

use super::theme::{self, icons};
use super::{table, widgets};
use crate::ui::review::ReviewIntent;
use crate::{DlssApp, GameFilter, GameRow, GameSort, SortKey, StoreFilter, View};
use eframe::egui;

impl DlssApp {
    pub(crate) fn library_view(&mut self, ui: &mut egui::Ui) {
        let rows = self.filtered_game_rows();
        let (requested_sort, requested_review) = self.render_game_rows(ui, &rows);
        if let Some(sort) = requested_sort {
            self.game_sort = sort;
        }
        if let Some(intent) = requested_review {
            self.open_review(intent);
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
        table::sort_rows(&mut rows, &self.games, self.game_sort);
        rows
    }

    fn game_matches_filters(&self, game: &GameRow, filter: &str) -> bool {
        let text_matches = filter.is_empty()
            || game.name.to_ascii_lowercase().contains(filter)
            || game.store.to_ascii_lowercase().contains(filter);
        let store_matches = match self.store_filter {
            StoreFilter::All => true,
            StoreFilter::Steam => game.store_kind == dlss_core::StoreKind::Steam,
            StoreFilter::Epic => game.store_kind == dlss_core::StoreKind::Epic,
            StoreFilter::Gog => game.store_kind == dlss_core::StoreKind::Gog,
            StoreFilter::Manual => game.store_kind == dlss_core::StoreKind::Manual,
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
    ) -> (Option<GameSort>, Option<ReviewIntent>) {
        let mut requested_review = None;
        let mut requested_sort = None;
        let mut requested_open = None;
        let latest_ready = self.latest_release_ready();
        egui_extras::TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .column(egui_extras::Column::exact(28.0))
            .column(egui_extras::Column::remainder().at_least(180.0))
            .column(egui_extras::Column::initial(90.0))
            .column(egui_extras::Column::initial(70.0))
            .column(egui_extras::Column::initial(110.0))
            .column(egui_extras::Column::initial(110.0))
            .column(egui_extras::Column::initial(120.0))
            .column(egui_extras::Column::initial(150.0))
            .header(32.0, |mut h| {
                requested_sort = header_columns(&mut h, self.game_sort);
            })
            .body(|body| {
                body.rows(32.0, rows.len(), |mut row| {
                    let index = rows[row.index()];
                    let in_flight = {
                        let game = &self.games[index];
                        self.upgrading.as_ref() == Some(&game.id)
                            || self.profiles_applying.contains_key(&game.id)
                    };
                    let game = &mut self.games[index];
                    row.col(|ui| {
                        ui.checkbox(&mut game.selected, "");
                    });
                    row.col(|ui| {
                        let name = egui::RichText::new(&game.name).strong();
                        if ui
                            .add(egui::Button::new(name).frame(false))
                            .on_hover_text("Open game details")
                            .clicked()
                        {
                            requested_open = Some(game.id.clone());
                        }
                    });
                    row.col(|ui| {
                        widgets::badge(ui, game.store, theme::INFO);
                    });
                    row.col(|ui| {
                        if game.dlls > 0 {
                            ui.label(game.dlls.to_string());
                        } else {
                            ui.weak("—");
                        }
                    });
                    row.col(|ui| {
                        match game.dlss_version {
                            Some(version) => ui.monospace(version.to_string()),
                            None => ui.weak("—"),
                        };
                    });
                    row.col(|ui| {
                        if game.upgrades > 0 {
                            widgets::badge(ui, game.upgrades.to_string(), theme::ACCENT);
                        } else {
                            ui.weak("—");
                        }
                    });
                    row.col(|ui| {
                        game_state_cell(ui, game, in_flight);
                    });
                    row.col(|ui| {
                        let available = self.catalog_release.is_some()
                            && game.dlls > 0
                            && self.upgrading.is_none()
                            && !in_flight;
                        requested_review =
                            game_row_action(ui, game, available, latest_ready, in_flight)
                                .or(requested_review.take());
                    });
                });
            });
        if let Some(id) = requested_open {
            self.view = View::Game(id);
        }
        (requested_sort, requested_review)
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
                ui.add_space(70.0);
                ui.label(widgets::icon(
                    icons::GAME_CONTROLLER,
                    52.0,
                    theme::TEXT_MUTED,
                ));
                ui.add_space(4.0);
                ui.heading("No games discovered yet");
                for report in &self.discovery_reports {
                    ui.add(egui::Label::new(discovery_report_label(report)).selectable(true))
                        .on_hover_text(report.detail.as_deref().unwrap_or("No additional detail"));
                }
                if let Some(error) = &self.last_error.clone() {
                    widgets::banner(ui, theme::DANGER, icons::WARNING_CIRCLE, error, false);
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
        }
    }
}

fn header_columns(h: &mut egui_extras::TableRow<'_, '_>, sort: GameSort) -> Option<GameSort> {
    let mut requested = None;
    h.col(|_| {});
    for (label, key) in [
        ("Game", SortKey::Name),
        ("Store", SortKey::Store),
        ("DLLs", SortKey::Dlls),
        ("DLSS", SortKey::DlssVersion),
        ("Updates", SortKey::Upgrades),
        ("State", SortKey::State),
    ] {
        h.col(|ui| {
            requested = table::sort_header(ui, label, key, sort).or(requested.take());
        });
    }
    h.col(|ui| {
        ui.strong("Action");
    });
    requested
}

fn game_state_cell(ui: &mut egui::Ui, game: &GameRow, in_flight: bool) {
    if in_flight {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Updating…");
        });
        return;
    }
    let (icon, color) = if game.inspection_errors > 0 {
        (icons::WARNING_CIRCLE, theme::DANGER)
    } else if game.state == "Unknown" {
        (icons::QUESTION, theme::WARNING)
    } else if game.state == "No DLLs" {
        (icons::MINUS, theme::INFO)
    } else {
        (icons::CHECK_CIRCLE, theme::SUCCESS)
    };
    widgets::chip(ui, icon, &game.state, color);
    if game.last_operation != "Never" {
        ui.response().on_hover_text(&game.last_operation);
    }
}

fn game_row_action(
    ui: &mut egui::Ui,
    game: &GameRow,
    available: bool,
    latest_ready: bool,
    in_flight: bool,
) -> Option<ReviewIntent> {
    if in_flight || game.dlls == 0 {
        return None;
    }
    if latest_ready && game.upgrades == 0 && game.dlss_upgrades == 0 {
        widgets::chip(ui, icons::CHECK_CIRCLE, "Up to date", theme::SUCCESS);
        return None;
    }
    ui.add_enabled(
        available,
        egui::Button::new(widgets::icon_text(icons::SPARKLE, "Update")),
    )
    .on_hover_text("Review and update this game's DLSS DLLs")
    .clicked()
    .then(|| ReviewIntent::QuickDlss(vec![game.id.clone()]))
}

pub(crate) fn discovery_report_label(report: &dlss_core::StoreDiscoveryReport) -> String {
    let status = match report.status {
        dlss_core::DiscoveryStatus::Found => format!("found ({} games)", report.games_found),
        dlss_core::DiscoveryStatus::NotDetected => "not detected".into(),
        dlss_core::DiscoveryStatus::Error => "error".into(),
    };
    format!("{} — {status}", report.store)
}
