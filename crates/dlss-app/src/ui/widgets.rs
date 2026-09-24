//! Small reusable visual building blocks shared by all views.

use super::theme::{self, icons};
use eframe::egui;

pub(crate) fn icon_text(icon: &str, label: &str) -> egui::WidgetText {
    colored_icon_text(icon, label, None)
}

fn colored_icon_text(icon: &str, label: &str, color: Option<egui::Color32>) -> egui::WidgetText {
    let mut job = egui::text::LayoutJob::default();
    let color = color.unwrap_or(egui::Color32::PLACEHOLDER);
    job.append(
        icon,
        0.0,
        egui::TextFormat {
            font_id: theme::icon_font(15.0),
            color,
            ..Default::default()
        },
    );
    if !label.is_empty() {
        job.append(
            label,
            5.0,
            egui::TextFormat {
                font_id: egui::FontId::new(14.0, egui::FontFamily::Proportional),
                color,
                ..Default::default()
            },
        );
    }
    job.into()
}

pub(crate) fn primary_button(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label.into()).color(theme::TEXT_ON_ACCENT))
        .fill(theme::ACCENT)
}

pub(crate) fn primary_icon_button(icon: &str, label: &str) -> egui::Button<'static> {
    egui::Button::new(colored_icon_text(icon, label, Some(theme::TEXT_ON_ACCENT)))
        .fill(theme::ACCENT)
}

/// Icon and label in one color and size, for chips that are also buttons.
pub(crate) fn colored_icon_label(
    icon: &str,
    label: &str,
    color: egui::Color32,
    size: f32,
) -> egui::WidgetText {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        icon,
        0.0,
        egui::TextFormat {
            font_id: theme::icon_font(size + 1.0),
            color,
            ..Default::default()
        },
    );
    job.append(
        label,
        5.0,
        egui::TextFormat {
            font_id: egui::FontId::new(size, egui::FontFamily::Proportional),
            color,
            ..Default::default()
        },
    );
    job.into()
}

/// A tab label followed by its count in a quieter color, like "Updates 3".
pub(crate) fn label_with_count(label: &str, count: usize, selected: bool) -> egui::WidgetText {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        label,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::new(13.5, egui::FontFamily::Proportional),
            color: egui::Color32::PLACEHOLDER,
            ..Default::default()
        },
    );
    job.append(
        &count.to_string(),
        6.0,
        egui::TextFormat {
            font_id: egui::FontId::new(12.0, egui::FontFamily::Proportional),
            color: if selected {
                theme::ACCENT
            } else {
                theme::TEXT_MUTED
            },
            ..Default::default()
        },
    );
    job.into()
}

/// Secondary button for the one recommended action in a row: accent text
/// and outline, so a column of them reads as actionable without shouting.
pub(crate) fn accent_button(icon: &str, label: &str) -> egui::Button<'static> {
    egui::Button::new(colored_icon_text(icon, label, Some(theme::ACCENT)))
        .stroke(egui::Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.55)))
}

/// Selection state of a group of checkboxes, for a "select all" box.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TriState {
    None,
    Some,
    All,
}

impl TriState {
    pub(crate) fn of(checked: usize, total: usize) -> Self {
        if total == 0 || checked == 0 {
            Self::None
        } else if checked >= total {
            Self::All
        } else {
            Self::Some
        }
    }
}

/// egui's checkbox with square corners and an accent fill when checked.
///
/// The global widget corner radius turns a 14 px box into a circle, and the
/// unchecked stroke is nearly invisible on the dark panels, so the style is
/// overridden only around the checkbox. It stays egui's widget, so keyboard
/// focus, Space to toggle, and screen reader state keep working.
pub(crate) fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    let filled = *checked;
    checkbox_scope(ui, filled, |ui| ui.checkbox(checked, label))
}

/// A "select all" checkbox. Returns true when clicked; the caller decides
/// what the click does, which is normally to select all unless all are.
pub(crate) fn tri_checkbox(
    ui: &mut egui::Ui,
    state: TriState,
    label: impl Into<egui::WidgetText>,
    hover: &str,
) -> bool {
    let mut checked = state == TriState::All;
    checkbox_scope(ui, state != TriState::None, |ui| {
        ui.add(egui::Checkbox::new(&mut checked, label).indeterminate(state == TriState::Some))
    })
    .on_hover_text(hover)
    .clicked()
}

fn checkbox_scope<R>(ui: &mut egui::Ui, filled: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().icon_width = 16.0;
        ui.spacing_mut().icon_width_inner = 9.0;
        let widgets = &mut ui.visuals_mut().widgets;
        for (state, hovered) in [
            (&mut widgets.inactive, false),
            (&mut widgets.hovered, true),
            (&mut widgets.active, true),
        ] {
            state.corner_radius = egui::CornerRadius::same(4);
            if filled {
                state.bg_fill = if hovered {
                    theme::ACCENT_HOVER
                } else {
                    theme::ACCENT
                };
                state.bg_stroke = egui::Stroke::new(1.0, state.bg_fill);
                state.fg_stroke = egui::Stroke::new(2.0, theme::TEXT_ON_ACCENT);
            } else {
                state.bg_fill = theme::BG_APP;
                state.bg_stroke = egui::Stroke::new(
                    1.0,
                    if hovered {
                        theme::TEXT_MUTED
                    } else {
                        theme::TEXT_FAINT
                    },
                );
            }
        }
        add(ui)
    })
    .inner
}

/// Icon and text in one color, with no pill behind them. Used where a chip
/// would be too loud, such as a status that repeats down a table column.
pub(crate) fn status_text(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    color: egui::Color32,
) -> egui::Response {
    ui.label(colored_icon_text(icon, label, Some(color)))
}

/// Paints the band behind a table header cell: the panel color with a rule
/// along the bottom, so column titles read as a different layer from rows.
pub(crate) fn table_header_background(ui: &egui::Ui) {
    // Each cell's painter is clipped to the cell, which would leave gaps
    // between the bands; paint half the column gap past each edge instead,
    // clipped only to what the table itself may draw on.
    let spacing = ui.spacing().item_spacing.x / 2.0;
    let rect = ui.max_rect().expand2(egui::vec2(spacing, 0.0));
    let painter = ui
        .ctx()
        .layer_painter(ui.layer_id())
        .with_clip_rect(ui.clip_rect().expand2(egui::vec2(spacing, 0.0)));
    painter.rect_filled(rect, 0.0, theme::BG_PANEL);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        egui::Stroke::new(1.0, theme::STROKE),
    );
}

/// A column title: small and muted, so it never competes with the data.
pub(crate) fn table_header_text(title: &str) -> egui::RichText {
    egui::RichText::new(title)
        .size(12.5)
        .color(theme::TEXT_MUTED)
}

/// A section title inside a dialog, with an optional action at the right.
/// Returns true when the action was clicked.
pub(crate) fn section_title(
    ui: &mut egui::Ui,
    title: &str,
    action: Option<(egui::Button<'_>, &str, bool)>,
) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.set_min_height(30.0);
        ui.label(egui::RichText::new(title).font(egui::FontId::new(
            15.5,
            egui::FontFamily::Name("semibold".into()),
        )));
        if let Some((button, hover, enabled)) = action {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                clicked = ui
                    .add_enabled(enabled, button)
                    .on_hover_text(hover)
                    .clicked();
            });
        }
    });
    clicked
}

/// The placeholder for an empty table cell.
pub(crate) fn empty_cell(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("—").color(theme::TEXT_FAINT));
}

pub(crate) fn icon(icon: &str, size: f32, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(icon)
        .font(theme::icon_font(size))
        .color(color)
}

/// The one dialog shape used by every popup in the app.
///
/// Built on [`egui::Modal`] rather than [`egui::Window`] deliberately: a
/// window remembers the position it was last dragged to, and because eframe
/// persists egui memory, that position outlives the session. Reopening after
/// the app was resized or made fullscreen then put the dialog wherever it sat
/// in the old, smaller viewport instead of in view. A modal is re-centred on
/// the current content rect every frame, so it is always where the user is
/// looking, and it brings a backdrop plus Esc-to-close for free.
///
/// Sets `*open` to false when the user dismisses the dialog; returns whatever
/// the body produced.
pub(crate) fn modal<R>(
    ctx: &egui::Context,
    dialog: Dialog<'_>,
    open: &mut bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    modal_with_actions(ctx, dialog, open, add_contents, |_| {})
}

/// How a dialog presents itself: identity, header, and preferred width.
#[derive(Clone, Copy)]
pub(crate) struct Dialog<'a> {
    id: &'a str,
    icon: &'a str,
    title: &'a str,
    width: f32,
}

pub(crate) fn dialog<'a>(id: &'a str, icon: &'a str, title: &'a str, width: f32) -> Dialog<'a> {
    Dialog {
        id,
        icon,
        title,
        width,
    }
}

/// A dialog whose primary action stays pinned below the scrolling body.
///
/// Anything the user must be able to reach — "Apply N changes" — belongs in
/// `actions`, not in the body: a long list of staged changes would otherwise
/// push the button out of the scroll viewport.
pub(crate) fn modal_with_actions<R>(
    ctx: &egui::Context,
    dialog: Dialog<'_>,
    open: &mut bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
    actions: impl FnOnce(&mut egui::Ui),
) -> R {
    // Leave room for the backdrop so a tall dialog never runs off-screen.
    let available = ctx.content_rect().size();
    let response = egui::Modal::new(egui::Id::new(dialog.id)).show(ctx, |ui| {
        ui.set_width(dialog.width.min(available.x - 48.0));
        let mut close = false;
        ui.horizontal(|ui| {
            ui.label(icon(dialog.icon, 19.0, theme::ACCENT));
            ui.heading(dialog.title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui
                    .add(egui::Button::new(icon(icons::X, 13.0, theme::TEXT_MUTED)).frame(false))
                    .on_hover_text("Close (Esc)")
                    .clicked();
            });
        });
        ui.separator();
        let body_height = (available.y - 190.0).max(180.0);
        // The modal's area is anchored at the center, so while it sizes itself
        // the scroll area only sees the space from the center to the bottom
        // edge, and a long body would settle at about half the window. The
        // minimum scrolled height lets overflowing content use the full budget;
        // short content still shrinks to fit.
        let inner = egui::ScrollArea::vertical()
            .max_height(body_height)
            .min_scrolled_height(body_height)
            .auto_shrink([false, true])
            .show(ui, add_contents)
            .inner;
        actions(ui);
        (inner, close)
    });
    let dismissed = response.should_close();
    let (inner, close) = response.inner;
    if close || dismissed {
        *open = false;
    }
    inner
}

/// Standard card surface: card background, hairline border, rounded corners.
pub(crate) fn card<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::STROKE))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add_contents(ui)
        })
}

/// Rounded pill with a tinted background, e.g. version numbers or counts.
pub(crate) fn badge(ui: &mut egui::Ui, text: impl Into<egui::RichText>, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(egui::CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(text.into().color(color).size(11.5));
        });
}

/// Icon + short label in a tinted pill; the generic form behind the
/// comparison and signature chips.
pub(crate) fn chip(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    color: egui::Color32,
) -> egui::Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(egui::CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(self::icon(icon, 12.0, color));
                ui.label(egui::RichText::new(label).color(color).size(11.5));
            });
        })
        .response
}

/// Accent-colored icon next to a heading, for titling view sections.
pub(crate) fn section_heading(ui: &mut egui::Ui, icon: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.label(self::icon(icon, 19.0, theme::ACCENT));
        ui.heading(text);
    });
}

/// Full-width tinted banner used for inline warnings and errors.
/// Returns `true` when the dismiss button (shown if `dismissible`) was clicked.
pub(crate) fn banner(
    ui: &mut egui::Ui,
    color: egui::Color32,
    icon: &str,
    message: &str,
    dismissible: bool,
) -> bool {
    let mut dismissed = false;
    banner_frame(color).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(self::icon(icon, 15.0, color));
            // Leave room for the dismiss button, or a long message wraps
            // across the whole width and pushes the button off the row.
            ui.scope(|ui| {
                if dismissible {
                    ui.set_max_width((ui.available_width() - 40.0).max(160.0));
                }
                ui.add(
                    egui::Label::new(egui::RichText::new(message).color(color))
                        .wrap()
                        .selectable(true),
                );
            });
            if dismissible {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    dismissed = ui
                        .add(egui::Button::new(self::icon(icons::X, 13.0, color)).frame(false))
                        .on_hover_text("Dismiss")
                        .clicked();
                });
            }
        });
    });
    dismissed
}

/// A warning banner with one button, such as "Check again".
/// Returns true when the button was clicked.
pub(crate) fn action_banner(
    ui: &mut egui::Ui,
    color: egui::Color32,
    icon: &str,
    message: &str,
    action: &str,
    enabled: bool,
) -> bool {
    let mut clicked = false;
    banner_frame(color).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(self::icon(icon, 15.0, color));
            ui.scope(|ui| {
                ui.set_max_width((ui.available_width() - 140.0).max(160.0));
                ui.add(
                    egui::Label::new(egui::RichText::new(message).color(color))
                        .wrap()
                        .selectable(true),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                clicked = ui.add_enabled(enabled, egui::Button::new(action)).clicked();
            });
        });
    });
    clicked
}

fn banner_frame(color: egui::Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.5)))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
}

/// Download or verification progress for an official release: a bar when
/// the size is known, otherwise a spinner, with the step in words.
pub(crate) fn download_progress(
    ui: &mut egui::Ui,
    state: dlss_core::ReleaseState,
    received: u64,
    total: Option<u64>,
) {
    let fraction = total.filter(|total| *total > 0).map(|total| {
        #[expect(
            clippy::cast_precision_loss,
            reason = "progress display only needs coarse precision"
        )]
        let fraction = received as f32 / total as f32;
        fraction.clamp(0.0, 1.0)
    });
    let label = super::windows::progress_label(state, received, total);
    match fraction {
        Some(fraction) if state == dlss_core::ReleaseState::Downloading => {
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(ui.available_width().min(260.0))
                    .text(label),
            );
        }
        _ => {
            ui.spinner();
            ui.label(label);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section_colors(text: egui::WidgetText) -> Vec<egui::Color32> {
        let egui::WidgetText::LayoutJob(job) = text else {
            panic!("icon text must remain a layout job");
        };
        job.sections
            .iter()
            .map(|section| section.format.color)
            .collect()
    }

    #[test]
    fn icon_text_inherits_widget_foreground_unless_overridden() {
        let inherited = section_colors(icon_text(icons::SPARKLE, "Update DLSS"));
        assert_eq!(inherited.len(), 2);
        assert!(
            inherited
                .iter()
                .all(|color| *color == egui::Color32::PLACEHOLDER)
        );

        let explicit = section_colors(colored_icon_text(
            icons::SPARKLE,
            "Update DLSS",
            Some(theme::TEXT_ON_ACCENT),
        ));
        assert_eq!(explicit.len(), 2);
        assert!(explicit.iter().all(|color| *color == theme::TEXT_ON_ACCENT));
    }

    #[test]
    fn accent_foreground_has_accessible_contrast() {
        fn luminance(color: egui::Color32) -> f32 {
            let channel = |value: u8| {
                let value = f32::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        }

        let ratio = (luminance(theme::ACCENT) + 0.05) / (luminance(theme::TEXT_ON_ACCENT) + 0.05);
        assert!(ratio >= 4.5, "accent contrast ratio was {ratio}");
        let _ = primary_button("Apply changes");
        let _ = primary_icon_button(icons::SPARKLE, "Update DLSS");
    }
}
