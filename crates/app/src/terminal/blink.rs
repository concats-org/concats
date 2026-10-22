//! A cursor that blinks: shown, then hidden, `HALF` seconds each, and shown
//! again from the start after every key, so it never vanishes while you type.

use makepad_widgets::{Cx, CxOsApi, Event, Timer};

/// How long a blinking cursor stays shown, and then hidden, in seconds.
const HALF: f64 = 0.6;

#[derive(Default)]
pub struct Blink {
    /// When the cursor last started over shown.
    from: f64,
    /// Wakes the view for the next flip.
    timer: Timer,
}

impl Blink {
    /// Start over shown on a key press. Whether the view has to redraw: the
    /// blink's own timer fired.
    pub fn see(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if matches!(event, Event::KeyDown(_)) {
            self.from = cx.seconds_since_app_start();
        }
        self.timer.is_event(event).is_some()
    }

    /// Whether the cursor shows now. While it blinks, the view is woken again
    /// at the next flip.
    pub fn shown(&mut self, cx: &mut Cx, blinking: bool) -> bool {
        if !blinking {
            return true;
        }
        let (shown, flip) = phase(cx.seconds_since_app_start() - self.from);
        cx.stop_timer(self.timer);
        self.timer = cx.start_timeout(flip);
        shown
    }
}

/// Where a blink is `since` seconds after it started over: shown or not, and
/// how long until that changes.
fn phase(since: f64) -> (bool, f64) {
    let halves = (since / HALF).floor();
    (halves % 2.0 == 0.0, HALF - since % HALF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blink_shows_then_hides_for_half_a_period_each() {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        let (shown, flip) = phase(0.0);
        assert!(shown && near(flip, HALF));
        let (shown, flip) = phase(HALF + 0.1);
        assert!(!shown && near(flip, HALF - 0.1));
        assert!(phase(2.0 * HALF + 0.1).0);
    }
}
