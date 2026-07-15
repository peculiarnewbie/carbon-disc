use eframe::egui::{self, Color32, CursorIcon, Pos2, Rect, Response, Sense, Stroke, Vec2};

use super::theme;

const MIN_RANGE_SECS: f64 = 0.1;
const HANDLE_HIT_RADIUS: f32 = 12.0;

#[derive(Clone, Copy)]
enum DragPart {
    Start,
    End,
    Selection,
}

#[derive(Clone, Copy)]
struct DragState {
    part: DragPart,
    start_secs: f64,
    end_secs: f64,
}

/// A two-handled timeline brush. Drag either edge to trim, or drag the highlighted
/// selection to move the whole range without changing its duration.
pub fn show(
    ui: &mut egui::Ui,
    start_secs: &mut f64,
    end_secs: &mut f64,
    duration_secs: f64,
) -> Response {
    let desired_size = Vec2::new(ui.available_width().max(120.0), 42.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());
    let state_id = response.id.with("drag_state");
    let track = Rect::from_min_max(
        Pos2::new(rect.left() + 10.0, rect.center().y - 4.0),
        Pos2::new(rect.right() - 10.0, rect.center().y + 4.0),
    );

    *start_secs = start_secs.clamp(0.0, (duration_secs - MIN_RANGE_SECS).max(0.0));
    *end_secs = end_secs.clamp(*start_secs + MIN_RANGE_SECS, duration_secs);

    let start_x = time_to_x(*start_secs, duration_secs, track);
    let end_x = time_to_x(*end_secs, duration_secs, track);

    if response.drag_started()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let part = if (pointer.x - start_x).abs() <= HANDLE_HIT_RADIUS {
            DragPart::Start
        } else if (pointer.x - end_x).abs() <= HANDLE_HIT_RADIUS {
            DragPart::End
        } else if pointer.x > start_x && pointer.x < end_x {
            DragPart::Selection
        } else if (pointer.x - start_x).abs() < (pointer.x - end_x).abs() {
            DragPart::Start
        } else {
            DragPart::End
        };
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
            }
            DragPart::End => {
                *end_secs = (state.end_secs + delta_secs)
                    .clamp(state.start_secs + MIN_RANGE_SECS, duration_secs);
            }
            DragPart::Selection => {
                let selection_duration = state.end_secs - state.start_secs;
                let moved_start =
                    (state.start_secs + delta_secs).clamp(0.0, duration_secs - selection_duration);
                *start_secs = moved_start;
                *end_secs = moved_start + selection_duration;
            }
        }
        response.mark_changed();
    }

    let pointer_x = response.hover_pos().map(|pointer| pointer.x);
    let over_handle = pointer_x.is_some_and(|x| {
        (x - start_x).abs() <= HANDLE_HIT_RADIUS || (x - end_x).abs() <= HANDLE_HIT_RADIUS
    });
    response = response.on_hover_cursor(if over_handle {
        CursorIcon::ResizeHorizontal
    } else {
        CursorIcon::Grab
    });

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
    response
}

fn time_to_x(seconds: f64, duration: f64, track: Rect) -> f32 {
    let fraction = if duration > 0.0 {
        (seconds / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    egui::lerp(track.x_range(), fraction)
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
