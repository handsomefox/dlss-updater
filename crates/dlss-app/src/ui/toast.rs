//! Short notices in the bottom-right corner, above the footer.
//!
//! A toast reports progress or a result. Failures go to the error banner
//! instead, because a failure must stay on screen until the user has read it.

use super::footer::FOOTER_HEIGHT;
use super::theme::{self, icons};
use super::widgets;
use crate::DlssApp;
use eframe::egui;
use std::time::{Duration, Instant};

/// How long a finished-operation toast stays up while the pointer is away.
const RESULT_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToastKind {
    /// Work in progress. Stays up until the work reports a result.
    Progress,
    Success,
    /// A result worth a second look, such as a run where some DLLs failed.
    Info,
}

pub(crate) struct Toast {
    message: String,
    kind: ToastKind,
    shown: Instant,
    /// Show an Undo button that reverts [`DlssApp::last_change`].
    pub(crate) offers_undo: bool,
}

impl Toast {
    fn expired(&self, now: Instant) -> bool {
        self.kind != ToastKind::Progress && now.duration_since(self.shown) >= RESULT_TIMEOUT
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BatchKind {
    Update,
    Undo,
}

impl BatchKind {
    pub(crate) const fn verb_ing(self) -> &'static str {
        match self {
            Self::Update => "Updating",
            Self::Undo => "Undoing changes in",
        }
    }
}

/// One user action that the worker carries out game by game.
pub(crate) struct Batch {
    pub(crate) kind: BatchKind,
    pub(crate) total: usize,
    pub(crate) done: usize,
    pub(crate) changed: usize,
    pub(crate) failed: usize,
    pub(crate) games_changed: usize,
    /// Games whose operation failed outright, before any DLL was tried.
    pub(crate) failed_games: usize,
    pub(crate) undoable: Vec<dlss_core::GameId>,
    single_name: Option<String>,
}

impl Batch {
    pub(crate) const fn new(kind: BatchKind, total: usize, single_name: Option<String>) -> Self {
        Self {
            kind,
            total,
            done: 0,
            changed: 0,
            failed: 0,
            games_changed: 0,
            failed_games: 0,
            undoable: Vec::new(),
            single_name,
        }
    }

    /// The one line that reports the whole run.
    pub(crate) fn summary(&self) -> String {
        let undo = self.kind == BatchKind::Undo;
        let mut summary = operation_summary(undo, self.changed, self.failed);
        if let Some(name) = &self.single_name {
            summary = format!("{name}: {}", lower_first(&summary));
        } else if self.games_changed > 0 {
            summary = format!("{summary} across {}", plural(self.games_changed, "game"));
        }
        if self.failed_games > 0 {
            summary = format!(
                "{summary}. {} could not be changed; see the error above",
                plural(self.failed_games, "game")
            );
        }
        summary
    }
}

/// "Updated 3 DLLs", "Restored 1 DLL, 1 failed", or "Nothing needed changing".
pub(crate) fn operation_summary(undo: bool, changed: usize, failed: usize) -> String {
    let mut summary = match (changed, undo) {
        (0, _) if failed == 0 => "Nothing needed changing".to_owned(),
        (0, false) => "No DLLs updated".to_owned(),
        (0, true) => "No DLLs restored".to_owned(),
        (count, false) => format!("Updated {}", plural(count, "DLL")),
        (count, true) => format!("Restored {}", plural(count, "DLL")),
    };
    if failed > 0 {
        summary = format!("{summary}, {failed} failed");
    }
    summary
}

/// "Updated 2 DLLs" becomes "updated 2 DLLs", for use after a game name.
pub(crate) fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

pub(crate) fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

impl DlssApp {
    /// Replaces the current toast. Repeating the same message keeps its
    /// timer, so a progress update does not restart the countdown.
    pub(crate) fn notify(&mut self, kind: ToastKind, message: impl Into<String>) {
        let message = message.into();
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.kind == kind && toast.message == message)
        {
            return;
        }
        self.toast = Some(Toast {
            message,
            kind,
            shown: Instant::now(),
            offers_undo: false,
        });
    }

    pub(crate) fn show_toast(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self.toast.as_ref().is_some_and(|toast| toast.expired(now)) {
            self.toast = None;
        }
        let busy = self.busy();
        let Some(toast) = &mut self.toast else {
            return;
        };
        let mut dismiss = false;
        let mut undo = false;
        let can_undo = toast.offers_undo && !self.last_change.is_empty() && !busy;
        let response = egui::Area::new("toast".into())
            .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -(FOOTER_HEIGHT + 12.0)])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (icon, color) = match toast.kind {
                    ToastKind::Progress => (None, theme::ACCENT),
                    ToastKind::Success => (Some(icons::CHECK_CIRCLE), theme::SUCCESS),
                    ToastKind::Info => (Some(icons::INFO), theme::WARNING),
                };
                egui::Frame::new()
                    .fill(theme::BG_CARD)
                    .stroke(egui::Stroke::new(1.0, theme::STROKE))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .shadow(ui.style().visuals.popup_shadow)
                    .show(ui, |ui| {
                        ui.set_max_width(440.0);
                        ui.horizontal(|ui| {
                            match icon {
                                Some(icon) => {
                                    ui.label(widgets::icon(icon, 16.0, color));
                                }
                                None => {
                                    ui.add(egui::Spinner::new().size(14.0).color(color));
                                }
                            }
                            ui.add(egui::Label::new(&toast.message).wrap());
                            if can_undo {
                                undo = ui
                                    .add(widgets::primary_icon_button(
                                        icons::ARROW_U_UP_LEFT,
                                        "Undo",
                                    ))
                                    .clicked();
                            }
                            if toast.kind != ToastKind::Progress {
                                dismiss = ui
                                    .add(
                                        egui::Button::new(widgets::icon(
                                            icons::X,
                                            12.0,
                                            theme::TEXT_MUTED,
                                        ))
                                        .frame(false),
                                    )
                                    .on_hover_text("Dismiss")
                                    .clicked();
                            }
                        });
                    });
            })
            .response;
        // Reading a toast should not race its timer.
        if response.contains_pointer() {
            toast.shown = now;
        }
        if toast.kind != ToastKind::Progress {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if dismiss || undo {
            self.toast = None;
        }
        if undo {
            let games = std::mem::take(&mut self.last_change);
            self.start_undo(games);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_count_dlls_and_games() {
        assert_eq!(operation_summary(false, 1, 0), "Updated 1 DLL");
        assert_eq!(operation_summary(false, 3, 1), "Updated 3 DLLs, 1 failed");
        assert_eq!(operation_summary(true, 2, 0), "Restored 2 DLLs");
        assert_eq!(operation_summary(false, 0, 0), "Nothing needed changing");
        assert_eq!(operation_summary(false, 0, 2), "No DLLs updated, 2 failed");

        let mut single = Batch::new(BatchKind::Update, 1, Some("Control".into()));
        single.changed = 2;
        single.games_changed = 1;
        assert_eq!(single.summary(), "Control: updated 2 DLLs");

        let mut bulk = Batch::new(BatchKind::Update, 3, None);
        bulk.changed = 5;
        bulk.games_changed = 2;
        bulk.failed_games = 1;
        assert_eq!(
            bulk.summary(),
            "Updated 5 DLLs across 2 games. 1 game could not be changed; see the error above"
        );
    }

    #[test]
    fn only_results_expire() {
        let shown = Instant::now();
        let toast = |kind| Toast {
            message: String::new(),
            kind,
            shown,
            offers_undo: false,
        };
        let later = shown + RESULT_TIMEOUT;
        assert!(toast(ToastKind::Success).expired(later));
        assert!(toast(ToastKind::Info).expired(later));
        assert!(!toast(ToastKind::Progress).expired(later));
        assert!(!toast(ToastKind::Success).expired(shown));
    }
}
