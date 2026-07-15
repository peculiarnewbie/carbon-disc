use eframe::egui::{self, Color32, CursorIcon, Pos2, Rect, Response, Sense, Stroke, Vec2};

use super::theme;

const MIN_RANGE_SECS: f64 = 0.1;
const HANDLE_HIT_RADIUS: f32 = 12.0;

#[derive(Clone, Copy)]
enum DragPart {
    Start,
    End,
    Playhead,
}

#[derive(Clone, Copy)]
struct DragState {
    part: DragPart,
    start_secs: f64,
    end_secs: f64,
}

/// A two-handled timeline brush. Drag either violet edge to trim. Clicking or dragging
/// anywhere else seeks the white preview marker.
pub fn show(
    ui: &mut egui::Ui,
    start_secs: &mut f64,
    end_secs: &mut f64,
    playhead_secs: &mut f64,
    duration_secs: f64,
) -> Response {
    let desired_size = Vec2::new(ui.available_width().max(120.0), 54.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());
    let state_id = response.id.with("drag_state");
    let track = Rect::from_min_max(
        Pos2::new(rect.left() + 10.0, rect.bottom() - 21.0),
        Pos2::new(rect.right() - 10.0, rect.bottom() - 13.0),
    );

    *start_secs = start_secs.clamp(0.0, (duration_secs - MIN_RANGE_SECS).max(0.0));
    *end_secs = end_secs.clamp(*start_secs + MIN_RANGE_SECS, duration_secs);
    let max_preview_time = (duration_secs - MIN_RANGE_SECS).max(0.0);
    *playhead_secs = playhead_secs.clamp(0.0, max_preview_time);

    let start_x = time_to_x(*start_secs, duration_secs, track);
    let end_x = time_to_x(*end_secs, duration_secs, track);

    if response.drag_started()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let part = hit_test(pointer, start_x, end_x, track);
        if matches!(part, DragPart::Playhead) {
            *playhead_secs = x_to_time(pointer.x, duration_secs, track).min(max_preview_time);
            response.mark_changed();
        }
        ui.data_mut(|data| {
            data.insert_temp(
                state_id,
                DragState {
                    part,
                    start_secs: *start_secs,
                    end_secs: *end_secs,
                },
            );
        });
    }

    if response.dragged()
        && let Some(state) = ui.data(|data| data.get_temp::<DragState>(state_id))
    {
        let delta_secs = f64::from(response.drag_delta().x / track.width()) * duration_secs;
        match state.part {
            DragPart::Start => {
                *start_secs =
                    (state.start_secs + delta_secs).clamp(0.0, state.end_secs - MIN_RANGE_SECS);
                *playhead_secs = *start_secs;
            }
            DragPart::End => {
                *end_secs = (state.end_secs + delta_secs)
                    .clamp(state.start_secs + MIN_RANGE_SECS, duration_secs);
                *playhead_secs = (*end_secs - MIN_RANGE_SECS).max(0.0);
            }
            DragPart::Playhead => {
                if let Some(pointer) = response.interact_pointer_pos() {
                    *playhead_secs =
                        x_to_time(pointer.x, duration_secs, track).min(max_preview_time);
                }
            }
        }
        response.mark_changed();
    }

    if response.clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        *playhead_secs = x_to_time(pointer.x, duration_secs, track).min(max_preview_time);
        response.mark_changed();
    }

    let start_x = time_to_x(*start_secs, duration_secs, track);
    let end_x = time_to_x(*end_secs, duration_secs, track);
    let playhead_x = time_to_x(*playhead_secs, duration_secs, track);

    let over_handle = response.hover_pos().is_some_and(|pointer| {
        matches!(
            hit_test(pointer, start_x, end_x, track),
            DragPart::Start | DragPart::End
        )
    });
    response = response.on_hover_cursor(if over_handle {
        CursorIcon::ResizeHorizontal
    } else {
        CursorIcon::PointingHand
    });
    response = response.on_hover_text(
        "Drag the violet handles to set start and end. Click or drag elsewhere to preview.",
    );

    let painter = ui.painter_at(rect);
    painter.rect_filled(track, 4.0, Color32::from_rgb(45, 50, 64));
    let selected = Rect::from_min_max(
        Pos2::new(start_x, track.top() - 3.0),
        Pos2::new(end_x, track.bottom() + 3.0),
    );
    painter.rect_filled(selected, 5.0, theme::VIOLET.gamma_multiply(0.65));

    for fraction in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
        let x = egui::lerp(track.x_range(), fraction);
        painter.line_segment(
            [
                Pos2::new(x, track.bottom() + 7.0),
                Pos2::new(x, track.bottom() + 10.0),
            ],
            Stroke::new(1.0, theme::BORDER),
        );
    }

    paint_handle(&painter, start_x, track.center().y, response.dragged());
    paint_handle(&painter, end_x, track.center().y, response.dragged());
    let playhead_top = rect.top() + 7.0;
    painter.line_segment(
        [
            Pos2::new(playhead_x, playhead_top),
            Pos2::new(playhead_x, track.bottom() + 13.0),
        ],
        Stroke::new(2.0, Color32::WHITE),
    );
    painter.circle_filled(Pos2::new(playhead_x, playhead_top), 5.0, Color32::WHITE);
    response
}

fn hit_test(pointer: Pos2, start_x: f32, end_x: f32, track: Rect) -> DragPart {
    // The white preview marker may share an x position with a trim handle. Its circle is
    // in the upper lane, while the violet handles occupy the lower lane.
    let in_handle_lane = pointer.y >= track.center().y - 14.0;
    if in_handle_lane && (pointer.x - start_x).abs() <= HANDLE_HIT_RADIUS {
        DragPart::Start
    } else if in_handle_lane && (pointer.x - end_x).abs() <= HANDLE_HIT_RADIUS {
        DragPart::End
    } else {
        DragPart::Playhead
    }
}

fn time_to_x(seconds: f64, duration: f64, track: Rect) -> f32 {
    let fraction = if duration > 0.0 {
        (seconds / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    egui::lerp(track.x_range(), fraction)
}

fn x_to_time(x: f32, duration: f64, track: Rect) -> f64 {
    let fraction = ((x - track.left()) / track.width()).clamp(0.0, 1.0);
    f64::from(fraction) * duration
}

fn paint_handle(painter: &egui::Painter, x: f32, center_y: f32, active: bool) {
    let handle = Rect::from_center_size(Pos2::new(x, center_y), Vec2::new(8.0, 28.0));
    let color = if active {
        theme::VIOLET
    } else {
        Color32::from_rgb(167, 139, 250)
    };
    painter.rect_filled(handle, 4.0, color);
    painter.line_segment(
        [Pos2::new(x, center_y - 6.0), Pos2::new(x, center_y + 6.0)],
        Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.75)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_preview_and_start_have_separate_hit_lanes() {
        let track = Rect::from_min_max(Pos2::new(10.0, 30.0), Pos2::new(290.0, 38.0));
        assert!(matches!(
            hit_test(Pos2::new(10.0, 8.0), 10.0, 290.0, track),
            DragPart::Playhead
        ));
        assert!(matches!(
            hit_test(Pos2::new(10.0, 34.0), 10.0, 290.0, track),
            DragPart::Start
        ));
    }

    #[test]
    fn timeline_background_drags_the_preview() {
        let track = Rect::from_min_max(Pos2::new(10.0, 30.0), Pos2::new(290.0, 38.0));
        assert!(matches!(
            hit_test(Pos2::new(150.0, 34.0), 10.0, 290.0, track),
            DragPart::Playhead
        ));
    }
}
