use eframe::egui::{
    self, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Vec2,
    pos2, vec2,
};

use super::theme;

#[derive(Clone, Copy)]
pub enum Icon {
    /// `[` with a playhead: start the clip here.
    MarkIn,
    /// `]` with a playhead: end the clip here.
    MarkOut,
    /// `|←→|`: select the whole video.
    FullRange,
    /// Two interlocking corners.
    Crop,
}

impl Icon {
    const fn name(self) -> &'static str {
        match self {
            Self::MarkIn => "Set start",
            Self::MarkOut => "Set end",
            Self::FullRange => "Whole video",
            Self::Crop => "Crop picture",
        }
    }
}

/// A square button with a painted icon, sized to line up with text fields.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, enabled: bool) -> Response {
    let size = Vec2::splat(ui.spacing().interact_size.y);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, icon.name()));
    paint_icon_button(ui, rect, &response, icon, false, enabled);
    response
}

/// An icon button that stays highlighted while its mode is on.
pub fn icon_toggle(ui: &mut egui::Ui, icon: Icon, selected: bool) -> Response {
    let size = Vec2::splat(ui.spacing().interact_size.y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, icon.name())
    });
    paint_icon_button(ui, rect, &response, icon, selected, true);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn paint_icon_button(
    ui: &egui::Ui,
    rect: Rect,
    response: &Response,
    icon: Icon,
    selected: bool,
    enabled: bool,
) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let visuals = ui.style().interact(response);
    let (fill, stroke, color) = if !enabled {
        (
            theme::CONTROL.gamma_multiply(0.6),
            Stroke::new(1.0, theme::BORDER),
            theme::MUTED.gamma_multiply(0.45),
        )
    } else if selected {
        (
            theme::VIOLET,
            Stroke::new(1.0, theme::VIOLET_LIGHT),
            Color32::WHITE,
        )
    } else {
        (
            visuals.weak_bg_fill,
            visuals.bg_stroke,
            Color32::from_gray(225),
        )
    };
    ui.painter()
        .rect(rect, theme::RADIUS, fill, stroke, StrokeKind::Inside);
    let size = rect.height() * 0.5;
    paint_icon(
        ui.painter(),
        icon,
        Rect::from_center_size(rect.center(), Vec2::splat(size)),
        color,
    );
}

fn paint_icon(painter: &egui::Painter, icon: Icon, r: Rect, color: Color32) {
    let stroke = Stroke::new(1.8, color);
    let thin = Stroke::new(1.2, color);
    let (l, t, right, b) = (r.left(), r.top(), r.right(), r.bottom());
    match icon {
        Icon::MarkIn | Icon::MarkOut => {
            // A bracket with a triangle pointing into the clip: "[▸" and "◂]".
            let (edge, lip, tip, base) = if matches!(icon, Icon::MarkIn) {
                (l + 1.5, l + 5.5, right - 0.5, l + 6.5)
            } else {
                (right - 1.5, right - 5.5, l + 0.5, right - 6.5)
            };
            painter.line(
                vec![pos2(lip, t), pos2(edge, t), pos2(edge, b), pos2(lip, b)],
                stroke,
            );
            let y = r.center().y;
            painter.add(egui::Shape::convex_polygon(
                if tip > base {
                    vec![pos2(base, y - 4.5), pos2(tip, y), pos2(base, y + 4.5)]
                } else {
                    vec![pos2(base, y - 4.5), pos2(base, y + 4.5), pos2(tip, y)]
                },
                color,
                Stroke::NONE,
            ));
        }
        Icon::FullRange => {
            let y = r.center().y;
            painter.line_segment([pos2(l, t + 1.0), pos2(l, b - 1.0)], stroke);
            painter.line_segment([pos2(right, t + 1.0), pos2(right, b - 1.0)], stroke);
            painter.line_segment([pos2(l + 2.0, y), pos2(right - 2.0, y)], thin);
            for (tip, dir) in [(l + 2.0, 1.0), (right - 2.0, -1.0)] {
                painter.line(
                    vec![
                        pos2(tip + 3.5 * dir, y - 3.0),
                        pos2(tip, y),
                        pos2(tip + 3.5 * dir, y + 3.0),
                    ],
                    thin,
                );
            }
        }
        Icon::Crop => {
            painter.line(
                vec![
                    pos2(l + 3.5, t),
                    pos2(l + 3.5, b - 3.5),
                    pos2(right, b - 3.5),
                ],
                stroke,
            );
            painter.line(
                vec![
                    pos2(l, t + 3.5),
                    pos2(right - 3.5, t + 3.5),
                    pos2(right - 3.5, b),
                ],
                stroke,
            );
        }
    }
}

/// A joined row of mutually exclusive options. `disabled` returns a reason for options
/// that cannot be picked right now. Returns true when the value changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut T,
    options: &[(T, &str)],
    disabled: impl Fn(T) -> Option<String>,
) -> bool {
    let height = ui.spacing().interact_size.y;
    let padding = 14.0;
    let font = FontId::proportional(14.0);
    let galleys: Vec<_> = options
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap((*label).to_owned(), font.clone(), Color32::PLACEHOLDER)
        })
        .collect();
    let widths: Vec<f32> = galleys
        .iter()
        .map(|galley| galley.size().x + padding * 2.0)
        .collect();
    let (outer, _) = ui.allocate_exact_size(vec2(widths.iter().sum(), height), Sense::hover());
    let painter = ui.painter();
    painter.rect(
        outer,
        theme::RADIUS,
        ui.visuals().extreme_bg_color,
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    let base_id = ui.make_persistent_id(id_salt);
    let mut changed = false;
    let mut x = outer.left();
    for (index, ((option, label), galley)) in options.iter().zip(galleys).enumerate() {
        let segment = Rect::from_min_size(pos2(x, outer.top()), vec2(widths[index], height));
        x += widths[index];
        let reason = disabled(*option);
        let sense = if reason.is_some() {
            Sense::hover()
        } else {
            Sense::click()
        };
        let mut response = ui.interact(segment, base_id.with(index), sense);
        let selected = *value == *option;
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                reason.is_none(),
                selected,
                *label,
            )
        });
        let inner = segment.shrink(2.0);
        if selected {
            ui.painter()
                .rect_filled(inner, CornerRadius::same(theme::RADIUS - 2), theme::VIOLET);
        } else if response.hovered() && reason.is_none() {
            ui.painter().rect_filled(
                inner,
                CornerRadius::same(theme::RADIUS - 2),
                theme::CONTROL_HOVER,
            );
        }
        let text_color = if selected {
            Color32::WHITE
        } else if reason.is_some() {
            theme::MUTED.gamma_multiply(0.4)
        } else {
            theme::MUTED
        };
        ui.painter()
            .galley(segment.center() - galley.size() / 2.0, galley, text_color);
        if let Some(reason) = reason {
            response = response.on_hover_text(reason);
        } else {
            response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
    }
    changed
}

/// Underlined tabs sitting on a hairline, meant for the top of a panel.
pub fn tab_bar<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, tabs: &[(T, String)]) {
    let height = 40.0;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter().line_segment(
        [row.left_bottom(), row.right_bottom()],
        Stroke::new(1.0, theme::BORDER),
    );
    let base_id = ui.make_persistent_id("tab_bar");
    let mut x = row.left();
    for (index, (tab, label)) in tabs.iter().enumerate() {
        let selected = *value == *tab;
        let font = FontId::proportional(15.0);
        let galley = ui
            .painter()
            .layout_no_wrap(label.clone(), font, Color32::PLACEHOLDER);
        let rect = Rect::from_min_size(pos2(x, row.top()), vec2(galley.size().x + 28.0, height));
        x = rect.right();
        let response = ui
            .interact(rect, base_id.with(index), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
        });
        let color = if selected {
            Color32::WHITE
        } else if response.hovered() {
            Color32::from_gray(220)
        } else {
            theme::MUTED
        };
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, color);
        if selected {
            let underline = Rect::from_min_max(
                pos2(rect.left() + 6.0, rect.bottom() - 2.5),
                pos2(rect.right() - 6.0, rect.bottom() + 0.5),
            );
            ui.painter()
                .rect_filled(underline, CornerRadius::same(2), theme::VIOLET);
        } else if response.hovered() {
            ui.painter().line_segment(
                [
                    pos2(rect.left() + 6.0, rect.bottom()),
                    pos2(rect.right() - 6.0, rect.bottom()),
                ],
                Stroke::new(1.0, theme::MUTED.gamma_multiply(0.6)),
            );
        }
        if response.clicked() {
            *value = *tab;
        }
    }
}

/// A small rounded label drawn on top of the preview.
pub fn badge(painter: &egui::Painter, anchor: Pos2, align: egui::Align2, text: String) -> Rect {
    let galley = painter.layout_no_wrap(text, FontId::monospace(12.0), Color32::WHITE);
    let size = galley.size() + vec2(12.0, 6.0);
    let rect = align.anchor_size(anchor, size);
    painter.rect_filled(rect, 4.0, Color32::from_black_alpha(170));
    painter.galley(rect.min + vec2(6.0, 3.0), galley, Color32::WHITE);
    rect
}
