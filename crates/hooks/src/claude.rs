use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

use concats_core::{
    Repository,
    error::{Error, Result},
};
use serde::Deserialize;

use crate::{InstallScope, find_worktree_root, handler, json_config};

const AGENT: &str = "claude";

pub(crate) struct ClaudeAgent;

impl crate::Agent for ClaudeAgent {
    fn name(&self) -> &'static str {
        AGENT
    }

    fn is_detected(&self) -> bool {
        dirs::home_dir().is_some_and(|home| home.join(".claude").is_dir())
    }

    fn dispatch(&self, event: Option<&str>, payload_json: &str) -> Result<()> {
        let event =
            event.ok_or_else(|| Error::session(format!("{AGENT} requires an event name")))?;
        match event {
            "SessionStart" => {
                let payload: SessionStartPayload =
                    serde_json::from_str(payload_json).map_err(|error| {
                        Error::session(format!("invalid SessionStart payload: {error}"))
                    })?;
                let worktree_root = find_worktree_root(payload.cwd.as_deref())?;
                let repo = Rc::new(Repository::open(&worktree_root)?);
                handler::on_session_started(repo, &payload.session_id)
            }
            "UserPromptSubmit" => {
                let payload: UserPromptSubmitPayload =
                    serde_json::from_str(payload_json).map_err(|error| {
                        Error::session(format!("invalid UserPromptSubmit payload: {error}"))
                    })?;
                let worktree_root = find_worktree_root(payload.cwd.as_deref())?;
                let repo = Rc::new(Repository::open(&worktree_root)?);
                handler::on_prompt_submitted(repo, &payload.session_id, "Claude", &payload.prompt)
            }
            "PostToolUse" => {
                let payload: PostToolUsePayload =
                    serde_json::from_str(payload_json).map_err(|error| {
                        Error::session(format!("invalid PostToolUse payload: {error}"))
                    })?;
                let worktree_root = find_worktree_root(payload.cwd.as_deref())?;
                let repo = Rc::new(Repository::open(&worktree_root)?);
                handler::on_files_changed(repo, &payload.session_id, "Claude")
            }
            "Stop" => {
                let payload: StopPayload = serde_json::from_str(payload_json)
                    .map_err(|error| Error::session(format!("invalid Stop payload: {error}")))?;
                let worktree_root = find_worktree_root(payload.cwd.as_deref())?;
                let repo = Rc::new(Repository::open(&worktree_root)?);
                let transcript_response = payload
                    .transcript_path
                    .as_deref()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .and_then(|data| extract_last_response(&data));
                let response = payload
                    .last_assistant_message
                    .as_deref()
                    .or(transcript_response.as_deref())
                    .unwrap_or("(response not captured)");
                handler::on_stop(repo, &payload.session_id, "Claude", response)
            }
            _ => Err(Error::session(format!(
                "unknown Claude hook event: {event}"
            ))),
        }
    }

    fn install(&self, binary: &Path, scope: &InstallScope) -> Result<()> {
        json_config::apply(&settings_path(scope)?, |value| install_hooks(value, binary))
    }

    fn uninstall(&self, scope: &InstallScope) -> Result<()> {
        let path = settings_path(scope)?;
        if !path.exists() {
            return Ok(());
        }
        json_config::apply(&path, |v| Ok(remove_hooks(v)))
    }

    fn is_installed(&self, scope: &InstallScope) -> bool {
        settings_path(scope).ok().is_some_and(|path| {
            std::fs::read_to_string(path)
                .is_ok_and(|data| data.contains(&format!("concats hook {AGENT}")))
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct SessionStartPayload {
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UserPromptSubmitPayload {
    pub session_id: String,
    pub prompt: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PostToolUsePayload {
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StopPayload {
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
}

fn settings_path(scope: &InstallScope) -> Result<PathBuf> {
    match scope {
        InstallScope::User => dirs::home_dir()
            .map(|home| home.join(".claude").join("settings.json"))
            .ok_or_else(|| Error::session("cannot determine home directory")),
        InstallScope::Project { root } => Ok(root.join(".claude").join("settings.json")),
    }
}

fn install_hooks(mut value: serde_json::Value, binary: &Path) -> Result<serde_json::Value> {
    let root = value
        .as_object_mut()
        .ok_or_else(|| Error::session("claude config root is not an object"))?;

    let command_prefix = format!("concats hook {AGENT}");
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::session("claude hooks is not an object"))?;

    for (event, matcher) in [
        ("SessionStart", ""),
        ("UserPromptSubmit", ""),
        ("PostToolUse", "Write|Edit"),
        ("Stop", ""),
    ] {
        let entries = hooks
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| Error::session(format!("claude hooks.{event} is not an array")))?;
        entries.retain(|entry| {
            !entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.contains(&command_prefix))
                    })
                })
        });
        entries.push(serde_json::json!({
            "matcher": matcher,
            "hooks": [{
                "type": "command",
                "command": format!("{} hook {AGENT} {event}", binary.display()),
            }]
        }));
    }

    Ok(value)
}

fn remove_hooks(mut value: serde_json::Value) -> serde_json::Value {
    let command_prefix = format!("concats hook {AGENT}");
    if let Some(hooks) = value.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for entries in hooks.values_mut() {
            if let Some(arr) = entries.as_array_mut() {
                arr.retain(|entry| {
                    !entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .is_some_and(|hooks| {
                            hooks.iter().any(|hook| {
                                hook.get("command")
                                    .and_then(|c| c.as_str())
                                    .is_some_and(|c| c.contains(&command_prefix))
                            })
                        })
                });
            }
        }
    }
    value
}

fn extract_last_response(data: &str) -> Option<String> {
    data.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .find_map(|entry| {
            let content = entry.get("content")?;
            if let Some(text) = content.as_str() {
                return Some(text.to_string());
            }
            let parts: Vec<_> = content
                .as_array()?
                .iter()
                .filter_map(|block| block.get("text")?.as_str())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        })
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::path::Path;

    use concats_core::turn::{TurnEntry, TurnEntryKind};

    use super::*;
    use crate::Agent;

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

    fn project_scope(dir: &std::path::Path) -> InstallScope {
        InstallScope::Project {
            root: dir.to_path_buf(),
        }
    }

    mod install {
        use super::*;

        #[test]
        fn creates_settings() {
            let dir = tempfile::tempdir().unwrap();
            let scope = project_scope(dir.path());
            ClaudeAgent.install(Path::new("concats"), &scope).unwrap();

            let settings = dir.path().join(".claude/settings.json");
            assert!(settings.exists());
            let data = std::fs::read_to_string(&settings).unwrap();
            assert!(data.contains("concats hook claude SessionStart"));
            assert!(data.contains("concats hook claude PostToolUse"));
        }

        #[test]
        fn merges_with_existing_hooks() {
            let dir = tempfile::tempdir().unwrap();
            let settings_dir = dir.path().join(".claude");
            std::fs::create_dir_all(&settings_dir).unwrap();
            std::fs::write(
                settings_dir.join("settings.json"),
                r#"{"hooks":{"PostToolUse":[{"matcher":"","hooks":[{"type":"command","command":"other-tool hook"}]}]}}"#,
            )
            .unwrap();

            let scope = project_scope(dir.path());
            ClaudeAgent.install(Path::new("concats"), &scope).unwrap();

            let data = std::fs::read_to_string(settings_dir.join("settings.json")).unwrap();
            assert!(data.contains("other-tool hook"));
            assert!(data.contains("concats hook claude PostToolUse"));
        }
    }

    mod uninstall {
        use super::*;

        #[test]
        fn preserves_non_concats_hooks() {
            let dir = tempfile::tempdir().unwrap();
            let settings_dir = dir.path().join(".claude");
            std::fs::create_dir_all(&settings_dir).unwrap();
            std::fs::write(
                settings_dir.join("settings.json"),
                r#"{"hooks":{"PostToolUse":[{"matcher":"Write|Edit","hooks":[{"type":"command","command":"concats hook claude PostToolUse"}]},{"matcher":"Write|Edit","hooks":[{"type":"command","command":"other-tool hook"}]}]}}"#,
            )
            .unwrap();

            let scope = project_scope(dir.path());
            ClaudeAgent.uninstall(&scope).unwrap();

            let data = std::fs::read_to_string(settings_dir.join("settings.json")).unwrap();
            assert!(!data.contains("concats hook claude"));
            assert!(data.contains("other-tool hook"));
        }
    }

    mod is_installed {
        use super::*;

        #[test]
        fn reflects_state() {
            let dir = tempfile::tempdir().unwrap();
            let scope = project_scope(dir.path());
            assert!(!ClaudeAgent.is_installed(&scope));
            ClaudeAgent.install(Path::new("concats"), &scope).unwrap();
            assert!(ClaudeAgent.is_installed(&scope));
            ClaudeAgent.uninstall(&scope).unwrap();
            assert!(!ClaudeAgent.is_installed(&scope));
        }
    }

    mod dispatch {
        use super::*;

        #[test]
        fn creates_turn_lifecycle() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());
            let cwd = dir.path().to_string_lossy().to_string();

            ClaudeAgent
                .dispatch(
                    Some("UserPromptSubmit"),
                    &serde_json::json!({
                        "session_id": "session-a",
                        "prompt": "hello",
                        "cwd": cwd,
                    })
                    .to_string(),
                )
                .unwrap();
            ClaudeAgent
                .dispatch(
                    Some("PostToolUse"),
                    &serde_json::json!({
                        "session_id": "session-a",
                        "cwd": cwd,
                    })
                    .to_string(),
                )
                .unwrap();
            ClaudeAgent
                .dispatch(
                    Some("Stop"),
                    &serde_json::json!({
                        "session_id": "session-a",
                        "cwd": cwd,
                        "last_assistant_message": "done",
                    })
                    .to_string(),
                )
                .unwrap();

            let repo = std::rc::Rc::new(concats_core::Repository::open(dir.path()).unwrap());
            let sessions = concats_core::session::list(&repo).unwrap();
            let turns = concats_core::turn::list(&sessions[0]).unwrap();
            assert_eq!(turns.len(), 1);
            assert!(matches!(
                turns[0].entries(),
                [
                    TurnEntry {
                        kind: TurnEntryKind::Prompt { text: prompt }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text: response }
                    }
                ] if prompt == "hello" && response == "done"
            ));
        }
    }

    mod find_worktree_root {
        use super::*;

        #[test]
        fn discovers_repo() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());
            let sub = dir.path().join("nested/dir");
            std::fs::create_dir_all(&sub).unwrap();
            let root = crate::find_worktree_root(Some(sub.to_str().unwrap())).unwrap();
            assert_eq!(
                root.canonicalize().unwrap(),
                dir.path().canonicalize().unwrap()
            );
        }
    }
}
