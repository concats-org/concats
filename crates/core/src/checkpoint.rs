use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub use crate::transcript::{Transcript, TranscriptEntry, TranscriptEntryKind};
use crate::{
    error::{Error, Result},
    git::Oid,
    session::{self, Session},
    transcript::decode_commit_message,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub tree: Oid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub session_id: String,
    pub repo_path: PathBuf,
    pub oid: Oid,
    pub created_at: OffsetDateTime,
    pub transcript: Transcript,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub transcript: Transcript,
    agent_name: Option<String>,
}

impl Draft {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = Some(name.into());
        self
    }

    #[must_use]
    pub fn from_checkpoint(checkpoint: &Checkpoint) -> Self {
        Self {
            transcript: checkpoint.transcript.clone(),
            agent_name: None,
        }
    }

    pub(crate) fn agent_name(&self) -> Option<&str> {
        self.agent_name.as_deref()
    }
}

/// List all checkpoints for a session in creation order.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, the session ref is
/// missing, or any checkpoint commit cannot be loaded.
pub fn list(session: &Session) -> Result<Vec<Checkpoint>> {
    let repo = git2::Repository::open(&session.repo_path)?;
    let tip = session::resolve_ref(&repo, &session::ref_name(&session.id))
        .ok_or_else(|| Error::session(format!("session not found: {}", session.id)))?;
    load_from_tip(&repo, &session.repo_path, &session.id, &tip)
}

/// Load a single checkpoint by object ID.
///
/// # Errors
///
/// Returns an error if checkpoint listing fails or the requested checkpoint is
/// not present in the session history.
pub fn get(session: &Session, oid: Oid) -> Result<Checkpoint> {
    let repo = git2::Repository::open(&session.repo_path)?;
    let tip = session::resolve_ref(&repo, &session::ref_name(&session.id))
        .ok_or_else(|| Error::session(format!("session not found: {}", session.id)))?;
    let checkpoint_oid = oid.as_git();
    let commit = repo
        .find_commit(checkpoint_oid)
        .map_err(|_| Error::session(format!("checkpoint not found: {oid}")))?;

    if tip.id() != checkpoint_oid && !repo.graph_descendant_of(tip.id(), checkpoint_oid)? {
        return Err(Error::session(format!("checkpoint not found: {oid}")));
    }

    load_from_commit(&commit, &session.id, &session.repo_path)
        .map_err(|_| Error::session(format!("checkpoint not found: {oid}")))
}

/// Restore the checkpoint snapshot into the repository working tree.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, the snapshot tree
/// cannot be loaded, or the checkout fails.
pub fn restore(checkpoint: &Checkpoint) -> Result<()> {
    let repo = git2::Repository::open(&checkpoint.repo_path)?;
    let tree = repo.find_tree(checkpoint.snapshot.tree.as_git())?;
    repo.checkout_tree(
        tree.as_object(),
        Some(git2::build::CheckoutBuilder::new().force()),
    )?;
    Ok(())
}

fn load_from_tip(
    repo: &git2::Repository,
    repo_path: &Path,
    session_id: &str,
    tip: &git2::Commit<'_>,
) -> Result<Vec<Checkpoint>> {
    let mut revwalk = repo.revwalk()?;
    revwalk.push(tip.id())?;
    revwalk.simplify_first_parent()?;

    let mut checkpoints = Vec::new();
    for oid_result in revwalk {
        let oid = oid_result?;
        let commit = repo.find_commit(oid)?;
        // Session boundary: keep only commits whose Session trailer matches.
        match decode_commit_message(commit.message().unwrap_or("")) {
            Some(decoded) if decoded.session_id == session_id => {
                checkpoints.push(load_from_commit(&commit, session_id, repo_path)?);
            }
            _ => break,
        }
    }

    checkpoints.reverse();
    Ok(checkpoints)
}

pub(crate) fn load_from_commit(
    commit: &git2::Commit<'_>,
    session_id: &str,
    repo_path: &Path,
) -> Result<Checkpoint> {
    let decoded = decode_commit_message(commit.message().unwrap_or(""))
        .ok_or_else(|| Error::session("invalid checkpoint commit"))?;
    Ok(Checkpoint {
        session_id: session_id.to_string(),
        repo_path: repo_path.to_path_buf(),
        oid: Oid::from(commit.id()),
        created_at: session::commit_time(commit.time())?,
        transcript: decoded.transcript,
        snapshot: Snapshot {
            tree: Oid::from(commit.tree_id()),
        },
    })
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::{diff, session, testutil::init_repo_with_commit};

    #[test]
    fn list_and_get_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let base = Oid::from(repo.head().unwrap().target().unwrap());
        let session = session::create(dir.path(), "session-a", base).unwrap();

        let mut draft = Draft::new();
        draft
            .transcript
            .append(TranscriptEntry::prompt_now("prompt"))
            .unwrap();
        draft
            .transcript
            .append(TranscriptEntry::response_now("done"))
            .unwrap();
        let checkpoint = session::commit(&session, &draft).unwrap();

        let checkpoints = list(&session).unwrap();
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(get(&session, checkpoint.oid).unwrap().oid, checkpoint.oid);
    }

    #[test]
    fn checkpoint_draft_copies_checkpoint_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let base = Oid::from(repo.head().unwrap().target().unwrap());
        let session = session::create(dir.path(), "session-a", base).unwrap();

        let mut draft = Draft::new();
        draft
            .transcript
            .append(TranscriptEntry::prompt_now("prompt"))
            .unwrap();
        draft
            .transcript
            .append(TranscriptEntry::response_now("first"))
            .unwrap();
        let checkpoint = session::commit(&session, &draft).unwrap();

        let copied = Draft::from_checkpoint(&checkpoint);
        assert_eq!(copied.transcript.len(), 2);
        assert!(matches!(
            copied.transcript.iter().nth(0).unwrap().kind,
            TranscriptEntryKind::Prompt { .. }
        ));
        assert!(matches!(
            copied.transcript.iter().nth(1).unwrap().kind,
            TranscriptEntryKind::Response { .. }
        ));
    }

    #[test]
    fn diff_for_checkpoint_uses_parent_relative_patch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let base = Oid::from(repo.head().unwrap().target().unwrap());
        let session = session::create(dir.path(), "session-a", base).unwrap();

        let checkpoint = session::commit(&session, &Draft::new()).unwrap();
        std::fs::write(dir.path().join("src.txt"), "hello").unwrap();
        let checkpoint = session::amend(&session, &Draft::from_checkpoint(&checkpoint)).unwrap();

        let diffs = diff::for_checkpoint(&checkpoint).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "src.txt");
    }

    #[test]
    fn snapshot_ignores_nested_git_roots() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let base = Oid::from(repo.head().unwrap().target().unwrap());
        let session = session::create(dir.path(), "session-a", base).unwrap();

        let checkpoint = session::commit(&session, &Draft::new()).unwrap();
        std::fs::write(dir.path().join("src.txt"), "hello").unwrap();
        let nested = dir.path().join("vendor/nested-repo");
        std::fs::create_dir_all(&nested).unwrap();
        git2::Repository::init(&nested).unwrap();
        std::fs::write(nested.join("ignored.txt"), "ignore me").unwrap();

        let checkpoint = session::amend(&session, &Draft::from_checkpoint(&checkpoint)).unwrap();
        let diffs = diff::for_checkpoint(&checkpoint).unwrap();

        assert!(diffs.iter().any(|diff| diff.path == "src.txt"));
        assert!(diffs.iter().all(|diff| !diff.path.starts_with("vendor/")));
    }

    #[test]
    fn snapshot_includes_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let base = Oid::from(repo.head().unwrap().target().unwrap());
        let session = session::create(dir.path(), "session-a", base).unwrap();

        let checkpoint = session::commit(&session, &Draft::new()).unwrap();
        std::fs::write(dir.path().join("src.txt"), "hello").unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink("src.txt", dir.path().join("link.txt")).unwrap();

        #[cfg(windows)]
        std::os::windows::fs::symlink_file("src.txt", dir.path().join("link.txt")).unwrap();

        let checkpoint = session::amend(&session, &Draft::from_checkpoint(&checkpoint)).unwrap();
        let diffs = diff::for_checkpoint(&checkpoint).unwrap();

        assert!(diffs.iter().any(|diff| diff.path == "src.txt"));
        assert!(diffs.iter().any(|diff| diff.path == "link.txt"));
    }
}
