use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{ install, state::find_worktree_root, HandlerAction};

const PLUGIN_TEMPLATE: &str = include_str!("../plugins/opencode.ts");

pub(crate) fn dispatch(event: &str, payload_json: &str) -> Result<()> {
    crate::dispatch_simple(
        "OpenCode",
        "opencode-default",
        event,
        payload_json,
        resolve_action,
    )
}

fn resolve_action(event: &str) -> Result<HandlerAction> {
    match event {
        "session.created" => Ok(HandlerAction::SessionStarted),
        "session.idle" => Ok(HandlerAction::Stop),
        "tool.execute.after" => Ok(HandlerAction::FilesChanged),
        "tool.execute.before" => Ok(HandlerAction::Ignore),
        _ => Err(Error::session(format!(
            "unknown OpenCode hook event: {event}"
        ))),
    }
}

/// Install the concats plugin into `~/.config/opencode/plugins/concats.ts`.
///
/// # Errors
///
/// Returns an error if the plugin file cannot be written.
pub(crate) fn install(binary: &Path) -> Result<()> {
    install::install_plugin(&plugin_path()?, PLUGIN_TEMPLATE, binary)
}

/// Remove the concats plugin from `OpenCode`.
///
/// # Errors
///
/// Returns an error if the plugin file cannot be removed.
pub(crate) fn uninstall() -> Result<()> {
    install::uninstall_plugin(&plugin_path()?)
}

/// Check whether the concats plugin is installed for `OpenCode`.
#[must_use]
pub(crate) fn is_installed() -> bool {
    plugin_path()
        .ok()
        .is_some_and(|p| install::is_plugin_installed(&p))
}

fn plugin_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| {
            h.join(".config")
                .join("opencode")
                .join("plugins")
                .join("concats.ts")
        })
        .ok_or_else(|| Error::session("cannot determine home directory"))
}
