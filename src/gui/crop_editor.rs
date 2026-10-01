use eframe::egui::{self, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};

use super::{theme, widgets};
use crate::CropMargins;

/// The smallest picture, in source pixels, that the crop may leave.
const MIN_SIZE: u32 = 16;
const EDGE_GRAB: f32 = 10.0;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Grab {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

impl Grab {
    const MOVE: Self = Self {
        left: true,
        right: true,
        top: true,
        bottom: true,
    };

    fn is_none(self) -> bool {
        self == Self::default()
    }

    fn cursor(self) -> CursorIcon {
        match (self.left || self.right, self.top || self.bottom) {
            _ if self == Self::MOVE => CursorIcon::Move,
            (true, true) if self.left == self.top => CursorIcon::ResizeNwSe,
            (true, true) => CursorIcon::ResizeNeSw,
            (true, false) => CursorIcon::ResizeHorizontal,
            (false, true) => CursorIcon::ResizeVertical,
            (false, false) => CursorIcon::Default,
        }
    }
}

#[derive(Clone, Copy)]
struct DragState {
    grab: Grab,
    origin: Pos2,
    crop: CropMargins,
}

/// Shades the cropped-away area of the preview. While `editing`, the crop box can be
/// resized from its edges and corners or moved by dragging inside it.
/// Returns true when the crop changed.
pub fn show(
    ui: &mut egui::Ui,
    image: Rect,
    source_width: u32,
    source_height: u32,
    crop: &mut CropMargins,
    editing: bool,
) -> bool {
    if source_width < MIN_SIZE || source_height < MIN_SIZE {
        return false;
    }
    let mut changed = false;
    if editing {
        let id = ui.make_persistent_id("crop_editor");
        let response = ui.interact(image, id, Sense::drag());
        let state_id = id.with("drag");
        let box_rect = crop_rect(image, source_width, source_height, *crop);
        if response.drag_started()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let grab = hit_test(box_rect, pointer);
            if !grab.is_none() {
                ui.data_mut(|data| {
                    data.insert_temp(
                        state_id,
                        DragState {
                            grab,
                            origin: pointer,
                            crop: *crop,
                        },
                    );
                });
            }
        }
        let drag = ui.data(|data| data.get_temp::<DragState>(state_id));
        if response.dragged()
            && let (Some(state), Some(pointer)) = (drag, response.interact_pointer_pos())
        {
            let scale_x = source_width as f32 / image.width();
            let scale_y = source_height as f32 / image.height();
            let dx = ((pointer.x - state.origin.x) * scale_x).round() as i64;
            let dy = ((pointer.y - state.origin.y) * scale_y).round() as i64;
            let updated = drag_crop(state.crop, state.grab, dx, dy, source_width, source_height);
            if updated != *crop {
                *crop = updated;
                changed = true;
            }
        }
        if response.drag_stopped() {
            ui.data_mut(|data| data.remove::<DragState>(state_id));
        }
        let active_grab = drag
            .filter(|_| response.dragged())
            .map(|state| state.grab)
            .or_else(|| {
                response
                    .hover_pos()
                    .map(|pointer| hit_test(box_rect, pointer))
            });
        if let Some(grab) = active_grab
            && !grab.is_none()
        {
            ui.ctx().set_cursor_icon(grab.cursor());
        }
        paint(
            ui,
            image,
            source_width,
            source_height,
            *crop,
            true,
            response.dragged(),
        );
    } else {
        paint(ui, image, source_width, source_height, *crop, false, false);
    }
    changed
}

fn crop_rect(image: Rect, source_width: u32, source_height: u32, crop: CropMargins) -> Rect {
    let x = |pixels: u32| image.width() * pixels as f32 / source_width as f32;
    let y = |pixels: u32| image.height() * pixels as f32 / source_height as f32;
    Rect::from_min_max(
        pos2(image.left() + x(crop.left), image.top() + y(crop.top)),
        pos2(
            image.right() - x(crop.right),
            image.bottom() - y(crop.bottom),
        ),
    )
}

fn hit_test(box_rect: Rect, pointer: Pos2) -> Grab {
    let near_x =
        pointer.x >= box_rect.left() - EDGE_GRAB && pointer.x <= box_rect.right() + EDGE_GRAB;
    let near_y =
        pointer.y >= box_rect.top() - EDGE_GRAB && pointer.y <= box_rect.bottom() + EDGE_GRAB;
    let grab = Grab {
        left: near_y && (pointer.x - box_rect.left()).abs() <= EDGE_GRAB,
        right: near_y && (pointer.x - box_rect.right()).abs() <= EDGE_GRAB,
        top: near_x && (pointer.y - box_rect.top()).abs() <= EDGE_GRAB,
        bottom: near_x && (pointer.y - box_rect.bottom()).abs() <= EDGE_GRAB,
    };
    if !grab.is_none() {
        // On a tiny box both opposite edges can be in range; keep the closer one.
        let mut grab = grab;
        if grab.left && grab.right {
            let left_closer =
                (pointer.x - box_rect.left()).abs() < (pointer.x - box_rect.right()).abs();
            grab.left = left_closer;
            grab.right = !left_closer;
        }
        if grab.top && grab.bottom {
            let top_closer =
                (pointer.y - box_rect.top()).abs() < (pointer.y - box_rect.bottom()).abs();
            grab.top = top_closer;
            grab.bottom = !top_closer;
        }
        grab
    } else if box_rect.contains(pointer) {
        Grab::MOVE
    } else {
        Grab::default()
    }
}

/// Applies a drag of `dx`/`dy` source pixels to the crop captured when the drag began.
fn drag_crop(
    start: CropMargins,
    grab: Grab,
    dx: i64,
    dy: i64,
    width: u32,
    height: u32,
) -> CropMargins {
    let (width, height, min) = (i64::from(width), i64::from(height), i64::from(MIN_SIZE));
    let (mut left, mut right) = (i64::from(start.left), i64::from(start.right));
    let (mut top, mut bottom) = (i64::from(start.top), i64::from(start.bottom));
    if grab == Grab::MOVE {
        let box_width = width - left - right;
        let box_height = height - top - bottom;
        left = (left + dx).clamp(0, width - box_width);
        right = width - box_width - left;
        top = (top + dy).clamp(0, height - box_height);
        bottom = height - box_height - top;
    } else {
        if grab.left {
            left = (left + dx).clamp(0, width - right - min);
        }
        if grab.right {
            right = (right - dx).clamp(0, width - left - min);
        }
        if grab.top {
            top = (top + dy).clamp(0, height - bottom - min);
        }
        if grab.bottom {
            bottom = (bottom - dy).clamp(0, height - top - min);
        }
    }
    let even = |value: i64| u32::try_from(value.max(0)).unwrap_or(0) & !1;
    CropMargins {
        top: even(top),
        bottom: even(bottom),
        left: even(left),
        right: even(right),
    }
}

fn paint(
    ui: &egui::Ui,
    image: Rect,
    source_width: u32,
    source_height: u32,
    crop: CropMargins,
    editing: bool,
    dragging: bool,
) {
    if crop.is_zero() && !editing {
        return;
    }
    let painter = ui.painter_at(image);
    let inner = crop_rect(image, source_width, source_height, crop);
    let shade = Color32::from_black_alpha(if editing { 150 } else { 190 });
    for rect in [
        Rect::from_min_max(image.min, pos2(image.right(), inner.top())),
        Rect::from_min_max(pos2(image.left(), inner.bottom()), image.max),
        Rect::from_min_max(
            pos2(image.left(), inner.top()),
            pos2(inner.left(), inner.bottom()),
        ),
        Rect::from_min_max(
            pos2(inner.right(), inner.top()),
            pos2(image.right(), inner.bottom()),
        ),
    ] {
        painter.rect_filled(rect, 0.0, shade);
    }
    if !editing {
        painter.rect_stroke(
            inner,
            0.0,
            Stroke::new(1.0, theme::VIOLET),
            StrokeKind::Inside,
        );
        return;
    }

    let line = Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.85));
    painter.rect_stroke(inner, 0.0, line, StrokeKind::Inside);
    if dragging {
        // Rule-of-thirds guides help line up the new framing.
        let guide = Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.25));
        for fraction in [1.0 / 3.0, 2.0 / 3.0] {
            let x = egui::lerp(inner.x_range(), fraction);
            let y = egui::lerp(inner.y_range(), fraction);
            painter.line_segment([pos2(x, inner.top()), pos2(x, inner.bottom())], guide);
            painter.line_segment([pos2(inner.left(), y), pos2(inner.right(), y)], guide);
        }
    }

    let handle = Stroke::new(3.0, Color32::WHITE);
    let arm = 14.0_f32.min(inner.width() / 3.0).min(inner.height() / 3.0);
    for (corner, dx, dy) in [
        (inner.left_top(), 1.0, 1.0),
        (inner.right_top(), -1.0, 1.0),
        (inner.left_bottom(), 1.0, -1.0),
        (inner.right_bottom(), -1.0, -1.0),
    ] {
        painter.line(
            vec![
                corner + vec2(dx * arm, 0.0),
                corner,
                corner + vec2(0.0, dy * arm),
            ],
            handle,
        );
    }
    let bar = 9.0_f32.min(inner.width() / 4.0).min(inner.height() / 4.0);
    for (center, horizontal) in [
        (inner.center_top(), true),
        (inner.center_bottom(), true),
        (inner.left_center(), false),
        (inner.right_center(), false),
    ] {
        let offset = if horizontal {
            vec2(bar, 0.0)
        } else {
            vec2(0.0, bar)
        };
        painter.line_segment([center - offset, center + offset], handle);
    }

    if !dragging {
        return;
    }
    let cropped_width = source_width - crop.left - crop.right;
    let cropped_height = source_height - crop.top - crop.bottom;
    widgets::badge(
        &painter,
        inner.center_bottom() - vec2(0.0, 10.0),
        egui::Align2::CENTER_BOTTOM,
        format!("{cropped_width} × {cropped_height}"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn margins(top: u32, bottom: u32, left: u32, right: u32) -> CropMargins {
        CropMargins {
            top,
            bottom,
            left,
            right,
        }
    }

    #[test]
    fn dragging_an_edge_moves_only_that_edge_and_stays_even() {
        let grab = Grab {
            top: true,
            ..Grab::default()
        };
        assert_eq!(
            drag_crop(CropMargins::ZERO, grab, 0, 141, 1920, 1080),
            margins(140, 0, 0, 0)
        );
    }

    #[test]
    fn edges_cannot_cross_or_leave_the_picture() {
        let grab = Grab {
            left: true,
            ..Grab::default()
        };
        assert_eq!(
            drag_crop(margins(0, 0, 0, 100), grab, 5000, 0, 1920, 1080),
            margins(0, 0, 1920 - 100 - 16, 100)
        );
        assert_eq!(
            drag_crop(margins(0, 0, 50, 0), grab, -500, 0, 1920, 1080),
            margins(0, 0, 0, 0)
        );
    }

    #[test]
    fn moving_keeps_the_box_size() {
        let start = margins(100, 100, 200, 200);
        let moved = drag_crop(start, Grab::MOVE, -500, 40, 1920, 1080);
        assert_eq!(moved, margins(140, 60, 0, 400));
    }

    #[test]
    fn grabbing_prefers_corners_then_edges_then_inside() {
        let box_rect = Rect::from_min_max(pos2(100.0, 100.0), pos2(500.0, 400.0));
        let corner = hit_test(box_rect, pos2(102.0, 98.0));
        assert!(corner.left && corner.top && !corner.right && !corner.bottom);
        let edge = hit_test(box_rect, pos2(300.0, 405.0));
        assert!(edge.bottom && !edge.left && !edge.right && !edge.top);
        assert!(hit_test(box_rect, pos2(300.0, 250.0)) == Grab::MOVE);
        assert!(hit_test(box_rect, pos2(20.0, 20.0)).is_none());
    }
}
