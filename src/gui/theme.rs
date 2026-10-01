use eframe::egui::{self, Color32, CornerRadius, Stroke};

pub const VIOLET: Color32 = Color32::from_rgb(139, 92, 246);
pub const VIOLET_LIGHT: Color32 = Color32::from_rgb(167, 139, 250);
pub const GREEN: Color32 = Color32::from_rgb(34, 197, 94);
pub const RED: Color32 = Color32::from_rgb(239, 68, 68);
pub const PANEL: Color32 = Color32::from_rgb(18, 20, 28);
pub const WINDOW: Color32 = Color32::from_rgb(26, 29, 38);
pub const BORDER: Color32 = Color32::from_rgb(38, 43, 56);
pub const MUTED: Color32 = Color32::from_rgb(148, 163, 184);
pub const CONTROL: Color32 = Color32::from_rgb(35, 39, 51);
pub const CONTROL_HOVER: Color32 = Color32::from_rgb(48, 53, 70);
pub const RADIUS: u8 = 6;

pub fn apply(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = WINDOW;
    visuals.extreme_bg_color = Color32::from_rgb(12, 14, 20);
    visuals.faint_bg_color = Color32::from_rgb(31, 35, 46);
    visuals.widgets.noninteractive.bg_fill = WINDOW;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);

    // Buttons paint with `weak_bg_fill`; text fields and sliders use `bg_fill`.
    let control_stroke = Stroke::new(1.0, Color32::from_rgb(52, 58, 76));
    let widgets = &mut visuals.widgets;
    widgets.inactive.weak_bg_fill = CONTROL;
    widgets.inactive.bg_fill = CONTROL;
    widgets.inactive.bg_stroke = control_stroke;
    widgets.hovered.weak_bg_fill = CONTROL_HOVER;
    widgets.hovered.bg_fill = CONTROL_HOVER;
    widgets.hovered.bg_stroke = Stroke::new(1.0, VIOLET.gamma_multiply(0.6));
    widgets.active.weak_bg_fill = Color32::from_rgb(60, 52, 96);
    widgets.active.bg_fill = VIOLET;
    widgets.active.bg_stroke = Stroke::new(1.0, VIOLET);
    widgets.open.weak_bg_fill = CONTROL_HOVER;
    widgets.open.bg_stroke = Stroke::new(1.0, VIOLET.gamma_multiply(0.6));
    for state in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.corner_radius = CornerRadius::same(RADIUS);
        state.expansion = 0.0;
    }
    visuals.selection.bg_fill = VIOLET.gamma_multiply(0.55);
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.menu_corner_radius = CornerRadius::same(RADIUS);
    context.set_visuals(visuals);

    let mut style = (*context.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 28.0;
    context.set_style_of(egui::Theme::Dark, style);
}
