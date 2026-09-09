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
use makepad_widgets::{Vec4f, vec4};

use crate::theme::Theme;

/// What a dim cell keeps of its colour.
const DIM: f32 = 0.66;

/// A cell's foreground, with bold and dim folded into the colour.
pub fn foreground(color: Color, flags: Flags, colors: &Colors, theme: &Theme) -> Vec4f {
    let rgba = match color {
        // A true colour says exactly what it wants; only dim still touches it.
        Color::Spec(rgb) if flags.contains(Flags::DIM) => dimmed(from_rgb(rgb)),
        Color::Spec(rgb) => from_rgb(rgb),
        Color::Named(named) => index(shade(named, flags) as usize, colors, theme),
        Color::Indexed(idx) => index(shade_indexed(idx, flags), colors, theme),
    };
    to_vec4(rgba)
}

/// A cell's background, or `None` when it is the panel's own — nothing to
/// paint there, and the chrome shows through.
pub fn background(color: Color, colors: &Colors, theme: &Theme) -> Option<Vec4f> {
    if color == Color::Named(NamedColor::Background) {
        return None;
    }
    let rgba = match color {
        Color::Spec(rgb) => from_rgb(rgb),
        Color::Named(named) => index(named as usize, colors, theme),
        Color::Indexed(idx) => index(idx as usize, colors, theme),
    };
    Some(to_vec4(rgba))
}

/// The palette slot for a named colour once bold and dim have had their say.
fn shade(named: NamedColor, flags: Flags) -> NamedColor {
    match flags & (Flags::BOLD | Flags::DIM) {
        Flags::BOLD => named.to_bright(),
        Flags::DIM => named.to_dim(),
        _ => named,
    }
}

/// The same for the indexed colours, where bright and dim are eight apart.
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
/// ramps say.
fn index(idx: usize, colors: &Colors, theme: &Theme) -> Rgba {
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
        259..=266 => dimmed(theme.ansi[idx - NamedColor::DimBlack as usize]),
        268 => dimmed(theme.terminal_fg),
        // The foreground (256) and its bright form (267): there is no bright
        // foreground of its own, so bold text on the default colour stays the
        // default colour.
        _ => theme.terminal_fg,
    }
}

fn dimmed(color: Rgba) -> Rgba {
    Rgba {
        r: color.r * DIM,
        g: color.g * DIM,
        b: color.b * DIM,
        a: color.a,
    }
}

fn from_rgb(rgb: Rgb) -> Rgba {
    Rgba::opaque(rgb.r, rgb.g, rgb.b)
}

fn to_vec4(color: Rgba) -> Vec4f {
    vec4(color.r, color.g, color.b, color.a)
}

#[cfg(test)]
mod tests {
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
        assert_eq!(red, to_vec4(theme.ansi[1]));
    }

    #[test]
    fn bold_reaches_for_the_bright_half_of_the_palette() {
        let theme = theme();
        let colors = Colors::default();
        let bold = foreground(Color::Named(NamedColor::Red), Flags::BOLD, &colors, &theme);
        assert_eq!(bold, to_vec4(theme.ansi[9]));
        let bold_indexed = foreground(Color::Indexed(1), Flags::BOLD, &colors, &theme);
        assert_eq!(bold_indexed, to_vec4(theme.ansi[9]));
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
        assert_eq!(dim, to_vec4(dimmed(from_rgb(spec))));
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
            to_vec4(Rgba::opaque(128, 128, 128))
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
            to_vec4(Rgba::opaque(1, 2, 3))
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
            Some(to_vec4(theme.ansi[4]))
        );
    }
}
