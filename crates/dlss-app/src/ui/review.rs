//! Mandatory review dialog: previews every planned DLL swap as a checkable
//! row, then converts the checked rows into a pinned `TargetProfile` that the
//! worker re-validates end to end via `plan_target_profile`.

use super::theme::{self, icons};
use super::widgets;
use crate::ui::detail::dll_kind_icon;
use crate::{Command, DlssApp};
use eframe::egui;

pub(crate) enum ReviewIntent {
    /// DLSS-family DLLs only; Streamline and Reflex are never touched.
    QuickDlss(Vec<dlss_core::GameId>),
    /// Strictly newer, same-named official DLLs across every managed kind.
    AllDlls(Vec<dlss_core::GameId>),
    /// The staged per-DLL "Desired" targets for these games.
    Profiles(Vec<dlss_core::GameId>),
}

pub(crate) struct ReviewRow {
    game_id: dlss_core::GameId,
    game_name: String,
    dll_id: dlss_core::DllInstallationId,
    file_name: std::ffi::OsString,
    installed: Option<dlss_core::DllVersion>,
    target: Option<dlss_core::DllVersion>,
    /// Pinned target applied when this row stays checked. For quick updates
    /// this is `Cached { release, sha256 }` of the exact previewed bytes.
    desired: dlss_core::DesiredDll,
    comparison: dlss_core::Comparison,
    checked: bool,
}

impl ReviewRow {
    fn is_streamline(&self) -> bool {
        dlss_core::DllKind::classify(&self.file_name) == Some(dlss_core::DllKind::Streamline)
    }
}

pub(crate) struct ReviewState {
    intent: ReviewIntent,
    rows: Vec<ReviewRow>,
    errors: Vec<String>,
    /// False while the latest official release still needs to be downloaded
    /// before the swaps can be previewed.
    ready: bool,
}

impl DlssApp {
    pub(crate) fn open_review(&mut self, intent: ReviewIntent) {
        let (rows, errors, ready) = self.build_review_rows(&intent);
        self.review = Some(ReviewState {
            intent,
            rows,
            errors,
            ready,
        });
    }

    fn build_review_rows(&self, intent: &ReviewIntent) -> (Vec<ReviewRow>, Vec<String>, bool) {
        match intent {
            ReviewIntent::QuickDlss(ids) | ReviewIntent::AllDlls(ids) => {
                let Some(release) = self.latest_release() else {
                    return (Vec::new(), Vec::new(), false);
                };
                let release_id = release.metadata.id.clone();
                let latest = release.dlls.clone();
                let mut rows = Vec::new();
                for game_id in ids {
                    let Some(game) = self.games.iter().find(|game| &game.id == game_id) else {
                        continue;
                    };
                    let plan = match intent {
                        ReviewIntent::QuickDlss(_) => {
                            dlss_core::plan_dlss_only_upgrades("preview", &game.details, &latest)
                        }
                        _ => dlss_core::plan_strict_upgrades("preview", &game.details, &latest),
                    };
                    for swap in plan.swaps {
                        let Some(installed) =
                            game.details.iter().find(|dll| dll.id == swap.installation)
                        else {
                            continue;
                        };
                        rows.push(ReviewRow {
                            game_id: game.id.clone(),
                            game_name: game.name.clone(),
                            dll_id: swap.installation.clone(),
                            file_name: installed.file_name.clone(),
                            installed: installed.metadata.version,
                            target: latest
                                .iter()
                                .find(|candidate| candidate.sha256 == swap.source_sha256)
                                .map(|candidate| candidate.version),
                            desired: dlss_core::DesiredDll::Cached {
                                release: release_id.clone(),
                                sha256: swap.source_sha256,
                            },
                            comparison: swap.comparison,
                            checked: true,
                        });
                    }
                }
                (rows, Vec::new(), true)
            }
            ReviewIntent::Profiles(ids) => {
                let mut rows = Vec::new();
                let mut errors = Vec::new();
                for game_id in ids {
                    let Some(game) = self.games.iter().find(|game| &game.id == game_id) else {
                        continue;
                    };
                    match self.preview_profile(game_id) {
                        Ok(plan) => {
                            for swap in plan.swaps {
                                let Some(installed) =
                                    game.details.iter().find(|dll| dll.id == swap.installation)
                                else {
                                    continue;
                                };
                                let desired = self
                                    .persisted
                                    .target_profile
                                    .targets
                                    .get(&swap.installation)
                                    .cloned()
                                    .unwrap_or(dlss_core::DesiredDll::KeepInstalled);
                                rows.push(ReviewRow {
                                    game_id: game.id.clone(),
                                    game_name: game.name.clone(),
                                    dll_id: swap.installation.clone(),
                                    file_name: installed.file_name.clone(),
                                    installed: installed.metadata.version,
                                    target: self.version_for_sha(swap.source_sha256),
                                    desired,
                                    comparison: swap.comparison,
                                    checked: true,
                                });
                            }
                        }
                        Err(error) => {
                            errors.push(format!(
                                "{}: {}",
                                game.name,
                                profile_preview_error_label(&error)
                            ));
                        }
                    }
                }
                (rows, errors, true)
            }
        }
    }

    /// Version of a known source (release, import, or backup) by content hash.
    fn version_for_sha(&self, sha256: [u8; 32]) -> Option<dlss_core::DllVersion> {
        self.releases
            .iter()
            .flat_map(|release| release.dlls.iter())
            .find(|candidate| candidate.sha256 == sha256)
            .map(|candidate| candidate.version)
            .or_else(|| {
                self.imports
                    .iter()
                    .find(|record| record.sha256 == sha256)
                    .map(|record| record.version)
            })
            .or_else(|| {
                self.backups
                    .iter()
                    .find(|backup| backup.sha256 == sha256)
                    .and_then(|backup| backup.version)
            })
    }

    pub(crate) fn review_window(&mut self, ctx: &egui::Context) {
        let Some(mut review) = self.review.take() else {
            return;
        };
        // The dialog opened before the latest release was downloaded; build
        // the swap preview as soon as the release turns Ready.
        if !review.ready && self.latest_release_ready() {
            let (rows, errors, ready) = self.build_review_rows(&review.intent);
            review.rows = rows;
            review.errors = errors;
            review.ready = ready;
        }
        let mut keep_open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("Review changes")
            .collapsible(false)
            .resizable(false)
            .open(&mut keep_open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(560.0)
            .show(ctx, |ui| {
                review_warnings(ui, &review);
                ui.add_space(6.0);
                if review.ready {
                    review_row_list(ui, &mut review);
                } else {
                    self.review_download_state(ui);
                }
                for error in &review.errors {
                    widgets::banner(ui, theme::DANGER, icons::WARNING_CIRCLE, error, false);
                }
                ui.add_space(4.0);
                ui.weak(
                    "If Windows denies access to a target, the app will request elevation \
                     only for the denied replacements.",
                );
                ui.add_space(8.0);
                let checked = review.rows.iter().filter(|row| row.checked).count();
                ui.horizontal(|ui| {
                    let label = if checked == 1 {
                        "Apply 1 change".to_owned()
                    } else {
                        format!("Apply {checked} changes")
                    };
                    let primary =
                        egui::Button::new(egui::RichText::new(label).color(egui::Color32::BLACK))
                            .fill(theme::ACCENT);
                    apply = ui.add_enabled(checked > 0, primary).clicked();
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            keep_open = false;
        }
        if apply {
            self.apply_review(&review);
            keep_open = false;
        }
        if keep_open {
            self.review = Some(review);
        }
    }

    /// Shown while the latest official release still needs downloading.
    fn review_download_state(&mut self, ui: &mut egui::Ui) {
        widgets::section_heading(ui, icons::DOWNLOAD_SIMPLE, "Download required");
        ui.label(
            "The latest official release must be downloaded and validated before \
             the changes can be previewed.",
        );
        let Some(release) = self.latest_release_meta().cloned() else {
            widgets::banner(
                ui,
                theme::DANGER,
                icons::WARNING_CIRCLE,
                "The latest release is missing from the catalog. Refresh the catalog and retry.",
                false,
            );
            return;
        };
        if let Some(error) = self.release_errors.get(&release.metadata.id).cloned() {
            widgets::banner(ui, theme::DANGER, icons::WARNING_CIRCLE, &error, false);
        }
        let busy = self.inspecting_release.is_some();
        if busy {
            let fraction = self
                .release_progress
                .as_ref()
                .filter(|(id, _, _)| *id == release.metadata.id)
                .and_then(|(_, received, total)| {
                    let total = (*total)?;
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "progress display only needs coarse precision"
                    )]
                    (total > 0).then(|| *received as f32 / total as f32)
                });
            match fraction {
                Some(fraction) => {
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(ui.available_width())
                            .show_percentage(),
                    );
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Validating official DLLs…");
                    });
                }
            }
        } else if ui
            .button(format!(
                "{} Download and validate {}",
                icons::DOWNLOAD_SIMPLE,
                release.metadata.tag
            ))
            .clicked()
        {
            self.inspecting_release = Some(release.metadata.id.clone());
            let _ = self
                .worker
                .commands
                .send(Command::InspectRelease(release.metadata.id));
        }
    }

    /// Converts the checked rows into one pinned profile per game and hands
    /// them to the worker, which re-plans and re-validates everything.
    fn apply_review(&mut self, review: &ReviewState) {
        let mut per_game: std::collections::BTreeMap<dlss_core::GameId, dlss_core::TargetProfile> =
            std::collections::BTreeMap::new();
        for row in review.rows.iter().filter(|row| row.checked) {
            per_game
                .entry(row.game_id.clone())
                .or_default()
                .targets
                .insert(row.dll_id.clone(), row.desired.clone());
        }
        for (game_id, profile) in per_game {
            self.profiles_applying
                .insert(game_id.clone(), profile.targets.keys().cloned().collect());
            let _ = self
                .worker
                .commands
                .send(Command::ApplyProfile(game_id, profile));
        }
        self.toast = Some("Operation queued…".into());
    }
}

fn review_warnings(ui: &mut egui::Ui, review: &ReviewState) {
    widgets::banner(
        ui,
        theme::WARNING,
        icons::WARNING,
        "Anti-cheat may treat DLL swaps as tampering.",
        false,
    );
    if matches!(review.intent, ReviewIntent::QuickDlss(_)) {
        ui.weak("Streamline and Reflex DLLs are never touched by Quick update.");
    } else if review
        .rows
        .iter()
        .any(|row| row.checked && row.is_streamline())
    {
        widgets::banner(
            ui,
            theme::DANGER,
            icons::WARNING,
            "Streamline replacements can reduce performance or crash games.",
            false,
        );
    }
    ui.weak("Each DLL is re-inspected, backed up, replaced independently, and verified.");
}

fn review_row_list(ui: &mut egui::Ui, review: &mut ReviewState) {
    if review.rows.is_empty() {
        if review.errors.is_empty() {
            widgets::chip(
                ui,
                icons::CHECK_CIRCLE,
                "Everything is already up to date.",
                theme::SUCCESS,
            );
        }
        return;
    }
    let many_games = {
        let first = &review.rows[0].game_id;
        review.rows.iter().any(|row| &row.game_id != first)
    };
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            let mut previous_game: Option<dlss_core::GameId> = None;
            // Rows arrive grouped per game, so a plain run-length header works.
            for row in &mut review.rows {
                if many_games && previous_game.as_ref() != Some(&row.game_id) {
                    ui.add_space(4.0);
                    ui.strong(&row.game_name);
                }
                previous_game = Some(row.game_id.clone());
                review_row(ui, row);
            }
        });
}

fn review_row(ui: &mut egui::Ui, row: &mut ReviewRow) {
    ui.horizontal(|ui| {
        ui.checkbox(&mut row.checked, "");
        let kind = dlss_core::DllKind::classify(&row.file_name);
        ui.label(widgets::icon(dll_kind_icon(kind), 15.0, theme::ACCENT));
        ui.label(dlss_core::friendly_dll_label(&row.file_name));
        ui.monospace(format!(
            "{} {} {}",
            version_text(row.installed),
            icons::ARROW_RIGHT,
            version_text(row.target),
        ));
        if row.comparison == dlss_core::Comparison::Downgrade {
            widgets::chip(ui, icons::CARET_DOWN, "Downgrade", theme::WARNING);
        }
        if row.is_streamline() {
            widgets::chip(ui, icons::WARNING, "Streamline", theme::WARNING);
        }
    });
}

fn version_text(version: Option<dlss_core::DllVersion>) -> String {
    version.map_or_else(|| "unknown".into(), |version| version.to_string())
}

pub(crate) fn profile_preview_error_label(error: &str) -> &'static str {
    if error.contains("desired source is unavailable") {
        "Required DLL source is unavailable. Download it or choose another target."
    } else if error.contains("unknown DLL installation") {
        "The selected DLL is no longer present. Rescan and review the target."
    } else {
        "The staged target cannot be applied."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_preview_errors_hide_internal_installation_ids() {
        let raw = "desired source is unavailable for DLL installation manual:00610062";
        assert_eq!(
            profile_preview_error_label(raw),
            "Required DLL source is unavailable. Download it or choose another target."
        );
        assert!(!profile_preview_error_label(raw).contains("manual:"));
    }
}
