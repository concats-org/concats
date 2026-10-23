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
        }
    }
}
