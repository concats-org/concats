use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{
    HandlerAction,
    install::{self, JsonHookSpec},
    state::find_worktree_root,
};

const SPEC: JsonHookSpec = JsonHookSpec {
    marker: "concats hook cursor",
    events: &["beforeSubmitPrompt", "afterFileEdit"],
    prepare_root: Some(prepare_root),
    entry: build_entry,
};

#[allow(clippy::unnecessary_wraps)]
fn prepare_root(root: &mut serde_json::Map<String, serde_json::Value>) -> Result<()> {
    root.insert("version".to_string(), serde_json::json!(1));
    Ok(())
}

fn build_entry(binary: &Path, event: &str) -> serde_json::Value {
    serde_json::json!({
        "command": format!("{} hook cursor {event}", binary.display())
    })
}

pub(crate) fn dispatch(event: &str, payload_json: &str) -> Result<()> {
    crate::dispatch_simple(
        "Cursor",
        "cursor-default",
        event,
        payload_json,
        resolve_action,
    )
}

fn resolve_action(event: &str) -> Result<HandlerAction> {
    match event {
        "afterFileEdit" => Ok(HandlerAction::FilesChanged),
        "beforeSubmitPrompt" => Ok(HandlerAction::PromptSubmitted),
        _ => Err(Error::session(format!(
            "unknown Cursor hook event: {event}"
        ))),
    }
}

/// Install concats hooks into `~/.cursor/hooks.json`.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn install(binary: &Path) -> Result<()> {
    install::install_json_hooks(&config_path()?, &SPEC, binary)
}

/// Remove concats hooks from Cursor config.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn uninstall() -> Result<()> {
    install::uninstall_json_hooks(&config_path()?, SPEC.marker)
}

/// Check whether concats hooks are installed for Cursor.
#[must_use]
pub(crate) fn is_installed() -> bool {
    config_path()
        .ok()
        .is_some_and(|p| install::is_json_hooks_installed(&p, SPEC.marker))
}

fn config_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".cursor").join("hooks.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}
