use std::{
    fs,
    path::{Path, PathBuf},
};

use concats_core::{
    Oid,
    error::{Error, Result},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookState {
    pub current_checkpoint: Option<Oid>,
}

fn state_dir(repo_path: &Path) -> PathBuf {
    repo_path.join(".git").join("concats").join("hooks")
}

fn state_path(repo_path: &Path, session_id: &str) -> PathBuf {
    state_dir(repo_path).join(format!("{session_id}.json"))
}

/// Load the persisted hook state for a session if it exists.
///
/// # Errors
///
/// Returns an error if the state file cannot be read or its JSON is invalid.
pub fn load(repo_path: &Path, session_id: &str) -> Result<Option<HookState>> {
    let path = state_path(repo_path, session_id);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)?;
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|e| Error::session(format!("invalid hook state: {e}")))
}

/// Persist the hook state for a session.
///
/// # Errors
///
/// Returns an error if the state directory cannot be created, the state cannot
/// be serialized, or the state file cannot be written.
pub fn save(repo_path: &Path, session_id: &str, state: &HookState) -> Result<()> {
    fs::create_dir_all(state_dir(repo_path))?;
    let data = serde_json::to_string_pretty(state)
        .map_err(|e| Error::session(format!("failed to serialize hook state: {e}")))?;
    fs::write(state_path(repo_path, session_id), data)?;
    Ok(())
}

/// Load the hook state, creating an empty one when it does not yet exist.
///
/// # Errors
///
/// Returns an error if the existing state cannot be loaded or the default
/// state cannot be written.
pub fn ensure(repo_path: &Path, session_id: &str) -> Result<HookState> {
    if let Some(state) = load(repo_path, session_id)? {
        Ok(state)
    } else {
        let state = HookState {
            current_checkpoint: None,
        };
        save(repo_path, session_id, &state)?;
        Ok(state)
    }
}

/// Discover the repository root from the current or supplied working
/// directory.
///
/// # Errors
///
/// Returns an error if the starting directory cannot be determined, no git
/// repository can be discovered, or the repository is bare.
pub fn find_repo_root(cwd: Option<&str>) -> Result<PathBuf> {
    let start = match cwd {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };
    let repo = git2::Repository::discover(&start)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::session("bare repository not supported"))?;
    Ok(workdir.to_path_buf())
}
