use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level application configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Sync settings (auto-push session turn refs to remote).
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub app: AppConfig,
}

/// Configuration for syncing session turn refs to a remote.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    /// When true, automatically push the session ref after each turn.
    pub auto_push: bool,
    /// Git remote name to push to.
    pub remote: String,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_push: false,
            remote: "origin".into(),
        }
    }
}

/// Desktop presentation settings; independent of the renderer and CLI features.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub theme: String,
    pub font: String,
    pub font_size: f64,
    pub terminal_font_size: f64,
    pub wrap: bool,
    pub placement: Placement,
    /// Keys over the built-in bindings, key to command name (`"cmd-w" =
    /// "close_tab"`); an empty name unbinds the key.
    pub keys: BTreeMap<String, String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: "Concats".into(),
            font: String::new(),
            font_size: 9.0,
            // 13 px, the size Ghostty draws its terminal at on macOS: makepad
            // reads a size in points at 96/72.
            terminal_font_size: 9.75,
            // A line you cannot read is not a line you can review, so the
            // default is to wrap it. `wrap = false` puts it back on one row.
            wrap: true,
            placement: Placement::default(),
            keys: BTreeMap::new(),
        }
    }
}

/// One of the window's four areas: the center, or a panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Area {
    Center,
    Left,
    Right,
    Bottom,
}

/// The area each view opens in when it isn't open yet. A view already open
/// stays where it is, and a saved layout wins over this for the tabs it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Placement {
    pub guide: Area,
    pub sessions: Area,
    pub commits: Area,
    /// File Diff.
    pub diff: Area,
    pub comments: Area,
    /// The file browser.
    pub browser: Area,
    pub terminal: Area,
    pub settings: Area,
    /// File tabs, Untitled ones included.
    pub file: Area,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            guide: Area::Center,
            sessions: Area::Center,
            commits: Area::Center,
            diff: Area::Center,
            comments: Area::Left,
            browser: Area::Right,
            terminal: Area::Bottom,
            settings: Area::Center,
            file: Area::Center,
        }
    }
}
