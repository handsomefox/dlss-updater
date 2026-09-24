//! The bar along the bottom of the window.
//!
//! It is always there, at one height, so selecting a game or staging a
//! change swaps its contents instead of sliding a panel in and resizing the
//! table above. In the library it shows a summary, or the actions for the
//! selected games; on a game page it shows the staged changes.

use super::theme::{self, icons};
use super::toast::plural;
use super::widgets;
use crate::ui::review::ReviewIntent;
use crate::{DlssApp, View};
use eframe::egui;

pub(crate) const FOOTER_HEIGHT: f32 = 48.0;

impl DlssApp {
    pub(crate) fn footer(&mut self, root: &mut egui::Ui) {
        egui::Panel::bottom("footer")
            .exact_size(FOOTER_HEIGHT)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_PANEL)
                    .inner_margin(egui::Margin::symmetric(14, 0)),
            )
            .show(root, |ui| {
                let game = match &self.view {
                    View::Library => None,
                    View::Game(id) => Some(id.clone()),
                };
                ui.horizontal_centered(|ui| match game {
                    None => self.library_footer(ui),
                    Some(id) => self.game_footer(ui, &id),
                });
            });
    }

    fn library_footer(&mut self, ui: &mut egui::Ui) {
        let selected: Vec<_> = self
            .games
            .iter()
            .filter(|game| game.selected)
            .map(|game| game.id.clone())
            .collect();
        if selected.is_empty() {
            self.library_summary(ui);
        } else {
            self.selection_actions(ui, selected);
        }
    }

    fn library_summary(&mut self, ui: &mut egui::Ui) {
        if self.runtime.scanning {
            ui.spinner();
            ui.label(egui::RichText::new("Scanning your games…").color(theme::TEXT_MUTED));
        } else {
            let with_dlls = self.games.iter().filter(|game| game.dlls > 0).count();
            let mut summary = format!(
                "{} · {with_dlls} with NVIDIA DLLs",
                plural(self.games.len(), "game")
            );
            if self.latest_release_ready() {
                let updates = self
                    .games
                    .iter()
                    .filter(|game| game.dlss_upgrades > 0)
                    .count();
                summary = format!("{summary} · {updates} with DLSS updates");
            }
            ui.label(egui::RichText::new(summary).color(theme::TEXT_MUTED));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            self.undo_last_change_button(ui);
            if !self.games.is_empty() && self.last_change.is_empty() {
                ui.label(
                    egui::RichText::new("Click a game for details · Ctrl+A selects all")
                        .color(theme::TEXT_FAINT)
                        .size(12.0),
                );
            }
        });
    }

    fn selection_actions(&mut self, ui: &mut egui::Ui, selected: Vec<dlss_core::GameId>) {
        let visible = self.filtered_game_rows();
        let hidden = selected
            .iter()
            .filter(|id| !visible.iter().any(|&index| &self.games[index].id == *id))
            .count();
        ui.label(
            egui::RichText::new(format!("{} selected", plural(selected.len(), "game"))).strong(),
        );
        if hidden > 0 {
            ui.label(
                egui::RichText::new(format!("({hidden} hidden by filters)"))
                    .color(theme::TEXT_MUTED),
            )
            .on_hover_text("Selected games stay selected when a filter hides them");
        }
        ui.add_space(8.0);
        let available = self.catalog_release.is_some() && !self.busy();
        let reason = if self.busy() {
            "Wait for the current update to finish"
        } else {
            "The release catalog has not loaded yet"
        };
        if ui
            .add_enabled(
                available,
                widgets::primary_when(available, icons::SPARKLE, "Update DLSS"),
            )
            .on_hover_text("Review DLSS updates for the selected games")
            .on_disabled_hover_text(reason)
            .clicked()
        {
            self.open_review(ReviewIntent::QuickDlss(selected.clone()));
        }
        if ui
            .add_enabled(
                available,
                egui::Button::new(widgets::icon_text(icons::STACK, "Update all DLLs")),
            )
            .on_hover_text(
                "Review updates for every NVIDIA DLL in the selected games, \
                 including Streamline and Reflex",
            )
            .on_disabled_hover_text(reason)
            .clicked()
        {
            self.open_review(ReviewIntent::AllDlls(selected));
        }
        if ui.button("Clear selection").on_hover_text("Esc").clicked() {
            self.set_selection(false, None);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new("Shift-click selects a range")
                    .color(theme::TEXT_FAINT)
                    .size(12.0),
            );
        });
    }

    fn undo_last_change_button(&mut self, ui: &mut egui::Ui) {
        if self.last_change.is_empty() {
            return;
        }
        let names: Vec<_> = self
            .last_change
            .iter()
            .map(|id| self.game_name(id))
            .collect();
        let label = if self.last_change.len() == 1 {
            "Undo last update".to_owned()
        } else {
            format!("Undo last update ({} games)", self.last_change.len())
        };
        if ui
            .add_enabled(
                !self.busy(),
                egui::Button::new(widgets::icon_text(icons::ARROW_U_UP_LEFT, &label)),
            )
            .on_hover_text(format!(
                "Put back the DLLs this app replaced in {}",
                names.join(", ")
            ))
            .clicked()
        {
            let games = std::mem::take(&mut self.last_change);
            self.start_undo(games);
        }
    }

    fn game_footer(&mut self, ui: &mut egui::Ui, game_id: &dlss_core::GameId) {
        let staged = self.staged_targets_for(game_id);
        if staged == 0 {
            ui.label(
                egui::RichText::new(
                    "Choose a version next to any DLL to stage a change. \
                     Nothing is written until you review and apply it.",
                )
                .color(theme::TEXT_MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.undoable.contains(game_id)
                    && ui
                        .add_enabled(
                            !self.busy(),
                            egui::Button::new(widgets::icon_text(
                                icons::ARROW_U_UP_LEFT,
                                "Undo last update",
                            )),
                        )
                        .on_hover_text("Put back the DLLs the last update replaced in this game")
                        .clicked()
                {
                    self.start_undo(vec![game_id.clone()]);
                }
            });
            return;
        }
        ui.label(widgets::icon(icons::LIST_CHECKS, 16.0, theme::ACCENT));
        ui.label(egui::RichText::new(format!("{} staged", plural(staged, "change"))).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    !self.busy(),
                    widgets::primary_when(!self.busy(), icons::LIST_CHECKS, "Review and apply"),
                )
                .on_disabled_hover_text("Wait for the current update to finish")
                .clicked()
            {
                self.open_review(ReviewIntent::Profiles(vec![game_id.clone()]));
            }
            if ui
                .button("Discard")
                .on_hover_text("Forget the staged versions for this game")
                .clicked()
            {
                self.clear_game_profile(game_id);
            }
        });
    }
}
