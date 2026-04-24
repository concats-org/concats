use std::{
    fmt,
    path::{Path, PathBuf},
    rc::Rc,
};

use concats_core::{
    Repository,
    error::{Error, Result},
};
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
    let worktree_root = find_worktree_root(payload.cwd.as_deref().map(Path::new))?;
    let repo = Rc::new(Repository::open(&worktree_root)?);

    match resolve(event)? {
        HandlerAction::SessionStarted => handler::on_session_started(repo, session_id),
        HandlerAction::PromptSubmitted => match payload.prompt.as_deref() {
            Some(prompt) => handler::on_prompt_submitted(repo, session_id, agent_name, prompt),
            None => Ok(()),
        },
        HandlerAction::FilesChanged => handler::on_files_changed(repo, session_id, agent_name),
        HandlerAction::Stop => {
            let response = payload
                .response
                .as_deref()
                .unwrap_or("(response not captured)");
            handler::on_stop(repo, session_id, agent_name, response)
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

/// Resolve the git worktree root from an optional starting directory,
/// falling back to the process's current directory.
///
/// # Errors
///
/// Returns an error if the current directory cannot be read, no enclosing
/// git repository is found, or the repository is bare.
pub fn find_worktree_root(cwd: Option<&Path>) -> Result<PathBuf> {
    let start = match cwd {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let repo = git2::Repository::discover(&start)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::session("bare repository not supported"))?;
    Ok(workdir.to_path_buf())
}

pub mod amp;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod droid;
pub mod gemini;
pub mod handler;
pub mod helpers;
pub mod json_config;
pub mod opencode;
pub mod plugin;
pub mod toml_config;
pub mod windsurf;

pub trait Agent: Sync {
    #[must_use]
    fn name(&self) -> &'static str;

    #[must_use]
    fn is_detected(&self) -> bool;

    /// Dispatch an incoming hook payload for this agent.
    ///
    /// # Errors
    ///
    /// Returns an error if the event is missing when required, the payload is
    /// invalid, or the underlying handler fails.
    fn dispatch(&self, event: Option<&str>, payload: &str) -> Result<()>;

    /// Install concats integration for this agent.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent configuration cannot be read or written.
    fn install(&self, binary: &Path, scope: &InstallScope) -> Result<()>;

    /// Remove concats integration for this agent.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent configuration cannot be read or written.
    fn uninstall(&self, scope: &InstallScope) -> Result<()>;

    #[must_use]
    fn is_installed(&self, scope: &InstallScope) -> bool;
}

impl fmt::Display for dyn Agent + '_ {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

static ALL_AGENTS: [&'static dyn Agent; 9] = [
    &claude::ClaudeAgent,
    &codex::CodexAgent,
    &cursor::CursorAgent,
    &windsurf::WindsurfAgent,
    &gemini::GeminiAgent,
    &copilot::CopilotAgent,
    &droid::DroidAgent,
    &amp::AmpAgent,
    &opencode::OpenCodeAgent,
];

#[must_use]
pub fn all_agents() -> &'static [&'static dyn Agent] {
    &ALL_AGENTS
}

#[must_use]
pub fn find_agent(name: &str) -> Option<&'static dyn Agent> {
    all_agents()
        .iter()
        .copied()
        .find(|agent| agent.name().eq_ignore_ascii_case(name))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::rc::Rc;

    use concats_core::{Repository, session};

    use super::*;

    fn init_repo_with_commit(dir: &std::path::Path) {
        let repo = git2::Repository::init(dir).unwrap();
        let mut index = repo.index().unwrap();
        std::fs::write(dir.join("init.txt"), "init").unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@test").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }

    mod find_agent {
        use super::*;

        #[test]
        fn resolves_names_case_insensitively() {
            assert_eq!(super::find_agent("ClAuDe").unwrap().name(), "claude");
            assert_eq!(super::find_agent("cOdEx").unwrap().name(), "codex");
            assert!(super::find_agent("missing").is_none());
        }

        #[test]
        fn dispatches_claude_session_start() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            super::find_agent("claude")
                .unwrap()
                .dispatch(
                    Some("SessionStart"),
                    &format!(
                        r#"{{"session_id":"session-a","cwd":"{}"}}"#,
                        dir.path().display()
                    ),
                )
                .unwrap();

            let repo = Rc::new(Repository::open(dir.path()).unwrap());
            assert!(session::open(repo, "session-a").is_ok());
        }
    }

    mod all_agents {
        #[test]
        fn preserve_current_order() {
            let names: Vec<_> = super::super::all_agents()
                .iter()
                .map(|agent| agent.name())
                .collect();
            assert_eq!(
                names,
                vec![
                    "claude", "codex", "cursor", "windsurf", "gemini", "copilot", "droid", "amp",
                    "opencode"
                ]
            );
        }
    }
}
