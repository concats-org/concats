use std::{fs, path::Path};

use concats_core::error::{Error, Result};
use serde::Deserialize;

use crate::{handler, state::find_worktree_root};

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

/// Dispatch a Claude hook event to the matching turn handler.
///
/// # Errors
///
/// Returns an error if the payload cannot be parsed, the worktree root cannot
/// be resolved, or the underlying turn handler fails.
pub fn dispatch(event: &str, payload_json: &str) -> Result<()> {
    match event {
        "SessionStart" => {
            let payload: SessionStartPayload =
                serde_json::from_str(payload_json).map_err(|error| {
                    Error::session(format!("invalid SessionStart payload: {error}"))
                })?;
            let worktree_root = find_worktree_root(payload.cwd.as_deref())?;
            handler::on_session_started(&worktree_root, &payload.session_id)
        }
        "UserPromptSubmit" => {
            let payload: UserPromptSubmitPayload =
                serde_json::from_str(payload_json).map_err(|error| {
                    Error::session(format!("invalid UserPromptSubmit payload: {error}"))
                })?;
            let worktree_root = find_worktree_root(payload.cwd.as_deref())?;
            handler::on_prompt_submitted(&worktree_root, &payload.session_id, &payload.prompt)
        }
        "PostToolUse" => {
            let payload: PostToolUsePayload = serde_json::from_str(payload_json)
                .map_err(|error| Error::session(format!("invalid PostToolUse payload: {error}")))?;
            let worktree_root = find_worktree_root(payload.cwd.as_deref())?;
            handler::on_files_changed(&worktree_root, &payload.session_id)
        }
        "Stop" => {
            let payload: StopPayload = serde_json::from_str(payload_json)
                .map_err(|error| Error::session(format!("invalid Stop payload: {error}")))?;
            let worktree_root = find_worktree_root(payload.cwd.as_deref())?;
            let transcript_response = payload
                .transcript_path
                .as_deref()
                .and_then(|path| extract_last_response(path).ok());
            let response = payload
                .last_assistant_message
                .as_deref()
                .or(transcript_response.as_deref())
                .unwrap_or("(response not captured)");
            handler::on_stop(&worktree_root, &payload.session_id, response)
        }
        _ => Err(Error::session(format!(
            "unknown Claude hook event: {event}"
        ))),
    }
}

#[allow(clippy::disallowed_methods)]
/// Install Claude hook commands into `.claude/settings.json`.
///
/// # Errors
///
/// Returns an error if the settings directory cannot be created, the existing
/// settings file cannot be read or parsed, or the updated settings cannot be
/// serialized and written.
pub fn install(project_root: &Path, binary_name: &str) -> Result<()> {
    let settings_dir = project_root.join(".claude");
    fs::create_dir_all(&settings_dir)?;

    let settings_path = settings_dir.join("settings.json");
    let mut settings = if settings_path.exists() {
        let data = fs::read_to_string(&settings_path)?;
        serde_json::from_str::<serde_json::Value>(&data)
            .map_err(|error| Error::session(format!("invalid settings.json: {error}")))?
    } else {
        serde_json::json!({})
    };

    let hooks = settings
        .as_object_mut()
        .ok_or_else(|| Error::session("settings.json root is not an object"))?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| Error::session("hooks is not an object"))?;

    for (event, matcher) in [
        ("SessionStart", ""),
        ("UserPromptSubmit", ""),
        ("PostToolUse", "Write|Edit"),
        ("Stop", ""),
    ] {
        hooks_obj.insert(
            event.to_string(),
            serde_json::json!([{
                "matcher": matcher,
                "hooks": [{
                    "type": "command",
                    "command": format!("{binary_name} hook {event}")
                }]
            }]),
        );
    }

    let output = serde_json::to_string_pretty(&settings)
        .map_err(|error| Error::session(format!("failed to serialize settings: {error}")))?;
    fs::write(settings_path, output)?;
    Ok(())
}

fn extract_last_response(transcript_path: &str) -> Result<String> {
    let data = fs::read_to_string(transcript_path)?;
    for line in data.lines().rev() {
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line)
            && entry.get("role").and_then(|role| role.as_str()) == Some("assistant")
            && let Some(content) = entry.get("content")
        {
            if let Some(text) = content.as_str() {
                return Ok(text.to_string());
            }
            if let Some(array) = content.as_array() {
                let text = array
                    .iter()
                    .filter_map(|block| block.get("text").and_then(|text| text.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    return Ok(text);
                }
            }
        }
    }
    Err(Error::session("no assistant message found in transcript"))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use concats_core::turn::{TurnEntry, TurnEntryKind};

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

    #[test]
    fn install_creates_settings() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), "concats").unwrap();
        assert!(dir.path().join(".claude/settings.json").exists());
    }

    #[test]
    fn dispatch_creates_turn_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        dispatch(
            "UserPromptSubmit",
            &serde_json::json!({
                "session_id": "session-a",
                "prompt": "hello",
                "cwd": dir.path().to_string_lossy().to_string(),
            })
            .to_string(),
        )
        .unwrap();
        dispatch(
            "PostToolUse",
            &serde_json::json!({
                "session_id": "session-a",
                "cwd": dir.path().to_string_lossy().to_string(),
            })
            .to_string(),
        )
        .unwrap();
        dispatch(
            "Stop",
            &serde_json::json!({
                "session_id": "session-a",
                "cwd": dir.path().to_string_lossy().to_string(),
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

    #[test]
    fn find_worktree_root_discovers_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let sub = dir.path().join("nested/dir");
        std::fs::create_dir_all(&sub).unwrap();
        let root = find_worktree_root(Some(sub.to_str().unwrap())).unwrap();
        assert_eq!(
            root.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }
}
