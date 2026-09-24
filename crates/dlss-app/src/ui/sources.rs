//! The DLL sources dialog: official Streamline releases to download, and
//! NVIDIA-signed DLLs imported from disk.

use super::theme::{self, icons};
use super::toast::plural;
use super::widgets;
use super::windows::format_date;
use crate::{AppWindow, Command, DlssApp};
use eframe::egui;

enum ReleaseAction {
    Download(dlss_core::ReleaseId),
    Remove(dlss_core::ReleaseId),
}

impl DlssApp {
    pub(crate) fn releases_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Releases);
        let mut action = None;
        let mut remove_import = None;
        let mut refresh = false;
        let mut import = false;
        widgets::modal(
            ctx,
            widgets::dialog("releases", icons::PACKAGE, "DLL sources", 660.0),
            &mut open,
            |ui| {
                refresh = self.official_releases(ui, &mut action);
                ui.add_space(16.0);
                import = widgets::section_title(
                    ui,
                    "Imported DLLs",
                    Some((
                        egui::Button::new(widgets::icon_text(icons::FOLDER_PLUS, "Import DLL…")),
                        "Pick a DLL file from your disk",
                        true,
                    )),
                );
                ui.label(
                    egui::RichText::new(
                        "Use a DLL you already have, such as one from another game. \
                         It must be an x86-64 DLL signed by NVIDIA; ZIP archives are not \
                         accepted.",
                    )
                    .color(theme::TEXT_MUTED),
                );
                ui.add_space(6.0);
                remove_import = self.imported_dlls(ui);
            },
        );
        if refresh {
            let _ = self.worker.commands.send(Command::RefreshCatalog);
        }
        if import
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Windows DLL", &["dll"])
                .pick_file()
        {
            let _ = self.worker.commands.send(Command::ImportDll(path));
        }
        match action {
            Some(ReleaseAction::Download(id)) => {
                self.inspecting_release = Some(id.clone());
                self.release_progress = None;
                let _ = self.worker.commands.send(Command::InspectRelease(id));
            }
            Some(ReleaseAction::Remove(id)) => {
                let _ = self.worker.commands.send(Command::RemoveRelease(id));
            }
            None => {}
        }
        if let Some(hash) = remove_import {
            let _ = self.worker.commands.send(Command::RemoveImport(hash));
        }
        self.set_window_open(AppWindow::Releases, open);
    }

    /// Returns true when the user asked to check GitHub again.
    fn official_releases(&self, ui: &mut egui::Ui, action: &mut Option<ReleaseAction>) -> bool {
        let loading = self.runtime.catalog_loading;
        let refresh = widgets::section_title(
            ui,
            "Official releases",
            Some((
                egui::Button::new(widgets::icon_text(
                    icons::ARROW_CLOCKWISE,
                    if loading {
                        "Checking…"
                    } else {
                        "Check again"
                    },
                )),
                "Ask GitHub for new Streamline releases",
                !loading,
            )),
        );
        ui.label(
            egui::RichText::new(
                "NVIDIA's Streamline SDK releases on GitHub. Games are compared against the \
                 newest; older releases can be picked for a single DLL on a game's page.",
            )
            .color(theme::TEXT_MUTED),
        );
        ui.add_space(6.0);
        if let Some(error) = &self.catalog_error {
            widgets::banner(
                ui,
                theme::DANGER,
                icons::WARNING_CIRCLE,
                &format!("Could not reach GitHub: {error}. Downloaded releases still work."),
                false,
            );
            ui.add_space(6.0);
        } else if !loading && self.releases.is_empty() {
            ui.label(
                egui::RichText::new("GitHub listed no stable Streamline releases.")
                    .color(theme::TEXT_FAINT),
            );
        }
        if self.releases.is_empty() {
            return refresh;
        }
        let busy = self.inspecting_release.is_some();
        widgets::card(ui, |ui| {
            for (position, release) in self.releases.iter().enumerate() {
                if position > 0 {
                    ui.separator();
                }
                let id = &release.metadata.id;
                let progress = self
                    .release_progress
                    .as_ref()
                    .filter(|(progress_id, _, _)| {
                        progress_id == id && self.inspecting_release.as_ref() == Some(id)
                    })
                    .map(|(_, received, total)| (*received, *total));
                let latest = self.catalog_release.as_ref() == Some(&release.metadata.tag);
                let row = ReleaseRow {
                    release,
                    latest,
                    busy,
                    progress,
                    error: self.release_errors.get(id),
                };
                if let Some(requested) = ui.push_id(id, |ui| row.show(ui)).inner {
                    *action = Some(requested);
                }
            }
        });
        refresh
    }

    /// Returns the hash of an import the user asked to remove.
    fn imported_dlls(&self, ui: &mut egui::Ui) -> Option<[u8; 32]> {
        if self.imports.is_empty() {
            ui.label(egui::RichText::new("Nothing imported.").color(theme::TEXT_FAINT));
            return None;
        }
        let mut remove = None;
        widgets::card(ui, |ui| {
            for (position, record) in self.imports.iter().enumerate() {
                if position > 0 {
                    ui.separator();
                }
                ui.push_id(record.sha256, |ui| {
                    ui.horizontal(|ui| {
                        ui.set_min_height(30.0);
                        ui.strong(dlss_core::friendly_dll_label(&record.file_name));
                        ui.label(
                            egui::RichText::new(record.version.to_string())
                                .monospace()
                                .color(theme::TEXT_MUTED),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · imported {}",
                                record.signer,
                                format_date(record.imported_unix)
                            ))
                            .size(12.5)
                            .color(theme::TEXT_MUTED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(widgets::icon_text(
                                        icons::TRASH_SIMPLE,
                                        "Remove",
                                    ))
                                    .frame(false),
                                )
                                .on_hover_text("Delete this imported copy")
                                .clicked()
                            {
                                remove = Some(record.sha256);
                            }
                        });
                    });
                });
            }
        });
        remove
    }
}

/// One release in the list: its tag and date on the left, its state and
/// the one thing to do with it on the right.
struct ReleaseRow<'a> {
    release: &'a dlss_core::CachedRelease,
    latest: bool,
    busy: bool,
    progress: Option<(u64, Option<u64>)>,
    error: Option<&'a String>,
}

impl ReleaseRow<'_> {
    fn show(&self, ui: &mut egui::Ui) -> Option<ReleaseAction> {
        let release = self.release;
        let mut action = None;
        ui.horizontal(|ui| {
            ui.set_min_height(32.0);
            ui.label(
                egui::RichText::new(&release.metadata.tag).font(egui::FontId::new(
                    14.0,
                    egui::FontFamily::Name("semibold".into()),
                )),
            );
            if self.latest {
                widgets::badge(ui, "Latest", theme::ACCENT);
            }
            if release.metadata.published_unix > 0 {
                ui.label(
                    egui::RichText::new(format_date(release.metadata.published_unix))
                        .size(12.5)
                        .color(theme::TEXT_MUTED),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.hyperlink_to(
                    widgets::icon(icons::ARROW_SQUARE_OUT, 14.0, theme::TEXT_MUTED),
                    format!(
                        "https://github.com/NVIDIA-RTX/Streamline/releases/tag/{}",
                        release.metadata.tag
                    ),
                )
                .on_hover_text("View on GitHub");
                action = self.state_and_action(ui);
            });
        });
        if let Some(error) = self.error
            && self.progress.is_none()
        {
            ui.add(
                egui::Label::new(egui::RichText::new(error).size(12.5).color(theme::DANGER))
                    .wrap()
                    .selectable(true),
            );
        }
        if release.state == dlss_core::ReleaseState::Ready
            && release.validation == dlss_core::ReleaseValidation::RevocationUnavailableFallback
        {
            widgets::status_text(
                ui,
                icons::WARNING,
                "Checked without the online revocation list",
                theme::WARNING,
            )
            .on_hover_text(
                "Windows could not reach revocation services. The signatures and the NVIDIA \
                 publisher were verified, but not the online revocation result.",
            );
        }
        action
    }

    /// Right-to-left: the button first, then the state to its left.
    fn state_and_action(&self, ui: &mut egui::Ui) -> Option<ReleaseAction> {
        let release = self.release;
        let id = release.metadata.id.clone();
        if let Some((received, total)) = self.progress {
            ui.scope(|ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    widgets::download_progress(ui, release.state, received, total);
                });
            });
            return None;
        }
        match release.state {
            dlss_core::ReleaseState::Ready => {
                let remove = ui
                    .add(
                        egui::Button::new(widgets::icon_text(icons::TRASH_SIMPLE, "Remove"))
                            .frame(false),
                    )
                    .on_hover_text("Delete the downloaded files. You can download it again.")
                    .clicked();
                widgets::status_text(
                    ui,
                    icons::CHECK_CIRCLE,
                    &format!("Downloaded · {}", plural(release.dlls.len(), "DLL")),
                    theme::SUCCESS,
                )
                .on_hover_ui(|ui| {
                    for dll in &release.dlls {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}  {}",
                                dll.file_name.to_string_lossy(),
                                dll.version
                            ))
                            .monospace()
                            .size(12.0),
                        );
                    }
                });
                remove.then_some(ReleaseAction::Remove(id))
            }
            state => {
                let failed = state == dlss_core::ReleaseState::Invalid || self.error.is_some();
                let label = if failed { "Retry" } else { "Download" };
                let button = if self.latest && !failed {
                    widgets::primary_icon_button(icons::DOWNLOAD_SIMPLE, label)
                } else {
                    egui::Button::new(widgets::icon_text(icons::DOWNLOAD_SIMPLE, label))
                };
                let clicked = ui
                    .add_enabled(!self.busy, button)
                    .on_hover_text("Download the release and check its NVIDIA signatures")
                    .on_disabled_hover_text("Another release is downloading")
                    .clicked();
                if failed {
                    widgets::status_text(ui, icons::WARNING_CIRCLE, "Failed", theme::DANGER);
                }
                clicked.then_some(ReleaseAction::Download(id))
            }
        }
    }
}
