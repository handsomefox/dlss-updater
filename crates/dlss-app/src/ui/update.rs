//! Updates to DLSS Updater itself: the toolbar chip that points out a new
//! version, and the dialog around the shared update content from
//! `app_update_ui`, drawn in this app's theme.

use super::theme::{self, icons};
use super::widgets;
use crate::{Action, AppWindow, DlssApp};
use app_update_ui::Icon;
use eframe::egui::{self, Color32, RichText, Ui, Visuals, WidgetText};

/// The app's theme, as the shared update dialog asks for it.
struct Look;

impl app_update_ui::Look for Look {
    fn muted(&self, _visuals: &Visuals) -> Color32 {
        theme::TEXT_MUTED
    }

    fn success(&self, _visuals: &Visuals) -> Color32 {
        theme::ACCENT
    }

    fn warning(&self, _visuals: &Visuals) -> Color32 {
        theme::WARNING
    }

    fn space_xs(&self) -> f32 {
        6.0
    }

    fn space_md(&self) -> f32 {
        10.0
    }

    fn heading(&self, text: RichText) -> RichText {
        text.font(theme::semibold_font(16.0))
    }

    fn section(&self, text: RichText) -> RichText {
        text.font(theme::semibold_font(15.5))
    }

    fn bold(&self, text: RichText) -> RichText {
        text.font(theme::semibold_font(14.0))
    }

    fn fine_print(&self, text: RichText) -> RichText {
        text.color(theme::TEXT_FAINT)
    }

    fn glyph(&self, icon: Icon) -> Option<&'static str> {
        match icon {
            Icon::CheckNow | Icon::Restart => Some(icons::ARROW_CLOCKWISE),
            Icon::Download => Some(icons::DOWNLOAD_SIMPLE),
            Icon::Success => Some(icons::CHECK_CIRCLE),
            Icon::Warning => Some(icons::WARNING),
            Icon::Link => Some(icons::ARROW_SQUARE_OUT),
            Icon::Skip | Icon::Later => None,
        }
    }

    fn icon_text(&self, icon: Icon, text: &str, color: Option<Color32>) -> WidgetText {
        match (self.glyph(icon), color) {
            (Some(glyph), Some(color)) => {
                widgets::colored_icon_label(glyph, text, color, widgets::LABEL_SIZE)
            }
            (Some(glyph), None) => widgets::icon_text(glyph, text),
            (None, Some(color)) => RichText::new(text).color(color).into(),
            (None, None) => text.into(),
        }
    }

    fn primary_button(&self, icon: Icon, text: &str) -> egui::Button<'static> {
        self.glyph(icon).map_or_else(
            || widgets::primary_button(text),
            |glyph| widgets::primary_icon_button(glyph, text),
        )
    }

    fn show_error(&self, ui: &mut Ui, message: &str) {
        widgets::banner(ui, theme::WARNING, icons::WARNING, message, false);
        ui.add_space(widgets::FRAME_TO_SEPARATOR);
    }

    /// The modal already scrolls its whole body.
    fn notes_height(&self, _ui: &Ui) -> Option<f32> {
        None
    }
}

impl DlssApp {
    /// A chip beside the window buttons while a new version is out, the way
    /// the Streamline chip points out a new NVIDIA release.
    pub(crate) fn app_update_chip(&mut self, ui: &mut Ui) {
        let Some(text) = app_update_ui::chip_text(&self.updater, &self.persisted.app_updates)
        else {
            return;
        };
        if ui
            .add(widgets::chip_button(
                icons::ARROW_CIRCLE_UP,
                &text,
                theme::ACCENT,
            ))
            .on_hover_text(app_update_ui::chip_hover_text(&self.updater))
            .clicked()
        {
            self.open_windows.insert(AppWindow::Update);
        }
    }

    pub(crate) fn update_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut chosen = None;
        let mut action = None;
        let busy = self
            .busy()
            .then_some("Wait for the DLL changes in progress to finish");
        let updater = &self.updater;
        let settings = &mut self.persisted.app_updates;
        widgets::modal_with_actions(
            ctx,
            widgets::dialog(
                "app-update",
                icons::ARROW_CIRCLE_UP,
                app_update_ui::TITLE,
                560.0,
            ),
            &mut open,
            |ui| chosen = app_update_ui::body(ui, &Look, updater, settings),
            |ui| action = app_update_ui::actions(ui, &Look, updater, busy),
        );
        // Before the actions, because Skip and Not now close the dialog.
        self.set_window_open(AppWindow::Update, open);
        if let Some(action) = action.or(chosen) {
            self.apply_all(ctx, vec![Action::AppUpdate(action)]);
        }
    }
}
