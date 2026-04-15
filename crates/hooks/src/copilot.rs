use std::path::Path;

use concats_core::error::{Error, Result};

use crate::{ install, state::find_worktree_root, HandlerAction};

pub(crate) fn dispatch(event: &str, payload_json: &str) -> Result<()> {
    crate::dispatch_simple(
        "Copilot",
        "copilot-default",
        event,
        payload_json,
        resolve_action,
    )
}

fn resolve_action(event: &str) -> Result<HandlerAction> {
    match event {
        "PostToolUse" => Ok(HandlerAction::FilesChanged),
        "PreToolUse" => Ok(HandlerAction::Ignore),
        _ => Err(Error::session(format!(
            "unknown Copilot hook event: {event}"
        ))),
    }
}

/// Install concats hooks into `~/.github/hooks/concats.json`.
///
/// # Errors
///
/// Returns an error if the config cannot be written.
pub(crate) fn install(binary: &Path) -> Result<()> {
    let path = config_path()?;
    let bin = binary.display();
    let config = serde_json::json!({
        "PreToolUse": [format!("{bin} hook copilot PreToolUse")],
        "PostToolUse": [format!("{bin} hook copilot PostToolUse")]
    });
    install::write_json_config(&path, &config)
}

/// Remove concats hooks from Copilot config.
///
/// # Errors
///
/// Returns an error if the file cannot be removed.
pub(crate) fn uninstall() -> Result<()> {
    let path = config_path()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Check whether concats hooks are installed for Copilot.
#[must_use]
pub(crate) fn is_installed() -> bool {
    config_path().ok().is_some_and(|p| p.exists())
}

fn config_path() -> Result<std::path::PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".github").join("hooks").join("concats.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}
