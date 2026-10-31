//! Updates to DLSS Updater itself: the toolbar chip that points out a new
//! version, and the dialog with its release notes, the install, and how often
//! to check.

use super::theme::{self, icons};
use super::widgets;
use crate::{APP_INFO, AppWindow, DlssApp};
use app_update::{CheckPeriod, NoteBlock, Settings, Updater};
use eframe::egui;

enum UpdateAction {
    Download,
    Restart,
    Skip,
    Later,
}

impl DlssApp {
    /// A chip beside the window buttons while a new version is out, the way
    /// the Streamline chip points out a new NVIDIA release.
    pub(crate) fn app_update_chip(&mut self, ui: &mut egui::Ui) {
        let Some(release) = self.updater.notification(&self.persisted.app_updates) else {
            return;
        };
        let text = if self.updater.staged().is_some() {
            format!("Restart for {}", release.version)
        } else {
            format!("Update {} available", release.version)
        };
        let chip = egui::Button::new(widgets::colored_icon_label(
            icons::ARROW_CIRCLE_UP,
            &text,
            theme::ACCENT,
            12.5,
        ))
        .fill(theme::ACCENT.gamma_multiply(0.14))
        .stroke(egui::Stroke::NONE)
        .corner_radius(egui::CornerRadius::same(10))
        .min_size(egui::vec2(0.0, 24.0));
        if ui
            .add(chip)
            .on_hover_text("A new version of DLSS Updater is out. Click to see what changed.")
            .clicked()
        {
            self.open_windows.insert(AppWindow::Update);
        }
    }

    pub(crate) fn update_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut period = self.persisted.app_updates.period;
        let mut check = false;
        let mut action = None;
        let busy = self.busy();
        let updater = &self.updater;
        let settings = &self.persisted.app_updates;
        widgets::modal_with_actions(
            ctx,
            widgets::dialog("app-update", icons::ARROW_CIRCLE_UP, "App updates", 560.0),
            &mut open,
            |ui| update_body(ui, updater, settings, &mut period, &mut check),
            |ui| action = update_actions(ui, updater, busy),
        );
        self.persisted.app_updates.period = period;
        if check {
            self.updater.check_now();
        }
        match action {
            Some(UpdateAction::Download) => self.updater.download(),
            Some(UpdateAction::Restart) => {
                if self.updater.install_and_restart().is_ok() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Some(UpdateAction::Skip) => {
                self.updater.skip(&mut self.persisted.app_updates);
                open = false;
            }
            Some(UpdateAction::Later) => {
                self.updater.dismiss();
                open = false;
            }
            None => {}
        }
        self.set_window_open(AppWindow::Update, open);
    }
}

fn semibold(text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text).font(egui::FontId::new(
        size,
        egui::FontFamily::Name("semibold".into()),
    ))
}

fn update_body(
    ui: &mut egui::Ui,
    updater: &Updater,
    settings: &Settings,
    period: &mut CheckPeriod,
    check: &mut bool,
) {
    let current = updater.current().map_or_else(
        || APP_INFO.version.to_owned(),
        |version| version.to_string(),
    );
    if let Some(update) = updater.update() {
        available_update(ui, updater, settings, update, &current);
    } else if updater.is_checking() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Checking GitHub for a new version…");
        });
    } else if updater.is_up_to_date() {
        ui.label(widgets::colored_icon_label(
            icons::CHECK_CIRCLE,
            &format!("Version {current} is the latest version."),
            theme::ACCENT,
            14.0,
        ));
    } else {
        ui.label(
            egui::RichText::new(format!("You have version {current}.")).color(theme::TEXT_MUTED),
        );
    }
    if let Some(error) = updater.error() {
        ui.add_space(8.0);
        widgets::banner(ui, theme::WARNING, icons::WARNING, error, false);
    }

    ui.add_space(12.0);
    ui.separator();
    widgets::section_title(ui, "Checking", None);
    ui.horizontal(|ui| {
        ui.label("Check for new versions");
        egui::ComboBox::from_id_salt("app-update-period")
            .selected_text(period.label())
            .show_ui(ui, |ui| {
                for option in CheckPeriod::ALL {
                    ui.selectable_value(period, option, option.label());
                }
            });
        *check = ui
            .add_enabled(
                !updater.is_busy(),
                egui::Button::new(widgets::icon_text(icons::ARROW_CLOCKWISE, "Check now")),
            )
            .clicked();
    });
    let last = settings.last_check.map_or_else(
        || "Never checked".to_owned(),
        |then| {
            format!(
                "Last checked {}",
                app_update::describe_age(then, app_update::now())
            )
        },
    );
    ui.label(
        egui::RichText::new(format!(
            "{last}. A check asks GitHub for this app's releases and sends nothing else."
        ))
        .color(theme::TEXT_FAINT),
    );
}

/// The newest version, where the install stands, and the notes of every
/// release since the running one.
fn available_update(
    ui: &mut egui::Ui,
    updater: &Updater,
    settings: &Settings,
    update: &app_update::Update,
    current: &str,
) {
    let latest = update.latest();
    ui.label(semibold(
        format!("Version {} is available", latest.version),
        16.0,
    ));
    let released = if latest.date.is_empty() {
        String::new()
    } else {
        format!(" It was released on {}.", latest.date)
    };
    ui.label(
        egui::RichText::new(format!("You have version {current}.{released}"))
            .color(theme::TEXT_MUTED),
    );
    if updater.staged() == Some(latest.version) {
        ui.add_space(6.0);
        ui.label(widgets::colored_icon_label(
            icons::CHECK_CIRCLE,
            "Downloaded and verified. Restart DLSS Updater to finish.",
            theme::ACCENT,
            13.0,
        ));
    } else if settings.skipped.as_deref() == Some(latest.version.to_string().as_str()) {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("You skipped this version, so the toolbar does not point it out.")
                .color(theme::TEXT_MUTED),
        );
    }
    if !updater.can_install() {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "This release has no build that DLSS Updater can install by itself. \
                 Download it from the release page instead.",
            )
            .color(theme::TEXT_MUTED),
        );
        ui.hyperlink_to(
            widgets::icon_text(icons::ARROW_SQUARE_OUT, "Release page"),
            &latest.page_url,
        );
    }
    ui.add_space(10.0);
    widgets::section_title(ui, "What's new", None);
    for release in update.releases() {
        ui.horizontal(|ui| {
            ui.label(semibold(release.version.to_string(), 14.0));
            if !release.date.is_empty() {
                ui.label(egui::RichText::new(&release.date).color(theme::TEXT_FAINT));
            }
        });
        release_notes(ui, &release.notes);
        ui.add_space(8.0);
    }
    ui.hyperlink_to(
        widgets::icon_text(icons::ARROW_SQUARE_OUT, "All releases"),
        APP_INFO.releases_page(),
    );
}

fn release_notes(ui: &mut egui::Ui, markdown: &str) {
    let blocks = app_update::parse_notes(markdown);
    if blocks.is_empty() {
        ui.label(egui::RichText::new("No notes for this release.").color(theme::TEXT_MUTED));
    }
    for block in blocks {
        match block {
            NoteBlock::Heading(text) => {
                ui.label(semibold(text, 13.5));
            }
            NoteBlock::Bullet(text) => {
                ui.horizontal_top(|ui| {
                    ui.label(egui::RichText::new("•").color(theme::TEXT_MUTED));
                    ui.add(egui::Label::new(text).wrap());
                });
            }
            NoteBlock::Paragraph(text) => {
                ui.add(egui::Label::new(text).wrap());
            }
        }
    }
}

fn update_actions(ui: &mut egui::Ui, updater: &Updater, busy: bool) -> Option<UpdateAction> {
    let update = updater.update()?;
    ui.add_space(8.0);
    if let Some((received, total)) = updater.progress() {
        let percent = total
            .filter(|total| *total > 0)
            .map_or(0, |total| received.saturating_mul(100) / total)
            .min(100);
        let fraction = f32::from(u8::try_from(percent).unwrap_or(100)) / 100.0;
        ui.add(egui::ProgressBar::new(fraction).text(format!(
            "Downloading {}… {percent}%",
            update.latest().version
        )));
        return None;
    }
    widgets::button_row(ui, |ui| {
        let mut action = None;
        if updater.staged().is_some() {
            if ui
                .add_enabled(
                    !busy,
                    widgets::primary_icon_button(icons::ARROW_CLOCKWISE, "Restart to update"),
                )
                .on_hover_text("Close DLSS Updater and open the new version")
                .on_disabled_hover_text("Wait for the DLL changes in progress to finish")
                .clicked()
            {
                action = Some(UpdateAction::Restart);
            }
        } else if updater.can_install()
            && ui
                .add_enabled(
                    !updater.is_busy(),
                    widgets::primary_icon_button(icons::DOWNLOAD_SIMPLE, "Download update"),
                )
                .on_hover_text(
                    "Download the new version and check it against its published SHA-256",
                )
                .clicked()
        {
            action = Some(UpdateAction::Download);
        }
        if ui
            .button("Skip this version")
            .on_hover_text("Stop pointing out this version. A newer one shows up again.")
            .clicked()
        {
            action = Some(UpdateAction::Skip);
        }
        if ui
            .button("Not now")
            .on_hover_text("Hide the reminder until DLSS Updater starts again")
            .clicked()
        {
            action = Some(UpdateAction::Later);
        }
        action
    })
}
