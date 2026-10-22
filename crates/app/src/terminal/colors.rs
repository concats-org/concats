//! Cell colours: what an application names, resolved against the theme.
//!
//! The library holds only what OSC changed at runtime — [`Colors`] is 269
//! slots of `Option<Rgb>`, empty until something sets one — so the palette
//! itself is the renderer's business and lives here. The 16 ANSI colours come
//! from the theme; 16..=255 are the cube and grey ramp every terminal
//! generates the same way.
//!
//! NOTE: bold is drawn in the bright colours, the way xterm has always done
//! it, because makepad's `TextStyle` carries no weight axis to reach for. When
//! it does, this is where that choice changes.

use alacritty_terminal::{
    term::{cell::Flags, color::Colors},
    vte::ansi::{Color, NamedColor, Rgb},
};
use concats_theme::Rgba;
use makepad_widgets::Vec4f;

use crate::theme::{Theme, paint};

/// What a dim cell keeps of its colour.
const DIM: f32 = 0.66;

/// A cell's foreground, with bold and dim folded into the colour.
// NOTE: every cell colour resolves through it; the tests pin each rule.
// coland-ignore: @coland/high_call_degree
pub fn foreground(color: Color, flags: Flags, colors: &Colors, theme: &Theme) -> Vec4f {
    paint(match color {
        // A true colour says exactly what it wants; only dim still touches it.
        Color::Spec(rgb) if flags.contains(Flags::DIM) => from_rgb(rgb).darken(DIM),
        Color::Spec(rgb) => from_rgb(rgb),
        Color::Named(named) => slot(shade(named, flags) as usize, colors, theme),
        Color::Indexed(idx) => slot(shade_indexed(idx, flags), colors, theme),
    })
}

/// A cell's background, or `None` when it is the panel's own — nothing to
/// paint there, and the chrome shows through.
pub fn background(color: Color, colors: &Colors, theme: &Theme) -> Option<Vec4f> {
    (color != Color::Named(NamedColor::Background))
        .then(|| foreground(color, Flags::empty(), colors, theme))
}

/// The palette slot for a named colour once bold and dim have had their say.
// NOTE: the bold/dim rule for named colours, apart from the slot lookup.
// coland-ignore: @coland/single_caller_private_function
fn shade(named: NamedColor, flags: Flags) -> NamedColor {
    match flags & (Flags::BOLD | Flags::DIM) {
        Flags::BOLD => named.to_bright(),
        Flags::DIM => named.to_dim(),
        _ => named,
    }
}

/// The same for the indexed colours, where bright and dim are eight apart.
// NOTE: the same rule for indexed colours, beside its named twin.
// coland-ignore: @coland/single_caller_private_function
fn shade_indexed(idx: u8, flags: Flags) -> usize {
    let idx = idx as usize;
    match (flags & (Flags::BOLD | Flags::DIM), idx) {
        (Flags::BOLD, 0..=7) => idx + 8,
        (Flags::DIM, 8..=15) => idx - 8,
        (Flags::DIM, 0..=7) => NamedColor::DimBlack as usize + idx,
        _ => idx,
    }
}

/// One palette slot: whatever OSC set, else what the theme and the standard
/// ramps say. 256 is the foreground, 257 the background, 258 the cursor.
#[expect(
    clippy::cast_possible_truncation,
    reason = "The cube and grey-ramp match arms bound each channel to 0..=255."
)]
pub fn slot(idx: usize, colors: &Colors, theme: &Theme) -> Rgba {
    if let Some(rgb) = colors[idx] {
        return from_rgb(rgb);
    }
    match idx {
        0..=15 => theme.ansi[idx],
        // The 6×6×6 cube: each channel is 0, then 95 and up in steps of 40.
        16..=231 => {
            let level = |v: usize| if v == 0 { 0 } else { (v * 40 + 55) as u8 };
            let i = idx - 16;
            Rgba::opaque(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        // The 24-step grey ramp between black and white.
        232..=255 => {
            let v = ((idx - 232) * 10 + 8) as u8;
            Rgba::opaque(v, v, v)
        }
        257 => theme.terminal_bg,
        258 => theme.terminal_cursor,
        // The eight dim slots sit right after the cursor, in ANSI order.
        259..=266 => theme.ansi[idx - NamedColor::DimBlack as usize].darken(DIM),
        268 => theme.terminal_fg.darken(DIM),
        // The foreground (256) and its bright form (267): there is no bright
        // foreground of its own, so bold text on the default colour stays the
        // default colour.
        _ => theme.terminal_fg,
    }
}

fn from_rgb(rgb: Rgb) -> Rgba {
    Rgba::opaque(rgb.r, rgb.g, rgb.b)
}

#[cfg(test)]
mod tests {
    use makepad_widgets::vec4;

    use super::*;

    fn theme() -> Theme {
        Theme::concats()
    }

    #[test]
    fn the_named_colours_come_from_the_theme() {
        let theme = theme();
        let colors = Colors::default();
        let red = foreground(
            Color::Named(NamedColor::Red),
            Flags::empty(),
            &colors,
            &theme,
        );
        assert_eq!(red, paint(theme.ansi[1]));
    }

    #[test]
    fn bold_reaches_for_the_bright_half_of_the_palette() {
        let theme = theme();
        let colors = Colors::default();
        let bold = foreground(Color::Named(NamedColor::Red), Flags::BOLD, &colors, &theme);
        assert_eq!(bold, paint(theme.ansi[9]));
        let bold_indexed = foreground(Color::Indexed(1), Flags::BOLD, &colors, &theme);
        assert_eq!(bold_indexed, paint(theme.ansi[9]));
    }

    #[test]
    fn dim_takes_a_third_off_a_true_colour() {
        let theme = theme();
        let colors = Colors::default();
        let spec = Rgb {
            r: 100,
            g: 200,
            b: 0,
        };
        let dim = foreground(Color::Spec(spec), Flags::DIM, &colors, &theme);
        assert_eq!(dim, paint(from_rgb(spec).darken(DIM)));
    }

    #[test]
    fn the_cube_and_the_grey_ramp_are_the_standard_ones() {
        let theme = theme();
        let colors = Colors::default();
        // 16 is the cube's black, 231 its white, and 244 sits mid-grey.
        assert_eq!(
            foreground(Color::Indexed(16), Flags::empty(), &colors, &theme),
            vec4(0.0, 0.0, 0.0, 1.0)
        );
        assert_eq!(
            foreground(Color::Indexed(231), Flags::empty(), &colors, &theme),
            vec4(1.0, 1.0, 1.0, 1.0)
        );
        assert_eq!(
            foreground(Color::Indexed(244), Flags::empty(), &colors, &theme),
            paint(Rgba::opaque(128, 128, 128))
        );
    }

    #[test]
    fn what_osc_set_wins_over_the_theme() {
        let theme = theme();
        let mut colors = Colors::default();
        colors[NamedColor::Red] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(
            foreground(
                Color::Named(NamedColor::Red),
                Flags::empty(),
                &colors,
                &theme
            ),
            paint(Rgba::opaque(1, 2, 3))
        );
    }

    /// The background and cursor a program asks about, or sets: OSC 11 and
    /// 12 win over the theme like any other slot.
    #[test]
    fn the_background_and_cursor_slots_answer_with_what_osc_set() {
        let theme = theme();
        let mut colors = Colors::default();
        let background = NamedColor::Background as usize;
        assert_eq!(slot(background, &colors, &theme), theme.terminal_bg);
        colors[NamedColor::Background] = Some(Rgb { r: 9, g: 8, b: 7 });
        colors[NamedColor::Cursor] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(slot(background, &colors, &theme).bytes(), [9, 8, 7]);
        assert_eq!(
            slot(NamedColor::Cursor as usize, &colors, &theme),
            Rgba::opaque(1, 2, 3)
        );
    }

    #[test]
    fn the_default_background_is_the_panels_own() {
        let theme = theme();
        let colors = Colors::default();
        assert_eq!(
            background(Color::Named(NamedColor::Background), &colors, &theme),
            None
        );
        assert_eq!(
            background(Color::Named(NamedColor::Blue), &colors, &theme),
            Some(paint(theme.ansi[4]))
        );
    }
}
