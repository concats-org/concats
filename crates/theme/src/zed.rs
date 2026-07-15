//! Zed theme import: what `zed.dev/theme-builder` exports, as a [`Theme`].
//!
//! Every slot is filled from the closest Zed key, with fallbacks among Zed
//! keys, and drops to the base theme only when a whole chain is absent. So a
//! complete file (theme-builder output) never touches the base, and a light Zed
//! file gives a light theme rather than a dark one with light text.

use std::collections::HashMap;

use concats_syntax::capture_to_hl;
use serde::Deserialize;

use crate::{Rgba, Theme};

/// A Zed theme *family* file: `{ name, author, themes: [...] }`.
#[derive(Deserialize)]
struct File {
    #[serde(default)]
    themes: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    #[serde(default)]
    style: Style,
}

#[derive(Deserialize, Default)]
struct Style {
    /// Every dotted colour key (`editor.background`, …). `Value` (not `String`)
    /// so non-colour members like `accents` (an array) don't fail the whole parse.
    #[serde(flatten)]
    map: HashMap<String, serde_json::Value>,
    #[serde(default)]
    syntax: HashMap<String, Highlight>,
    #[serde(default)]
    players: Vec<Player>,
}

#[derive(Deserialize)]
struct Highlight {
    #[serde(default)]
    color: Option<String>,
    // font_style / font_weight are intentionally ignored for now — the diff
    // renderer varies only colour per span (bold/italic syntax is a later add).
}

#[derive(Deserialize)]
struct Player {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    selection: Option<String>,
}

/// Parse a Zed hex colour (`#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`).
fn parse_color(spec: &str) -> Option<Rgba> {
    let hex = spec.strip_prefix('#')?;
    // A one-digit channel means both of its nibbles: "f" -> 0xff.
    let nibble = |at: usize| {
        u8::from_str_radix(hex.get(at..at + 1)?, 16)
            .ok()
            .map(|c| (c << 4) | c)
    };
    let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
    let (r, g, b, a) = match hex.len() {
        3 => (nibble(0)?, nibble(1)?, nibble(2)?, 0xff),
        4 => (nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?),
        6 => (byte(0)?, byte(2)?, byte(4)?, 0xff),
        8 => (byte(0)?, byte(2)?, byte(4)?, byte(6)?),
        _ => return None,
    };
    Some(Rgba::new(r, g, b, a))
}

/// Every theme in a Zed theme-family JSON. An unparseable file yields none.
pub fn import(json: &str) -> Vec<Theme> {
    let base = Theme::concats();
    serde_json::from_str::<File>(json)
        .map(|file| file.themes.iter().map(|zt| theme(zt, &base)).collect())
        .unwrap_or_default()
}

fn theme(zt: &Entry, base: &Theme) -> Theme {
    let m = &zt.style.map;
    let hex = |k: &str| m.get(k).and_then(|v| v.as_str()).and_then(parse_color);
    let player = |f: fn(&Player) -> Option<&String>| {
        zt.style
            .players
            .first()
            .and_then(f)
            .and_then(|s| parse_color(s))
    };

    let background = hex("background").unwrap_or(base.background);
    let surface = hex("surface.background")
        .or(hex("elevated_surface.background"))
        .unwrap_or(base.surface);
    let border = hex("border").unwrap_or(base.border);
    let text = hex("text").unwrap_or(base.text);
    let text_muted = hex("text.muted")
        .or(hex("text.placeholder"))
        .unwrap_or(text);
    let accent = hex("text.accent")
        .or(player(|p| p.cursor.as_ref()))
        .unwrap_or(base.accent);
    let element_hover = hex("element.hover")
        .or(hex("element.background"))
        .unwrap_or(surface);
    let editor_bg = hex("editor.background").unwrap_or(background);

    // Syntax: map each Zed token name onto the Hl vocabulary, starting from the
    // base map so unspecified tokens keep sensible (in-appearance) colours.
    let mut syntax = base.syntax.clone();
    for (tok, hl) in &zt.style.syntax {
        if let (Some(h), Some(c)) = (
            capture_to_hl(tok),
            hl.color.as_deref().and_then(parse_color),
        ) {
            syntax.insert(h, c);
        }
    }

    // The 16 ANSI colours: `terminal.ansi.<name>` (0-7) and `bright_<name>`
    // (8-15). Any absent entry keeps the base default.
    let names = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    let mut ansi = base.ansi;
    for (i, slot) in ansi.iter_mut().enumerate() {
        let n = names[i % 8];
        let key = if i < 8 {
            format!("terminal.ansi.{n}")
        } else {
            format!("terminal.ansi.bright_{n}")
        };
        *slot = hex(&key).unwrap_or(base.ansi[i]);
    }

    Theme {
        name: zt.name.clone(),

        background,
        // No Zed key for this; derive it so imported themes get a shadow that
        // stays darker than their own page colour.
        shadow: background.darken(0.72).with_alpha(1.0),
        surface,
        chrome: hex("toolbar.background")
            .or(hex("title_bar.background"))
            .or(hex("status_bar.background"))
            .unwrap_or(surface),

        border,
        border_hover: hex("border.variant").unwrap_or(border),
        border_focus: hex("border.focused").unwrap_or(accent),

        text,
        text_muted,
        text_faint: hex("editor.line_number")
            .or(hex("text.placeholder"))
            .unwrap_or(text_muted),

        accent,
        // No Zed key for "glyph drawn over accent"; white reads on any accent.
        on_accent: base.on_accent,
        added: hex("version_control.added")
            .or(hex("created"))
            .unwrap_or(base.added),
        deleted: hex("version_control.deleted")
            .or(hex("deleted"))
            .unwrap_or(base.deleted),
        modified: hex("version_control.modified")
            .or(hex("modified"))
            .unwrap_or(base.modified),

        checkbox_bg: hex("element.background").unwrap_or(surface),
        checkbox_hover: element_hover,
        element_hover,

        syntax,

        terminal_bg: hex("terminal.background").unwrap_or(editor_bg),
        terminal_fg: hex("terminal.foreground")
            .or(hex("editor.foreground"))
            .unwrap_or(text),
        terminal_cursor: player(|p| p.cursor.as_ref()).unwrap_or(base.terminal_cursor),
        terminal_cell_bg: hex("terminal.ansi.background").unwrap_or(surface),
        selection: player(|p| p.selection.as_ref()).unwrap_or(base.selection),
        ansi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_color_forms() {
        assert_eq!(parse_color("#282c33"), Some(Rgba::opaque(0x28, 0x2c, 0x33)));
        assert_eq!(
            parse_color("#282c33ff"),
            Some(Rgba::opaque(0x28, 0x2c, 0x33))
        );
        assert_eq!(parse_color("#fff"), Some(Rgba::opaque(0xff, 0xff, 0xff)));
        assert_eq!(parse_color("#0000"), Some(Rgba::new(0, 0, 0, 0)));
        assert_eq!(parse_color("4d5ad0"), None); // missing '#'
        assert_eq!(parse_color("#zzz"), None);
    }

    #[test]
    fn a_file_that_is_not_a_theme_yields_none_rather_than_a_wrong_one() {
        assert!(import("{ not json").is_empty());
        assert!(import(r#"{"themes": []}"#).is_empty());
    }
}
