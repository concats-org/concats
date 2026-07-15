use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use concats_core::{
    error::{Error, Result},
    turn::TurnEntry,
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
                let worktree_root = find_worktree_root(payload.cwd.as_deref().map(Path::new))?;
                let repo = Rc::new(gix::open(&worktree_root).map_err(Error::git)?);
                handler::on_session_started(repo, &payload.session_id)
            }
            "UserPromptSubmit" => {
                let payload: UserPromptSubmitPayload =
                    serde_json::from_str(payload_json).map_err(|error| {
                        Error::session(format!("invalid UserPromptSubmit payload: {error}"))
                    })?;
                let worktree_root = find_worktree_root(payload.cwd.as_deref().map(Path::new))?;
                let repo = Rc::new(gix::open(&worktree_root).map_err(Error::git)?);
                handler::on_prompt_submitted(repo, &payload.session_id, "Claude", &payload.prompt)
            }
            "PostToolUse" => {
                let payload: PostToolUsePayload =
                    serde_json::from_str(payload_json).map_err(|error| {
                        Error::session(format!("invalid PostToolUse payload: {error}"))
                    })?;
                let worktree_root = find_worktree_root(payload.cwd.as_deref().map(Path::new))?;
                let repo = Rc::new(gix::open(&worktree_root).map_err(Error::git)?);
                handler::on_files_changed(repo, &payload.session_id, "Claude")
            }
            "Stop" => {
                let payload: StopPayload = serde_json::from_str(payload_json)
                    .map_err(|error| Error::session(format!("invalid Stop payload: {error}")))?;
                let worktree_root = find_worktree_root(payload.cwd.as_deref().map(Path::new))?;
                let repo = Rc::new(gix::open(&worktree_root).map_err(Error::git)?);
                let transcript = payload
                    .transcript_path
                    .as_deref()
                    .and_then(|path| std::fs::read_to_string(path).ok());
                let entries = resolve_stop_entries(
                    transcript.as_deref(),
                    payload.last_assistant_message.as_deref(),
                );
                handler::on_stop(repo, &payload.session_id, "Claude", &entries)
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

const APPROVED_PLAN_MARKER: &str = "\n## Approved Plan:";

// Resolve the transcript entries to append on a Claude Stop event.
// Plan-mode cycles can include user feedback delivered as tool results, so
// Claude may append both `Prompt` and `Response` entries for a single Stop.
fn resolve_stop_entries(
    transcript: Option<&str>,
    last_assistant_message: Option<&str>,
) -> Vec<TurnEntry> {
    if let Some(data) = transcript
        && let Some(entries) = extract_plan_mode_entries(data)
    {
        return entries;
    }
    let fallback = last_assistant_message
        .map(str::to_string)
        .or_else(|| transcript.and_then(extract_last_response))
        .unwrap_or_else(|| "(response not captured)".to_string());
    vec![TurnEntry::response_now(fallback)]
}

fn extract_last_response(data: &str) -> Option<String> {
    data.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|entry| assistant_text(&entry))
}

fn extract_plan_mode_entries(data: &str) -> Option<Vec<TurnEntry>> {
    let entries: Vec<serde_json::Value> = data
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    let start = current_prompt_start(&entries);
    let turn_entries = &entries[start..];
    let tool_names = tool_use_names(turn_entries);
    let mut out = Vec::new();
    let mut last_plan_path = None;
    let mut seen_plan_mode = false;

    for entry in turn_entries {
        if entry_role(entry) == Some("assistant") {
            collect_assistant_plan_entries(
                entry,
                &mut last_plan_path,
                &mut seen_plan_mode,
                &mut out,
            );
        } else if let Some(feedback) = plan_feedback_text(entry, &tool_names) {
            out.push(TurnEntry::prompt_now(feedback));
        }
    }

    if seen_plan_mode { Some(out) } else { None }
}

fn collect_assistant_plan_entries(
    entry: &serde_json::Value,
    last_plan_path: &mut Option<String>,
    seen_plan_mode: &mut bool,
    out: &mut Vec<TurnEntry>,
) {
    let Some(blocks) = content_blocks(entry) else {
        if *seen_plan_mode && let Some(text) = assistant_text(entry) {
            out.push(TurnEntry::response_now(text));
        }
        return;
    };

    let mut text = Vec::new();
    for block in blocks {
        collect_text_after_plan(block, *seen_plan_mode, &mut text);
        update_plan_write_path(block, last_plan_path);
        if let Some(plan) = plan_from_exit_block(block, last_plan_path.as_deref()) {
            push_pending_text(&mut text, out);
            out.push(TurnEntry::response_now(plan));
            *seen_plan_mode = true;
        }
    }
    push_pending_text(&mut text, out);
}

fn collect_text_after_plan<'a>(
    block: &'a serde_json::Value,
    seen_plan_mode: bool,
    text: &mut Vec<&'a str>,
) {
    if seen_plan_mode && let Some(part) = block.get("text").and_then(|text| text.as_str()) {
        text.push(part);
    }
}

fn update_plan_write_path(block: &serde_json::Value, last_plan_path: &mut Option<String>) {
    if is_tool_use(block, "Write")
        && let Some(path) = plan_write_file_path(block)
    {
        *last_plan_path = Some(path);
    }
}

fn plan_from_exit_block(block: &serde_json::Value, last_plan_path: Option<&str>) -> Option<String> {
    if !is_tool_use(block, "ExitPlanMode") {
        return None;
    }
    exit_plan_text(block)
        .or_else(|| exit_plan_file_path(block).and_then(|path| read_plan_file(&path)))
        .or_else(|| last_plan_path.and_then(read_plan_file))
}

fn push_pending_text(text: &mut Vec<&str>, out: &mut Vec<TurnEntry>) {
    if !text.is_empty() {
        out.push(TurnEntry::response_now(text.join("\n")));
        text.clear();
    }
}

fn current_prompt_start(entries: &[serde_json::Value]) -> usize {
    entries
        .iter()
        .rposition(is_submitted_user_prompt)
        .map_or(0, |index| index + 1)
}

fn is_submitted_user_prompt(entry: &serde_json::Value) -> bool {
    if entry_role(entry) != Some("user") {
        return false;
    }
    match entry_content(entry) {
        Some(serde_json::Value::String(_)) => true,
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .any(|block| block.get("type").and_then(|t| t.as_str()) == Some("text")),
        _ => false,
    }
}

fn tool_use_names(entries: &[serde_json::Value]) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for entry in entries {
        if entry_role(entry) != Some("assistant") {
            continue;
        }
        for block in content_blocks(entry).unwrap_or_default() {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                && let (Some(id), Some(name)) = (
                    block.get("id").and_then(|id| id.as_str()),
                    block.get("name").and_then(|name| name.as_str()),
                )
            {
                names.insert(id.to_string(), name.to_string());
            }
        }
    }
    names
}

fn plan_feedback_text(
    entry: &serde_json::Value,
    tool_names: &HashMap<String, String>,
) -> Option<String> {
    if entry_role(entry) != Some("user") {
        return None;
    }
    for block in content_blocks(entry)? {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
            continue;
        }
        let tool_use_id = block.get("tool_use_id").and_then(|id| id.as_str())?;
        let tool_name = tool_names.get(tool_use_id)?;
        if tool_name != "AskUserQuestion" && tool_name != "ExitPlanMode" {
            continue;
        }
        let text = tool_result_text(block)?;
        let text = text
            .split_once(APPROVED_PLAN_MARKER)
            .map_or(text.as_str(), |(head, _)| head)
            .trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    None
}

fn tool_result_text(block: &serde_json::Value) -> Option<String> {
    let content = block.get("content")?;
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

fn exit_plan_text(block: &serde_json::Value) -> Option<String> {
    block
        .get("input")?
        .get("plan")?
        .as_str()
        .map(str::to_string)
}

fn exit_plan_file_path(block: &serde_json::Value) -> Option<String> {
    block
        .get("input")?
        .get("planFilePath")?
        .as_str()
        .and_then(plan_file_path)
}

fn read_plan_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn plan_write_file_path(block: &serde_json::Value) -> Option<String> {
    let path = block.get("input")?.get("file_path")?.as_str()?;
    plan_file_path(path)
}

fn plan_file_path(path: &str) -> Option<String> {
    let is_md = Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    if path.contains("/.claude/plans/") && is_md {
        Some(path.to_string())
    } else {
        None
    }
}

fn assistant_text(entry: &serde_json::Value) -> Option<String> {
    if entry_role(entry) != Some("assistant") {
        return None;
    }
    let content = entry_content(entry)?;
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

fn is_tool_use(block: &serde_json::Value, name: &str) -> bool {
    block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
        && block.get("name").and_then(|n| n.as_str()) == Some(name)
}

fn content_blocks(entry: &serde_json::Value) -> Option<&[serde_json::Value]> {
    entry_content(entry)?.as_array().map(Vec::as_slice)
}

fn entry_content(entry: &serde_json::Value) -> Option<&serde_json::Value> {
    entry
        .get("content")
        .or_else(|| entry.get("message")?.get("content"))
}

fn entry_role(entry: &serde_json::Value) -> Option<&str> {
    entry
        .get("role")
        .and_then(|role| role.as_str())
        .or_else(|| {
            entry
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(|role| role.as_str())
        })
        .or_else(|| {
            let ty = entry.get("type").and_then(|ty| ty.as_str());
            if matches!(ty, Some("assistant" | "user")) {
                ty
            } else {
                None
            }
        })
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::needless_pass_by_value)]
mod tests {
    use std::path::Path;

    use concats_core::turn::{TurnEntry, TurnEntryKind};

    use super::*;
    use crate::Agent;

    fn init_repo_with_commit(dir: &std::path::Path) {
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .envs([
                    // Hermetic: the user's git config (signing, hooks) must
                    // not leak into fixtures.
                    ("GIT_CONFIG_GLOBAL", "/dev/null"),
                    ("GIT_CONFIG_SYSTEM", "/dev/null"),
                    ("GIT_AUTHOR_NAME", "test"),
                    ("GIT_AUTHOR_EMAIL", "test@test"),
                    ("GIT_COMMITTER_NAME", "test"),
                    ("GIT_COMMITTER_EMAIL", "test@test"),
                ])
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("init.txt"), "init").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "initial"]);
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

            let repo = std::rc::Rc::new(gix::open(dir.path()).unwrap());
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

        fn jsonl(entries: impl IntoIterator<Item = serde_json::Value>) -> String {
            entries
                .into_iter()
                .map(|entry| entry.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        }

        fn real_user_text(text: &str) -> serde_json::Value {
            serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": text},
            })
        }

        fn real_assistant_text(text: &str) -> serde_json::Value {
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "content": text},
            })
        }

        fn real_tool_use(id: &str, name: &str, input: serde_json::Value) -> serde_json::Value {
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input,
                    }],
                },
            })
        }

        fn real_tool_result(tool_use_id: &str, content: &str) -> serde_json::Value {
            serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                    }],
                },
            })
        }

        #[test]
        fn records_plan_and_post_exit_responses() {
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

            let repo = std::rc::Rc::new(gix::open(dir.path()).unwrap());
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
                        kind: TurnEntryKind::Response { text: first }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text: second }
                    }
                ] if plan == "# Plan\n\nStep one.\n" && first == "Implementing now." && second == "All done."
            ));
        }

        #[test]
        fn records_real_transcript_plan_feedback_in_order() {
            let dir = tempfile::tempdir().unwrap();
            let plans_dir = dir.path().join(".claude/plans");
            std::fs::create_dir_all(&plans_dir).unwrap();
            let plan_path = plans_dir.join("plan.md");
            let plan_path = plan_path.to_str().unwrap();
            let jsonl = jsonl([
                real_user_text("please plan"),
                real_tool_use(
                    "ask-1",
                    "AskUserQuestion",
                    serde_json::json!({"question": "Which package split?"}),
                ),
                real_tool_result("ask-1", "User answered: keep provider packages separate"),
                real_tool_use(
                    "write-1",
                    "Write",
                    serde_json::json!({"file_path": plan_path, "content": "draft one"}),
                ),
                real_tool_use(
                    "exit-1",
                    "ExitPlanMode",
                    serde_json::json!({"plan": "plan one", "planFilePath": plan_path}),
                ),
                real_tool_result(
                    "exit-1",
                    "User rejected the plan: add the verification commands",
                ),
                real_tool_use(
                    "write-2",
                    "Write",
                    serde_json::json!({"file_path": plan_path, "content": "draft two"}),
                ),
                real_tool_use(
                    "exit-2",
                    "ExitPlanMode",
                    serde_json::json!({"plan": "plan two", "planFilePath": plan_path}),
                ),
                real_tool_result(
                    "exit-2",
                    "User has approved your plan.\n## Approved Plan:\nplan two",
                ),
                real_assistant_text("implementation done"),
            ]);

            let entries = resolve_stop_entries(Some(&jsonl), None);
            assert!(matches!(
                entries.as_slice(),
                [
                    TurnEntry {
                        kind: TurnEntryKind::Prompt { text: answer }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text: first_plan }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Prompt { text: rejection }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text: second_plan }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Prompt { text: approval }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text: done }
                    }
                ] if answer == "User answered: keep provider packages separate"
                    && first_plan == "plan one"
                    && rejection == "User rejected the plan: add the verification commands"
                    && second_plan == "plan two"
                    && approval == "User has approved your plan."
                    && done == "implementation done"
            ));
        }

        #[test]
        fn later_non_plan_stop_ignores_prior_plan() {
            let jsonl = jsonl([
                real_user_text("please plan"),
                real_tool_use(
                    "exit-old",
                    "ExitPlanMode",
                    serde_json::json!({"plan": "stale plan"}),
                ),
                real_user_text("now answer normally"),
                real_assistant_text("current response"),
            ]);

            let entries = resolve_stop_entries(Some(&jsonl), Some("fallback response"));
            assert!(matches!(
                entries.as_slice(),
                [TurnEntry {
                    kind: TurnEntryKind::Response { text }
                }] if text == "fallback response"
            ));
        }

        #[test]
        fn ask_user_question_without_plan_falls_back() {
            let jsonl = jsonl([
                real_user_text("ask then answer"),
                real_tool_use(
                    "ask-1",
                    "AskUserQuestion",
                    serde_json::json!({"question": "Continue?"}),
                ),
                real_tool_result("ask-1", "User answered: yes"),
                real_assistant_text("normal response"),
            ]);

            let entries = resolve_stop_entries(Some(&jsonl), Some("normal response"));
            assert!(matches!(
                entries.as_slice(),
                [TurnEntry {
                    kind: TurnEntryKind::Response { text }
                }] if text == "normal response"
            ));
        }

        #[test]
        fn exit_uses_nearest_write_before_it() {
            let dir = tempfile::tempdir().unwrap();
            let plans_dir = dir.path().join(".claude/plans");
            std::fs::create_dir_all(&plans_dir).unwrap();
            let old_path = plans_dir.join("old.md");
            let nearest_path = plans_dir.join("nearest.md");
            let after_path = plans_dir.join("after.md");
            std::fs::write(&old_path, "old").unwrap();
            std::fs::write(&nearest_path, "nearest").unwrap();
            std::fs::write(&after_path, "after").unwrap();

            let jsonl = [
                serde_json::json!({"role": "user", "content": "please plan"}),
                serde_json::json!({
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "name": "Write",
                            "input": {"file_path": old_path.to_str().unwrap(), "content": "old"},
                        },
                        {
                            "type": "tool_use",
                            "name": "Write",
                            "input": {"file_path": nearest_path.to_str().unwrap(), "content": "nearest"},
                        },
                        {"type": "tool_use", "name": "ExitPlanMode", "input": {}},
                        {
                            "type": "tool_use",
                            "name": "Write",
                            "input": {"file_path": after_path.to_str().unwrap(), "content": "after"},
                        },
                    ],
                }),
            ]
            .into_iter()
            .map(|entry| entry.to_string())
            .collect::<Vec<_>>()
            .join("\n");

            let entries = resolve_stop_entries(Some(&jsonl), None);
            assert!(matches!(
                entries.as_slice(),
                [TurnEntry {
                    kind: TurnEntryKind::Response { text }
                }] if text == "nearest"
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

            let repo = std::rc::Rc::new(gix::open(dir.path()).unwrap());
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

            let repo = std::rc::Rc::new(gix::open(dir.path()).unwrap());
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
            let root = crate::find_worktree_root(Some(sub.as_path())).unwrap();
            assert_eq!(
                root.canonicalize().unwrap(),
                dir.path().canonicalize().unwrap()
            );
        }
    }
}
