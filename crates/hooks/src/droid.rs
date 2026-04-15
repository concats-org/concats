use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{
    HandlerAction,
    install::{self, JsonHookSpec},
    state::find_worktree_root,
};

const SPEC: JsonHookSpec = JsonHookSpec {
    marker: "concats hook droid",
    events: &["PreToolUse", "PostToolUse"],
    prepare_root: Some(prepare_root),
    entry: build_entry,
};

#[allow(clippy::unnecessary_wraps)]
fn prepare_root(root: &mut serde_json::Map<String, serde_json::Value>) -> Result<()> {
    root.insert("claudeHooksImported".to_string(), serde_json::json!(true));
    Ok(())
}

fn build_entry(binary: &Path, event: &str) -> serde_json::Value {
    serde_json::json!({
        "matcher": "^(Edit|Write|Create|ApplyPatch)$",
        "hooks": [{
            "type": "command",
            "command": format!("{} hook droid {event}", binary.display())
        }]
    })
}

pub(crate) fn dispatch(event: &str, payload_json: &str) -> Result<()> {
    crate::dispatch_simple(
        "Droid",
        "droid-default",
        event,
        payload_json,
        resolve_action,
    )
}

fn resolve_action(event: &str) -> Result<HandlerAction> {
    match event {
        "PostToolUse" => Ok(HandlerAction::FilesChanged),
        "PreToolUse" => Ok(HandlerAction::Ignore),
        _ => Err(Error::session(format!("unknown Droid hook event: {event}"))),
    }
}

/// Install concats hooks into `~/.factory/settings.json`.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn install(binary: &Path) -> Result<()> {
    install::install_json_hooks(&config_path()?, &SPEC, binary)
}

/// Remove concats hooks from Droid settings.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn uninstall() -> Result<()> {
    install::uninstall_json_hooks(&config_path()?, SPEC.marker)
}

/// Check whether concats hooks are installed for Droid.
#[must_use]
pub(crate) fn is_installed() -> bool {
    config_path()
        .ok()
        .is_some_and(|p| install::is_json_hooks_installed(&p, SPEC.marker))
}

fn config_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".factory").join("settings.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}
