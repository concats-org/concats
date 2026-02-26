use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Top-level application configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Default agent ID to use when none specified.
    #[serde(default)]
    pub default_agent: Option<String>,

    /// Known agent definitions, keyed by ID.
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,

    /// Default workspace root (overridden by CLI --workspace).
    #[serde(default)]
    pub workspace: Option<PathBuf>,

    /// Sync settings (auto-push checkpoints to remote).
    #[serde(default)]
    pub sync: SyncConfig,
}

fn default_remote() -> String {
    "origin".into()
}

/// Configuration for syncing session checkpoints to a remote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// When true, automatically push the session ref after each checkpoint.
    #[serde(default)]
    pub auto_push: bool,
    /// Git remote name to push to.
    #[serde(default = "default_remote")]
    pub remote: String,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_push: false,
            remote: default_remote(),
        }
    }
}

/// Configuration for a single agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}
