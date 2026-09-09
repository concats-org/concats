//! Key presses to terminal bytes.
//!
//! A port of alacritty's `build_sequence` (`alacritty/src/input/keyboard.rs`)
//! together with the default escape bindings beside it (`config/bindings.rs`),
//! written against makepad's [`KeyEvent`] instead of winit's. The library stops
//! at the grid on purpose, so this half is ours, and it is most of the
//! difference between an agent feeling right in the panel and not.
//!
//! Two divergences from upstream, both from where key text comes from. Winit
//! hands over the platform's text for a press; makepad reports the key and
//! delivers text separately as `TextInput`, so ordinary typing is encoded by
//! [`encode_text`] and everything here works off the US-layout base of the key.
//! Under the full kitty protocol a non-Latin layout therefore reports its base
//! key rather than the composed one. And `Alt` is Meta — it prefixes ESC
//! instead of composing a character, which is what readline, and every agent's
//! line editor, expects.

use std::fmt::Write as _;

use alacritty_terminal::term::TermMode;
use bitflags::bitflags;
use makepad_widgets::{KeyCode, KeyEvent, KeyModifiers};

bitflags! {
    /// The modifier bits of a CSI sequence, in the order the protocol numbers
    /// them. Sent as `bits + 1`.
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Modifiers: u8 {
        const SHIFT = 0b0001;
        const ALT = 0b0010;
        const CONTROL = 0b0100;
        const SUPER = 0b1000;
    }
}

impl Modifiers {
    fn held(modifiers: KeyModifiers) -> Self {
        let mut bits = Self::empty();
        bits.set(Self::SHIFT, modifiers.shift);
        bits.set(Self::ALT, modifiers.alt);
        bits.set(Self::CONTROL, modifiers.control);
        bits.set(Self::SUPER, modifiers.logo);
        bits
    }

    fn encode(self) -> u8 {
        self.bits() + 1
    }
}

/// How a sequence ends: the letter xterm gives the key, or `u` for kitty.
#[derive(Clone, Copy)]
enum Terminator {
    Normal(char),
    Kitty,
}

impl Terminator {
    fn encode(self) -> char {
        match self {
            Terminator::Normal(c) => c,
            Terminator::Kitty => 'u',
        }
    }
}

/// The number (and any kitty alternate) a sequence carries, with its ending.
struct Base {
    payload: String,
    terminator: Terminator,
}

impl Base {
    fn new(payload: impl Into<String>, terminator: Terminator) -> Self {
        Self {
            payload: payload.into(),
            terminator,
        }
    }
}

/// The bytes for a key press, or `None` when the key produces text instead —
/// that arrives separately as `TextInput` and goes through [`encode_text`].
///
/// `pressed` is false for a key release, which only the kitty protocol asks to
/// hear about.
pub fn encode(key: &KeyEvent, pressed: bool, mode: TermMode) -> Option<Vec<u8>> {
    if !pressed && !mode.contains(TermMode::REPORT_EVENT_TYPES) {
        return None;
    }
    if let Some(bytes) = legacy_binding(key, mode) {
        return Some(bytes);
    }
    if should_build_sequence(key, mode) {
        return build_sequence(key, pressed, mode);
    }
    control_bytes(key)
}

/// The bytes for text the platform produced: typing, a dead key, an IME
/// commit. `None` while the kitty protocol is encoding every key itself, since
/// the press already carried this text.
pub fn encode_text(text: &str, modifiers: KeyModifiers, mode: TermMode) -> Option<Vec<u8>> {
    if text.is_empty() || mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() + 1);
    if modifiers.alt {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(text.as_bytes());
    Some(bytes)
}

/// Whether the terminal drives the keyboard itself: any of the kitty protocol's
/// flags puts every functional key into the CSI form it numbers.
fn kitty_sequence(mode: TermMode) -> bool {
    mode.intersects(
        TermMode::REPORT_ALL_KEYS_AS_ESC
            | TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_EVENT_TYPES,
    )
}

/// The legacy forms the generic encoder cannot express, which alacritty ships
/// as default bindings resolved before it. All but plain Backspace step aside
/// once the terminal drives the keyboard itself.
fn legacy_binding(key: &KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
    let modifiers = key.modifiers;
    let unmodified = !modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.logo;
    let kitty =
        mode.intersects(TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::DISAMBIGUATE_ESC_CODES);

    // Backspace is DEL, not the BS its ASCII name suggests, and stays that way
    // under disambiguation.
    if key.key_code == KeyCode::Backspace
        && !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)
        && (unmodified || (modifiers.shift && !modifiers.control && !modifiers.logo))
    {
        return match (modifiers.alt, kitty) {
            (true, false) => Some(b"\x1b\x7f".to_vec()),
            (true, true) => None,
            (false, _) => Some(b"\x7f".to_vec()),
        };
    }

    if unmodified && mode.contains(TermMode::APP_CURSOR) {
        let ss3 = match key.key_code {
            KeyCode::Home => Some('H'),
            KeyCode::End => Some('F'),
            KeyCode::ArrowUp => Some('A'),
            KeyCode::ArrowDown => Some('B'),
            KeyCode::ArrowRight => Some('C'),
            KeyCode::ArrowLeft => Some('D'),
            _ => None,
        };
        if let Some(ss3) = ss3 {
            return Some(format!("\x1bO{ss3}").into_bytes());
        }
    }

    if kitty {
        return None;
    }

    if unmodified {
        let ss3 = match key.key_code {
            KeyCode::F1 => Some('P'),
            KeyCode::F2 => Some('Q'),
            KeyCode::F3 => Some('R'),
            KeyCode::F4 => Some('S'),
            _ => None,
        };
        if let Some(ss3) = ss3 {
            return Some(format!("\x1bO{ss3}").into_bytes());
        }
        if key.key_code == KeyCode::NumpadEnter {
            return Some(b"\n".to_vec());
        }
    }

    // Backtab, with alt as meta in front of it.
    if key.key_code == KeyCode::Tab && modifiers.shift && !modifiers.control && !modifiers.logo {
        return Some(if modifiers.alt {
            b"\x1b\x1b[Z".to_vec()
        } else {
            b"\x1b[Z".to_vec()
        });
    }

    None
}

/// Whether this key wants a CSI sequence rather than its own bytes.
fn should_build_sequence(key: &KeyEvent, mode: TermMode) -> bool {
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        return true;
    }
    let modifiers = key.modifiers;
    let any_mods = modifiers.shift || modifiers.control || modifiers.alt || modifiers.logo;
    let only_shift = modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.logo;
    let disambiguate = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES)
        && (key.key_code == KeyCode::Escape
            || numpad_code(key.key_code).is_some()
            || (any_mods
                && (!only_shift
                    || matches!(
                        key.key_code,
                        KeyCode::Tab | KeyCode::ReturnKey | KeyCode::Backspace
                    ))));

    disambiguate || named_without_text(key.key_code)
}

/// The functional keys that have no textual form, so they are always a
/// sequence — arrows, function keys, navigation.
fn named_without_text(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::ArrowUp
            | KeyCode::ArrowDown
            | KeyCode::ArrowLeft
            | KeyCode::ArrowRight
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Insert
            | KeyCode::Delete
            | KeyCode::F1
            | KeyCode::F2
            | KeyCode::F3
            | KeyCode::F4
            | KeyCode::F5
            | KeyCode::F6
            | KeyCode::F7
            | KeyCode::F8
            | KeyCode::F9
            | KeyCode::F10
            | KeyCode::F11
            | KeyCode::F12
            | KeyCode::PrintScreen
            | KeyCode::ScrollLock
            | KeyCode::Pause
    )
}

fn build_sequence(key: &KeyEvent, pressed: bool, mode: TermMode) -> Option<Vec<u8>> {
    let mut modifiers = Modifiers::held(key.modifiers);
    // NOTE: modifier key events may omit their own bit; report the state after this event.
    let own_bit = match key.key_code {
        KeyCode::Shift => Modifiers::SHIFT,
        KeyCode::Control => Modifiers::CONTROL,
        KeyCode::Alt => Modifiers::ALT,
        KeyCode::Logo => Modifiers::SUPER,
        _ => Modifiers::empty(),
    };
    modifiers.set(own_bit, pressed);

    let kitty_event_type =
        mode.contains(TermMode::REPORT_EVENT_TYPES) && (key.is_repeat || !pressed);

    // The key's own text, when the protocol asked to hear it. Control keys
    // carry their own codes and never report text.
    let associated_text = mode
        .contains(TermMode::REPORT_ASSOCIATED_TEXT)
        .then(|| {
            pressed
                .then(|| key.key_code.to_char(key.modifiers.shift))
                .flatten()
        })
        .flatten()
        .filter(|c| !c.is_control());

    // Everything after the number is optional, and the number itself defaults
    // to 1: a bare key sends neither.
    let has_params = kitty_event_type || !modifiers.is_empty() || associated_text.is_some();

    let base = numpad(key, mode)
        .or_else(|| named_kitty(key, mode))
        .or_else(|| named_normal(key, has_params))
        .or_else(|| control_char_or_mod(key, mode))
        .or_else(|| textual(key, mode))?;

    let mut payload = format!("\x1b[{}", base.payload);
    if has_params {
        let _ = write!(payload, ";{}", modifiers.encode());
    }
    if kitty_event_type {
        payload.push(':');
        payload.push(match (key.is_repeat, pressed) {
            (true, _) => '2',
            (_, true) => '1',
            (_, false) => '3',
        });
    }
    if let Some(text) = associated_text {
        let _ = write!(payload, ";{}", u32::from(text));
    }
    payload.push(base.terminator.encode());
    Some(payload.into_bytes())
}

/// The kitty protocol gives the numpad its own codes, so an application can
/// tell `Numpad1` from `1`.
fn numpad_code(key: KeyCode) -> Option<&'static str> {
    Some(match key {
        KeyCode::Numpad0 => "57399",
        KeyCode::Numpad1 => "57400",
        KeyCode::Numpad2 => "57401",
        KeyCode::Numpad3 => "57402",
        KeyCode::Numpad4 => "57403",
        KeyCode::Numpad5 => "57404",
        KeyCode::Numpad6 => "57405",
        KeyCode::Numpad7 => "57406",
        KeyCode::Numpad8 => "57407",
        KeyCode::Numpad9 => "57408",
        KeyCode::NumpadDecimal => "57409",
        KeyCode::NumpadDivide => "57410",
        KeyCode::NumpadMultiply => "57411",
        KeyCode::NumpadSubtract => "57412",
        KeyCode::NumpadAdd => "57413",
        KeyCode::NumpadEnter => "57414",
        KeyCode::NumpadEquals => "57415",
        _ => return None,
    })
}

fn numpad(key: &KeyEvent, mode: TermMode) -> Option<Base> {
    if !kitty_sequence(mode) {
        return None;
    }
    Some(Base::new(numpad_code(key.key_code)?, Terminator::Kitty))
}

/// Functional keys the kitty protocol numbers differently from xterm.
fn named_kitty(key: &KeyEvent, mode: TermMode) -> Option<Base> {
    if !kitty_sequence(mode) {
        return None;
    }
    let (payload, terminator) = match key.key_code {
        // F3 in the kitty protocol diverges from alacritty's terminfo.
        KeyCode::F3 => ("13", Terminator::Normal('~')),
        KeyCode::ScrollLock => ("57359", Terminator::Kitty),
        KeyCode::PrintScreen => ("57361", Terminator::Kitty),
        KeyCode::Pause => ("57362", Terminator::Kitty),
        _ => return None,
    };
    Some(Base::new(payload, terminator))
}

/// The xterm/DEC table every terminal has spoken for forty years. `has_params`
/// says whether anything follows the number, which is left out otherwise.
fn named_normal(key: &KeyEvent, has_params: bool) -> Option<Base> {
    let one = if has_params { "1" } else { "" };
    let (payload, terminator) = match key.key_code {
        KeyCode::PageUp => ("5", Terminator::Normal('~')),
        KeyCode::PageDown => ("6", Terminator::Normal('~')),
        KeyCode::Insert => ("2", Terminator::Normal('~')),
        KeyCode::Delete => ("3", Terminator::Normal('~')),
        KeyCode::Home => (one, Terminator::Normal('H')),
        KeyCode::End => (one, Terminator::Normal('F')),
        KeyCode::ArrowLeft => (one, Terminator::Normal('D')),
        KeyCode::ArrowRight => (one, Terminator::Normal('C')),
        KeyCode::ArrowUp => (one, Terminator::Normal('A')),
        KeyCode::ArrowDown => (one, Terminator::Normal('B')),
        KeyCode::F1 => (one, Terminator::Normal('P')),
        KeyCode::F2 => (one, Terminator::Normal('Q')),
        KeyCode::F3 => (one, Terminator::Normal('R')),
        KeyCode::F4 => (one, Terminator::Normal('S')),
        KeyCode::F5 => ("15", Terminator::Normal('~')),
        KeyCode::F6 => ("17", Terminator::Normal('~')),
        KeyCode::F7 => ("18", Terminator::Normal('~')),
        KeyCode::F8 => ("19", Terminator::Normal('~')),
        KeyCode::F9 => ("20", Terminator::Normal('~')),
        KeyCode::F10 => ("21", Terminator::Normal('~')),
        KeyCode::F11 => ("23", Terminator::Normal('~')),
        KeyCode::F12 => ("24", Terminator::Normal('~')),
        _ => return None,
    };
    Some(Base::new(payload, terminator))
}

/// Control keys, and the modifier keys themselves once the terminal asked to
/// hear about every one.
fn control_char_or_mod(key: &KeyEvent, mode: TermMode) -> Option<Base> {
    if !kitty_sequence(mode) {
        return None;
    }
    let control = match key.key_code {
        KeyCode::Tab => "9",
        KeyCode::ReturnKey => "13",
        KeyCode::Escape => "27",
        KeyCode::Space => "32",
        KeyCode::Backspace => "127",
        _ => "",
    };
    let encode_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    if !encode_all && control.is_empty() {
        return None;
    }

    let payload = match key.key_code {
        KeyCode::Shift => "57441",
        KeyCode::Control => "57442",
        KeyCode::Alt => "57443",
        KeyCode::Logo => "57444",
        KeyCode::Capslock => "57358",
        KeyCode::Numlock => "57360",
        _ => control,
    };
    (!payload.is_empty()).then(|| Base::new(payload, Terminator::Kitty))
}

/// A printing key under the kitty protocol, reported by its unshifted
/// codepoint — and, when asked, the shifted one beside it.
fn textual(key: &KeyEvent, mode: TermMode) -> Option<Base> {
    if !kitty_sequence(mode) {
        return None;
    }
    let unshifted = key.key_code.to_char(false)?;
    let alternate = key.key_code.to_char(key.modifiers.shift)?;

    let payload = if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) && alternate != unshifted {
        format!("{}:{}", u32::from(unshifted), u32::from(alternate))
    } else {
        u32::from(unshifted).to_string()
    };
    Some(Base::new(payload, Terminator::Kitty))
}

/// The keys that carry their own byte with no sequence around it: the control
/// characters, and `ctrl` over a printing key. `alt` puts ESC in front, the
/// way every line editor reads Meta.
fn control_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    let modifiers = key.modifiers;
    let byte = match key.key_code {
        KeyCode::ReturnKey => Some(b'\r'),
        KeyCode::Tab => Some(b'\t'),
        KeyCode::Escape => Some(0x1b),
        KeyCode::Backspace => Some(0x7f),
        KeyCode::Space if modifiers.control => Some(0),
        _ if modifiers.control => control_char(key.key_code.to_char(false)?),
        _ => None,
    }?;
    Some(if modifiers.alt {
        vec![0x1b, byte]
    } else {
        vec![byte]
    })
}

/// `ctrl` folded into a character the way a terminal has always done it.
fn control_char(c: char) -> Option<u8> {
    match c.to_ascii_uppercase() {
        c @ 'A'..='Z' => Some(c as u8 - 0x40),
        '@' | ' ' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' | '/' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key_code: KeyCode) -> KeyEvent {
        KeyEvent {
            key_code,
            is_repeat: false,
            modifiers: KeyModifiers::default(),
            time: 0.0,
        }
    }

    fn with(key_code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            modifiers,
            ..key(key_code)
        }
    }

    fn ctrl() -> KeyModifiers {
        KeyModifiers {
            control: true,
            ..Default::default()
        }
    }

    fn shift() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            ..Default::default()
        }
    }

    fn alt() -> KeyModifiers {
        KeyModifiers {
            alt: true,
            ..Default::default()
        }
    }

    /// What a key sends, as the text a terminal log would show.
    fn sent(event: &KeyEvent, mode: TermMode) -> String {
        String::from_utf8(encode(event, true, mode).unwrap_or_default()).unwrap()
    }

    #[test]
    fn arrows_are_csi_until_the_application_asks_for_ss3() {
        assert_eq!(sent(&key(KeyCode::ArrowUp), TermMode::empty()), "\x1b[A");
        assert_eq!(sent(&key(KeyCode::ArrowUp), TermMode::APP_CURSOR), "\x1bOA");
    }

    #[test]
    fn a_modifier_turns_an_arrow_into_its_numbered_form() {
        assert_eq!(
            sent(&with(KeyCode::ArrowUp, ctrl()), TermMode::empty()),
            "\x1b[1;5A"
        );
        assert_eq!(
            sent(&with(KeyCode::ArrowRight, alt()), TermMode::empty()),
            "\x1b[1;3C"
        );
        // Even in application cursor mode: only the bare key is SS3.
        assert_eq!(
            sent(&with(KeyCode::ArrowUp, ctrl()), TermMode::APP_CURSOR),
            "\x1b[1;5A"
        );
    }

    #[test]
    fn backspace_is_del_and_alt_puts_meta_in_front() {
        assert_eq!(sent(&key(KeyCode::Backspace), TermMode::empty()), "\x7f");
        assert_eq!(
            sent(&with(KeyCode::Backspace, alt()), TermMode::empty()),
            "\x1b\x7f"
        );
    }

    #[test]
    fn shift_tab_is_backtab() {
        assert_eq!(
            sent(&with(KeyCode::Tab, shift()), TermMode::empty()),
            "\x1b[Z"
        );
        assert_eq!(sent(&key(KeyCode::Tab), TermMode::empty()), "\t");
    }

    #[test]
    fn ctrl_folds_a_letter_into_its_control_code() {
        assert_eq!(
            sent(&with(KeyCode::KeyC, ctrl()), TermMode::empty()),
            "\x03"
        );
        assert_eq!(
            sent(&with(KeyCode::KeyD, ctrl()), TermMode::empty()),
            "\x04"
        );
        assert_eq!(sent(&with(KeyCode::Space, ctrl()), TermMode::empty()), "\0");
    }

    #[test]
    fn function_keys_keep_their_two_legacy_shapes() {
        assert_eq!(sent(&key(KeyCode::F1), TermMode::empty()), "\x1bOP");
        assert_eq!(sent(&key(KeyCode::F5), TermMode::empty()), "\x1b[15~");
        assert_eq!(
            sent(&with(KeyCode::F1, ctrl()), TermMode::empty()),
            "\x1b[1;5P"
        );
        assert_eq!(sent(&key(KeyCode::Delete), TermMode::empty()), "\x1b[3~");
    }

    /// The one an agent's line editor is waiting for: a newline that is not a
    /// submit. Legacy has no way to say it; disambiguation does.
    #[test]
    fn shift_enter_is_only_expressible_under_the_kitty_protocol() {
        assert_eq!(
            sent(&with(KeyCode::ReturnKey, shift()), TermMode::empty()),
            "\r"
        );
        assert_eq!(
            sent(
                &with(KeyCode::ReturnKey, shift()),
                TermMode::DISAMBIGUATE_ESC_CODES
            ),
            "\x1b[13;2u"
        );
    }

    #[test]
    fn disambiguation_reports_modified_keys_by_codepoint() {
        assert_eq!(
            sent(
                &with(KeyCode::KeyC, ctrl()),
                TermMode::DISAMBIGUATE_ESC_CODES
            ),
            "\x1b[99;5u"
        );
        assert_eq!(
            sent(&key(KeyCode::Escape), TermMode::DISAMBIGUATE_ESC_CODES),
            "\x1b[27u"
        );
        // Unmodified printing keys still arrive as text, not as a sequence.
        assert_eq!(
            encode(&key(KeyCode::KeyC), true, TermMode::DISAMBIGUATE_ESC_CODES),
            None
        );
    }

    #[test]
    fn the_alternate_key_rides_along_when_asked_for() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALTERNATE_KEYS;
        let ctrl_shift = KeyModifiers {
            shift: true,
            control: true,
            ..Default::default()
        };
        // The unshifted key first, then what shift made of it.
        assert_eq!(
            sent(&with(KeyCode::Key1, ctrl_shift), mode),
            "\x1b[49:33;6u"
        );
        // Shift alone is not a disambiguation: `!` is text, and text is text.
        assert_eq!(encode(&with(KeyCode::Key1, shift()), true, mode), None);
    }

    #[test]
    fn event_types_distinguish_repeat_and_release() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
        let repeat = KeyEvent {
            is_repeat: true,
            ..with(KeyCode::KeyC, ctrl())
        };
        assert_eq!(sent(&repeat, mode), "\x1b[99;5:2u");
        let release = String::from_utf8(
            encode(&with(KeyCode::KeyC, ctrl()), false, mode).expect("release reported"),
        )
        .unwrap();
        assert_eq!(release, "\x1b[99;5:3u");
        // Without the mode, a release says nothing.
        assert_eq!(
            encode(
                &with(KeyCode::KeyC, ctrl()),
                false,
                TermMode::DISAMBIGUATE_ESC_CODES
            ),
            None
        );
    }

    #[test]
    fn reporting_every_key_covers_the_modifiers_themselves() {
        let mode = TermMode::REPORT_ALL_KEYS_AS_ESC;
        // A modifier reports itself with its own bit already set: the protocol
        // reads the state as it is *after* the press.
        assert_eq!(sent(&key(KeyCode::Shift), mode), "\x1b[57441;2u");
        assert_eq!(sent(&key(KeyCode::KeyC), mode), "\x1b[99u");
    }

    #[test]
    fn typed_text_carries_meta_but_steps_aside_for_the_full_protocol() {
        assert_eq!(
            encode_text("a", KeyModifiers::default(), TermMode::empty()),
            Some(b"a".to_vec())
        );
        assert_eq!(
            encode_text("f", alt(), TermMode::empty()),
            Some(b"\x1bf".to_vec())
        );
        assert_eq!(
            encode_text(
                "a",
                KeyModifiers::default(),
                TermMode::REPORT_ALL_KEYS_AS_ESC
            ),
            None
        );
    }
}
