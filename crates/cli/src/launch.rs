use std::{collections::HashMap, path::PathBuf};

use concats_acp::SessionConfig;
use concats_config::AgentConfig;
use concats_core::Oid;

#[derive(Clone)]
pub struct SessionTabConfig {
    pub agent_label: String,
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub agent_env: HashMap<String, String>,
    pub auto_push: bool,
    pub push_remote: String,
}

pub struct SessionLaunchSpec {
    pub label: String,
    pub session_config: SessionConfig,
    pub tab_config: SessionTabConfig,
}

impl SessionLaunchSpec {
    #[must_use]
    pub fn new(
        workspace_root: PathBuf,
        agent_id: &str,
        agent_config: &AgentConfig,
        auto_push: bool,
        push_remote: &str,
        fork_from: Option<Oid>,
        label: Option<String>,
    ) -> Self {
        let agent_label = agent_config.display_name(agent_id);
        let push_remote = push_remote.to_string();

        Self {
            label: label.unwrap_or_else(|| agent_label.clone()),
            session_config: SessionConfig {
                agent_name: agent_label.clone(),
                agent_command: agent_config.command.clone(),
                agent_args: agent_config.args.clone(),
                workspace_root,
                env: agent_config.env.clone(),
                fork_from,
                auto_push,
                push_remote: push_remote.clone(),
            },
            tab_config: SessionTabConfig {
                agent_label,
                agent_command: agent_config.command.clone(),
                agent_args: agent_config.args.clone(),
                agent_env: agent_config.env.clone(),
                auto_push,
                push_remote,
            },
        }
    }
}

#[must_use]
pub fn fork_tab_label(source_session_id: &str) -> String {
    format!(
        "fork:{}",
        &source_session_id[..8.min(source_session_id.len())]
    )
}
