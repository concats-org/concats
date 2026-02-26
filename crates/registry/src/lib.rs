use std::collections::HashMap;

use serde::Deserialize;

use concats_config::{AgentConfig, Config};

const REGISTRY_URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";

/// ACP registry top-level structure.
#[derive(Debug, Deserialize)]
pub struct Registry {
    pub version: String,
    pub agents: Vec<Agent>,
}

/// A single agent entry from the ACP registry.
#[derive(Debug, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub distribution: Distribution,
}

/// Distribution methods for an agent.
#[derive(Debug, Deserialize)]
pub struct Distribution {
    pub npx: Option<NpxDist>,
    pub uvx: Option<UvxDist>,
}

/// NPX distribution info.
#[derive(Debug, Deserialize)]
pub struct NpxDist {
    pub package: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// UVX (Python) distribution info.
#[derive(Debug, Deserialize)]
pub struct UvxDist {
    pub package: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Fetch the ACP agent registry from the CDN.
pub async fn fetch_registry() -> miette::Result<Registry> {
    let response = reqwest::get(REGISTRY_URL)
        .await
        .map_err(|e| miette::miette!("failed to fetch registry: {e}"))?;

    let registry: Registry = response
        .json()
        .await
        .map_err(|e| miette::miette!("failed to parse registry: {e}"))?;

    Ok(registry)
}

impl TryFrom<&Agent> for AgentConfig {
    type Error = ();

    fn try_from(agent: &Agent) -> Result<Self, Self::Error> {
        if let Some(npx) = &agent.distribution.npx {
            let mut args = vec![npx.package.clone()];
            args.extend(npx.args.iter().cloned());
            return Ok(AgentConfig {
                name: agent.name.clone(),
                command: "npx".into(),
                args,
                env: npx.env.clone(),
            });
        }

        if let Some(uvx) = &agent.distribution.uvx {
            let mut args = vec![uvx.package.clone()];
            args.extend(uvx.args.iter().cloned());
            return Ok(AgentConfig {
                name: agent.name.clone(),
                command: "uvx".into(),
                args,
                env: uvx.env.clone(),
            });
        }

        Err(())
    }
}

/// Merge registry agents into the config's agents map.
///
/// Existing entries are **not** overwritten — only new agents are added.
pub fn install_agents(registry: &Registry, config: &mut Config) {
    for agent in &registry.agents {
        if config.agents.contains_key(&agent.id) {
            continue;
        }
        if let Ok(agent_config) = AgentConfig::try_from(agent) {
            config.agents.insert(agent.id.clone(), agent_config);
        }
    }
}
