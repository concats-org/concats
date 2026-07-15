//! The active theme and font: which palette the app wears, where it keeps the
//! ones you can pick, and how a colour reaches makepad.
//!
//! The palette itself lives in [`concats_theme`], which knows nothing about a
//! renderer. What is here belongs to the application: the process-wide
//! selection, the settings file it persists to, the font, and [`paint`], the
//! one place an [`Rgba`] becomes a makepad colour.
//!
//! Colour space: makepad reads `#xRRGGBB` as raw channels over 255 (the window
//! clear colour `vec4(0.157, 0.173, 0.20)` is `#x282c33`), and the palette
//! parses hex the same way. So colours round-trip, and [`paint`] only relabels
//! channels.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock, RwLock},
};

use concats_theme::Rgba;
pub(crate) use concats_theme::Theme;
use makepad_widgets::Vec4f;

/// A palette colour as makepad wants it. Everything that draws goes through
/// here, or through the DSL palette `main.rs` bakes from the same fields.
///
/// Not called `vec4`: makepad has one, and this relabels channels rather than
/// building a vector.
pub(crate) fn paint(c: Rgba) -> Vec4f {
    Vec4f {
        x: c.r,
        y: c.g,
        z: c.b,
        w: c.a,
    }
}

fn themes_dir() -> PathBuf {
    concats_config::config_dir().join("themes")
}

pub(crate) fn config_file() -> PathBuf {
    concats_config::config_dir().join("config.json")
}

/// The process-wide theme registry (built once): the built-in default, the
/// bundled themes, then the user's own. Order is the picker's order.
pub fn registry() -> &'static [Theme] {
    static R: OnceLock<Vec<Theme>> = OnceLock::new();
    R.get_or_init(|| concats_theme::registry(Some(&themes_dir())))
}

/// Find a theme by name in the registry, cloned.
pub fn by_name(name: &str) -> Option<Theme> {
    registry().iter().find(|t| t.name == name).cloned()
}

/// Every registered theme's name — shown as a hint in the settings editor.
pub fn theme_names() -> Vec<String> {
    registry().iter().map(|t| t.name.clone()).collect()
}

/// The persisted selection (`config.json`'s `theme`), if any.
fn persisted_selection() -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_file()).ok()?).ok()?;
    v.get("theme")?.as_str().map(|s| s.to_string())
}

/// The settings text to show in the in-app JSON editor — the persisted
/// `config.json` if present, else a default naming the active theme.
pub fn settings_text() -> String {
    if let Ok(text) = std::fs::read_to_string(config_file()) {
        if !text.trim().is_empty() {
            return text;
        }
    }
    let f = active_font();
    serde_json::to_string_pretty(&serde_json::json!({
        "theme": active_theme().name,
        "font": "",
        "font_size": f.size,
    }))
    .unwrap_or_else(|_| "{\n  \"theme\": \"Concats\"\n}".into())
}

/// Why a settings text was refused. Each message is what the editor shows.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("missing \"theme\"")]
    MissingTheme,
    #[error("\"{key}\" must be {expected}")]
    WrongType {
        key: &'static str,
        expected: &'static str,
    },
    #[error("unknown theme {name:?} — try one of: {known}")]
    UnknownTheme { name: String, known: String },
    #[error(
        "font not found: {spec:?} — use an absolute path to a .ttf/.otf/.ttc, \
         or a family name as shown in Font Book"
    )]
    FontNotFound { spec: String },
}

/// Parse edited settings JSON, switch to its `theme`, and persist the raw text.
/// Returns the applied theme name; the caller triggers the live refresh
/// (`request_live_edit` and the terminal retheme).
pub fn apply_settings_text(text: &str) -> Result<String, SettingsError> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    let wrong = |key, expected| SettingsError::WrongType { key, expected };

    // theme (required): a string naming a registered theme.
    let name = match v.get("theme") {
        None => return Err(SettingsError::MissingTheme),
        Some(t) => t.as_str().ok_or_else(|| wrong("theme", "a string"))?,
    };
    let theme = by_name(name).ok_or_else(|| SettingsError::UnknownTheme {
        name: name.to_string(),
        known: theme_names().join(", "),
    })?;

    // font (optional): "" or absent = bundled; otherwise a family name or an
    // absolute path that must resolve to a font file on disk.
    let font_spec = match v.get("font") {
        None => "",
        Some(f) => f.as_str().ok_or_else(|| wrong("font", "a string"))?,
    };
    let font_path = if font_spec.trim().is_empty() {
        None
    } else {
        Some(
            resolve_font_path(font_spec).ok_or_else(|| SettingsError::FontNotFound {
                spec: font_spec.to_string(),
            })?,
        )
    };

    // font_size (optional): a positive number.
    let size = match v.get("font_size") {
        None => 9.0,
        Some(s) => {
            let n = s.as_f64().ok_or_else(|| wrong("font_size", "a number"))?;
            if !(n.is_finite() && n > 0.0) {
                return Err(wrong("font_size", "a positive number"));
            }
            n
        }
    };

    // wrap (optional): whether long lines break instead of running off.
    let wrap = match v.get("wrap") {
        None => false,
        Some(w) => w.as_bool().ok_or_else(|| wrong("wrap", "true or false"))?,
    };

    // Everything validated — only now apply and persist (no partial writes).
    set_active_theme(theme);
    set_active_font(FontSetting {
        path: font_path,
        size,
        wrap,
    });
    let path = config_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, text);
    Ok(name.to_string())
}

fn theme_slot() -> &'static RwLock<Arc<Theme>> {
    static T: OnceLock<RwLock<Arc<Theme>>> = OnceLock::new();
    T.get_or_init(|| {
        // CONCATS_APP_THEME overrides the persisted selection — dev/screenshot
        // convenience, matching the app's other CONCATS_APP_* env knobs.
        let initial = std::env::var("CONCATS_APP_THEME")
            .ok()
            .or_else(persisted_selection)
            .and_then(|name| by_name(&name))
            .unwrap_or_else(Theme::concats);
        RwLock::new(Arc::new(initial))
    })
}

/// The active theme — cloned `Arc`, cheap; clone once per draw pass, not per use.
pub fn active_theme() -> Arc<Theme> {
    theme_slot().read().unwrap().clone()
}

/// Swap the active theme (call `redraw` / reapply after).
pub fn set_active_theme(theme: Theme) {
    *theme_slot().write().unwrap() = Arc::new(theme);
}

// ---------------------------------------------------------------------------
// Font — configurable, loaded from the system. Makepad has no font-name lookup,
// so a family name is resolved to a file here; an absolute path is used as-is;
// empty falls back to the bundled JetBrains Mono. `main.rs::install_app_font`
// feeds the resolved path into the DSL via `mod.app_font`.
// ---------------------------------------------------------------------------

/// The resolved app font: a file to load (`None` = bundled), plus base size.
#[derive(Clone)]
pub struct FontSetting {
    pub path: Option<String>,
    pub size: f64,
    /// Whether a line too long for its row wraps onto the next instead of
    /// running off the edge. It rides with the font because it is a layout
    /// property, and it re-bakes through the same live edit.
    pub wrap: bool,
}

/// Resolve a `font` spec — absolute path (used as-is if it exists) or family
/// name (matched against the macOS system font dirs, alphanumerics only, so
/// "SF Mono" ~ "SFMono-Regular", "Menlo" ~ "Menlo.ttc"). `None` = bundled font.
fn resolve_font_path(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let p = std::path::Path::new(spec);
    if p.is_absolute() {
        return p.exists().then(|| spec.to_string());
    }
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    let want = norm(spec);
    if want.is_empty() {
        return None;
    }
    let mut dirs = vec![
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/System/Library/Fonts/Supplemental"),
        PathBuf::from("/Library/Fonts"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Library/Fonts"));
    }
    // Rank matches so a family name lands on its Regular face, not a random
    // weight: exact stem (0) > "<name>regular" (1) > prefix (2); ties break to
    // the shortest stem (Regular is shorter than Bold/Italic/NL variants).
    let mut best: Option<(u8, usize, String)> = None;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_font = path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "ttf" | "ttc" | "otf"));
            if !is_font {
                continue;
            }
            let stem = norm(path.file_stem().and_then(|s| s.to_str()).unwrap_or(""));
            let rank = if stem == want {
                0
            } else if stem == format!("{want}regular") {
                1
            } else if stem.starts_with(&want) {
                2
            } else {
                continue;
            };
            let Some(path) = path.to_str().map(str::to_string) else {
                continue;
            };
            let cand = (rank, stem.len(), path);
            if best.as_ref().is_none_or(|b| (cand.0, cand.1) < (b.0, b.1)) {
                best = Some(cand);
            }
        }
    }
    best.map(|(.., p)| p)
}

/// The `font`/`font_size` persisted in `config.json` (spec, size).
fn persisted_font() -> (String, f64, bool) {
    let read = || -> Option<(String, f64, bool)> {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(config_file()).ok()?).ok()?;
        Some((
            v.get("font")
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_string(),
            v.get("font_size").and_then(|s| s.as_f64()).unwrap_or(9.0),
            v.get("wrap").and_then(|w| w.as_bool()).unwrap_or(false),
        ))
    };
    read().unwrap_or_else(|| (String::new(), 9.0, false))
}

fn font_slot() -> &'static RwLock<Arc<FontSetting>> {
    static F: OnceLock<RwLock<Arc<FontSetting>>> = OnceLock::new();
    F.get_or_init(|| {
        let (spec, size, wrap) = persisted_font();
        RwLock::new(Arc::new(FontSetting {
            path: resolve_font_path(&spec),
            size,
            wrap,
        }))
    })
}

/// The active font — cloned `Arc`, cheap.
pub fn active_font() -> Arc<FontSetting> {
    font_slot().read().unwrap().clone()
}

fn set_active_font(font: FontSetting) {
    *font_slot().write().unwrap() = Arc::new(font);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_settings_validates() {
        // Malformed JSON, unknown themes, and a missing key are each rejected
        // with a message — the text the in-app editor surfaces. These error
        // paths return before touching disk (the success path persists
        // config.json, so it's left to the runtime rather than the test).
        assert!(apply_settings_text("{ not json").is_err());
        assert!(apply_settings_text(r#"{"theme": "Nope"}"#).is_err());
        assert!(apply_settings_text(r#"{"nope": 1}"#).is_err());
        assert!(apply_settings_text(r#"{"theme": 3}"#).is_err());
        // A non-empty font that resolves to nothing is an error (not a silent
        // fallback); bad font_size is rejected too. All fail before touching disk.
        assert!(apply_settings_text(r#"{"theme":"Concats","font":"No Such Font 9Z"}"#).is_err());
        assert!(apply_settings_text(r#"{"theme":"Concats","font_size":"big"}"#).is_err());
        assert!(apply_settings_text(r#"{"theme":"Concats","font_size":-3}"#).is_err());
        // The default theme is always resolvable by name.
        assert!(by_name("Concats").is_some());
    }
}
