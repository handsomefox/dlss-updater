//! Dark application theme: palette, egui visuals, fonts, and icon glyphs.
//!
//! Every color used by the UI lives here so views never hard-code
//! `Color32` literals. The icon glyphs map to the vendored Phosphor icon
//! font (`assets/fonts/Phosphor.ttf`, MIT licensed).

use eframe::egui;

pub(crate) const ICON_FAMILY_NAME: &str = "phosphor-icons";

pub(crate) fn icon_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(ICON_FAMILY_NAME.into()))
}

// Layered backgrounds, darkest (app chrome) to lightest (interactive widgets).
pub(crate) const BG_APP: egui::Color32 = egui::Color32::from_rgb(0x0E, 0x11, 0x16);
pub(crate) const BG_PANEL: egui::Color32 = egui::Color32::from_rgb(0x15, 0x1A, 0x21);
pub(crate) const BG_CARD: egui::Color32 = egui::Color32::from_rgb(0x1C, 0x23, 0x2C);
pub(crate) const BG_WIDGET: egui::Color32 = egui::Color32::from_rgb(0x24, 0x2D, 0x38);
pub(crate) const BG_WIDGET_HOVER: egui::Color32 = egui::Color32::from_rgb(0x2D, 0x38, 0x45);
pub(crate) const STROKE: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x33, 0x40);

pub(crate) const TEXT: egui::Color32 = egui::Color32::from_rgb(0xE2, 0xE8, 0xF0);
pub(crate) const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(0x93, 0xA1, 0xB0);

/// NVIDIA green; doubles as the success color so the palette stays cohesive.
pub(crate) const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x76, 0xB9, 0x00);
pub(crate) const SUCCESS: egui::Color32 = ACCENT;
pub(crate) const WARNING: egui::Color32 = egui::Color32::from_rgb(0xF0, 0xB4, 0x29);
pub(crate) const DANGER: egui::Color32 = egui::Color32::from_rgb(0xE5, 0x54, 0x58);
pub(crate) const INFO: egui::Color32 = TEXT_MUTED;

/// Installs the dark theme, fonts, and widget styling. Call once at startup.
pub(crate) fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_fonts(font_definitions());
    ctx.style_mut_of(egui::Theme::Dark, style);
}

fn style(style: &mut egui::Style) {
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.window_margin = egui::Margin::same(14);
    style.spacing.menu_margin = egui::Margin::same(8);
    style.interaction.selectable_labels = false;
    style.text_styles = [
        (
            egui::TextStyle::Heading,
            egui::FontId::new(19.0, egui::FontFamily::Name("semibold".into())),
        ),
        (
            egui::TextStyle::Body,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Button,
            egui::FontId::new(14.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Small,
            egui::FontId::new(11.5, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            egui::FontId::new(13.0, egui::FontFamily::Monospace),
        ),
    ]
    .into();
    visuals(&mut style.visuals);
}

fn visuals(visuals: &mut egui::Visuals) {
    let corner = egui::CornerRadius::same(6);
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = BG_CARD;
    visuals.window_stroke = egui::Stroke::new(1.0, STROKE);
    visuals.window_corner_radius = egui::CornerRadius::same(10);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);
    visuals.extreme_bg_color = BG_APP;
    visuals.faint_bg_color = egui::Color32::from_rgb(0x19, 0x1F, 0x27);
    visuals.code_bg_color = BG_APP;
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = DANGER;
    visuals.hyperlink_color = egui::Color32::from_rgb(0x9A, 0xD1, 0x3D);
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = BG_PANEL;
    widgets.noninteractive.weak_bg_fill = BG_PANEL;
    widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, STROKE);
    widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    widgets.noninteractive.corner_radius = corner;
    widgets.inactive.bg_fill = BG_WIDGET;
    widgets.inactive.weak_bg_fill = BG_WIDGET;
    widgets.inactive.bg_stroke = egui::Stroke::new(1.0, STROKE);
    widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    widgets.inactive.corner_radius = corner;
    widgets.hovered.bg_fill = BG_WIDGET_HOVER;
    widgets.hovered.weak_bg_fill = BG_WIDGET_HOVER;
    widgets.hovered.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(0x3D, 0x4A, 0x59));
    widgets.hovered.fg_stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
    widgets.hovered.corner_radius = corner;
    widgets.active.bg_fill = ACCENT.gamma_multiply(0.30);
    widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.30);
    widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    widgets.active.fg_stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
    widgets.active.corner_radius = corner;
    widgets.open.bg_fill = BG_WIDGET_HOVER;
    widgets.open.weak_bg_fill = BG_WIDGET_HOVER;
    widgets.open.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    widgets.open.fg_stroke = egui::Stroke::new(1.0, TEXT);
    widgets.open.corner_radius = corner;
}

fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        (
            "inter",
            &include_bytes!("../../assets/fonts/Inter-Regular.ttf")[..],
        ),
        (
            "inter-semibold",
            &include_bytes!("../../assets/fonts/Inter-SemiBold.ttf")[..],
        ),
        (
            "jetbrains-mono",
            &include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf")[..],
        ),
        (
            "phosphor",
            &include_bytes!("../../assets/fonts/Phosphor.ttf")[..],
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), egui::FontData::from_static(bytes).into());
    }
    let proportional = fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default();
    proportional.insert(0, "inter".into());
    let monospace = fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default();
    monospace.insert(0, "jetbrains-mono".into());
    fonts.families.insert(
        egui::FontFamily::Name("semibold".into()),
        vec!["inter-semibold".into()],
    );
    fonts.families.insert(
        egui::FontFamily::Name(ICON_FAMILY_NAME.into()),
        vec!["phosphor".into(), "inter".into()],
    );
    fonts
}

/// Glyphs from the vendored Phosphor icon font (regular variant).
///
/// Codepoints must match `assets/fonts/Phosphor.ttf`; they are copied from
/// the `egui-phosphor` 0.12 generated tables for that exact font file.
pub(crate) mod icons {
    pub(crate) const ARROW_CIRCLE_UP: &str = "\u{E030}";
    pub(crate) const ARROW_CLOCKWISE: &str = "\u{E036}";
    pub(crate) const ARROW_LEFT: &str = "\u{E058}";
    pub(crate) const ARROW_RIGHT: &str = "\u{E06C}";
    pub(crate) const ARROW_SQUARE_OUT: &str = "\u{E5DE}";
    pub(crate) const ARROW_U_UP_LEFT: &str = "\u{E08A}";
    pub(crate) const CARET_DOWN: &str = "\u{E136}";
    pub(crate) const CARET_UP: &str = "\u{E13C}";
    pub(crate) const CHECK_CIRCLE: &str = "\u{E184}";
    pub(crate) const CIRCLE: &str = "\u{E18A}";
    pub(crate) const CLOCK_COUNTER_CLOCKWISE: &str = "\u{E1A0}";
    pub(crate) const DOWNLOAD_SIMPLE: &str = "\u{E20C}";
    pub(crate) const EYE: &str = "\u{E220}";
    pub(crate) const FOLDER_PLUS: &str = "\u{E258}";
    pub(crate) const FOLDER_SIMPLE: &str = "\u{E25A}";
    pub(crate) const GAME_CONTROLLER: &str = "\u{E26E}";
    pub(crate) const INFO: &str = "\u{E2CE}";
    pub(crate) const LIGHTNING: &str = "\u{E2DE}";
    pub(crate) const LIST_CHECKS: &str = "\u{EADC}";
    pub(crate) const MAGNIFYING_GLASS: &str = "\u{E30C}";
    pub(crate) const MINUS: &str = "\u{E32A}";
    pub(crate) const PACKAGE: &str = "\u{E390}";
    pub(crate) const PULSE: &str = "\u{E000}";
    pub(crate) const QUESTION: &str = "\u{E3E8}";
    pub(crate) const SHIELD_CHECK: &str = "\u{E40C}";
    pub(crate) const SHIELD_SLASH: &str = "\u{E410}";
    pub(crate) const SHIELD_WARNING: &str = "\u{E412}";
    pub(crate) const SLIDERS_HORIZONTAL: &str = "\u{E434}";
    pub(crate) const SPARKLE: &str = "\u{E6A2}";
    pub(crate) const STACK: &str = "\u{E466}";
    pub(crate) const TRASH_SIMPLE: &str = "\u{E4A8}";
    pub(crate) const WARNING: &str = "\u{E4E0}";
    pub(crate) const WARNING_CIRCLE: &str = "\u{E4E2}";
    pub(crate) const WRENCH: &str = "\u{E5D4}";
    pub(crate) const X: &str = "\u{E4F6}";
}
