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

pub(crate) fn config_file() -> Option<PathBuf> {
    concats_config::config_dir().map(|dir| dir.join("config.toml"))
}

fn settings() -> &'static concats_config::Config {
    static SETTINGS: OnceLock<concats_config::Config> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        concats_config::load_config(&concats_config::ConfigCliArgs::default()).unwrap_or_else(
            |error| {
                eprintln!("cannot load settings: {error}");
                concats_config::Config::default()
            },
        )
    })
}

/// The process-wide theme registry (built once): the built-in default, the
/// bundled themes, then the user's own. Order is the picker's order.
pub fn registry() -> &'static [Theme] {
    static R: OnceLock<Vec<Theme>> = OnceLock::new();
    R.get_or_init(|| {
        concats_theme::registry(
            concats_config::config_dir()
                .map(|dir| dir.join("themes"))
                .as_deref(),
        )
    })
}

/// Find a theme by name in the registry, cloned.
pub fn by_name(name: &str) -> Option<Theme> {
    registry().iter().find(|t| t.name == name).cloned()
}

/// Every registered theme's name — shown as a hint in the settings editor.
pub fn theme_names() -> Vec<String> {
    registry().iter().map(|t| t.name.clone()).collect()
}

/// The shared TOML configuration edited by both the CLI and desktop app.
pub fn settings_text() -> String {
    config_file()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_else(|| {
            toml::to_string_pretty(&concats_config::Config::default())
                .expect("configuration is serializable")
        })
}

/// Why a settings text was refused. Each message is what the editor shows.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("font_size must be a positive finite number")]
    FontSize,
    #[error("cannot save configuration: {0}")]
    Persist(String),
    #[error("unknown theme {name:?} — try one of: {known}")]
    UnknownTheme { name: String, known: String },
    #[error(
        "no font found in {spec:?} — list them the way CSS does, most wanted \
         first, each an absolute path to a .ttf/.otf/.ttc or a family name as \
         shown in Font Book"
    )]
    FontNotFound { spec: String },
}

/// Validate the shared configuration before persisting or changing the UI.
pub fn apply_settings_text(text: &str) -> Result<String, SettingsError> {
    let config: concats_config::Config = toml::from_str(text)?;
    let settings = &config.app;
    let theme = by_name(&settings.theme).ok_or_else(|| SettingsError::UnknownTheme {
        name: settings.theme.clone(),
        known: theme_names().join(", "),
    })?;
    let paths = resolve_font_paths(&settings.font);
    if paths.is_empty() && !font_specs(&settings.font).is_empty() {
        return Err(SettingsError::FontNotFound {
            spec: settings.font.clone(),
        });
    }
    let size = font_size(settings.font_size)?;
    concats_config::save_config(&config)
        .map_err(|error| SettingsError::Persist(error.to_string()))?;
    set_active_theme(theme);
    set_active_font(FontSetting {
        paths,
        size,
        wrap: settings.wrap,
    });
    Ok(settings.theme.clone())
}

fn theme_slot() -> &'static RwLock<Arc<Theme>> {
    static T: OnceLock<RwLock<Arc<Theme>>> = OnceLock::new();
    T.get_or_init(|| {
        // CONCATS_APP_THEME overrides the persisted selection — dev/screenshot
        // convenience, matching the app's other CONCATS_APP_* env knobs.
        let initial = crate::dev_hooks::var("CONCATS_APP_THEME")
            .ok()
            .or_else(|| Some(settings().app.theme.clone()))
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
// so a family name is resolved to a file here; an absolute path is used as-is.
// The setting is a list, the way CSS writes `font-family`, and the embedded
// fonts always follow it. `main.rs::install_app_font` feeds the resolved paths
// into the DSL via `mod.app_font`.
// ---------------------------------------------------------------------------

/// The resolved app font: the files to try in order (empty = the embedded
/// fonts alone), plus base size.
///
/// TODO: one size does not fit all three surfaces. The shape to grow into is
/// a global `font_size` with `ui_font_size`, `editor_font_size` and
/// `terminal_font_size` beside it, each falling back to the global when
/// unset — and the same layering for anything else worth varying per
/// surface, line height first. Today the terminal borrows this family and
/// pins its own size in the DSL, which is why its cell cannot be tuned.
#[derive(Clone)]
pub struct FontSetting {
    pub paths: Vec<String>,
    pub size: f64,
    /// Whether a line too long for its row wraps onto the next instead of
    /// running off the edge. It rides with the font because it is a layout
    /// property, and it re-bakes through the same live edit.
    pub wrap: bool,
}

/// Split a `font` setting the way CSS splits `font-family`: a comma-separated
/// list, most wanted first. Quotes around a name are decorative and dropped, so
/// habits carried over from CSS work — but a name or path containing a comma
/// cannot be written here.
fn font_specs(spec: &str) -> Vec<&str> {
    spec.split(',')
        .map(|name| name.trim().trim_matches(['"', '\'']).trim())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Every font in a `font` setting that this machine actually has, in the order
/// asked for. A name that resolves to nothing is dropped rather than refused:
/// the point of a list is that it survives a machine missing one of them.
fn resolve_font_paths(spec: &str) -> Vec<String> {
    font_specs(spec)
        .into_iter()
        .filter_map(resolve_font_path)
        .collect()
}

/// Resolve a `font` spec — absolute path (used as-is if it exists) or family
/// name (matched against the macOS system font dirs, alphanumerics only, so
/// "SF Mono" ~ "SFMono-Regular", "Menlo" ~ "Menlo.ttc"). `None` = bundled font.
#[expect(
    clippy::cognitive_complexity,
    reason = "Font candidates are ranked across system directories in one search."
)]
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
            .flat_map(char::to_lowercase)
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

fn font_size(size: f64) -> Result<f64, SettingsError> {
    if size.is_finite() && size > 0.0 {
        Ok(size)
    } else {
        Err(SettingsError::FontSize)
    }
}

fn font_slot() -> &'static RwLock<Arc<FontSetting>> {
    static F: OnceLock<RwLock<Arc<FontSetting>>> = OnceLock::new();
    F.get_or_init(|| {
        let config = &settings().app;
        RwLock::new(Arc::new(FontSetting {
            paths: resolve_font_paths(&config.font),
            size: font_size(config.font_size).unwrap_or_else(|error| {
                // NOTE: an invalid persisted size must not reach text layout.
                eprintln!("cannot apply font size: {error}");
                concats_config::AppConfig::default().font_size
            }),
            wrap: config.wrap,
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
    fn a_font_setting_is_a_css_style_list() {
        assert_eq!(font_specs("SF Mono"), ["SF Mono"]);
        assert_eq!(font_specs("SF Mono, Menlo"), ["SF Mono", "Menlo"]);
        // Quotes are habit carried over from CSS, and spacing is free.
        assert_eq!(font_specs("  'SF Mono' ,\"Menlo\" "), ["SF Mono", "Menlo"]);
        // Nothing named is nothing to load — the embedded fonts stand alone.
        assert_eq!(font_specs(""), Vec::<&str>::new());
        assert_eq!(font_specs("  , ,"), Vec::<&str>::new());
    }

    #[test]
    fn apply_settings_validates() {
        for text in [
            "not = [toml",
            "[app]\ntheme = 'Nope'",
            "[app]\ntheme = 3",
            "[app]\nfont = 'No Such Font 9Z'",
            "[app]\nfont_size = -3",
            "[app]\nfont_size = 'big'",
            "[app]\nwrap = 'yes'",
        ] {
            assert!(apply_settings_text(text).is_err(), "{text}");
        }
    }
}
