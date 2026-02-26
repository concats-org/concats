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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_config_serialization() {
        let mut agents = HashMap::new();
        agents.insert(
            "claude".to_string(),
            AgentConfig {
                name: "Claude".to_string(),
                command: "claude-acp".to_string(),
                args: vec!["--debug".to_string()],
                env: {
                    let mut m = HashMap::new();
                    m.insert("API_KEY".to_string(), "sk-123".to_string());
                    m
                },
            },
        );

        let config = Config {
            default_agent: Some("claude".to_string()),
            agents,
            workspace: Some(PathBuf::from("/work")),
            sync: SyncConfig::default(),
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let decoded: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(config.default_agent, decoded.default_agent);
        assert_eq!(config.workspace, decoded.workspace);
        assert_eq!(config.agents.len(), decoded.agents.len());
        assert_eq!(
            config.agents["claude"].command,
            decoded.agents["claude"].command
        );
    }

    #[test]
    fn test_config_defaults() {
        let toml_str = "";
        let decoded: Config = toml::from_str(toml_str).unwrap();

        assert_eq!(decoded.default_agent, None);
        assert_eq!(decoded.workspace, None);
        assert!(decoded.agents.is_empty());
    }
}
