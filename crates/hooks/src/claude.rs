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
                let transcript = payload
                    .transcript_path
                    .as_deref()
                    .and_then(|path| std::fs::read_to_string(path).ok());
                let responses = resolve_stop_responses(
                    transcript.as_deref(),
                    payload.last_assistant_message.as_deref(),
                );
                let refs: Vec<&str> = responses.iter().map(String::as_str).collect();
                handler::on_stop(repo, &payload.session_id, "Claude", &refs)
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

// Resolve the `Response` entry texts to append on a Claude Stop event.
// When the transcript ends a plan-mode cycle and the plan file is readable,
// return `[plan]` or `[plan, post_exit_text]`. Otherwise fall back to the
// payload's `last_assistant_message`, then the last assistant message in the
// transcript, then a stable placeholder.
fn resolve_stop_responses(
    transcript: Option<&str>,
    last_assistant_message: Option<&str>,
) -> Vec<String> {
    if let Some(data) = transcript
        && let Some((plan_path, post)) = extract_plan_mode_output(data)
        && let Ok(plan) = std::fs::read_to_string(&plan_path)
    {
        let mut out = vec![plan];
        if !post.is_empty() {
            out.push(post);
        }
        return out;
    }
    let fallback = last_assistant_message
        .map(str::to_string)
        .or_else(|| transcript.and_then(extract_last_response))
        .unwrap_or_else(|| "(response not captured)".to_string());
    vec![fallback]
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

// Returns `(plan_file_path, post_exit_text)` when the transcript ends a
// plan-mode cycle. `post_exit_text` may be empty (plan rejected, or Claude
// produced no further text after `ExitPlanMode`).
fn extract_plan_mode_output(data: &str) -> Option<(String, String)> {
    let entries: Vec<serde_json::Value> = data
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    let (exit_entry_idx, exit_block_idx) =
        entries.iter().enumerate().rev().find_map(|(i, entry)| {
            find_tool_use_block_index(entry, "ExitPlanMode").map(|b| (i, b))
        })?;

    let plan_path = entries[..=exit_entry_idx]
        .iter()
        .rev()
        .find_map(write_plan_file_path)?;

    let mut post: Vec<String> = Vec::new();
    if let Some(tail) = assistant_text_after(&entries[exit_entry_idx], exit_block_idx) {
        post.push(tail);
    }
    for entry in &entries[exit_entry_idx + 1..] {
        if let Some(text) = assistant_text(entry) {
            post.push(text);
        }
    }

    Some((plan_path, post.join("\n\n")))
}

fn find_tool_use_block_index(entry: &serde_json::Value, name: &str) -> Option<usize> {
    if entry.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return None;
    }
    entry
        .get("content")?
        .as_array()?
        .iter()
        .position(|block| is_tool_use(block, name))
}

fn write_plan_file_path(entry: &serde_json::Value) -> Option<String> {
    if entry.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return None;
    }
    entry
        .get("content")?
        .as_array()?
        .iter()
        .filter(|block| is_tool_use(block, "Write"))
        .find_map(|block| {
            let path = block.get("input")?.get("file_path")?.as_str()?;
            let is_md = Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
            if path.contains("/.claude/plans/") && is_md {
                Some(path.to_string())
            } else {
                None
            }
        })
}

fn assistant_text(entry: &serde_json::Value) -> Option<String> {
    if entry.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return None;
    }
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
}

fn assistant_text_after(entry: &serde_json::Value, block_idx: usize) -> Option<String> {
    if entry.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return None;
    }
    let parts: Vec<_> = entry
        .get("content")?
        .as_array()?
        .iter()
        .enumerate()
        .skip(block_idx + 1)
        .filter_map(|(_, block)| block.get("text")?.as_str())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn is_tool_use(block: &serde_json::Value, name: &str) -> bool {
    block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
        && block.get("name").and_then(|n| n.as_str()) == Some(name)
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

    mod plan_mode {
        use super::*;

        // Build a JSONL transcript with an assistant `Write` of `plan_path`
        // followed by `ExitPlanMode`, plus any additional assistant messages
        // given in `trailing_texts`.
        fn build_jsonl(plan_path: &str, trailing_texts: &[&str]) -> String {
            let mut lines = vec![
                serde_json::json!({"role": "user", "content": "please plan"}).to_string(),
                serde_json::json!({
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "drafting"},
                        {
                            "type": "tool_use",
                            "name": "Write",
                            "input": {"file_path": plan_path, "content": "ignored"},
                        },
                        {"type": "tool_use", "name": "ExitPlanMode", "input": {}},
                    ],
                })
                .to_string(),
            ];
            for text in trailing_texts {
                lines.push(
                    serde_json::json!({
                        "role": "assistant",
                        "content": [{"type": "text", "text": *text}],
                    })
                    .to_string(),
                );
            }
            lines.join("\n")
        }

        fn dispatch_plan_stop(
            dir: &std::path::Path,
            transcript_path: &std::path::Path,
            last_assistant_message: Option<&str>,
        ) {
            let cwd = dir.to_string_lossy().to_string();
            ClaudeAgent
                .dispatch(
                    Some("UserPromptSubmit"),
                    &serde_json::json!({
                        "session_id": "session-plan",
                        "prompt": "please plan",
                        "cwd": cwd,
                    })
                    .to_string(),
                )
                .unwrap();

            let mut payload = serde_json::json!({
                "session_id": "session-plan",
                "cwd": cwd,
                "transcript_path": transcript_path.to_string_lossy(),
            });
            if let Some(msg) = last_assistant_message {
                payload["last_assistant_message"] = serde_json::Value::String(msg.to_string());
            }
            ClaudeAgent
                .dispatch(Some("Stop"), &payload.to_string())
                .unwrap();
        }

        #[test]
        fn records_plan_and_post_exit_as_two_entries() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let plans_dir = dir.path().join(".claude/plans");
            std::fs::create_dir_all(&plans_dir).unwrap();
            let plan_path = plans_dir.join("the-plan.md");
            std::fs::write(&plan_path, "# Plan\n\nStep one.\n").unwrap();

            let transcript_path = dir.path().join("transcript.jsonl");
            std::fs::write(
                &transcript_path,
                build_jsonl(
                    plan_path.to_str().unwrap(),
                    &["Implementing now.", "All done."],
                ),
            )
            .unwrap();

            dispatch_plan_stop(dir.path(), &transcript_path, None);

            let repo = std::rc::Rc::new(concats_core::Repository::open(dir.path()).unwrap());
            let session = concats_core::session::open(repo, "session-plan").unwrap();
            let turns = concats_core::turn::list(&session).unwrap();
            assert!(matches!(
                turns[0].entries(),
                [
                    TurnEntry {
                        kind: TurnEntryKind::Prompt { .. }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text: plan }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text: post }
                    }
                ] if plan == "# Plan\n\nStep one.\n" && post == "Implementing now.\n\nAll done."
            ));
        }

        #[test]
        fn plan_only_yields_single_response() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let plans_dir = dir.path().join(".claude/plans");
            std::fs::create_dir_all(&plans_dir).unwrap();
            let plan_path = plans_dir.join("the-plan.md");
            std::fs::write(&plan_path, "plan body").unwrap();

            let transcript_path = dir.path().join("transcript.jsonl");
            std::fs::write(
                &transcript_path,
                build_jsonl(plan_path.to_str().unwrap(), &[]),
            )
            .unwrap();

            dispatch_plan_stop(dir.path(), &transcript_path, None);

            let repo = std::rc::Rc::new(concats_core::Repository::open(dir.path()).unwrap());
            let session = concats_core::session::open(repo, "session-plan").unwrap();
            let turns = concats_core::turn::list(&session).unwrap();
            assert!(matches!(
                turns[0].entries(),
                [
                    TurnEntry {
                        kind: TurnEntryKind::Prompt { .. }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text }
                    }
                ] if text == "plan body"
            ));
        }

        #[test]
        fn missing_plan_file_falls_back_to_last_message() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            // Reference a plan path that will not be created on disk.
            let plans_dir = dir.path().join(".claude/plans");
            std::fs::create_dir_all(&plans_dir).unwrap();
            let plan_path = plans_dir.join("ghost.md");

            let transcript_path = dir.path().join("transcript.jsonl");
            std::fs::write(
                &transcript_path,
                build_jsonl(plan_path.to_str().unwrap(), &["done"]),
            )
            .unwrap();

            dispatch_plan_stop(dir.path(), &transcript_path, Some("fallback"));

            let repo = std::rc::Rc::new(concats_core::Repository::open(dir.path()).unwrap());
            let session = concats_core::session::open(repo, "session-plan").unwrap();
            let turns = concats_core::turn::list(&session).unwrap();
            assert!(matches!(
                turns[0].entries(),
                [
                    TurnEntry {
                        kind: TurnEntryKind::Prompt { .. }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text }
                    }
                ] if text == "fallback"
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
