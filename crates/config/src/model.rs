use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Default agent ID to use when none specified.
    #[serde(default = "default_agent")]
    pub default_agent: Option<String>,

    /// Known agent definitions, keyed by ID.
    #[serde(default = "default_agents")]
    pub agents: HashMap<String, AgentConfig>,

    /// Default workspace root (overridden by CLI --workspace).
    #[serde(default)]
    pub workspace: Option<PathBuf>,

    /// Sync settings (auto-push session turn refs to remote).
    #[serde(default)]
    pub sync: SyncConfig,
}

#[allow(clippy::unnecessary_wraps)]
fn default_agent() -> Option<String> {
    Some("claude".into())
}

fn default_agents() -> HashMap<String, AgentConfig> {
    HashMap::from([
        (
            "claude".into(),
            AgentConfig {
                name: "Claude".into(),
                command: "claude".into(),
                args: vec![],
                env: HashMap::new(),
            },
        ),
        (
            "codex".into(),
            AgentConfig {
                name: "Codex".into(),
                command: "codex".into(),
                args: vec![],
                env: HashMap::new(),
            },
        ),
    ])
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_agent: default_agent(),
            agents: default_agents(),
            workspace: None,
            sync: SyncConfig::default(),
        }
    }
}

fn default_remote() -> String {
    "origin".into()
}

/// Configuration for syncing session turn refs to a remote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// When true, automatically push the session ref after each turn.
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

impl AgentConfig {
    #[must_use]
    pub fn display_name(&self, fallback_id: &str) -> String {
        if self.name.trim().is_empty() {
            fallback_id.to_string()
        } else {
            self.name.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

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

        let toml_str = toml::to_string_pretty(&config).expect("should serialize config");
        let decoded: Config = toml::from_str(&toml_str).expect("should deserialize config");

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
        let decoded: Config = toml::from_str(toml_str).expect("should deserialize empty config");

        assert_eq!(decoded.default_agent, Some("claude".to_string()));
        assert_eq!(decoded.workspace, None);
        assert_eq!(decoded.agents.len(), 2);
        assert_eq!(decoded.agents["claude"].command, "claude");
        assert_eq!(decoded.agents["claude"].name, "Claude");
        assert_eq!(decoded.agents["codex"].command, "codex");
        assert_eq!(decoded.agents["codex"].name, "Codex");
    }

    #[test]
    fn test_user_overrides_default_agent() {
        let toml_str = r#"
[agents.claude]
name = "My Claude"
command = "claude"
args = ["--verbose"]
"#;
        let decoded: Config = toml::from_str(toml_str).expect("should deserialize config");

        assert_eq!(decoded.agents["claude"].name, "My Claude");
        assert_eq!(decoded.agents["claude"].args, vec!["--verbose"]);
    }

    #[test]
    fn test_custom_agent_coexists_with_defaults_via_figment() {
        use figment::{
            Figment,
            providers::{Format, Serialized, Toml},
        };

        let toml_str = r#"
[agents.custom]
name = "Custom Agent"
command = "my-agent"
"#;
        let config: Config = Figment::from(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_str))
            .extract()
            .expect("should merge config");

        assert!(config.agents.contains_key("custom"));
        assert_eq!(config.agents["custom"].command, "my-agent");
        assert!(config.agents.contains_key("claude"));
        assert!(config.agents.contains_key("codex"));
    }
}
