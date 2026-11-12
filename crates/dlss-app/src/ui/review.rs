//! Mandatory review dialog: previews every planned DLL swap as a checkable
//! row, then converts the checked rows into a pinned `TargetProfile` that the
//! worker re-validates end to end via `plan_target_profile`.

use super::theme::{self, icons};
use super::toast::plural;
use super::widgets::{self, TriState};
use crate::ui::detail::dll_kind_icon;
use crate::{Action, Command, DlssApp};
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
    /// Games in an update whose DLSS 1 DLL the update leaves alone.
    dlss1_games: Vec<String>,
}

impl DlssApp {
    pub(crate) fn open_review(&mut self, intent: ReviewIntent) {
        let (rows, errors, ready) = self.build_review_rows(&intent);
        let dlss1_games = match &intent {
            ReviewIntent::QuickDlss(ids) | ReviewIntent::AllDlls(ids) => self
                .games
                .iter()
                .filter(|game| game.dlss1 && ids.contains(&game.id))
                .map(|game| game.name.clone())
                .collect(),
            // Staged targets that cross the DLSS 1 line fail their preview,
            // and the error says why.
            ReviewIntent::Profiles(_) => Vec::new(),
        };
        self.review = Some(ReviewState {
            intent,
            rows,
            errors,
            ready,
            dlss1_games,
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
                                let desired =
                                    pin_latest(desired, swap.source_sha256, &self.releases);
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
        let mut action = None;
        // Until the latest release is downloaded there is nothing to apply,
        // so the footer's primary slot downloads it instead.
        let footer = Footer {
            ready: review.ready,
            pending_download: self.latest_release_meta().map(|release| PendingDownload {
                id: release.metadata.id.clone(),
                tag: release.metadata.tag.clone(),
                failed: self.release_errors.contains_key(&release.metadata.id),
            }),
            downloading: self.release_download.is_some(),
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
            |ui| action = footer.show(ui, checked.get()),
        );
        // Applying takes the review back out and closes it, even when Esc
        // dismissed the dialog in the same frame.
        if keep_open || matches!(action, Some(Action::ApplyReview)) {
            self.review = Some(review);
        }
        if let Some(action) = action {
            self.apply(ctx, action);
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
        if self.release_download.is_some() {
            let (state, received, total) = self.release_progress_now();
            ui.horizontal(|ui| widgets::download_progress(ui, state, received, total));
        }
    }

    /// Converts the checked rows into one pinned profile per game and hands
    /// them to the worker, which re-plans and re-validates everything.
    pub(crate) fn apply_review(&mut self, review: &ReviewState) {
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
        let pending = per_game
            .iter()
            .map(|(game_id, profile)| (game_id.clone(), profile.targets.keys().cloned().collect()))
            .collect();
        if !self.start_run(crate::BatchKind::Update, pending) {
            return;
        }
        for (game_id, profile) in per_game {
            if !self.send(Command::ApplyProfile(game_id, profile)) {
                break;
            }
        }
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
    if !review.dlss1_games.is_empty() {
        ui.add_space(4.0);
        widgets::banner(
            ui,
            theme::INFO,
            icons::INFO,
            &dlss1_note(&review.dlss1_games),
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

fn dlss1_note(games: &[String]) -> String {
    let names = match games {
        [game] => game.clone(),
        [first, second] => format!("{first} and {second}"),
        _ => plural(games.len(), "game"),
    };
    let (verb, kept) = if games.len() == 1 {
        ("uses", "its DLSS DLL as it is")
    } else {
        ("use", "their DLSS DLLs as they are")
    };
    format!(
        "{names} {verb} DLSS 1. DLSS 2 and later turn DLSS off in a DLSS 1 game, \
         so this update leaves {kept}."
    )
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
                let detail = if review.dlss1_games.is_empty() {
                    "Every DLL here already matches the latest release."
                } else {
                    "No other DLL here has a newer release."
                };
                ui.label(egui::RichText::new(detail).color(theme::TEXT_MUTED));
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
                .font(theme::semibold_font(14.0))
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

/// The latest official release, which the review downloads before it can
/// list any change.
struct PendingDownload {
    id: dlss_core::ReleaseId,
    tag: String,
    /// The last download of it failed.
    failed: bool,
}

/// What the review footer needs to know to pick its buttons.
struct Footer {
    ready: bool,
    pending_download: Option<PendingDownload>,
    downloading: bool,
    busy: bool,
}

impl Footer {
    fn show(&self, ui: &mut egui::Ui, checked: usize) -> Option<Action> {
        let mut action = None;
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
        widgets::button_row(ui, |ui| {
            action = if self.ready {
                self.apply_button(ui, checked)
                    .then_some(Action::ApplyReview)
            } else {
                self.download_button(ui)
            };
            if ui.button("Cancel").clicked() {
                action = Some(Action::CloseReview);
            }
        });
        action
    }

    fn apply_button(&self, ui: &mut egui::Ui, checked: usize) -> bool {
        let label = format!("Apply {}", plural(checked, "change"));
        let enabled = checked > 0 && !self.busy;
        ui.add_enabled(
            enabled,
            widgets::primary_when(enabled, icons::CHECK, &label),
        )
        .on_disabled_hover_text(if self.busy {
            "Wait for the current update to finish"
        } else {
            "Select at least one change"
        })
        .clicked()
    }

    fn download_button(&self, ui: &mut egui::Ui) -> Option<Action> {
        let release = self.pending_download.as_ref()?;
        let label = if release.failed {
            "Try again".to_owned()
        } else {
            format!("Download {}", release.tag)
        };
        ui.add_enabled(
            !self.downloading,
            widgets::primary_when(!self.downloading, icons::DOWNLOAD_SIMPLE, &label),
        )
        .on_disabled_hover_text("Downloading…")
        .clicked()
        .then(|| Action::DownloadRelease(release.id.clone()))
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
            widgets::icon_format(widgets::LABEL_ICON_SIZE, theme::ACCENT, widgets::LABEL_SIZE),
        );
        label.append(
            &dlss_core::friendly_dll_label(&row.file_name),
            6.0,
            egui::TextFormat {
                font_id: egui::FontId::new(widgets::LABEL_SIZE, egui::FontFamily::Proportional),
                // An unnamed color falls back to the widget style, which the
                // checkbox scope darkens for the check mark.
                color: theme::TEXT,
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
    } else if error.contains(dlss_core::DLSS_GENERATION_MISMATCH) {
        "DLSS 1 and DLSS 2 or later cannot replace each other. Keep the installed DLL \
         or restore a backup."
    } else if error.contains("unknown DLL installation") {
        "The selected DLL is no longer present. Rescan and review the target."
    } else {
        "The staged target cannot be applied."
    }
}

/// Pins a staged "latest official" target to the release the preview
/// resolved, `source_sha256`. The worker would otherwise resolve "latest"
/// again when it applies, and a catalog refresh while the dialog is open
/// would install a version nobody reviewed.
fn pin_latest(
    desired: dlss_core::DesiredDll,
    source_sha256: [u8; 32],
    releases: &[dlss_core::CachedRelease],
) -> dlss_core::DesiredDll {
    if desired != dlss_core::DesiredDll::LatestOfficial {
        return desired;
    }
    releases
        .iter()
        .flat_map(|release| release.dlls.iter())
        .find(|dll| dll.sha256 == source_sha256)
        .map_or(desired, |dll| dlss_core::DesiredDll::Cached {
            release: dll.release.clone(),
            sha256: source_sha256,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_latest_target_is_pinned_to_the_previewed_release() {
        let dll = |release: &str, byte: u8| dlss_core::CatalogDll {
            file_name: "nvngx_dlss.dll".into(),
            version: dlss_core::DllVersion::new(310, u16::from(byte), 0, 0),
            sha256: [byte; 32],
            source: std::path::PathBuf::from("x"),
            release: dlss_core::ReleaseId::from(release),
        };
        let release = |id: &str, byte: u8| dlss_core::CachedRelease {
            metadata: dlss_core::ReleaseMetadata {
                id: dlss_core::ReleaseId::from(id),
                tag: id.into(),
                asset_name: format!("{id}.zip"),
                published_unix: 0,
            },
            state: dlss_core::ReleaseState::Ready,
            dlls: vec![dll(id, byte)],
            validation: dlss_core::ReleaseValidation::Full,
        };
        let releases = [release("v2", 2), release("v1", 1)];
        assert_eq!(
            pin_latest(dlss_core::DesiredDll::LatestOfficial, [1; 32], &releases),
            dlss_core::DesiredDll::Cached {
                release: dlss_core::ReleaseId::from("v1"),
                sha256: [1; 32],
            }
        );
        assert_eq!(
            pin_latest(dlss_core::DesiredDll::KeepInstalled, [1; 32], &releases),
            dlss_core::DesiredDll::KeepInstalled
        );
    }

    #[test]
    fn profile_preview_errors_hide_internal_installation_ids() {
        let raw = "desired source is unavailable for DLL installation manual:00610062";
        assert_eq!(
            profile_preview_error_label(raw),
            "Required DLL source is unavailable. Download it or choose another target."
        );
        assert!(!profile_preview_error_label(raw).contains("manual:"));

        let raw = format!(
            "validation failed: {} for DLL installation manual:00610062",
            dlss_core::DLSS_GENERATION_MISMATCH
        );
        assert!(profile_preview_error_label(&raw).starts_with("DLSS 1 and DLSS 2"));
    }

    #[test]
    fn dlss1_note_names_up_to_two_games() {
        assert!(dlss1_note(&["Battlefield V".into()]).starts_with("Battlefield V uses DLSS 1."));
        assert!(dlss1_note(&["A".into(), "B".into()]).starts_with("A and B use DLSS 1."));
        assert!(
            dlss1_note(&["A".into(), "B".into(), "C".into()]).starts_with("3 games use DLSS 1.")
        );
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
