use eframe::egui::{self, Color32};

pub const VIOLET: Color32 = Color32::from_rgb(139, 92, 246);
pub const GREEN: Color32 = Color32::from_rgb(34, 197, 94);
pub const RED: Color32 = Color32::from_rgb(239, 68, 68);
pub const PANEL: Color32 = Color32::from_rgb(18, 20, 28);
pub const WINDOW: Color32 = Color32::from_rgb(26, 29, 38);
pub const BORDER: Color32 = Color32::from_rgb(38, 43, 56);
pub const MUTED: Color32 = Color32::from_rgb(148, 163, 184);

pub fn apply(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = WINDOW;
    visuals.extreme_bg_color = Color32::from_rgb(12, 14, 20);
    visuals.faint_bg_color = Color32::from_rgb(31, 35, 46);
    visuals.widgets.noninteractive.bg_fill = WINDOW;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(35, 39, 51);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(51, 45, 78);
    visuals.widgets.active.bg_fill = VIOLET;
    visuals.selection.bg_fill = VIOLET.gamma_multiply(0.55);
    visuals.window_corner_radius = egui::CornerRadius::same(8);
    context.set_visuals(visuals);
    let mut style = (*context.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    context.set_style_of(egui::Theme::Dark, style);
}
