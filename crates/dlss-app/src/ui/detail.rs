//! Game detail view: header with primary actions, per-DLL cards grouped by
//! kind, and the staged-changes ribbon.

use super::inspector::{comparison_label, desired_label, signature_label};
use super::theme::{self, icons};
use super::widgets;
use super::windows::format_timestamp;
use crate::ui::review::ReviewIntent;
use crate::{DlssApp, View};
use eframe::egui;

pub(crate) fn dll_kind_rank(file_name: &std::ffi::OsStr) -> u8 {
    match dlss_core::DllKind::classify(file_name) {
        Some(dlss_core::DllKind::DlssSuperResolution) => 0,
        Some(dlss_core::DllKind::DlssFrameGeneration) => 1,
        Some(dlss_core::DllKind::DlssRayReconstruction) => 2,
        Some(dlss_core::DllKind::ReflexLowLatency) => 3,
        Some(dlss_core::DllKind::Streamline) => 4,
        Some(dlss_core::DllKind::OtherNgx) => 5,
        None => 6,
    }
}

pub(crate) fn dll_kind_heading(kind: Option<dlss_core::DllKind>) -> &'static str {
    match kind {
        Some(dlss_core::DllKind::DlssSuperResolution) => "DLSS Super Resolution",
        Some(dlss_core::DllKind::DlssFrameGeneration) => "DLSS Frame Generation",
        Some(dlss_core::DllKind::DlssRayReconstruction) => "DLSS Ray Reconstruction",
        Some(dlss_core::DllKind::ReflexLowLatency) => "NVIDIA Reflex",
        Some(dlss_core::DllKind::Streamline) => "Streamline",
        Some(dlss_core::DllKind::OtherNgx) => "Other NGX",
        None => "Other",
    }
}

pub(crate) fn dll_kind_icon(kind: Option<dlss_core::DllKind>) -> &'static str {
    match kind {
        Some(dlss_core::DllKind::DlssSuperResolution) => icons::SPARKLE,
        Some(dlss_core::DllKind::DlssFrameGeneration) => icons::LIGHTNING,
        Some(dlss_core::DllKind::DlssRayReconstruction) => icons::EYE,
        Some(dlss_core::DllKind::ReflexLowLatency) => icons::PULSE,
        Some(dlss_core::DllKind::Streamline) => icons::STACK,
        Some(dlss_core::DllKind::OtherNgx) | None => icons::PACKAGE,
    }
}

impl DlssApp {
    /// Number of DLLs in this game with a staged target other than
    /// "keep installed".
    pub(crate) fn staged_targets_for(&self, game_id: &dlss_core::GameId) -> usize {
        self.profile_for_game(game_id)
            .targets
            .values()
            .filter(|target| **target != dlss_core::DesiredDll::KeepInstalled)
            .count()
    }

    pub(crate) fn game_detail_view(&mut self, ui: &mut egui::Ui, game_id: &dlss_core::GameId) {
        let Some(index) = self.games.iter().position(|game| &game.id == game_id) else {
            self.view = View::Library;
            return;
        };
        let mut requested_review = None;
        let mut go_back = false;
        self.detail_header(ui, index, &mut go_back, &mut requested_review);
        self.dll_table(ui, index);
        if go_back {
            self.view = View::Library;
        }
        if let Some(intent) = requested_review {
            self.open_review(intent);
        }
    }

    /// The page title: back link, game name, where it lives, and the two
    /// update actions, closed off from the DLL table by a rule.
    fn detail_header(
        &self,
        ui: &mut egui::Ui,
        index: usize,
        go_back: &mut bool,
        requested_review: &mut Option<ReviewIntent>,
    ) {
        let game = &self.games[index];
        if ui
            .add(
                egui::Button::new(widgets::colored_icon_label(
                    icons::ARROW_LEFT,
                    "Library",
                    theme::TEXT_MUTED,
                    13.0,
                ))
                .frame(false),
            )
            .on_hover_text("Back to the library (Esc)")
            .clicked()
        {
            *go_back = true;
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&game.name).font(egui::FontId::new(
                        24.0,
                        egui::FontFamily::Name("semibold".into()),
                    )));
                    widgets::badge(ui, game.store, theme::INFO);
                    if let Some(risk_name) = game.known_risk {
                        widgets::chip(ui, icons::WARNING, "Known risk", theme::WARNING)
                            .on_hover_text(format!(
                                "{risk_name}: {}",
                                dlss_core::KNOWN_GAME_RISK_WARNING
                            ));
                    }
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let path = game.root.display().to_string();
                    ui.scope(|ui| {
                        ui.set_max_width((ui.available_width() - 420.0).max(200.0));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&path)
                                    .monospace()
                                    .size(12.0)
                                    .color(theme::TEXT_MUTED),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&path);
                    });
                    if ui
                        .add(
                            egui::Button::new(widgets::colored_icon_label(
                                icons::FOLDER_OPEN,
                                "Open",
                                theme::TEXT_MUTED,
                                12.5,
                            ))
                            .frame(false),
                        )
                        .on_hover_text("Open in File Explorer")
                        .clicked()
                    {
                        crate::diagnostics::open_existing(&game.root);
                    }
                    let mut facts = vec![super::toast::plural(game.dlls, "NVIDIA DLL")];
                    if game.upgrades > 0 {
                        facts.push(super::toast::plural(game.upgrades, "update"));
                    }
                    if let Some(operation) = &game.last_operation {
                        facts.push(operation.clone());
                    }
                    ui.label(
                        egui::RichText::new(format!("· {}", facts.join(" · ")))
                            .size(12.5)
                            .color(theme::TEXT_MUTED),
                    );
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.detail_actions(ui, index, requested_review);
            });
        });
        ui.add_space(10.0);
        let rule = ui.available_rect_before_wrap();
        ui.painter().hline(
            rule.x_range(),
            rule.top(),
            egui::Stroke::new(1.0, theme::STROKE),
        );
        ui.add_space(8.0);
    }

    /// Update buttons at the right of the game header, or a spinner while
    /// this game is being changed.
    fn detail_actions(
        &self,
        ui: &mut egui::Ui,
        index: usize,
        requested_review: &mut Option<ReviewIntent>,
    ) {
        let game = &self.games[index];
        let in_flight = self.upgrading.as_ref() == Some(&game.id)
            || self.profiles_applying.contains_key(&game.id);
        let can_update = self.catalog_release.is_some() && game.dlls > 0 && !self.busy();
        if in_flight {
            ui.label("Updating…");
            ui.spinner();
            return;
        }
        let reason = if self.busy() {
            "Wait for the current update to finish"
        } else if game.dlls == 0 {
            "This game has no NVIDIA DLLs"
        } else {
            "The release catalog has not loaded yet"
        };
        if ui
            .add_enabled(
                can_update,
                widgets::primary_when(can_update, icons::SPARKLE, "Update DLSS"),
            )
            .on_hover_text("Review DLSS updates for this game")
            .on_disabled_hover_text(reason)
            .clicked()
        {
            *requested_review = Some(ReviewIntent::QuickDlss(vec![game.id.clone()]));
        }
        if ui
            .add_enabled(
                can_update,
                egui::Button::new(widgets::icon_text(icons::STACK, "Update all DLLs")),
            )
            .on_hover_text("Review updates for every NVIDIA DLL, including Streamline and Reflex")
            .on_disabled_hover_text(reason)
            .clicked()
        {
            *requested_review = Some(ReviewIntent::AllDlls(vec![game.id.clone()]));
        }
    }

    /// Every NVIDIA DLL in the game, one row each, grouped by kind through
    /// their order and icon rather than repeated headings.
    fn dll_table(&mut self, ui: &mut egui::Ui, index: usize) {
        let mut details = self.games[index].details.clone();
        details.sort_by_key(|dll| (dll_kind_rank(&dll.file_name), dll.file_name.clone()));
        if details.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(widgets::icon(icons::PACKAGE, 40.0, theme::TEXT_FAINT));
                ui.add_space(6.0);
                ui.heading("No NVIDIA DLLs in this game");
                ui.label(
                    egui::RichText::new(
                        "The folder has no DLSS, Streamline, or Reflex DLLs, so there is \
                         nothing to update.",
                    )
                    .color(theme::TEXT_MUTED),
                );
            });
            return;
        }
        let latest = self.latest_catalog();
        egui_extras::TableBuilder::new(ui)
            .id_salt("dll_table")
            .striped(true)
            .resizable(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(egui_extras::Column::remainder().at_least(200.0).clip(true))
            .column(egui_extras::Column::exact(100.0))
            .column(egui_extras::Column::exact(100.0))
            .column(egui_extras::Column::exact(165.0))
            .column(egui_extras::Column::exact(28.0))
            .column(egui_extras::Column::exact(232.0))
            .header(30.0, |mut header| {
                for title in ["DLL", "Installed", "Latest", "Status", "", "Version"] {
                    header.col(|ui| {
                        widgets::table_header_background(ui);
                        ui.label(widgets::table_header_text(title));
                    });
                }
            })
            .body(|body| {
                body.rows(36.0, details.len(), |mut row| {
                    let dll = &details[row.index()];
                    let staged = self
                        .persisted
                        .target_profile
                        .targets
                        .get(&dll.id)
                        .is_some_and(|target| *target != dlss_core::DesiredDll::KeepInstalled);
                    row.set_selected(staged);
                    let target = latest
                        .iter()
                        .filter(|candidate| {
                            dlss_core::same_file_name(&candidate.file_name, &dll.file_name)
                        })
                        .max_by_key(|candidate| (candidate.version, candidate.sha256));
                    let comparison = target.map_or(dlss_core::Comparison::Unavailable, |target| {
                        dlss_core::compare_dll(Some(&dll.metadata), Some(target))
                    });
                    row.col(|ui| {
                        let kind = dlss_core::DllKind::classify(&dll.file_name);
                        ui.label(widgets::icon(dll_kind_icon(kind), 15.0, theme::ACCENT));
                        ui.label(dlss_core::friendly_dll_label(&dll.file_name))
                            .on_hover_text(format!(
                                "{}\n{}",
                                dll_kind_heading(kind),
                                dll.path.display()
                            ));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(dll.file_name.to_string_lossy())
                                    .monospace()
                                    .size(11.5)
                                    .color(theme::TEXT_FAINT),
                            )
                            .truncate(),
                        );
                    });
                    row.col(|ui| version_cell(ui, dll.metadata.version, theme::TEXT));
                    row.col(|ui| {
                        let color = if comparison == dlss_core::Comparison::Upgrade {
                            theme::ACCENT
                        } else {
                            theme::TEXT_MUTED
                        };
                        version_cell(ui, target.map(|target| target.version), color);
                    });
                    row.col(|ui| comparison_cell(ui, comparison));
                    row.col(|ui| signature_cell(ui, dll.metadata.signature));
                    row.col(|ui| self.desired_target_combo(ui, dll));
                });
            });
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the selector builds official, imported, and backup choices in one widget"
    )]
    fn desired_target_combo(&mut self, ui: &mut egui::Ui, dll: &dlss_core::DllInstallation) {
        let mut cached_options: Vec<_> = self
            .releases
            .iter()
            .filter(|release| release.state == dlss_core::ReleaseState::Ready)
            .flat_map(|release| {
                release
                    .dlls
                    .iter()
                    .filter(|candidate| {
                        dlss_core::same_file_name(&candidate.file_name, &dll.file_name)
                    })
                    .map(|candidate| {
                        (
                            dlss_core::DesiredDll::Cached {
                                release: release.metadata.id.clone(),
                                sha256: candidate.sha256,
                            },
                            format!("{} · {}", candidate.version, release.metadata.tag),
                        )
                    })
            })
            .collect();
        cached_options.extend(
            self.imports
                .iter()
                .filter(|record| dlss_core::same_file_name(&record.file_name, &dll.file_name))
                .map(|record| {
                    (
                        dlss_core::DesiredDll::Cached {
                            release: dlss_core::imported_release_id(record.sha256),
                            sha256: record.sha256,
                        },
                        format!("Imported {}", record.version),
                    )
                }),
        );
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
                        "Restore backup {} · {}",
                        backup
                            .version
                            .map_or_else(|| "Unknown".into(), |version| version.to_string()),
                        format_timestamp(backup.created_unix)
                    ),
                )
            })
            .collect();
        ui.horizontal(|ui| {
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
            let installed_label = dll.metadata.version.map_or_else(
                || "Keep installed".into(),
                |version| format!("Keep installed ({version})"),
            );
            let selected_label = match &desired {
                dlss_core::DesiredDll::KeepInstalled => installed_label.clone(),
                dlss_core::DesiredDll::Cached { .. } => cached_options
                    .iter()
                    .find(|(target, _)| target == &desired)
                    .map_or_else(|| desired_label(&desired), |(_, label)| label.clone()),
                dlss_core::DesiredDll::Restore { .. } => restore_options
                    .iter()
                    .find(|(target, _)| target == &desired)
                    .map_or_else(|| desired_label(&desired), |(_, label)| label.clone()),
                dlss_core::DesiredDll::LatestOfficial => self
                    .latest_catalog()
                    .iter()
                    .filter(|candidate| {
                        dlss_core::same_file_name(&candidate.file_name, &dll.file_name)
                    })
                    .max_by_key(|candidate| (candidate.version, candidate.sha256))
                    .map_or_else(
                        || "Latest official".into(),
                        |candidate| format!("{} · Latest official", candidate.version),
                    ),
            };
            egui::ComboBox::from_id_salt(("desired", &dll.id.0))
                .width(220.0)
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut desired,
                        dlss_core::DesiredDll::KeepInstalled,
                        &installed_label,
                    );
                    ui.selectable_value(
                        &mut desired,
                        dlss_core::DesiredDll::LatestOfficial,
                        "Latest official",
                    );
                    for (target, label) in &cached_options {
                        ui.selectable_value(&mut desired, target.clone(), label);
                    }
                    for (target, label) in &restore_options {
                        ui.selectable_value(&mut desired, target.clone(), label);
                    }
                });
            if desired != before {
                if desired == dlss_core::DesiredDll::KeepInstalled {
                    self.persisted.target_profile.targets.remove(&dll.id);
                } else {
                    self.persisted
                        .target_profile
                        .targets
                        .insert(dll.id.clone(), desired);
                }
            }
        });
    }
}

fn version_cell(ui: &mut egui::Ui, version: Option<dlss_core::DllVersion>, color: egui::Color32) {
    match version {
        Some(version) => {
            ui.label(
                egui::RichText::new(version.to_string())
                    .monospace()
                    .color(color),
            );
        }
        None => widgets::empty_cell(ui),
    }
}

/// The comparison against the latest release, as colored text rather than a
/// pill, so a column of them stays quiet.
fn comparison_cell(ui: &mut egui::Ui, comparison: dlss_core::Comparison) {
    let (icon, color) = match comparison {
        dlss_core::Comparison::Upgrade => (icons::ARROW_CIRCLE_UP, theme::ACCENT),
        dlss_core::Comparison::Identical => (icons::CHECK, theme::SUCCESS),
        dlss_core::Comparison::Downgrade | dlss_core::Comparison::DifferentBuild => {
            (icons::STACK, theme::TEXT_MUTED)
        }
        dlss_core::Comparison::Unknown => (icons::QUESTION, theme::WARNING),
        dlss_core::Comparison::Unavailable => (icons::MINUS, theme::TEXT_FAINT),
    };
    widgets::status_text(ui, icon, comparison_label(comparison), color);
}

/// A trusted signature is the normal case and gets a quiet icon; anything
/// else is colored. The hover names the state either way.
fn signature_cell(ui: &mut egui::Ui, status: dlss_core::SignatureStatus) {
    let (icon, color) = match status {
        dlss_core::SignatureStatus::Trusted => (icons::SHIELD_CHECK, theme::TEXT_MUTED),
        dlss_core::SignatureStatus::Untrusted => (icons::SHIELD_WARNING, theme::DANGER),
        dlss_core::SignatureStatus::Unsigned => (icons::SHIELD_SLASH, theme::WARNING),
        dlss_core::SignatureStatus::Unavailable => (icons::QUESTION, theme::TEXT_FAINT),
    };
    ui.label(widgets::icon(icon, 15.0, color))
        .on_hover_text(signature_label(status));
}
