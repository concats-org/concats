use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{
    HandlerAction, find_worktree_root,
    install::{self, JsonHookSpec},
};

const SPEC: JsonHookSpec = JsonHookSpec {
    marker: "concats hook gemini",
    events: &["BeforeTool", "AfterTool"],
    prepare_root: Some(prepare_root),
    entry: build_entry,
};

fn prepare_root(root: &mut serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let tools = root
        .entry("tools".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::session("tools is not an object"))?;
    tools.insert("enableHooks".to_string(), serde_json::json!(true));
    Ok(())
}

fn build_entry(binary: &Path, event: &str) -> serde_json::Value {
    serde_json::json!({
        "matcher": "write_file|replace",
        "hooks": [{
            "type": "command",
            "command": format!("{} hook gemini {event}", binary.display())
        }]
    })
}

pub(crate) fn dispatch(event: &str, payload_json: &str) -> Result<()> {
    crate::dispatch_simple(
        "Gemini",
        "gemini-default",
        event,
        payload_json,
        resolve_action,
    )
}

fn resolve_action(event: &str) -> Result<HandlerAction> {
    match event {
        "AfterTool" | "BeforeTool" => Ok(HandlerAction::FilesChanged),
        _ => Err(Error::session(format!(
            "unknown Gemini hook event: {event}"
        ))),
    }
}

/// Install concats hooks into `~/.gemini/settings.json`.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn install(binary: &Path) -> Result<()> {
    install::install_json_hooks(&config_path()?, &SPEC, binary)
}

/// Remove concats hooks from Gemini settings.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn uninstall() -> Result<()> {
    install::uninstall_json_hooks(&config_path()?, SPEC.marker)
}

/// Check whether concats hooks are installed for Gemini.
#[must_use]
pub(crate) fn is_installed() -> bool {
    config_path()
        .ok()
        .is_some_and(|p| install::is_json_hooks_installed(&p, SPEC.marker))
}

fn config_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".gemini").join("settings.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}
