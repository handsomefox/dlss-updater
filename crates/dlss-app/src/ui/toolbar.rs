use super::library::has_discovery_errors;
use super::theme::{self, icons};
use super::widgets;
use crate::ui::review::ReviewIntent;
use crate::{AppWindow, Command, DlssApp, GameFilter, StoreFilter, View};
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
            ui.label(egui::RichText::new("DLSS Updater").font(egui::FontId::new(
                20.0,
                egui::FontFamily::Name("semibold".into()),
            )));
            ui.add_space(4.0);
            self.release_status(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.window_buttons(ui);
            });
        });
        ui.add_space(8.0);
        if matches!(self.view, View::Library) {
            self.library_controls(ui);
        }
    }

    /// Which official release the app compares against, and whether it is
    /// downloaded. Clicking it opens the DLL sources.
    fn release_status(&mut self, ui: &mut egui::Ui) {
        let ready = self.latest_release_ready();
        let (icon, text, color, hover) = match (&self.catalog_release, ready) {
            (Some(tag), true) => (
                icons::CHECK_CIRCLE,
                format!("Streamline {tag}"),
                theme::ACCENT,
                "The latest official release is downloaded and verified",
            ),
            (Some(tag), false) => (
                icons::DOWNLOAD_SIMPLE,
                format!("Streamline {tag} · not downloaded"),
                theme::WARNING,
                "Download the latest official release to check your games",
            ),
            (None, _) if self.runtime.catalog_loading => {
                ui.spinner();
                ui.label(egui::RichText::new("Checking for releases…").color(theme::TEXT_MUTED));
                return;
            }
            (None, _) => (
                icons::WARNING_CIRCLE,
                "No release catalog".to_owned(),
                theme::DANGER,
                "The release list could not be loaded",
            ),
        };
        let chip = egui::Button::new(widgets::colored_icon_label(icon, &text, color, 12.5))
            .fill(color.gamma_multiply(0.14))
            .stroke(egui::Stroke::NONE)
            .corner_radius(egui::CornerRadius::same(10))
            .min_size(egui::vec2(0.0, 24.0));
        if ui
            .add(chip)
            .on_hover_text(format!("{hover}. Click to manage DLL sources."))
            .clicked()
        {
            self.open_windows.insert(AppWindow::Releases);
        }
        if self.runtime.catalog_loading {
            ui.spinner()
                .on_hover_text("Checking GitHub for a newer release");
        }
    }

    /// Right-to-left, so the first button added sits at the far right.
    fn window_buttons(&mut self, ui: &mut egui::Ui) {
        if ui
            .button(widgets::icon(icons::INFO, 16.0, theme::TEXT_MUTED))
            .on_hover_text("About DLSS Updater, logs, and data folders")
            .clicked()
        {
            self.open_windows.insert(AppWindow::About);
        }
        if ui
            .button(widgets::icon_text(icons::WRENCH, "Tools"))
            .on_hover_text("Machine-wide NVIDIA settings, such as the DLSS indicator")
            .clicked()
        {
            self.open_windows.insert(AppWindow::Tools);
            self.refresh_tool_state();
            self.staged_tool_state = self.tool_state.clone();
        }
        if ui
            .button(widgets::icon_text(icons::PACKAGE, "DLL sources"))
            .on_hover_text("Official releases and imported DLLs to update from")
            .clicked()
        {
            self.open_windows.insert(AppWindow::Releases);
        }
        if ui
            .button(widgets::icon_text(
                icons::CLOCK_COUNTER_CLOCKWISE,
                "Activity",
            ))
            .on_hover_text("Every change this app has made")
            .clicked()
        {
            self.open_windows.insert(AppWindow::Activity);
        }
        let store_error = has_discovery_errors(&self.discovery_reports);
        let folders = if store_error {
            egui::Button::new(widgets::colored_icon_label(
                icons::WARNING,
                "Game folders",
                theme::WARNING,
                14.0,
            ))
        } else {
            egui::Button::new(widgets::icon_text(icons::FOLDER_SIMPLE, "Game folders"))
        };
        if ui
            .add(folders)
            .on_hover_text(if store_error {
                "A store or folder could not be read. Open for details."
            } else {
                "Where games are found, and folders you added yourself"
            })
            .clicked()
        {
            self.open_windows.insert(AppWindow::Roots);
        }
        if matches!(
            self.tool_state,
            SystemToolState::DlssIndicatorDebug | SystemToolState::DlssIndicatorProduction
        ) {
            widgets::chip(ui, icons::CIRCLE, "Indicator on", theme::WARNING);
        }
    }

    fn library_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.search_box(ui);
            ui.add_space(4.0);
            self.filter_tabs(ui);
            self.store_combo(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.update_all_button(ui);
                let rescan = if self.runtime.scanning {
                    "Scanning…"
                } else {
                    "Rescan"
                };
                if ui
                    .add_enabled(
                        !self.runtime.scanning,
                        egui::Button::new(widgets::icon_text(icons::ARROW_CLOCKWISE, rescan)),
                    )
                    .on_hover_text("Look for games again (F5)")
                    .clicked()
                {
                    let _ = self.worker.commands.send(Command::Scan);
                }
            });
        });
    }

    fn search_box(&mut self, ui: &mut egui::Ui) {
        let response = ui.add_sized(
            [240.0, 30.0],
            egui::TextEdit::singleline(&mut self.filter)
                .id(search_field_id())
                .margin(egui::Margin::symmetric(8, 6))
                .hint_text(widgets::icon_text(icons::MAGNIFYING_GLASS, "Search games")),
        );
        response.on_hover_text("Ctrl+F");
        if !self.filter.is_empty()
            && ui
                .add(
                    egui::Button::new(widgets::icon(icons::X, 13.0, theme::TEXT_MUTED))
                        .frame(false),
                )
                .on_hover_text("Clear search")
                .clicked()
        {
            self.filter.clear();
        }
    }

    /// The library tabs, each with its count. Staged, Problems, and Changed
    /// only appear when they have something in them, or are selected.
    fn filter_tabs(&mut self, ui: &mut egui::Ui) {
        let counts = self.tab_counts();
        egui::Frame::new()
            .fill(theme::BG_APP)
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (tab, count) in counts {
                    let always = matches!(
                        tab,
                        GameFilter::Updates | GameFilter::WithDlls | GameFilter::All
                    );
                    if !always && count == 0 && self.filter_mode != tab {
                        continue;
                    }
                    let (label, hover) = tab_label(tab);
                    let selected = self.filter_mode == tab;
                    let text = widgets::label_with_count(label, count, selected);
                    if ui
                        .add(
                            egui::Button::selectable(selected, text)
                                .min_size(egui::vec2(0.0, 26.0)),
                        )
                        .on_hover_text(hover)
                        .clicked()
                    {
                        self.filter_mode = tab;
                    }
                }
            });
    }

    fn store_combo(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_id_salt("store_filter")
            .selected_text(match self.store_filter {
                StoreFilter::All => "All stores",
                StoreFilter::Steam => "Steam",
                StoreFilter::Epic => "Epic",
                StoreFilter::Gog => "GOG",
                StoreFilter::Manual => "Added by you",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.store_filter, StoreFilter::All, "All stores");
                ui.selectable_value(&mut self.store_filter, StoreFilter::Steam, "Steam");
                ui.selectable_value(&mut self.store_filter, StoreFilter::Epic, "Epic");
                ui.selectable_value(&mut self.store_filter, StoreFilter::Gog, "GOG");
                ui.selectable_value(&mut self.store_filter, StoreFilter::Manual, "Added by you");
            });
    }

    /// Reviews a DLSS update for every game that has one. The count on the
    /// button is the number of games the review will list, so the two agree.
    /// Before the latest release is downloaded nothing can be counted, and
    /// the review downloads it first.
    fn update_all_button(&mut self, ui: &mut egui::Ui) {
        let ready = self.latest_release_ready();
        let ids: Vec<_> = self
            .games
            .iter()
            .filter(|game| {
                if ready {
                    game.dlss_upgrades > 0
                } else {
                    game.dlls > 0
                }
            })
            .map(|game| game.id.clone())
            .collect();
        let label = if ready && !ids.is_empty() {
            format!("Update DLSS in {} games", ids.len())
        } else {
            "Update DLSS".to_owned()
        };
        let enabled = self.catalog_release.is_some()
            && !self.runtime.scanning
            && !self.busy()
            && !ids.is_empty();
        let hover = if self.busy() {
            "Wait for the current update to finish"
        } else if ready && ids.is_empty() {
            "Every game already has the latest DLSS"
        } else if self.catalog_release.is_none() {
            "The release catalog has not loaded yet"
        } else {
            "Review DLSS updates for every game at once. Streamline and Reflex are left alone."
        };
        if ui
            .add_enabled(
                enabled,
                widgets::primary_icon_button(icons::SPARKLE, &label),
            )
            .on_hover_text(hover)
            .on_disabled_hover_text(hover)
            .clicked()
        {
            self.open_review(ReviewIntent::QuickDlss(ids));
        }
    }
}

const fn tab_label(tab: GameFilter) -> (&'static str, &'static str) {
    match tab {
        GameFilter::Updates => ("Updates", "Games with a newer official DLL"),
        GameFilter::WithDlls => (
            "With DLLs",
            "Games that ship DLSS, Streamline, or Reflex DLLs",
        ),
        GameFilter::All => (
            "All",
            "Every game found, including those without NVIDIA DLLs",
        ),
        GameFilter::Staged => ("Staged", "Games with version changes waiting to be applied"),
        GameFilter::Problems => (
            "Problems",
            "Games with files that could not be read or have no version",
        ),
        GameFilter::Recent => ("Changed", "Games updated or restored this session"),
    }
}
