//! Game detail view: header with primary actions, per-DLL cards grouped by
//! kind, and the staged-changes ribbon.

use super::inspector::desired_label;
use super::theme::{self, icons};
use super::widgets;
use super::windows::format_timestamp;
use crate::ui::review::ReviewIntent;
use crate::{Command, DlssApp, View};
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
        let mut undo = false;
        self.detail_header(ui, index, &mut go_back, &mut undo, &mut requested_review);
        ui.add_space(6.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.detail_dll_cards(ui, index);
        });
        if go_back {
            self.view = View::Library;
        }
        if undo {
            let game_id = game_id.clone();
            self.undo_game = None;
            self.toast = Some("Restoring backed-up DLLs…".into());
            let _ = self.worker.commands.send(Command::UndoLast(game_id));
        }
        if let Some(intent) = requested_review {
            self.open_review(intent);
        }
    }

    fn detail_header(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        go_back: &mut bool,
        undo: &mut bool,
        requested_review: &mut Option<ReviewIntent>,
    ) {
        let game = &self.games[index];
        let in_flight = self.upgrading.as_ref() == Some(&game.id)
            || self.profiles_applying.contains_key(&game.id);
        let can_update = self.catalog_release.is_some()
            && game.dlls > 0
            && self.upgrading.is_none()
            && !in_flight;
        let can_undo = self.undo_game.as_ref() == Some(&game.id);
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button(widgets::icon_text(icons::ARROW_LEFT, "Library"))
                    .clicked()
                {
                    *go_back = true;
                }
                ui.heading(&game.name);
                widgets::badge(ui, game.store, theme::INFO);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if in_flight {
                        ui.spinner();
                        ui.label("Updating…");
                        return;
                    }
                    let primary = widgets::primary_button("Update DLSS");
                    if ui
                        .add_enabled(can_update, primary)
                        .on_hover_text("Review and update this game's DLSS DLLs")
                        .clicked()
                    {
                        *requested_review = Some(ReviewIntent::QuickDlss(vec![game.id.clone()]));
                    }
                    if ui
                        .add_enabled(
                            can_update,
                            egui::Button::new(widgets::icon_text(icons::STACK, "All DLLs")),
                        )
                        .on_hover_text(
                            "Review updates for every managed DLL, including Streamline and Reflex",
                        )
                        .clicked()
                    {
                        *requested_review = Some(ReviewIntent::AllDlls(vec![game.id.clone()]));
                    }
                    if can_undo
                        && ui
                            .button(widgets::icon_text(
                                icons::ARROW_U_UP_LEFT,
                                "Undo last change",
                            ))
                            .clicked()
                    {
                        *undo = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(game.root.display().to_string())
                            .monospace()
                            .size(11.5)
                            .color(theme::TEXT_MUTED),
                    )
                    .selectable(true),
                );
            });
            ui.horizontal(|ui| {
                ui.weak(format!("{} managed DLLs", game.dlls));
                if game.last_operation != "Never" {
                    ui.weak("·");
                    ui.weak(&game.last_operation);
                }
            });
        });
    }

    fn detail_dll_cards(&mut self, ui: &mut egui::Ui, index: usize) {
        let mut details = self.games[index].details.clone();
        details.sort_by_key(|dll| dll_kind_rank(&dll.file_name));
        if details.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.weak("No managed DLLs were found in this game's folder.");
            });
            return;
        }
        let latest = self.latest_catalog();
        let mut previous_kind = None;
        for dll in &details {
            let kind = dlss_core::DllKind::classify(&dll.file_name);
            if kind != previous_kind || previous_kind.is_none() {
                ui.add_space(6.0);
                widgets::section_heading(ui, dll_kind_icon(kind), dll_kind_heading(kind));
                previous_kind = kind;
            }
            self.dll_card(ui, dll, &latest);
            ui.add_space(4.0);
        }
    }

    fn dll_card(
        &mut self,
        ui: &mut egui::Ui,
        dll: &dlss_core::DllInstallation,
        latest: &[dlss_core::CatalogDll],
    ) {
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(dlss_core::friendly_dll_label(&dll.file_name));
                ui.weak(egui::RichText::new(dll.file_name.to_string_lossy()).size(11.5));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let comparison = latest
                        .iter()
                        .filter(|candidate| {
                            dlss_core::same_file_name(&candidate.file_name, &dll.file_name)
                        })
                        .max_by_key(|candidate| (candidate.version, candidate.sha256))
                        .map_or(dlss_core::Comparison::Unavailable, |target| {
                            dlss_core::compare_dll(Some(&dll.metadata), Some(target))
                        });
                    widgets::status_chip(ui, comparison);
                    widgets::signature_chip(ui, dll.metadata.signature);
                });
            });
            ui.add(
                egui::Label::new(
                    egui::RichText::new(dll.path.display().to_string())
                        .monospace()
                        .size(11.5)
                        .color(theme::TEXT_MUTED),
                )
                .selectable(true),
            );
            ui.horizontal(|ui| {
                ui.label("Version:");
                self.desired_target_combo(ui, dll);
                if let Some(backup) = self
                    .backups
                    .iter()
                    .filter(|backup| backup.original_path == dll.path)
                    .max_by_key(|backup| backup.created_unix)
                    .cloned()
                    && ui
                        .button(widgets::icon_text(icons::ARROW_U_UP_LEFT, "Undo this DLL"))
                        .on_hover_text("Stage this DLL's most recent backup for restore")
                        .clicked()
                {
                    self.persisted.target_profile.targets.insert(
                        dll.id.clone(),
                        dlss_core::DesiredDll::Restore {
                            backup_sha256: backup.sha256,
                        },
                    );
                }
            });
            ui.weak("Choose a version for this DLL. The change is staged until Review & apply.");
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
                        "Restore {} · {}",
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
                || "Installed · version unknown".into(),
                |version| format!("{version} · Installed"),
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
                .width(260.0)
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

    /// Bottom bar shown while this game has staged advanced targets.
    pub(crate) fn staged_ribbon(&mut self, ui: &mut egui::Ui, game_id: &dlss_core::GameId) {
        let staged = self.staged_targets_for(game_id);
        ui.horizontal(|ui| {
            ui.label(widgets::icon(icons::LIST_CHECKS, 15.0, theme::ACCENT));
            ui.strong(format!(
                "{staged} staged DLL {}",
                if staged == 1 { "change" } else { "changes" }
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let primary = widgets::primary_button("Review & apply");
                if ui.add(primary).clicked() {
                    self.open_review(ReviewIntent::Profiles(vec![game_id.clone()]));
                }
                if ui.button("Discard").clicked() {
                    self.clear_game_profile(game_id);
                }
            });
        });
    }
}
