//! Deduplicate simultaneous pointer + touch events on Android touchscreens so a single tap on a
//! menu title or toolbar button does not open and immediately close the menu.

use egui::{Event, PointerButton, RawInput};

/// Longest gap, in seconds, between a press and its twin from the other event family.
const TWIN_WINDOW: f64 = 0.5;

/// Drops a primary press that arrives while the primary button is already held (pressed within
/// [`TWIN_WINDOW`]) and a primary release that arrives while it is already up.
/// Call from `raw_input_hook`, before the frame runs.
pub fn drop_twins(ctx: &egui::Context, raw: &mut RawInput) {
    let (mut down, mut pressed_at, last) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.press_start_time(), i.time));
    let now = raw.time.unwrap_or(last + f64::from(raw.predicted_dt));
    raw.events.retain(|e| {
        let Event::PointerButton { button: PointerButton::Primary, pressed, .. } = e else { return true };
        if *pressed != down {
            down = *pressed;
            pressed_at = pressed.then_some(now);
            return true;
        }
        // A press while held is a twin only while fresh; a release while up always is.
        *pressed && !pressed_at.is_some_and(|t| now - t <= TWIN_WINDOW)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, Pos2, pos2};

    fn button(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    #[test]
    fn a_press_release_press_in_one_frame_keeps_all_three() {
        let ctx = egui::Context::default();
        let p = pos2(10.0, 10.0);
        let mut raw = RawInput { time: Some(1.0), events: vec![button(p, true), button(p, false), button(p, true)], ..Default::default() };
        drop_twins(&ctx, &mut raw);
        assert_eq!(raw.events.len(), 3);
    }

    #[test]
    fn duplicate_twin_press_and_release_are_dropped() {
        let ctx = egui::Context::default();
        let p = pos2(10.0, 10.0);
        let frame = |events: Vec<Event>, time: f64| {
            let mut raw = RawInput { time: Some(time), events, ..Default::default() };
            drop_twins(&ctx, &mut raw);
            let kept = raw.events.clone();
            ctx.run_ui(raw, |_| {}).textures_delta.clear();
            kept
        };
        assert_eq!(frame(vec![button(p, true)], 1.0).len(), 1);
        assert!(frame(vec![button(p, true)], 1.1).is_empty(), "twin press dropped");
        assert_eq!(frame(vec![button(p, true)], 3.0).len(), 1, "stale press allowed");
        assert_eq!(frame(vec![button(p, false), button(p, false)], 3.1).len(), 1, "one release per press");
        assert!(frame(vec![button(p, false)], 3.2).is_empty(), "release while up dropped");
    }
}
