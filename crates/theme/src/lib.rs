//! The palette: one [`Theme`] holds every colour — chrome, diff, syntax and
//! terminal.
//!
//! A theme holds [`Rgba`] rather than a renderer's vector type, and that is why
//! this is a crate. A palette that needs a GUI toolkit just to be held cannot
//! be loaded by a terminal renderer, and colouring a diff in a terminal is what
//! the diff model is for. Converting at the renderer's edge costs one function;
//! the alternative costs the renderer.
//!
//! Themes come from Zed's theme JSON (what `zed.dev/theme-builder` exports),
//! see [`zed`], plus the built-in [`Theme::concats`]. Both key their token
//! colours on tree-sitter capture names via [`concats_syntax`], so a theme is
//! written without knowing which engine renders it.
//!
//! Colour space: makepad treats `#xRRGGBB` as raw /255 (its window clear colour
//! is `vec4(0.157, 0.173, 0.20)` == `#x282c33`), so Zed's hex is parsed the
//! same way, no sRGB decode, and colours round-trip as they are.

use std::{collections::HashMap, path::Path};

use concats_syntax::Hl;

mod zed;

/// A colour as rgba in 0..1 — the form both a GPU uniform and an ANSI escape
/// want, and neither renderer's own type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    /// From 8-bit channels, which is how every theme file writes them.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    /// Opaque.
    #[must_use]
    pub const fn opaque(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, 0xff)
    }

    /// The same colour at another alpha — how one accent becomes a drag tint, a
    /// focused selection and an unfocused one.
    #[must_use]
    pub const fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// Scaled towards black, alpha untouched.
    #[must_use]
    pub fn darken(self, by: f32) -> Self {
        Self {
            r: self.r * by,
            g: self.g * by,
            b: self.b * by,
            a: self.a,
        }
    }
}

/// Every colour, in one place. Field names mirror Zed's `style` keys where a
/// clean correspondence exists, so the importer maps ~1:1.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Display name — what a settings editor shows and a config file persists.
    pub name: String,

    // -- surfaces -----------------------------------------------------------
    /// Window + content + active tab.
    pub background: Rgba,
    /// Cast by the tab strip and the pinned file header. Darker than
    /// `background` so it darkens the page as well as a card.
    pub shadow: Rgba,
    /// File cards, inactive chrome, dropdown wells.
    pub surface: Rgba,
    /// Header band + status bar.
    pub chrome: Rgba,

    // -- borders / lines ----------------------------------------------------
    pub border: Rgba,
    pub border_hover: Rgba,
    pub border_focus: Rgba,

    // -- text ---------------------------------------------------------------
    pub text: Rgba,
    pub text_muted: Rgba,
    /// Line numbers and other faint text.
    pub text_faint: Rgba,

    // -- accents / status ---------------------------------------------------
    /// Comment markers, selection tint, the `+` affordance, composer bars.
    pub accent: Rgba,
    /// Foreground drawn over `accent` (the white `+` glyph).
    pub on_accent: Rgba,
    /// Added lines' marker; also the "viewed" tick.
    pub added: Rgba,
    /// Deleted lines' marker.
    pub deleted: Rgba,
    /// Modified/attention (the design's yellow).
    pub modified: Rgba,

    // -- interactive fills --------------------------------------------------
    pub checkbox_bg: Rgba,
    pub checkbox_hover: Rgba,
    /// Hover fill for combo/menu rows.
    pub element_hover: Rgba,

    // -- syntax -------------------------------------------------------------
    /// Per-[`Hl`] token colour; absent falls back to [`Theme::text`].
    pub syntax: HashMap<Hl, Rgba>,

    // -- terminal -----------------------------------------------------------
    pub terminal_bg: Rgba,
    pub terminal_fg: Rgba,
    /// Cursor colour incl. its blend alpha (drawn as `#fff7` by default).
    pub terminal_cursor: Rgba,
    /// A styled-but-unset cell's background.
    pub terminal_cell_bg: Rgba,
    /// Text-selection tint (alpha carried; the renderer uses it as-is).
    pub selection: Rgba,
    /// The 16 ANSI colours (0-7 normal, 8-15 bright).
    pub ansi: [Rgba; 16],
}

impl Theme {
    /// The token colour for a highlight span; the default text colour otherwise.
    #[must_use]
    pub fn syntax_color(&self, hl: Option<Hl>) -> Rgba {
        hl.and_then(|h| self.syntax.get(&h).copied())
            .unwrap_or(self.text)
    }

    /// The house palette, and the base an import falls back to: One Dark's
    /// syntax and accents over near-black surfaces.
    #[must_use]
    pub fn concats() -> Theme {
        let syntax = [
            (Hl::Comment, Rgba::opaque(0x5d, 0x63, 0x6f)),
            (Hl::Keyword, Rgba::opaque(0xb4, 0x77, 0xcf)),
            (Hl::String, Rgba::opaque(0xa1, 0xc1, 0x81)),
            (Hl::Function, Rgba::opaque(0x73, 0xad, 0xe9)),
            (Hl::Type, Rgba::opaque(0x6e, 0xb4, 0xbf)),
            (Hl::Property, Rgba::opaque(0x6e, 0xb4, 0xbf)),
            (Hl::Number, Rgba::opaque(0xbf, 0x95, 0x6a)),
            (Hl::Constant, Rgba::opaque(0xbf, 0x95, 0x6a)),
            (Hl::Parameter, Rgba::opaque(0xbf, 0x95, 0x6a)),
            (Hl::Attribute, Rgba::opaque(0xde, 0xc1, 0x84)),
            (Hl::Operator, Rgba::opaque(0xb2, 0xb9, 0xc6)),
            (Hl::Punctuation, Rgba::opaque(0xb2, 0xb9, 0xc6)),
            (Hl::Variable, Rgba::opaque(0xac, 0xb2, 0xbe)),
        ]
        .into_iter()
        .collect();

        Theme {
            name: "Concats".into(),

            background: Rgba::opaque(0x1e, 0x1f, 0x22),
            shadow: Rgba::opaque(0x17, 0x17, 0x1a),
            surface: Rgba::opaque(0x26, 0x28, 0x2b),
            chrome: Rgba::opaque(0x31, 0x33, 0x37),

            border: Rgba::opaque(0x39, 0x3b, 0x41),
            border_hover: Rgba::opaque(0x49, 0x4b, 0x51),
            border_focus: Rgba::opaque(0x60, 0x64, 0x6c),

            text: Rgba::opaque(0xdc, 0xe0, 0xe5),
            text_muted: Rgba::opaque(0xa9, 0xaf, 0xbc),
            text_faint: Rgba::opaque(0x4e, 0x5a, 0x5f),

            accent: Rgba::opaque(0x4d, 0x5a, 0xd0),
            on_accent: Rgba::opaque(0xff, 0xff, 0xff),
            added: Rgba::opaque(0x4d, 0xd0, 0x7e),
            deleted: Rgba::opaque(0xd0, 0x72, 0x77),
            modified: Rgba::opaque(0xde, 0xc1, 0x84),

            checkbox_bg: Rgba::opaque(0x26, 0x28, 0x2b),
            checkbox_hover: Rgba::opaque(0x2b, 0x2d, 0x31),
            element_hover: Rgba::opaque(0x30, 0x33, 0x37),

            syntax,

            terminal_bg: Rgba::opaque(0x1e, 0x1f, 0x22),
            terminal_fg: Rgba::opaque(0xdc, 0xe0, 0xe5),
            terminal_cursor: Rgba::new(0xff, 0xff, 0xff, 0x77),
            terminal_cell_bg: Rgba::opaque(0x30, 0x31, 0x34),
            selection: Rgba::new(0x4d, 0x5a, 0xd0, 0x80),

            // makepad-terminal-core's default 16 (Tomorrow-Night-ish); kept
            // identical so terminals look unchanged until a theme overrides them.
            ansi: [
                Rgba::opaque(0x1d, 0x1f, 0x21), // black
                Rgba::opaque(0xcc, 0x66, 0x66), // red
                Rgba::opaque(0xb5, 0xbd, 0x68), // green
                Rgba::opaque(0xf0, 0xc6, 0x74), // yellow
                Rgba::opaque(0x81, 0xa2, 0xbe), // blue
                Rgba::opaque(0xb2, 0x94, 0xbb), // magenta
                Rgba::opaque(0x8a, 0xbe, 0xb7), // cyan
                Rgba::opaque(0xc5, 0xc8, 0xc6), // white
                Rgba::opaque(0x66, 0x66, 0x66), // bright black
                Rgba::opaque(0xd5, 0x4e, 0x53), // bright red
                Rgba::opaque(0xb9, 0xca, 0x4a), // bright green
                Rgba::opaque(0xe7, 0xc5, 0x47), // bright yellow
                Rgba::opaque(0x7a, 0xa6, 0xda), // bright blue
                Rgba::opaque(0xc3, 0x97, 0xd8), // bright magenta
                Rgba::opaque(0x70, 0xc0, 0xb1), // bright cyan
                Rgba::opaque(0xea, 0xea, 0xea), // bright white
            ],
        }
    }
}

/// Themes that ship with the binary, imported alongside the hardcoded default.
///
/// NOTE: every bundled file is a permissively licensed Zed community theme,
/// vendored verbatim. The GPL-3 Zed export of One Light that used to sit here
/// was dropped so the bundle stays relicensable. Provenance:
/// - `catppuccin-mauve.json` — github.com/catppuccin/zed, MIT, © Catppuccin.
/// - `tokyo-night.json` — github.com/ssaunderss/zed-tokyo-night, MIT.
/// - `dracula.json` — github.com/dracula/zed, MIT, © Dracula Theme.
/// - `rose-pine*.json` — github.com/rose-pine/zed, MIT, © Rosé Pine.
const BUNDLED: &[&str] = &[
    include_str!("../assets/themes/catppuccin-mauve.json"),
    include_str!("../assets/themes/dracula.json"),
    include_str!("../assets/themes/rose-pine-dawn.json"),
    include_str!("../assets/themes/rose-pine-moon.json"),
    include_str!("../assets/themes/rose-pine.json"),
    include_str!("../assets/themes/tokyo-night.json"),
];

/// Every theme available: the built-in default, then the bundled ones, then the
/// `.json` files in `user_dir` in name order. This is the picker's order.
///
/// The directory is a parameter, not a lookup: where a user's themes live is
/// the application's rule, not the palette's. A CLI and a GUI answer it
/// differently, and neither answer belongs here.
#[must_use]
pub fn registry(user_dir: Option<&Path>) -> Vec<Theme> {
    let mut themes = vec![Theme::concats()];
    for json in BUNDLED {
        themes.extend(zed::import(json));
    }
    let Some(entries) = user_dir.and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return themes;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    themes.extend(
        files
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .flat_map(|json| zed::import(&json)),
    );
    themes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_file_imports_and_light_means_light() {
        // `zed::import` swallows a parse failure into an empty vec, so a
        // bundled file that stopped parsing would otherwise vanish silently —
        // count each one. And a light variant must come out genuinely light:
        // a broken importer or dark-base leakage shows up as a dark
        // background.
        for json in BUNDLED {
            assert!(!zed::import(json).is_empty(), "a bundled file imports");
        }
        let themes = registry(None);
        let light = themes
            .iter()
            .find(|t| t.name == "Rosé Pine Dawn")
            .expect("bundled");
        assert!(light.background.r > 0.8, "light background");
        assert!(light.text.r < 0.4, "dark text on a light background");
        assert_ne!(
            light.syntax_color(Some(Hl::Keyword)),
            Theme::concats().syntax_color(Some(Hl::Keyword)),
            "syntax got real overrides, not just the dark base"
        );
    }

    #[test]
    fn the_built_in_theme_is_always_first() {
        assert_eq!(registry(None)[0].name, "Concats");
    }

    #[test]
    fn an_unnamed_token_falls_back_to_the_text_colour() {
        let t = Theme::concats();
        assert_eq!(t.syntax_color(None), t.text);
        assert_eq!(t.syntax_color(Some(Hl::Comment)), t.syntax[&Hl::Comment]);
    }

    #[test]
    fn alpha_rides_on_a_colour_without_moving_it() {
        let base = Rgba::opaque(0x40, 0x80, 0xc0);
        assert_eq!(base.with_alpha(0.25), Rgba { a: 0.25, ..base });
    }
}
