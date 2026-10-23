use eframe::egui::{self, Color32, CursorIcon, Pos2, Rect, Response, Sense, Stroke, Vec2};

use super::theme;

const MIN_RANGE_SECS: f64 = 0.1;
const HANDLE_HIT_RADIUS: f32 = 15.0;

#[derive(Clone, Copy)]
enum DragPart {
    Start,
    End,
    Playhead,
}

#[derive(Clone, Copy)]
struct GrabState {
    part: DragPart,
    start_secs: f64,
    end_secs: f64,
    origin_x: f32,
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
    let state_id = response.id.with("grab_state");
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

    let button_down = response.is_pointer_button_down_on();
    let mut grab = ui.data(|data| data.get_temp::<GrabState>(state_id));

    // Capture the grab on the first frame the button is down on the brush, and
    // hit-test the press position. The pointer may travel far past the handle
    // before egui reports the drag as started, so testing the current pointer
    // position here would silently turn every normal-speed grab into a scrub.
    if grab.is_none() && button_down && let Some(origin) = press_position(ui, &response) {
        let part = hit_test(origin, start_x, end_x);
        grab = Some(GrabState {
            part,
            start_secs: *start_secs,
            end_secs: *end_secs,
            origin_x: origin.x,
        });
        ui.data_mut(|data| data.insert_temp(state_id, grab.expect("grab was just created")));
    }

    if let Some(grab) = grab
        && let Some(pointer) = response.interact_pointer_pos()
    {
        match grab.part {            DragPart::Start => {
                let offset_secs =
                    f64::from((pointer.x - grab.origin_x) / track.width()) * duration_secs;
                *start_secs =
                    (grab.start_secs + offset_secs).clamp(0.0, grab.end_secs - MIN_RANGE_SECS);
                *playhead_secs = *start_secs;
                response.mark_changed();
            }
            DragPart::End => {
                let offset_secs =
                    f64::from((pointer.x - grab.origin_x) / track.width()) * duration_secs;
                *end_secs = (grab.end_secs + offset_secs)
                    .clamp(grab.start_secs + MIN_RANGE_SECS, duration_secs);
                *playhead_secs = (*end_secs - MIN_RANGE_SECS).max(0.0);
                response.mark_changed();
            }
            // The open track scrubs only once the press becomes a real drag, so a
            // plain click still lands in the click handler below.
            DragPart::Playhead => {
                if response.dragged() {
                    *playhead_secs =
                        x_to_time(pointer.x, duration_secs, track).min(max_preview_time);
                    response.mark_changed();
                }
            }
        }
    }

    // A press that ends without a drag is a click: seek the preview marker there.
    // Clicks that started on a trim handle are tiny trims, not seeks.
    if !button_down {
        if let (Some(grab), Some(pointer)) = (grab, press_position(ui, &response))
            && matches!(grab.part, DragPart::Playhead)
        {
            *playhead_secs = x_to_time(pointer.x, duration_secs, track).min(max_preview_time);
            response.mark_changed();
        }
        ui.data_mut(|data| data.remove::<GrabState>(state_id));
    }

    let start_x = time_to_x(*start_secs, duration_secs, track);
    let end_x = time_to_x(*end_secs, duration_secs, track);
    let playhead_x = time_to_x(*playhead_secs, duration_secs, track);

    let over_handle = response.hover_pos().is_some_and(|pointer| {
        matches!(hit_test(pointer, start_x, end_x), DragPart::Start | DragPart::End)
    });
    response = response.on_hover_cursor(if over_handle {
        CursorIcon::ResizeHorizontal
    } else {
        CursorIcon::PointingHand
    });
    response = response.on_hover_text(
        "Drag the violet handles to trim. Click or drag elsewhere to preview.\n⬅ / ➡ step 1 s (hold Shift for 10 s) · I / O set start / end",
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

/// Where the pointer went down for the press currently held on this widget, or
/// where the pointer is when no press origin is known.
fn press_position(ui: &egui::Ui, response: &Response) -> Option<Pos2> {
    ui.input(|input| input.pointer.press_origin()).or_else(|| response.interact_pointer_pos())
}

fn hit_test(pointer: Pos2, start_x: f32, end_x: f32) -> DragPart {
    // Trim handles own their full-height strip: after a trim the white preview
    // marker sits directly above a handle, and grabbing it must trim again.
    let start_distance = (pointer.x - start_x).abs();
    let end_distance = (pointer.x - end_x).abs();
    if start_distance <= HANDLE_HIT_RADIUS && start_distance <= end_distance {
        DragPart::Start
    } else if end_distance <= HANDLE_HIT_RADIUS {
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
    let handle = Rect::from_center_size(Pos2::new(x, center_y), Vec2::new(12.0, 36.0));
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
    use egui::{CentralPanel, Context, Event, Modifiers, PointerButton, RawInput, ViewportId};

    #[test]
    fn a_press_near_a_handle_grabs_it_in_any_lane() {
        // The white preview marker often shares the x position of a trim handle,
        // so grabbing near that x must take the handle no matter the height.
        assert!(matches!(
            hit_test(Pos2::new(10.0, 8.0), 10.0, 290.0),
            DragPart::Start
        ));
        assert!(matches!(
            hit_test(Pos2::new(10.0, 16.0), 10.0, 290.0),
            DragPart::Start
        ));
        assert!(matches!(
            hit_test(Pos2::new(290.0, 16.0), 10.0, 290.0),
            DragPart::End
        ));
    }

    #[test]
    fn timeline_background_drags_the_preview() {
        assert!(matches!(
            hit_test(Pos2::new(150.0, 34.0), 10.0, 290.0),
            DragPart::Playhead
        ));
    }

    struct State {
        start: f64,
        end: f64,
        playhead: f64,
        duration: f64,
    }

    /// Headless egui pass driving the brush with real pointer events.
    struct Harness {
        ctx: Context,
        time: f64,
        rect: Rect,
    }

    impl Harness {
        fn new() -> Self {
            let ctx = Context::default();
            ctx.set_pixels_per_point(1.0);
            let time = 0.0;
            let mut harness = Self { ctx, time, rect: Rect::NOTHING };
            // First pass only to learn where the brush lands.
            let mut probe = state();
            harness.pass(Vec::new(), &mut probe);
            harness
        }

        fn pass(&mut self, events: Vec<Event>, state: &mut State) -> Response {
            let input = RawInput {
                viewport_id: ViewportId::ROOT,
                screen_rect: Some(Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(600.0, 200.0),
                )),
                time: Some(self.time),
                events,
                ..RawInput::default()
            };
            self.time += 1.0 / 60.0;
            let mut response = None;
            self.ctx.run_ui(input, |ui| {
                CentralPanel::default().show(ui, |ui| {
                    response = Some(show(
                        ui,
                        &mut state.start,
                        &mut state.end,
                        &mut state.playhead,
                        state.duration,
                    ));
                });
            });
            let response = response.expect("brush not shown");
            self.rect = response.rect;
            response
        }

        fn press(&mut self, at: Pos2, state: &mut State) -> Response {
            self.pass(
                vec![Event::PointerButton {
                    pos: at,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::default(),
                }],
                state,
            )
        }

        fn move_to(&mut self, at: Pos2, state: &mut State) -> Response {
            self.pass(vec![Event::PointerMoved(at)], state)
        }

        fn release(&mut self, at: Pos2, state: &mut State) -> Response {
            self.pass(
                vec![Event::PointerButton {
                    pos: at,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::default(),
                }],
                state,
            )
        }

        fn start_handle(&self) -> Pos2 {
            let track_left = self.rect.left() + 10.0;
            Pos2::new(track_left, self.rect.bottom() - 17.0)
        }

        fn end_handle(&self) -> Pos2 {
            let track_right = self.rect.right() - 10.0;
            Pos2::new(track_right, self.rect.bottom() - 17.0)
        }

        fn playhead_knob(&self, state: &State) -> Pos2 {
            let left = self.rect.left() + 10.0;
            let right = self.rect.right() - 10.0;
            let x = egui::lerp(left..=right, (state.start / state.duration) as f32);
            Pos2::new(x, self.rect.top() + 7.0)
        }

        fn track_point(&self, fraction: f32) -> Pos2 {
            let left = self.rect.left() + 10.0;
            let right = self.rect.right() - 10.0;
            Pos2::new(egui::lerp(left..=right, fraction), self.rect.bottom() - 17.0)
        }
    }

    fn state() -> State {
        State { start: 0.0, end: 100.0, playhead: 0.0, duration: 100.0 }
    }

    /// Pressing on the trim handle and dragging right must move the start of
    /// the range. This used to scrub instead: by the time egui reports the
    /// drag as started, the pointer has already left the handle, and the old
    /// code hit-tested that late position.
    #[test]
    fn dragging_the_start_handle_trims_the_range() {
        let mut harness = Harness::new();
        let mut state = state();
        harness.press(harness.start_handle(), &mut state);
        harness.move_to(harness.track_point(0.25), &mut state);
        harness.release(harness.track_point(0.25), &mut state);
        assert!(state.start > 20.0 && state.start < 30.0, "start moved to {}", state.start);
        assert!((state.playhead - state.start).abs() < 1e-9);
    }

    /// A tiny nudge on the handle never reaches egui's drag threshold, so it
    /// would be reported as a click; that must trim, not seek.
    #[test]
    fn nudging_the_start_handle_trims_without_seeking() {
        let mut harness = Harness::new();
        let mut state = state();
        let from = harness.start_handle();
        let to = from + Vec2::new(4.0, 0.0);
        harness.press(from, &mut state);
        harness.move_to(to, &mut state);
        harness.release(to, &mut state);
        let expected = f64::from(4.0 / (harness.rect.width() - 20.0) * 100.0);
        assert!(
            (state.start - expected).abs() < 0.5,
            "start should move to ~{expected}, got {}",
            state.start
        );
        assert!((state.playhead - state.start).abs() < 1e-9);
    }

    /// Pressing the white preview knob directly above the start handle is how
    /// the grab looks after marking a start; it must not scrub instead.
    #[test]
    fn grabbing_the_knob_over_a_handle_trims() {
        let mut harness = Harness::new();
        let mut state = State { start: 20.0, end: 80.0, playhead: 20.0, duration: 100.0 };
        let knob = harness.playhead_knob(&state);
        harness.press(knob, &mut state);
        harness.move_to(harness.track_point(0.4), &mut state);
        harness.release(harness.track_point(0.4), &mut state);
        assert!(state.start > 30.0, "start handle should move, got {}", state.start);
        assert!((state.playhead - state.start).abs() < 1e-9);
    }

    /// The open timeline still scrubs: drag moves the playhead, no trim change.
    #[test]
    fn dragging_the_open_track_scrubs_the_playhead() {
        let mut harness = Harness::new();
        let mut state = state();
        harness.press(harness.track_point(0.5), &mut state);
        harness.move_to(harness.track_point(0.75), &mut state);
        harness.release(harness.track_point(0.75), &mut state);
        assert!(
            (state.playhead - 75.0).abs() < 2.0,
            "playhead should scrub to ~75, got {}",
            state.playhead
        );
        assert_eq!(state.start, 0.0);
        assert_eq!(state.end, 100.0);
    }

    /// Grabbing the end handle works too.
    #[test]
    fn dragging_the_end_handle_trims_the_range() {
        let mut harness = Harness::new();
        let mut state = state();
        harness.press(harness.end_handle(), &mut state);
        harness.move_to(harness.track_point(0.6), &mut state);
        harness.release(harness.track_point(0.6), &mut state);
        assert!(state.end > 55.0 && state.end < 65.0, "end moved to {}", state.end);
        assert!((state.playhead - (state.end - 0.1)).abs() < 1e-9);
    }
}
