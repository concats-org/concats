use std::{fs, path::Path};

use concats_core::error::{Error, Result};
use serde::Deserialize;

use crate::{handler, state::find_repo_root};

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

/// Dispatch a Claude hook event to the matching checkpoint handler.
///
/// # Errors
///
/// Returns an error if the payload cannot be parsed, the repository root
/// cannot be resolved, or the underlying checkpoint handler fails.
pub fn dispatch(event: &str, payload_json: &str) -> Result<()> {
    match event {
        "SessionStart" => {
            let payload: SessionStartPayload = serde_json::from_str(payload_json)
                .map_err(|e| Error::session(format!("invalid SessionStart payload: {e}")))?;
            let repo_path = find_repo_root(payload.cwd.as_deref())?;
            handler::on_session_started(&repo_path, &payload.session_id)
        }
        "UserPromptSubmit" => {
            let payload: UserPromptSubmitPayload = serde_json::from_str(payload_json)
                .map_err(|e| Error::session(format!("invalid UserPromptSubmit payload: {e}")))?;
            let repo_path = find_repo_root(payload.cwd.as_deref())?;
            handler::on_prompt_submitted(&repo_path, &payload.session_id, &payload.prompt)
        }
        "PostToolUse" => {
            let payload: PostToolUsePayload = serde_json::from_str(payload_json)
                .map_err(|e| Error::session(format!("invalid PostToolUse payload: {e}")))?;
            let repo_path = find_repo_root(payload.cwd.as_deref())?;
            handler::on_files_changed(&repo_path, &payload.session_id)
        }
        "Stop" => {
            let payload: StopPayload = serde_json::from_str(payload_json)
                .map_err(|e| Error::session(format!("invalid Stop payload: {e}")))?;
            let repo_path = find_repo_root(payload.cwd.as_deref())?;
            let transcript_response = payload
                .transcript_path
                .as_deref()
                .and_then(|path| extract_last_response(path).ok());
            let response = payload
                .last_assistant_message
                .as_deref()
                .or(transcript_response.as_deref())
                .unwrap_or("(response not captured)");
            handler::on_stop(&repo_path, &payload.session_id, response)
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
            .map_err(|e| Error::session(format!("invalid settings.json: {e}")))?
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
        .map_err(|e| Error::session(format!("failed to serialize settings: {e}")))?;
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
    use concats_core::testutil::init_repo_with_commit;

    use super::*;

    #[test]
    fn install_creates_settings() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), "concats").unwrap();
        assert!(dir.path().join(".claude/settings.json").exists());
    }

    #[test]
    fn dispatch_creates_checkpoint_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let _ = init_repo_with_commit(dir.path());

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

        let sessions = concats_core::session::list(dir.path()).unwrap();
        let checkpoints = concats_core::checkpoint::list(&sessions[0]).unwrap();
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].transcript.len(), 2);
    }

    #[test]
    fn find_repo_root_discovers_repo() {
        let dir = tempfile::tempdir().unwrap();
        let _ = init_repo_with_commit(dir.path());
        let sub = dir.path().join("nested/dir");
        std::fs::create_dir_all(&sub).unwrap();
        let root = find_repo_root(Some(sub.to_str().unwrap())).unwrap();
        assert_eq!(
            root.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }
}
