use super::theme::{self, icons};
use super::widgets;
use crate::ui::review::ReviewIntent;
use crate::{Command, DlssApp, GameFilter, StoreFilter};
use dlss_core::SystemToolState;
use eframe::egui;

/// Stable id for the search box, so a keyboard shortcut can focus it from
/// anywhere in the app.
pub(crate) fn search_field_id() -> egui::Id {
    egui::Id::new("game_search")
}

impl DlssApp {
    pub(crate) fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("DLSS Updater");
            if let Some(release) = &self.catalog_release {
                widgets::badge(ui, format!("Official {release}"), theme::ACCENT);
            } else if self.runtime.catalog_loading {
                ui.spinner();
                ui.weak("Loading catalog");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(widgets::icon(icons::INFO, 15.0, theme::TEXT_MUTED))
                    .on_hover_text("About DLSS Updater, logs, and data folders")
                    .clicked()
                {
                    self.open_windows.insert(super::super::AppWindow::About);
                }
                if ui
                    .button(widgets::icon_text(icons::WRENCH, "Tools"))
                    .on_hover_text("Machine-wide NVIDIA settings, such as the DLSS indicator")
                    .clicked()
                {
                    self.open_windows.insert(super::super::AppWindow::Tools);
                    self.refresh_tool_state();
                }
                if ui
                    .button(widgets::icon_text(icons::PACKAGE, "Releases"))
                    .on_hover_text("Official releases and imported DLLs to update from")
                    .clicked()
                {
                    self.open_windows.insert(super::super::AppWindow::Releases);
                }
                if ui
                    .button(widgets::icon_text(
                        icons::CLOCK_COUNTER_CLOCKWISE,
                        "Activity",
                    ))
                    .on_hover_text("Every change this app has made")
                    .clicked()
                {
                    self.open_windows.insert(super::super::AppWindow::Activity);
                }
                if ui
                    .button(widgets::icon_text(icons::FOLDER_SIMPLE, "Game folders"))
                    .on_hover_text("Where games are discovered, and folders you added yourself")
                    .clicked()
                {
                    self.open_windows.insert(super::super::AppWindow::Roots);
                }
                if matches!(
                    self.tool_state,
                    SystemToolState::DlssIndicatorDebug | SystemToolState::DlssIndicatorProduction
                ) {
                    widgets::chip(ui, icons::CIRCLE, "Indicator active", theme::WARNING);
                }
            });
        });
        self.toolbar_controls(ui);
    }

    /// The "which games" and "which store" narrowing selectors.
    fn filter_combos(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_id_salt("game_filter")
            .selected_text(match self.filter_mode {
                GameFilter::All => "All games",
                GameFilter::HasDlls => "Has DLLs",
                GameFilter::Upgrades => "Upgrades",
                GameFilter::Custom => "Custom",
                GameFilter::Errors => "Errors",
                GameFilter::Recent => "Recently changed",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filter_mode, GameFilter::All, "All games");
                ui.selectable_value(&mut self.filter_mode, GameFilter::HasDlls, "Has DLLs");
                ui.selectable_value(&mut self.filter_mode, GameFilter::Upgrades, "Upgrades");
                ui.selectable_value(&mut self.filter_mode, GameFilter::Custom, "Custom");
                ui.selectable_value(&mut self.filter_mode, GameFilter::Errors, "Errors");
                ui.selectable_value(
                    &mut self.filter_mode,
                    GameFilter::Recent,
                    "Recently changed",
                );
            });
        egui::ComboBox::from_id_salt("store_filter")
            .selected_text(match self.store_filter {
                StoreFilter::All => "All stores",
                StoreFilter::Steam => "Steam",
                StoreFilter::Epic => "Epic",
                StoreFilter::Gog => "GOG",
                StoreFilter::Manual => "Manual",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.store_filter, StoreFilter::All, "All stores");
                ui.selectable_value(&mut self.store_filter, StoreFilter::Steam, "Steam");
                ui.selectable_value(&mut self.store_filter, StoreFilter::Epic, "Epic");
                ui.selectable_value(&mut self.store_filter, StoreFilter::Gog, "GOG");
                ui.selectable_value(&mut self.store_filter, StoreFilter::Manual, "Manual");
            });
    }

    fn toolbar_controls(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_sized(
                [ui.available_width().min(320.0), 28.0],
                egui::TextEdit::singleline(&mut self.filter)
                    .id(search_field_id())
                    .hint_text(widgets::icon_text(
                        icons::MAGNIFYING_GLASS,
                        "Search games and stores…",
                    )),
            )
            .on_hover_text("Filter by game or store name (Ctrl+F)");
            if !self.filter.is_empty()
                && ui
                    .add(egui::Button::new(widgets::icon(
                        icons::X,
                        13.0,
                        theme::TEXT_MUTED,
                    )))
                    .on_hover_text("Clear search")
                    .clicked()
            {
                self.filter.clear();
            }
            self.filter_combos(ui);
            if ui
                .add_enabled(
                    !self.runtime.scanning,
                    egui::Button::new(widgets::icon_text(icons::ARROW_CLOCKWISE, "Rescan")),
                )
                .on_hover_text("Search every store and manual folder again (F5)")
                .clicked()
            {
                let _ = self.worker.commands.send(Command::Scan);
            }
            if self.runtime.scanning {
                ui.spinner();
                ui.weak("Scanning…");
            }
            let quick_ready = self.catalog_release.is_some()
                && !self.runtime.scanning
                && self.upgrading.is_none()
                && self.games.iter().any(|game| game.dlls > 0);
            if ui
                .add_enabled(
                    quick_ready,
                    egui::Button::new(widgets::icon_text(icons::SPARKLE, "Quick update DLSS")),
                )
                .clicked()
            {
                let ids = self
                    .games
                    .iter()
                    .filter(|game| game.dlls > 0)
                    .map(|game| game.id.clone())
                    .collect();
                self.open_review(ReviewIntent::QuickDlss(ids));
            }
            if self.discovery_reports.iter().any(|report| {
                report.games_found == 0
                    && matches!(
                        report.status,
                        dlss_core::DiscoveryStatus::NotDetected | dlss_core::DiscoveryStatus::Error
                    )
            }) && ui
                .button(widgets::icon_text(icons::WARNING, "Store warnings"))
                .clicked()
            {
                self.open_windows
                    .insert(super::super::AppWindow::StoreWarnings);
            }
        });
    }
}
