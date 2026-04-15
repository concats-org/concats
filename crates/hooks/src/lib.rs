use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

use concats_core::error::{Error, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct HookPayload {
    #[serde(alias = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    response: Option<String>,
}

enum HandlerAction {
    SessionStarted,
    PromptSubmitted,
    FilesChanged,
    Stop,
    Ignore,
}

fn dispatch_simple(
    agent_name: &str,
    default_session_id: &str,
    event: &str,
    payload_json: &str,
    resolve: fn(&str) -> Result<HandlerAction>,
) -> Result<()> {
    let payload: HookPayload = serde_json::from_str(payload_json)
        .map_err(|error| Error::session(format!("invalid {agent_name} payload: {error}")))?;
    let session_id = payload.session_id.as_deref().unwrap_or(default_session_id);
    let worktree_root = find_worktree_root(payload.cwd.as_deref())?;

    match resolve(event)? {
        HandlerAction::SessionStarted => handler::on_session_started(&worktree_root, session_id),
        HandlerAction::PromptSubmitted => match payload.prompt.as_deref() {
            Some(prompt) => handler::on_prompt_submitted(&worktree_root, session_id, prompt),
            None => Ok(()),
        },
        HandlerAction::FilesChanged => handler::on_files_changed(&worktree_root, session_id),
        HandlerAction::Stop => {
            let response = payload
                .response
                .as_deref()
                .unwrap_or("(response not captured)");
            handler::on_stop(&worktree_root, session_id, response)
        }
        HandlerAction::Ignore => Ok(()),
    }
}

/// Where to install hooks for an agent.
///
/// Only Claude distinguishes between project-level and user-level settings;
/// all other agents install at the user level and ignore the project root.
#[derive(Debug, Clone)]
pub enum InstallScope {
    /// User-level settings (e.g. `~/.claude/settings.json`).
    User,
    /// Project-level settings (e.g. `<project_root>/.claude/settings.json`).
    Project { root: PathBuf },
}

impl InstallScope {
    fn claude_settings_path(&self) -> Result<PathBuf> {
        match self {
            Self::User => dirs::home_dir()
                .map(|h| h.join(".claude").join("settings.json"))
                .ok_or_else(|| Error::session("cannot determine home directory")),
            Self::Project { root } => Ok(root.join(".claude").join("settings.json")),
        }
    }
}

pub mod amp;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod droid;
pub mod gemini;
pub mod handler;
pub mod install;
pub mod opencode;
pub mod state;
pub mod windsurf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Cursor,
    Windsurf,
    Gemini,
    Copilot,
    Droid,
    Amp,
    OpenCode,
}

impl Agent {
    pub const ALL: &[Self] = &[
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::Windsurf,
        Self::Gemini,
        Self::Copilot,
        Self::Droid,
        Self::Amp,
        Self::OpenCode,
    ];

    #[must_use]
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::Gemini => "gemini",
            Self::Copilot => "copilot",
            Self::Droid => "droid",
            Self::Amp => "amp",
            Self::OpenCode => "opencode",
        }
    }

    #[must_use]
    pub fn is_detected(self) -> bool {
        let Some(home) = dirs::home_dir() else {
            return false;
        };
        match self {
            Self::Claude => home.join(".claude").is_dir(),
            Self::Codex => home.join(".codex").is_dir(),
            Self::Cursor => home.join(".cursor").is_dir(),
            Self::Windsurf => home.join(".codeium").join("windsurf").is_dir(),
            Self::Gemini => home.join(".gemini").is_dir(),
            // Copilot CLI requires the gh CLI (~/.config/gh).
            Self::Copilot => home.join(".config").join("gh").is_dir(),
            Self::Droid => home.join(".factory").is_dir(),
            Self::Amp => home.join(".config").join("amp").is_dir(),
            Self::OpenCode => home.join(".config").join("opencode").is_dir(),
        }
    }

    /// Dispatch a hook event to the agent-specific handler.
    ///
    /// # Errors
    ///
    /// Returns an error if the event is missing when required, the payload
    /// cannot be parsed, or the underlying handler fails.
    pub fn dispatch(self, event: Option<&str>, payload: &str) -> Result<()> {
        if self == Self::Codex {
            return codex::dispatch(payload);
        }
        let event =
            event.ok_or_else(|| Error::session(format!("{self} requires an event name")))?;
        match self {
            Self::Claude => claude::dispatch(event, payload),
            Self::Codex => unreachable!(),
            Self::Cursor => cursor::dispatch(event, payload),
            Self::Windsurf => windsurf::dispatch(event, payload),
            Self::Gemini => gemini::dispatch(event, payload),
            Self::Copilot => copilot::dispatch(event, payload),
            Self::Droid => droid::dispatch(event, payload),
            Self::Amp => amp::dispatch(event, payload),
            Self::OpenCode => opencode::dispatch(event, payload),
        }
    }

    /// Install concats hooks for this agent.
    ///
    /// `scope` is only consulted by Claude; other agents always install at
    /// the user level.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent configuration cannot be read or written.
    pub fn install(self, binary: &Path, scope: &InstallScope) -> Result<()> {
        match self {
            Self::Claude => claude::install(&scope.claude_settings_path()?, binary),
            Self::Codex => codex::install(binary),
            Self::Cursor => cursor::install(binary),
            Self::Windsurf => windsurf::install(binary),
            Self::Gemini => gemini::install(binary),
            Self::Copilot => copilot::install(binary),
            Self::Droid => droid::install(binary),
            Self::Amp => amp::install(binary),
            Self::OpenCode => opencode::install(binary),
        }
    }

    /// Remove concats hooks for this agent.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent configuration cannot be read or written.
    pub fn uninstall(self, scope: &InstallScope) -> Result<()> {
        match self {
            Self::Claude => claude::uninstall(&scope.claude_settings_path()?),
            Self::Codex => codex::uninstall(),
            Self::Cursor => cursor::uninstall(),
            Self::Windsurf => windsurf::uninstall(),
            Self::Gemini => gemini::uninstall(),
            Self::Copilot => copilot::uninstall(),
            Self::Droid => droid::uninstall(),
            Self::Amp => amp::uninstall(),
            Self::OpenCode => opencode::uninstall(),
        }
    }

    /// Check whether concats hooks are installed for this agent.
    #[must_use]
    pub fn is_installed(self, scope: &InstallScope) -> bool {
        match self {
            Self::Claude => scope
                .claude_settings_path()
                .is_ok_and(|p| claude::is_installed(&p)),
            Self::Codex => codex::is_installed(),
            Self::Cursor => cursor::is_installed(),
            Self::Windsurf => windsurf::is_installed(),
            Self::Gemini => gemini::is_installed(),
            Self::Copilot => copilot::is_installed(),
            Self::Droid => droid::is_installed(),
            Self::Amp => amp::is_installed(),
            Self::OpenCode => opencode::is_installed(),
        }
    }
}

impl FromStr for Agent {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "cursor" => Ok(Self::Cursor),
            "windsurf" => Ok(Self::Windsurf),
            "gemini" => Ok(Self::Gemini),
            "copilot" => Ok(Self::Copilot),
            "droid" => Ok(Self::Droid),
            "amp" => Ok(Self::Amp),
            "opencode" => Ok(Self::OpenCode),
            _ => {
                let names: Vec<_> = Self::ALL.iter().map(|a| a.cli_name()).collect();
                Err(format!(
                    "unknown agent: {s}. Expected one of: {}",
                    names.join(", ")
                ))
            }
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.cli_name())
    }
}
