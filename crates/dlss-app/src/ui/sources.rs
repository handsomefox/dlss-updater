//! The DLL sources dialog: official Streamline releases to download, and
//! NVIDIA-signed DLLs imported from disk.

use super::theme::{self, icons};
use super::toast::plural;
use super::widgets;
use super::windows::format_date;
use crate::{Action, AppWindow, DlssApp};
use eframe::egui;

impl DlssApp {
    pub(crate) fn releases_window(&mut self, ctx: &egui::Context) {
        let mut open = self.open_windows.contains(&AppWindow::Releases);
        let mut actions = Vec::new();
        widgets::modal(
            ctx,
            widgets::dialog("releases", icons::PACKAGE, "DLL sources", 660.0),
            &mut open,
            |ui| {
                self.official_releases(ui, &mut actions);
                // A separator between the two sections, as in the other
                // dialogs. The release list ends in a card frame, which, like
                // a button, gets a little extra space before it.
                if !self.releases.is_empty() {
                    ui.add_space(widgets::FRAME_TO_SEPARATOR);
                }
                ui.separator();
                if widgets::section_title(
                    ui,
                    "Imported DLLs",
                    Some((
                        egui::Button::new(widgets::icon_text(icons::FOLDER_PLUS, "Import DLL…")),
                        "Pick a DLL file from your disk",
                        true,
                    )),
                ) {
                    actions.push(Action::ImportDll);
                }
                ui.label(
                    egui::RichText::new(
                        "Use a DLL you already have, such as one from another game. \
                         It must be an x86-64 DLL signed by NVIDIA; ZIP archives are not \
                         accepted.",
                    )
                    .color(theme::TEXT_MUTED),
                );
                ui.add_space(6.0);
                self.imported_dlls(ui, &mut actions);
            },
        );
        self.set_window_open(AppWindow::Releases, open);
        self.apply_all(ctx, actions);
    }

    fn official_releases(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let loading = self.runtime.catalog_loading;
        if widgets::section_title(
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
        ) {
            actions.push(Action::RefreshCatalog);
        }
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
            return;
        }
        let busy = self.release_download.is_some();
        widgets::card(ui, |ui| {
            for (position, release) in self.releases.iter().enumerate() {
                if position > 0 {
                    ui.separator();
                }
                let id = &release.metadata.id;
                let progress = self
                    .release_download
                    .as_ref()
                    .filter(|download| &download.id == id)
                    .and_then(|download| download.progress);
                let latest = self.catalog_release.as_ref() == Some(&release.metadata.tag);
                let row = ReleaseRow {
                    release,
                    latest,
                    busy,
                    progress,
                    error: self.release_errors.get(id),
                };
                if let Some(action) = ui.push_id(id, |ui| row.show(ui)).inner {
                    actions.push(action);
                }
            }
        });
    }

    fn imported_dlls(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if self.imports.is_empty() {
            ui.label(egui::RichText::new("Nothing imported.").color(theme::TEXT_FAINT));
            return;
        }
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
                                actions.push(Action::RemoveImport(record.sha256));
                            }
                        });
                    });
                });
            }
        });
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
    fn show(&self, ui: &mut egui::Ui) -> Option<Action> {
        let release = self.release;
        let mut action = None;
        ui.horizontal(|ui| {
            ui.set_min_height(32.0);
            ui.label(egui::RichText::new(&release.metadata.tag).font(theme::semibold_font(14.0)));
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
    fn state_and_action(&self, ui: &mut egui::Ui) -> Option<Action> {
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
                remove.then_some(Action::RemoveRelease(id))
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
                clicked.then_some(Action::DownloadRelease(id))
            }
        }
    }
}
