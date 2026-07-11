//! Small reusable visual building blocks shared by all views.

use super::inspector::{comparison_label, signature_label};
use super::theme::{self, icons};
use eframe::egui;

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
pub(crate) fn chip(ui: &mut egui::Ui, icon: &str, label: &str, color: egui::Color32) {
    badge(ui, format!("{icon} {label}"), color);
}

/// Comparison of an installed DLL against its best available target.
pub(crate) fn status_chip(ui: &mut egui::Ui, comparison: dlss_core::Comparison) {
    let (icon, color) = match comparison {
        dlss_core::Comparison::Upgrade => (icons::ARROW_CIRCLE_UP, theme::ACCENT),
        dlss_core::Comparison::Identical => (icons::CHECK_CIRCLE, theme::SUCCESS),
        dlss_core::Comparison::Downgrade => (icons::CARET_DOWN, theme::INFO),
        dlss_core::Comparison::DifferentBuild => (icons::STACK, theme::INFO),
        dlss_core::Comparison::Unknown => (icons::QUESTION, theme::WARNING),
        dlss_core::Comparison::Unavailable => (icons::MINUS, theme::INFO),
    };
    chip(ui, icon, comparison_label(comparison), color);
}

/// Authenticode signature state of an installed DLL.
pub(crate) fn signature_chip(ui: &mut egui::Ui, status: dlss_core::SignatureStatus) {
    let (icon, color) = match status {
        dlss_core::SignatureStatus::Trusted => (icons::SHIELD_CHECK, theme::SUCCESS),
        dlss_core::SignatureStatus::Untrusted => (icons::SHIELD_WARNING, theme::DANGER),
        dlss_core::SignatureStatus::Unsigned => (icons::SHIELD_SLASH, theme::WARNING),
        dlss_core::SignatureStatus::Unavailable => (icons::QUESTION, theme::INFO),
    };
    chip(ui, icon, signature_label(status), color);
}

/// Accent-colored icon next to a heading, for titling view sections.
pub(crate) fn section_heading(ui: &mut egui::Ui, icon: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(icon)
                .color(theme::ACCENT)
                .text_style(egui::TextStyle::Heading),
        );
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
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.5)))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(icon).color(color));
                ui.add(
                    egui::Label::new(egui::RichText::new(message).color(color))
                        .wrap()
                        .selectable(true),
                );
                if dismissible {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        dismissed = ui.small_button(icons::X).clicked();
                    });
                }
            });
        });
    dismissed
}
