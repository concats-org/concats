use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::checkpoint::CheckpointStore;
use crate::error::Result;

// ── Claude Code hook payloads (deserialized from stdin JSON) ─────────

#[derive(Debug, Deserialize)]
pub struct SessionStartPayload {
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// How the session was initiated: "startup", "resume", "clear", "compact".
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UserPromptSubmitPayload {
    pub session_id: String,
    pub prompt: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PostToolUsePayload {
    pub session_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StopPayload {
    pub session_id: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
}

// ── Persisted session state ─────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct HookSessionState {
    pub session_id: String,
    pub turn_count: u32,
    pub current_prompt: String,
    pub repo_path: String,
}

/// Return the directory for hook state files: `.git/concats/hooks/`.
fn state_dir(repo_path: &Path) -> PathBuf {
    repo_path.join(".git").join("concats").join("hooks")
}

/// Return the state file path for a given session.
fn state_path(repo_path: &Path, session_id: &str) -> PathBuf {
    state_dir(repo_path).join(format!("{session_id}.json"))
}

pub fn load_state(repo_path: &Path, session_id: &str) -> Result<Option<HookSessionState>> {
    let path = state_path(repo_path, session_id);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)?;
    let state: HookSessionState =
        serde_json::from_str(&data).map_err(|e| crate::error::Error::session(e.to_string()))?;
    Ok(Some(state))
}

pub fn save_state(repo_path: &Path, state: &HookSessionState) -> Result<()> {
    let dir = state_dir(repo_path);
    fs::create_dir_all(&dir)?;
    let data = serde_json::to_string_pretty(state)
        .map_err(|e| crate::error::Error::session(e.to_string()))?;
    fs::write(state_path(repo_path, &state.session_id), data)?;
    Ok(())
}

// ── Event handlers ──────────────────────────────────────────────────

/// Handle the `SessionStart` hook event.
///
/// Ensures session state exists so that subsequent `PostToolUse` and `Stop`
/// events always find an initialized session. Fires on startup, resume,
/// `/clear`, and compaction — covering every path that could reset state.
pub fn handle_session_start(payload: &SessionStartPayload) -> Result<()> {
    let repo_path = find_repo_root(payload.cwd.as_deref())?;
    if load_state(&repo_path, &payload.session_id)?.is_none() {
        init_session_state(&repo_path, &payload.session_id)?;
    }
    Ok(())
}

/// Handle the `UserPromptSubmit` hook event.
///
/// Creates or loads session state, then creates an initial checkpoint commit.
pub fn handle_user_prompt_submit(payload: &UserPromptSubmitPayload) -> Result<()> {
    let repo_path = find_repo_root(payload.cwd.as_deref())?;
    let existing = load_state(&repo_path, &payload.session_id)?;

    let turn_count = existing.as_ref().map_or(0, |s| s.turn_count);
    let store = CheckpointStore::new_with_turn_count(
        repo_path.clone(),
        payload.session_id.clone(),
        turn_count,
    );

    store.create_checkpoint(&payload.prompt)?;

    let state = HookSessionState {
        session_id: payload.session_id.clone(),
        turn_count,
        current_prompt: payload.prompt.clone(),
        repo_path: repo_path.to_string_lossy().into_owned(),
    };
    save_state(&repo_path, &state)?;

    Ok(())
}

/// Handle the `PostToolUse` hook event (Write/Edit tools).
///
/// Amends the current checkpoint to capture file changes.
/// If no session state exists yet (e.g. `UserPromptSubmit` was never fired),
/// lazily initializes the session with a checkpoint so later events work.
pub fn handle_post_tool_use(payload: &PostToolUsePayload) -> Result<()> {
    let repo_path = find_repo_root(payload.cwd.as_deref())?;
    let state = match load_state(&repo_path, &payload.session_id)? {
        Some(s) => s,
        None => {
            let state = init_session_state(&repo_path, &payload.session_id)?;
            // After init, amend captures the current workdir into the new checkpoint.
            let store = CheckpointStore::new_with_turn_count(
                repo_path,
                state.session_id.clone(),
                state.turn_count,
            );
            store.amend_checkpoint()?;
            return Ok(());
        }
    };

    let store = CheckpointStore::new_with_turn_count(
        repo_path,
        state.session_id.clone(),
        state.turn_count,
    );
    store.amend_checkpoint()?;

    Ok(())
}

/// Handle the `Stop` hook event.
///
/// Finalizes the checkpoint with the prompt, response summary, and stop reason,
/// then increments the turn count.
/// If no session state exists yet, lazily initializes the session so we still
/// record the stop event.
pub fn handle_stop(payload: &StopPayload) -> Result<()> {
    let repo_path = find_repo_root(payload.cwd.as_deref())?;
    let state = match load_state(&repo_path, &payload.session_id)? {
        Some(s) => s,
        None => init_session_state(&repo_path, &payload.session_id)?,
    };

    let mut store = CheckpointStore::new_with_turn_count(
        PathBuf::from(&state.repo_path),
        state.session_id.clone(),
        state.turn_count,
    );

    let transcript_response = payload
        .transcript_path
        .as_deref()
        .and_then(|p| extract_last_response(p).ok());

    let response_summary = payload
        .last_assistant_message
        .as_deref()
        .or(transcript_response.as_deref())
        .unwrap_or("(response not captured)");

    let stop_reason = payload.stop_reason.as_deref().unwrap_or("end_turn");

    store.finalize_checkpoint(&state.current_prompt, response_summary, stop_reason)?;

    let updated = HookSessionState {
        turn_count: state.turn_count + 1,
        current_prompt: String::new(),
        ..state
    };
    save_state(&PathBuf::from(&updated.repo_path), &updated)?;

    Ok(())
}

/// Lazily initialize session state and create an initial checkpoint.
///
/// Called when `PostToolUse` or `Stop` fires but no prior `UserPromptSubmit`
/// created the state (e.g. after `/clear` or when the session was started
/// externally).
fn init_session_state(repo_path: &Path, session_id: &str) -> Result<HookSessionState> {
    let store = CheckpointStore::new_with_turn_count(
        repo_path.to_path_buf(),
        session_id.to_string(),
        0,
    );
    store.create_checkpoint("(session joined mid-flight)")?;

    let state = HookSessionState {
        session_id: session_id.to_string(),
        turn_count: 0,
        current_prompt: String::new(),
        repo_path: repo_path.to_string_lossy().into_owned(),
    };
    save_state(repo_path, &state)?;
    Ok(state)
}

// ── Transcript parsing ──────────────────────────────────────────────

/// Best-effort extraction of the last assistant response from the transcript.
///
/// Returns the owned string so it can be used in the fallback chain.
fn extract_last_response(transcript_path: &str) -> Result<String> {
    let data = fs::read_to_string(transcript_path)?;
    // Walk lines in reverse to find the last assistant message.
    for line in data.lines().rev() {
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
            if entry.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                if let Some(content) = entry.get("content") {
                    // Content may be a string or array of blocks.
                    if let Some(text) = content.as_str() {
                        return Ok(text.to_string());
                    }
                    if let Some(arr) = content.as_array() {
                        let text: String = arr
                            .iter()
                            .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !text.is_empty() {
                            return Ok(text);
                        }
                    }
                }
            }
        }
    }
    Err(crate::error::Error::session(
        "no assistant message found in transcript",
    ))
}

// ── Hook installation ───────────────────────────────────────────────

/// Generate or merge hook configuration into `.claude/settings.json`.
///
/// `project_root` is the project directory (where `.claude/` lives).
/// `binary_name` is the name of the concats binary (e.g. `"concats"`).
pub fn install_hooks(project_root: &Path, binary_name: &str) -> Result<()> {
    let settings_dir = project_root.join(".claude");
    fs::create_dir_all(&settings_dir)?;

    let settings_path = settings_dir.join("settings.json");

    let mut settings: serde_json::Value = if settings_path.exists() {
        let data = fs::read_to_string(&settings_path)?;
        serde_json::from_str(&data)
            .map_err(|e| crate::error::Error::session(format!("invalid settings.json: {e}")))?
    } else {
        serde_json::json!({})
    };

    let hooks = settings
        .as_object_mut()
        .ok_or_else(|| crate::error::Error::session("settings.json root is not an object"))?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| crate::error::Error::session("hooks is not an object"))?;

    // SessionStart (fires on startup, resume, /clear, compact)
    hooks_obj.insert(
        "SessionStart".into(),
        serde_json::json!([
            {
                "matcher": "",
                "hooks": [
                    {
                        "type": "command",
                        "command": format!("{binary_name} hook SessionStart")
                    }
                ]
            }
        ]),
    );

    // UserPromptSubmit
    hooks_obj.insert(
        "UserPromptSubmit".into(),
        serde_json::json!([
            {
                "matcher": "",
                "hooks": [
                    {
                        "type": "command",
                        "command": format!("{binary_name} hook UserPromptSubmit")
                    }
                ]
            }
        ]),
    );

    // PostToolUse (Write|Edit)
    hooks_obj.insert(
        "PostToolUse".into(),
        serde_json::json!([
            {
                "matcher": "Write|Edit",
                "hooks": [
                    {
                        "type": "command",
                        "command": format!("{binary_name} hook PostToolUse")
                    }
                ]
            }
        ]),
    );

    // Stop
    hooks_obj.insert(
        "Stop".into(),
        serde_json::json!([
            {
                "matcher": "",
                "hooks": [
                    {
                        "type": "command",
                        "command": format!("{binary_name} hook Stop")
                    }
                ]
            }
        ]),
    );

    let output = serde_json::to_string_pretty(&settings)
        .map_err(|e| crate::error::Error::session(e.to_string()))?;
    fs::write(&settings_path, output)?;

    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Discover the git repository root from the given working directory
/// (or the current directory if `None`).
pub fn find_repo_root(cwd: Option<&str>) -> Result<PathBuf> {
    let start = match cwd {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };
    let repo = git2::Repository::discover(&start)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| crate::error::Error::session("bare repository not supported"))?;
    Ok(workdir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temp git repo with an initial commit.
    fn init_repo_with_commit(dir: &Path) -> git2::Repository {
        let repo = git2::Repository::init(dir).unwrap();
        {
            let mut index = repo.index().unwrap();
            fs::write(dir.join("init.txt"), "init").unwrap();
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
        repo
    }

    #[test]
    fn session_start_initializes_state() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let repo_path = dir.path().to_path_buf();
        let session_id = "session-start-test";

        // SessionStart should create state even without a prompt.
        handle_session_start(&SessionStartPayload {
            session_id: session_id.into(),
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            transcript_path: None,
            source: Some("startup".into()),
        })
        .unwrap();

        // State file should exist.
        let state = load_state(&repo_path, session_id).unwrap().unwrap();
        assert_eq!(state.turn_count, 0);

        // Session ref should exist.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let ref_name = format!("refs/agent/sessions/{session_id}");
        assert!(repo.find_reference(&ref_name).is_ok());
    }

    #[test]
    fn session_start_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let repo_path = dir.path().to_path_buf();
        let session_id = "idempotent-test";
        let cwd = Some(dir.path().to_string_lossy().into_owned());

        // Run a full turn first.
        handle_session_start(&SessionStartPayload {
            session_id: session_id.into(),
            cwd: cwd.clone(),
            transcript_path: None,
            source: Some("startup".into()),
        })
        .unwrap();
        handle_user_prompt_submit(&UserPromptSubmitPayload {
            session_id: session_id.into(),
            prompt: "do something".into(),
            cwd: cwd.clone(),
            transcript_path: None,
        })
        .unwrap();
        handle_stop(&StopPayload {
            session_id: session_id.into(),
            stop_reason: Some("end_turn".into()),
            transcript_path: None,
            cwd: cwd.clone(),
            last_assistant_message: Some("done".into()),
        })
        .unwrap();

        // SessionStart again (e.g. after /clear) should NOT reset turn_count.
        handle_session_start(&SessionStartPayload {
            session_id: session_id.into(),
            cwd: cwd.clone(),
            transcript_path: None,
            source: Some("clear".into()),
        })
        .unwrap();

        let state = load_state(&repo_path, session_id).unwrap().unwrap();
        assert_eq!(state.turn_count, 1, "turn_count should be preserved");
    }

    #[test]
    fn full_hook_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let repo_path = dir.path().to_path_buf();
        let session_id = "hook-test-session";

        // 1. UserPromptSubmit
        let submit = UserPromptSubmitPayload {
            session_id: session_id.into(),
            prompt: "fix the bug".into(),
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            transcript_path: None,
        };
        handle_user_prompt_submit(&submit).unwrap();

        // State file should exist.
        let state = load_state(&repo_path, session_id).unwrap().unwrap();
        assert_eq!(state.turn_count, 0);
        assert_eq!(state.current_prompt, "fix the bug");

        // Session ref should exist.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let ref_name = format!("refs/agent/sessions/{session_id}");
        assert!(repo.find_reference(&ref_name).is_ok());

        // 2. PostToolUse (simulate file edit)
        fs::write(dir.path().join("fixed.txt"), "fixed content").unwrap();
        let tool_use = PostToolUsePayload {
            session_id: session_id.into(),
            tool_name: "Write".into(),
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            transcript_path: None,
        };
        handle_post_tool_use(&tool_use).unwrap();

        // 3. Stop
        let stop = StopPayload {
            session_id: session_id.into(),
            stop_reason: Some("end_turn".into()),
            transcript_path: None,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            last_assistant_message: Some("I fixed the bug by editing fixed.txt".into()),
        };
        handle_stop(&stop).unwrap();

        // Verify finalized commit.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let tip = repo
            .find_reference(&ref_name)
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let msg = tip.message().unwrap();
        assert!(msg.contains("Agent-Session:"));
        assert!(msg.contains("Agent-Turn: 0"));
        assert!(msg.contains("Agent-Stop-Reason: end_turn"));
        assert!(msg.contains("I fixed the bug"));

        // State should have incremented turn count.
        let state = load_state(&repo_path, session_id).unwrap().unwrap();
        assert_eq!(state.turn_count, 1);
    }

    #[test]
    fn multi_turn_hook_session() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let repo_path = dir.path().to_path_buf();
        let session_id = "multi-turn-hook";
        let cwd = Some(dir.path().to_string_lossy().into_owned());

        // Turn 0
        handle_user_prompt_submit(&UserPromptSubmitPayload {
            session_id: session_id.into(),
            prompt: "turn zero".into(),
            cwd: cwd.clone(),
            transcript_path: None,
        })
        .unwrap();
        handle_stop(&StopPayload {
            session_id: session_id.into(),
            stop_reason: Some("end_turn".into()),
            transcript_path: None,
            cwd: cwd.clone(),
            last_assistant_message: Some("done zero".into()),
        })
        .unwrap();

        // Turn 1
        handle_user_prompt_submit(&UserPromptSubmitPayload {
            session_id: session_id.into(),
            prompt: "turn one".into(),
            cwd: cwd.clone(),
            transcript_path: None,
        })
        .unwrap();
        handle_stop(&StopPayload {
            session_id: session_id.into(),
            stop_reason: Some("end_turn".into()),
            transcript_path: None,
            cwd: cwd.clone(),
            last_assistant_message: Some("done one".into()),
        })
        .unwrap();

        // Verify: tip should be turn 1, parent should be turn 0.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let ref_name = format!("refs/agent/sessions/{session_id}");
        let tip = repo
            .find_reference(&ref_name)
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert!(tip.message().unwrap().contains("Agent-Turn: 1"));

        let parent = tip.parent(0).unwrap();
        assert!(parent.message().unwrap().contains("Agent-Turn: 0"));

        // State should show turn_count = 2.
        let state = load_state(&repo_path, session_id).unwrap().unwrap();
        assert_eq!(state.turn_count, 2);
    }

    #[test]
    fn install_hooks_creates_settings() {
        let dir = tempfile::tempdir().unwrap();

        install_hooks(dir.path(), "concats").unwrap();

        let settings_path = dir.path().join(".claude").join("settings.json");
        assert!(settings_path.exists());

        let data = fs::read_to_string(&settings_path).unwrap();
        let settings: serde_json::Value = serde_json::from_str(&data).unwrap();

        assert!(settings["hooks"]["SessionStart"].is_array());
        assert!(settings["hooks"]["UserPromptSubmit"].is_array());
        assert!(settings["hooks"]["PostToolUse"].is_array());
        assert!(settings["hooks"]["Stop"].is_array());

        let cmd = settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.contains("concats hook UserPromptSubmit"));
    }

    #[test]
    fn install_hooks_preserves_existing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let settings_dir = dir.path().join(".claude");
        fs::create_dir_all(&settings_dir).unwrap();
        fs::write(
            settings_dir.join("settings.json"),
            r#"{"permissions":{"allow":["Read"]}}"#,
        )
        .unwrap();

        install_hooks(dir.path(), "concats").unwrap();

        let data = fs::read_to_string(settings_dir.join("settings.json")).unwrap();
        let settings: serde_json::Value = serde_json::from_str(&data).unwrap();

        // Original key preserved.
        assert!(settings["permissions"]["allow"].is_array());
        // Hooks added.
        assert!(settings["hooks"]["UserPromptSubmit"].is_array());
    }

    #[test]
    fn session_history_integration() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let session_id = "history-test";
        let cwd = Some(dir.path().to_string_lossy().into_owned());

        // Run two turns via hooks.
        handle_user_prompt_submit(&UserPromptSubmitPayload {
            session_id: session_id.into(),
            prompt: "first prompt".into(),
            cwd: cwd.clone(),
            transcript_path: None,
        })
        .unwrap();
        handle_stop(&StopPayload {
            session_id: session_id.into(),
            stop_reason: Some("end_turn".into()),
            transcript_path: None,
            cwd: cwd.clone(),
            last_assistant_message: Some("first response".into()),
        })
        .unwrap();

        handle_user_prompt_submit(&UserPromptSubmitPayload {
            session_id: session_id.into(),
            prompt: "second prompt".into(),
            cwd: cwd.clone(),
            transcript_path: None,
        })
        .unwrap();
        handle_stop(&StopPayload {
            session_id: session_id.into(),
            stop_reason: Some("end_turn".into()),
            transcript_path: None,
            cwd: cwd.clone(),
            last_assistant_message: Some("second response".into()),
        })
        .unwrap();

        // Use session_history to load turns — should work without changes.
        let turns =
            crate::session_history::load_session_turns(dir.path(), session_id).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].turn_number, 0);
        assert_eq!(turns[0].prompt, "first prompt");
        assert_eq!(turns[0].response_summary, "first response");
        assert_eq!(turns[1].turn_number, 1);
        assert_eq!(turns[1].prompt, "second prompt");
    }

    #[test]
    fn stop_without_prior_submit_initializes_session() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let repo_path = dir.path().to_path_buf();
        let session_id = "orphan-stop";

        // Stop fires without any prior UserPromptSubmit — should not error.
        handle_stop(&StopPayload {
            session_id: session_id.into(),
            stop_reason: Some("end_turn".into()),
            transcript_path: None,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            last_assistant_message: Some("response without prompt".into()),
        })
        .unwrap();

        // State file should have been created with turn_count incremented to 1.
        let state = load_state(&repo_path, session_id).unwrap().unwrap();
        assert_eq!(state.turn_count, 1);

        // Session ref should exist.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let ref_name = format!("refs/agent/sessions/{session_id}");
        assert!(repo.find_reference(&ref_name).is_ok());
    }

    #[test]
    fn post_tool_use_without_prior_submit_initializes_session() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let repo_path = dir.path().to_path_buf();
        let session_id = "orphan-tool-use";

        // Write a file, then fire PostToolUse without prior UserPromptSubmit.
        fs::write(dir.path().join("new.txt"), "content").unwrap();
        handle_post_tool_use(&PostToolUsePayload {
            session_id: session_id.into(),
            tool_name: "Write".into(),
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            transcript_path: None,
        })
        .unwrap();

        // State file should have been created.
        let state = load_state(&repo_path, session_id).unwrap().unwrap();
        assert_eq!(state.turn_count, 0);

        // Session ref should exist.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let ref_name = format!("refs/agent/sessions/{session_id}");
        assert!(repo.find_reference(&ref_name).is_ok());
    }

    #[test]
    fn find_repo_root_discovers_repo() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();

        // A subdirectory should still find the root.
        let sub = dir.path().join("sub").join("dir");
        fs::create_dir_all(&sub).unwrap();

        let root = find_repo_root(Some(sub.to_str().unwrap())).unwrap();
        assert_eq!(root.canonicalize().unwrap(), dir.path().canonicalize().unwrap());
    }
}
