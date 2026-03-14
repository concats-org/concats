use std::{fs, path::PathBuf};

use concats_core::{
    Oid,
    error::{Error, Result},
};
use serde::{Deserialize, Serialize};

const CLAUDE_STATE_DIR: &str = "claude";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ClaudeLifecycleState {
    Idle,
    ActiveTurn { turn_oid: Oid },
}

#[derive(Debug, Clone)]
pub struct ClaudeStateStore {
    worktree_root: PathBuf,
}

impl ClaudeStateStore {
    #[must_use]
    pub fn new(worktree_root: impl Into<PathBuf>) -> Self {
        Self {
            worktree_root: worktree_root.into(),
        }
    }

    /// Load the persisted Claude lifecycle state for a session if it exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the state file cannot be read or its JSON is invalid.
    pub fn load(&self, session_id: &str) -> Result<Option<ClaudeLifecycleState>> {
        let path = self.state_path(session_id);
        if !path.exists() {
            return Ok(None);
        }

        let data = fs::read_to_string(&path)?;
        serde_json::from_str(&data)
            .map(Some)
            .map_err(|error| Error::session(format!("invalid Claude hook state: {error}")))
    }

    /// Persist the Claude lifecycle state for a session.
    ///
    /// # Errors
    ///
    /// Returns an error if the state directory cannot be created, the state
    /// cannot be serialized, or the state file cannot be written.
    pub fn save(&self, session_id: &str, state: &ClaudeLifecycleState) -> Result<()> {
        fs::create_dir_all(self.state_dir())?;
        let data = serde_json::to_string_pretty(state).map_err(|error| {
            Error::session(format!("failed to serialize Claude hook state: {error}"))
        })?;
        fs::write(self.state_path(session_id), data)?;
        Ok(())
    }

    /// Load the persisted Claude lifecycle state, creating an idle one when it
    /// does not yet exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the existing state cannot be loaded or the default
    /// idle state cannot be written.
    pub fn load_or_init(&self, session_id: &str) -> Result<ClaudeLifecycleState> {
        if let Some(state) = self.load(session_id)? {
            Ok(state)
        } else {
            let state = ClaudeLifecycleState::Idle;
            self.save(session_id, &state)?;
            Ok(state)
        }
    }

    fn state_dir(&self) -> PathBuf {
        self.worktree_root
            .join(".git")
            .join("concats")
            .join(CLAUDE_STATE_DIR)
    }

    fn state_path(&self, session_id: &str) -> PathBuf {
        self.state_dir().join(format!("{session_id}.json"))
    }
}

/// Discover the repository worktree root from the current or supplied working
/// directory.
///
/// # Errors
///
/// Returns an error if the starting directory cannot be determined, no git
/// repository can be discovered, or the repository is bare.
pub fn find_worktree_root(cwd: Option<&str>) -> Result<PathBuf> {
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

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    fn init_repo_with_commit(dir: &std::path::Path) {
        let repo = git2::Repository::init(dir).unwrap();
        let mut index = repo.index().unwrap();
        std::fs::write(dir.join("init.txt"), "init").unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@test").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }

    #[test]
    fn load_or_init_persists_idle_state() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let store = ClaudeStateStore::new(dir.path());

        let state = store.load_or_init("session-a").unwrap();

        assert_eq!(state, ClaudeLifecycleState::Idle);
        assert_eq!(
            store.load("session-a").unwrap(),
            Some(ClaudeLifecycleState::Idle)
        );
    }
}
