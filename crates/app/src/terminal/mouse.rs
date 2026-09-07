//! Pointer events to terminal bytes.
//!
//! A port of alacritty's `mouse_report` and its two encodings
//! (`alacritty/src/input/mod.rs`). Which one an application gets is its own
//! choice: SGR once it asks for 1006, the original packed form otherwise.
//!
//! Nothing here decides whether the pointer belongs to the application — ask
//! [`wants_pointer`] first, and keep the event for the panel's own selection
//! when it says no.

use alacritty_terminal::{index::Point, term::TermMode};
use makepad_widgets::{KeyModifiers, MouseButton};

/// Modifier bits, in the positions the packed report gives them.
const SHIFT: u8 = 4;
const ALT: u8 = 8;
const CONTROL: u8 = 16;
/// Added to a button code to say "the pointer moved".
const MOTION: u8 = 32;
/// The code for "no button", which a release also reports under the packed
/// encoding — it cannot say which button was let go.
const NONE: u8 = 3;

const WHEEL_UP: u8 = 64;
const WHEEL_DOWN: u8 = 65;

/// Whether the application is tracking the pointer. Holding shift takes it
/// back, the way every terminal lets you select over a full-screen app.
pub fn wants_pointer(mods: &KeyModifiers, mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE) && !mods.shift
}

/// Whether motion should be reported: with a button held (1002), or with none
/// (1003).
pub fn wants_motion(held: bool, mode: TermMode) -> bool {
    mode.contains(TermMode::MOUSE_MOTION) || (held && mode.contains(TermMode::MOUSE_DRAG))
}

/// A button going down or coming up.
pub fn report(
    button: MouseButton,
    pressed: bool,
    mods: &KeyModifiers,
    point: Point,
    mode: TermMode,
) -> Option<Vec<u8>> {
    encode(code(button)? + modifiers(mods), pressed, point, mode)
}

/// The pointer moving, with whichever button is held.
pub fn motion(
    held: Option<MouseButton>,
    mods: &KeyModifiers,
    point: Point,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let button = held.and_then(code).unwrap_or(NONE);
    encode(button + modifiers(mods) + MOTION, true, point, mode)
}

/// The wheel: a report per line while the application is tracking, arrow keys
/// under alternate scroll on the alt screen, and otherwise nothing — the panel
/// scrolls its own scrollback.
pub fn wheel(
    up: bool,
    lines: usize,
    mods: &KeyModifiers,
    point: Point,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if lines == 0 {
        return None;
    }
    if wants_pointer(mods, mode) {
        let button = if up { WHEEL_UP } else { WHEEL_DOWN } + modifiers(mods);
        let mut out = Vec::new();
        for _ in 0..lines {
            out.extend(encode(button, true, point, mode)?);
        }
        return Some(out);
    }
    if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && !mods.shift {
        // The same bytes the arrow keys send in application cursor mode.
        let arrow = if up { b'A' } else { b'B' };
        return Some(
            std::iter::repeat_n([0x1b, b'O', arrow], lines)
                .flatten()
                .collect(),
        );
    }
    None
}

fn code(button: MouseButton) -> Option<u8> {
    // Beyond three there is no room in the packed encoding to say which.
    if button.contains(MouseButton::PRIMARY) {
        Some(0)
    } else if button.contains(MouseButton::MIDDLE) {
        Some(1)
    } else if button.contains(MouseButton::SECONDARY) {
        Some(2)
    } else {
        None
    }
}

fn modifiers(mods: &KeyModifiers) -> u8 {
    let mut bits = 0;
    if mods.shift {
        bits += SHIFT;
    }
    if mods.alt {
        bits += ALT;
    }
    if mods.control {
        bits += CONTROL;
    }
    bits
}

fn encode(button: u8, pressed: bool, point: Point, mode: TermMode) -> Option<Vec<u8>> {
    // The scrollback is the panel's business, not the application's.
    if point.line < alacritty_terminal::index::Line(0) {
        return None;
    }
    let line = point.line.0 as usize;
    let column = point.column.0;

    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{button};{};{}{end}", column + 1, line + 1).into_bytes());
    }

    // The packed form has one byte per coordinate, so it runs out of screen.
    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    let max = if utf8 { 2015 } else { 223 };
    if line >= max || column >= max {
        return None;
    }
    // A release cannot name its button here: the low two bits become "none",
    // and the modifier and motion bits above them stay.
    let button = if pressed {
        button
    } else {
        (button & !0b11) | NONE
    };
    let mut msg = vec![0x1b, b'[', b'M', 32 + button];
    for pos in [column, line] {
        if utf8 && pos >= 95 {
            let pos = 32 + 1 + pos;
            msg.push((0xC0 + pos / 64) as u8);
            msg.push((0x80 + (pos & 63)) as u8);
        } else {
            msg.push(32 + 1 + pos as u8);
        }
    }
    Some(msg)
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::{Column, Line};

    use super::*;

    fn point(column: usize, line: i32) -> Point {
        Point::new(Line(line), Column(column))
    }

    fn shift() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            ..Default::default()
        }
    }

    fn text(bytes: Option<Vec<u8>>) -> String {
        String::from_utf8(bytes.unwrap_or_default()).unwrap()
    }

    #[test]
    fn tracking_is_off_until_asked_for_and_shift_takes_it_back() {
        assert!(!wants_pointer(&KeyModifiers::default(), TermMode::empty()));
        assert!(wants_pointer(
            &KeyModifiers::default(),
            TermMode::MOUSE_REPORT_CLICK
        ));
        assert!(!wants_pointer(&shift(), TermMode::MOUSE_REPORT_CLICK));
    }

    #[test]
    fn sgr_names_the_button_on_the_way_up() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let down = report(
            MouseButton::PRIMARY,
            true,
            &KeyModifiers::default(),
            point(4, 2),
            mode,
        );
        assert_eq!(text(down), "\x1b[<0;5;3M");
        let up = report(
            MouseButton::PRIMARY,
            false,
            &KeyModifiers::default(),
            point(4, 2),
            mode,
        );
        assert_eq!(text(up), "\x1b[<0;5;3m");
    }

    #[test]
    fn the_packed_form_offsets_every_field_by_a_space() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let down = report(
            MouseButton::SECONDARY,
            true,
            &KeyModifiers::default(),
            point(0, 0),
            mode,
        );
        assert_eq!(text(down), "\x1b[M\x22\x21\x21");
    }

    #[test]
    fn modifiers_ride_in_the_button_code() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let mods = KeyModifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(
            text(report(MouseButton::PRIMARY, true, &mods, point(0, 0), mode)),
            "\x1b[<16;1;1M"
        );
    }

    #[test]
    fn motion_marks_itself_and_says_which_button_is_down() {
        let mode = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert!(wants_motion(true, mode));
        assert!(!wants_motion(false, mode));
        assert_eq!(
            text(motion(
                Some(MouseButton::PRIMARY),
                &KeyModifiers::default(),
                point(1, 1),
                mode
            )),
            "\x1b[<32;2;2M"
        );
        assert_eq!(
            text(motion(None, &KeyModifiers::default(), point(1, 1), mode)),
            "\x1b[<35;2;2M"
        );
    }

    #[test]
    fn the_wheel_reports_per_line_then_falls_back_to_arrows() {
        let tracked = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            text(wheel(
                true,
                2,
                &KeyModifiers::default(),
                point(0, 0),
                tracked
            )),
            "\x1b[<64;1;1M\x1b[<64;1;1M"
        );

        let alt = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(
            text(wheel(false, 2, &KeyModifiers::default(), point(0, 0), alt)),
            "\x1bOB\x1bOB"
        );

        // A plain terminal keeps its wheel: the panel scrolls the scrollback.
        assert_eq!(
            wheel(
                true,
                2,
                &KeyModifiers::default(),
                point(0, 0),
                TermMode::empty()
            ),
            None
        );
    }

    #[test]
    fn the_scrollback_is_never_reported() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            report(
                MouseButton::PRIMARY,
                true,
                &KeyModifiers::default(),
                point(0, -3),
                mode
            ),
            None
        );
    }
}
