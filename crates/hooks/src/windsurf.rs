use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{
    HandlerAction, find_worktree_root,
    install::{self, JsonHookSpec},
};

const SPEC: JsonHookSpec = JsonHookSpec {
    marker: "concats hook windsurf",
    events: &[
        "pre_write_code",
        "post_write_code",
        "post_cascade_response_with_transcript",
    ],
    prepare_root: None,
    entry: build_entry,
};

fn build_entry(binary: &Path, event: &str) -> serde_json::Value {
    serde_json::json!({
        "command": format!("{} hook windsurf {event}", binary.display()),
        "show_output": false,
    })
}

pub(crate) fn dispatch(event: &str, payload_json: &str) -> Result<()> {
    crate::dispatch_simple(
        "Windsurf",
        "windsurf-default",
        event,
        payload_json,
        resolve_action,
    )
}

fn resolve_action(event: &str) -> Result<HandlerAction> {
    match event {
        "pre_write_code" | "post_write_code" => Ok(HandlerAction::FilesChanged),
        "post_cascade_response_with_transcript" => Ok(HandlerAction::Stop),
        _ => Err(Error::session(format!(
            "unknown Windsurf hook event: {event}"
        ))),
    }
}

/// Install concats hooks into `~/.codeium/windsurf/hooks.json`.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn install(binary: &Path) -> Result<()> {
    install::install_json_hooks(&config_path()?, &SPEC, binary)
}

/// Remove concats hooks from Windsurf config.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn uninstall() -> Result<()> {
    install::uninstall_json_hooks(&config_path()?, SPEC.marker)
}

/// Check whether concats hooks are installed for Windsurf.
#[must_use]
pub(crate) fn is_installed() -> bool {
    config_path()
        .ok()
        .is_some_and(|p| install::is_json_hooks_installed(&p, SPEC.marker))
}

fn config_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".codeium").join("windsurf").join("hooks.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}
