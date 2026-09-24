//! Mandatory review dialog: previews every planned DLL swap as a checkable
//! row, then converts the checked rows into a pinned `TargetProfile` that the
//! worker re-validates end to end via `plan_target_profile`.

use super::theme::{self, icons};
use super::toast::plural;
use super::widgets::{self, TriState};
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
    known_risk: Option<&'static str>,
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
                            known_risk: game.known_risk,
                        });
                    }
                }
                order_rows(&mut rows);
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
                                    known_risk: game.known_risk,
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
                order_rows(&mut rows);
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
        let mut clicked = FooterClick::None;
        // Until the latest release is downloaded there is nothing to apply,
        // so the footer's primary slot downloads it instead.
        let footer = Footer {
            ready: review.ready,
            pending_download: self.latest_release_meta().map(|release| {
                (
                    release.metadata.tag.clone(),
                    self.release_errors.contains_key(&release.metadata.id),
                )
            }),
            downloading: self.inspecting_release.is_some(),
            busy: self.busy(),
        };
        // The body decides how many rows are checked and the pinned footer
        // renders from it, both within one frame, so the button label and its
        // enabled state never lag a checkbox click.
        let checked = std::cell::Cell::new(0_usize);
        let title = match review.intent {
            ReviewIntent::QuickDlss(_) => "Review DLSS updates",
            ReviewIntent::AllDlls(_) => "Review DLL updates",
            ReviewIntent::Profiles(_) => "Review staged changes",
        };
        widgets::modal_with_actions(
            ctx,
            widgets::dialog("review", icons::LIST_CHECKS, title, 640.0),
            &mut keep_open,
            |ui| {
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
                checked.set(review.rows.iter().filter(|row| row.checked).count());
            },
            // Pinned below the scrolling list: with many staged changes the
            // list is what scrolls, never the button that applies them.
            |ui| clicked = footer.show(ui, checked.get()),
        );
        let cancel = clicked == FooterClick::Cancel;
        let download = clicked == FooterClick::Download;
        let apply = clicked == FooterClick::Apply;
        if cancel {
            keep_open = false;
        }
        if download && let Some(release) = self.latest_release_meta() {
            let id = release.metadata.id.clone();
            self.inspecting_release = Some(id.clone());
            self.release_progress = None;
            let _ = self.worker.commands.send(Command::InspectRelease(id));
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
    /// The Download button itself sits in the footer, where Apply will be.
    fn review_download_state(&self, ui: &mut egui::Ui) {
        widgets::section_heading(ui, icons::DOWNLOAD_SIMPLE, "Download required");
        ui.label(
            egui::RichText::new(
                "The changes are listed here once the latest official release is \
                 downloaded and its signatures are checked.",
            )
            .color(theme::TEXT_MUTED),
        );
        ui.add_space(6.0);
        let Some(release) = self.latest_release_meta() else {
            widgets::banner(
                ui,
                theme::DANGER,
                icons::WARNING_CIRCLE,
                "The release list has not loaded. Use Check again in DLL sources, then retry.",
                false,
            );
            return;
        };
        if let Some(error) = self.release_errors.get(&release.metadata.id) {
            widgets::banner(ui, theme::DANGER, icons::WARNING_CIRCLE, error, false);
        }
        if self.inspecting_release.is_some() {
            let (state, received, total) = self.release_progress_now();
            ui.horizontal(|ui| widgets::download_progress(ui, state, received, total));
        }
    }

    /// Converts the checked rows into one pinned profile per game and hands
    /// them to the worker, which re-plans and re-validates everything.
    fn apply_review(&mut self, review: &ReviewState) {
        // One run at a time: a second batch would overwrite the counts of
        // the first and its summary would be wrong.
        if self.busy() {
            return;
        }
        let mut per_game: std::collections::BTreeMap<dlss_core::GameId, dlss_core::TargetProfile> =
            std::collections::BTreeMap::new();
        for row in review.rows.iter().filter(|row| row.checked) {
            per_game
                .entry(row.game_id.clone())
                .or_default()
                .targets
                .insert(row.dll_id.clone(), row.desired.clone());
        }
        let per_game_ids: Vec<_> = per_game.keys().cloned().collect();
        for (game_id, profile) in per_game {
            self.profiles_applying
                .insert(game_id.clone(), profile.targets.keys().cloned().collect());
            let _ = self
                .worker
                .commands
                .send(Command::ApplyProfile(game_id, profile));
        }
        let games: Vec<_> = per_game_ids;
        self.start_batch(crate::BatchKind::Update, &games);
    }
}

fn review_warnings(ui: &mut egui::Ui, review: &ReviewState) {
    widgets::banner(
        ui,
        theme::WARNING,
        icons::WARNING,
        "Anti-cheat may treat a replaced DLL as tampering. Avoid this for online games \
         with anti-cheat.",
        false,
    );
    for risk_name in checked_risk_games(&review.rows) {
        ui.add_space(4.0);
        widgets::banner(
            ui,
            theme::WARNING,
            icons::WARNING,
            &format!("{risk_name}: {}", dlss_core::KNOWN_GAME_RISK_WARNING),
            false,
        );
    }
    if review
        .rows
        .iter()
        .any(|row| row.checked && row.is_streamline())
    {
        ui.add_space(4.0);
        widgets::banner(
            ui,
            theme::DANGER,
            icons::WARNING,
            "Replacing Streamline can make a game run worse or crash. Leave it unchecked \
             unless you are fixing a specific problem.",
            false,
        );
    }
    if matches!(review.intent, ReviewIntent::QuickDlss(_)) {
        ui.label(
            egui::RichText::new(
                "DLSS updates never touch Streamline or Reflex. \
                 Use Update all DLLs on a game for those.",
            )
            .color(theme::TEXT_MUTED)
            .size(12.0),
        );
    }
}

fn checked_risk_games(rows: &[ReviewRow]) -> std::collections::BTreeSet<&'static str> {
    rows.iter()
        .filter(|row| row.checked)
        .filter_map(|row| row.known_risk)
        .collect()
}

fn review_row_list(ui: &mut egui::Ui, review: &mut ReviewState) {
    if review.rows.is_empty() {
        if review.errors.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(widgets::icon(icons::CHECK_CIRCLE, 36.0, theme::SUCCESS));
                ui.add_space(4.0);
                ui.heading("Nothing to update");
                ui.label(
                    egui::RichText::new("Every DLL here already matches the latest release.")
                        .color(theme::TEXT_MUTED),
                );
            });
            ui.add_space(12.0);
        }
        return;
    }
    let games = group_by_game(&review.rows);
    let checked = review.rows.iter().filter(|row| row.checked).count();
    let all = TriState::of(checked, review.rows.len());
    if review.rows.len() > 1
        && widgets::tri_checkbox(
            ui,
            all,
            format!(
                "{} across {}",
                plural(review.rows.len(), "change"),
                plural(games.len(), "game")
            ),
            "Select or clear every change",
        )
    {
        let select = all != TriState::All;
        for row in &mut review.rows {
            row.checked = select;
        }
    }
    ui.add_space(4.0);
    for range in games {
        widgets::card(ui, |ui| {
            let rows = &mut review.rows[range];
            let checked = rows.iter().filter(|row| row.checked).count();
            let state = TriState::of(checked, rows.len());
            // Strong text takes its color from the widget style, which the
            // checkbox scope darkens for the check mark; name the color.
            let name = egui::RichText::new(&rows[0].game_name)
                .font(egui::FontId::new(
                    14.0,
                    egui::FontFamily::Name("semibold".into()),
                ))
                .color(theme::TEXT);
            if widgets::tri_checkbox(ui, state, name, "Select or clear this game's changes") {
                let select = state != TriState::All;
                for row in rows.iter_mut() {
                    row.checked = select;
                }
            }
            ui.indent("review_rows", |ui| {
                for row in rows.iter_mut() {
                    review_row(ui, row);
                }
            });
        });
        ui.add_space(6.0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FooterClick {
    None,
    Apply,
    Download,
    Cancel,
}

/// What the review footer needs to know to pick its buttons.
struct Footer {
    ready: bool,
    /// The latest release's tag, and whether its last download failed.
    pending_download: Option<(String, bool)>,
    downloading: bool,
    busy: bool,
}

impl Footer {
    fn show(&self, ui: &mut egui::Ui, checked: usize) -> FooterClick {
        let mut clicked = FooterClick::None;
        ui.add_space(8.0);
        ui.separator();
        ui.label(
            egui::RichText::new(
                "Each DLL is backed up, replaced, and checked again. \
                 Windows asks for permission only if a folder needs it.",
            )
            .color(theme::TEXT_MUTED)
            .size(12.0),
        );
        ui.add_space(6.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let primary = if self.ready {
                self.apply_button(ui, checked)
            } else {
                self.download_button(ui)
            };
            if primary {
                clicked = if self.ready {
                    FooterClick::Apply
                } else {
                    FooterClick::Download
                };
            }
            if ui.button("Cancel").clicked() {
                clicked = FooterClick::Cancel;
            }
        });
        clicked
    }

    fn apply_button(&self, ui: &mut egui::Ui, checked: usize) -> bool {
        let label = format!("Apply {}", plural(checked, "change"));
        // A faded accent button reads as broken rather than disabled, so an
        // empty selection gets a plain one.
        let button = if checked > 0 {
            widgets::primary_icon_button(icons::CHECK, &label)
        } else {
            egui::Button::new(widgets::icon_text(icons::CHECK, &label))
        };
        ui.add_enabled(checked > 0 && !self.busy, button)
            .on_disabled_hover_text(if self.busy {
                "Wait for the current update to finish"
            } else {
                "Select at least one change"
            })
            .clicked()
    }

    fn download_button(&self, ui: &mut egui::Ui) -> bool {
        let Some((tag, failed)) = &self.pending_download else {
            return false;
        };
        let label = if *failed {
            "Try again".to_owned()
        } else {
            format!("Download {tag}")
        };
        ui.add_enabled(
            !self.downloading,
            widgets::primary_icon_button(icons::DOWNLOAD_SIMPLE, &label),
        )
        .on_disabled_hover_text("Downloading…")
        .clicked()
    }
}

/// Keeps each game's rows together, in the order the games came in, and
/// lists a game's DLLs in the same kind order as its game page.
fn order_rows(rows: &mut [ReviewRow]) {
    let mut first_seen: std::collections::HashMap<dlss_core::GameId, usize> =
        std::collections::HashMap::new();
    for row in rows.iter() {
        let next = first_seen.len();
        first_seen.entry(row.game_id.clone()).or_insert(next);
    }
    rows.sort_by_key(|row| {
        (
            first_seen[&row.game_id],
            crate::ui::detail::dll_kind_rank(&row.file_name),
        )
    });
}

/// Consecutive rows that belong to one game. Rows are built game by game,
/// so each game is one run.
fn group_by_game(rows: &[ReviewRow]) -> Vec<std::ops::Range<usize>> {
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match groups.last_mut() {
            Some(group) if rows[group.start].game_id == row.game_id => group.end = index + 1,
            _ => groups.push(index..index + 1),
        }
    }
    groups
}

fn review_row(ui: &mut egui::Ui, row: &mut ReviewRow) {
    ui.horizontal(|ui| {
        let kind = dlss_core::DllKind::classify(&row.file_name);
        let mut label = egui::text::LayoutJob::default();
        label.append(
            dll_kind_icon(kind),
            0.0,
            egui::TextFormat {
                font_id: theme::icon_font(15.0),
                color: theme::ACCENT,
                ..Default::default()
            },
        );
        label.append(
            &dlss_core::friendly_dll_label(&row.file_name),
            6.0,
            egui::TextFormat {
                font_id: egui::FontId::new(14.0, egui::FontFamily::Proportional),
                color: egui::Color32::PLACEHOLDER,
                ..Default::default()
            },
        );
        widgets::checkbox(ui, &mut row.checked, label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(version_text(row.target))
                    .monospace()
                    .color(theme::ACCENT),
            );
            ui.label(widgets::icon(icons::ARROW_RIGHT, 12.0, theme::TEXT_MUTED));
            ui.label(
                egui::RichText::new(version_text(row.installed))
                    .monospace()
                    .color(theme::TEXT_MUTED),
            );
            if row.comparison == dlss_core::Comparison::Downgrade {
                widgets::chip(ui, icons::CARET_DOWN, "Older", theme::WARNING)
                    .on_hover_text("The chosen version is older than the installed one");
            }
            if row.is_streamline() {
                widgets::chip(ui, icons::WARNING, "Streamline", theme::WARNING);
            }
        });
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

    fn risk_row(game_id: &str, risk: Option<&'static str>, checked: bool) -> ReviewRow {
        ReviewRow {
            game_id: dlss_core::GameId::from(game_id),
            game_name: game_id.into(),
            dll_id: dlss_core::DllInstallationId::from(format!("{game_id}-dll")),
            file_name: "nvngx_dlss.dll".into(),
            installed: None,
            target: None,
            desired: dlss_core::DesiredDll::KeepInstalled,
            comparison: dlss_core::Comparison::Unknown,
            checked,
            known_risk: risk,
        }
    }

    #[test]
    fn rows_keep_game_order_and_list_super_resolution_first() {
        let mut rows = vec![
            risk_row("b", None, true),
            risk_row("a", None, true),
            risk_row("b", None, true),
        ];
        rows[0].file_name = "nvngx_dlssd.dll".into();
        rows[2].file_name = "nvngx_dlss.dll".into();
        order_rows(&mut rows);
        let order: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.game_id.0.as_str(),
                    row.file_name.to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(
            order,
            [
                ("b", "nvngx_dlss.dll".to_owned()),
                ("b", "nvngx_dlssd.dll".to_owned()),
                ("a", "nvngx_dlss.dll".to_owned()),
            ]
        );
    }

    #[test]
    fn rows_group_into_one_run_per_game() {
        let rows = [
            risk_row("a", None, true),
            risk_row("a", None, true),
            risk_row("b", None, false),
            risk_row("c", None, true),
            risk_row("c", None, true),
        ];
        assert_eq!(group_by_game(&rows), [0..2, 2..3, 3..5]);
        assert!(group_by_game(&[]).is_empty());
    }

    #[test]
    fn checked_risk_warnings_are_deduplicated_and_dynamic() {
        let rows = [
            risk_row("fortnite-1", Some("Fortnite"), true),
            risk_row("fortnite-2", Some("Fortnite"), true),
            risk_row("finals", Some("The Finals"), false),
            risk_row("safe", None, true),
        ];
        assert_eq!(checked_risk_games(&rows), ["Fortnite"].into());
        assert!(checked_risk_games(&rows[2..]).is_empty());
    }
}
