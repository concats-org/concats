use std::path::Path;

use concats_core::error::{Error, Result};
use serde::Deserialize;

use crate::{handler, install, state::find_worktree_root};

#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(alias = "thread_id", alias = "thread-id")]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
}

/// Dispatch a Codex notification to the turn handlers.
///
/// Codex fires a single notification event (no event name). The payload
/// is provided via the `--payload` CLI argument rather than stdin.
///
/// # Errors
///
/// Returns an error if the payload cannot be parsed, the repository root
/// cannot be resolved, or a handler fails.
pub(crate) fn dispatch(payload_json: &str) -> Result<()> {
    let payload: Payload = serde_json::from_str(payload_json)
        .map_err(|error| Error::session(format!("invalid Codex payload: {error}")))?;
    let session_id = payload.session_id.as_deref().unwrap_or("codex-default");
    let worktree_root = find_worktree_root(payload.cwd.as_deref())?;

    handler::on_files_changed(&worktree_root, session_id)?;

    if let Some(transcript) = &payload.transcript_path {
        match std::fs::read_to_string(transcript) {
            Ok(response) => handler::on_stop(&worktree_root, session_id, &response)?,
            Err(error) => {
                tracing::warn!("failed to read codex transcript at {transcript}: {error}");
            }
        }
    }

    Ok(())
}

/// Install concats hooks into `~/.codex/config.toml`.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn install(binary: &Path) -> Result<()> {
    let path = config_path()?;
    let mut config = install::read_toml_config(&path)?;
    let table = config
        .as_table_mut()
        .ok_or_else(|| Error::session("codex config root is not a table"))?;
    let hooks = table
        .entry("hooks")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| Error::session("hooks is not a table"))?;
    hooks.insert(
        "notify".to_string(),
        toml::Value::Array(vec![
            toml::Value::String(binary.display().to_string()),
            toml::Value::String("hook".to_string()),
            toml::Value::String("codex".to_string()),
        ]),
    );
    install::write_toml_config(&path, &config)
}

/// Remove concats hooks from Codex config.
///
/// # Errors
///
/// Returns an error if the config cannot be read, updated, or written.
pub(crate) fn uninstall() -> Result<()> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(());
    }
    let mut config = install::read_toml_config(&path)?;
    if let Some(hooks) = config
        .as_table_mut()
        .and_then(|t| t.get_mut("hooks"))
        .and_then(|h| h.as_table_mut())
    {
        hooks.remove("notify");
    }
    install::write_toml_config(&path, &config)
}

/// Check whether concats hooks are installed for Codex.
#[must_use]
pub(crate) fn is_installed() -> bool {
    config_path()
        .ok()
        .is_some_and(|p| std::fs::read_to_string(p).is_ok_and(|s| s.contains("concats")))
}

fn config_path() -> Result<std::path::PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".codex").join("config.toml"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}
