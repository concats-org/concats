use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{ install, HandlerAction, find_worktree_root};

const PLUGIN_TEMPLATE: &str = include_str!("../plugins/amp.ts");

pub(crate) fn dispatch(event: &str, payload_json: &str) -> Result<()> {
    crate::dispatch_simple("Amp", "amp-default", event, payload_json, resolve_action)
}

fn resolve_action(event: &str) -> Result<HandlerAction> {
    match event {
        "session.start" => Ok(HandlerAction::SessionStarted),
        "agent.start" => Ok(HandlerAction::PromptSubmitted),
        "agent.end" => Ok(HandlerAction::Stop),
        "tool.result" => Ok(HandlerAction::FilesChanged),
        "tool.call" => Ok(HandlerAction::Ignore),
        _ => Err(Error::session(format!("unknown Amp hook event: {event}"))),
    }
}

/// Install the concats plugin into `~/.config/amp/plugins/concats.ts`.
///
/// # Errors
///
/// Returns an error if the plugin file cannot be written.
pub(crate) fn install(binary: &Path) -> Result<()> {
    install::install_plugin(&plugin_path()?, PLUGIN_TEMPLATE, binary)
}

/// Remove the concats plugin from Amp.
///
/// # Errors
///
/// Returns an error if the plugin file cannot be removed.
pub(crate) fn uninstall() -> Result<()> {
    install::uninstall_plugin(&plugin_path()?)
}

/// Check whether the concats plugin is installed for Amp.
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
                .join("amp")
                .join("plugins")
                .join("concats.ts")
        })
        .ok_or_else(|| Error::session("cannot determine home directory"))
}
