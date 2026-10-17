// NOTE: pedantic asks these helpers for `# Panics` sections and `#[must_use]`.
// Panicking is how a fixture fails a test, and none of them is called for its
// value alone. `all`, `style` and `complexity` stay on, as everywhere.
#![allow(clippy::pedantic)]

use std::{path::Path, rc::Rc};

use concats_core::{Oid, session, session::Session};
use concats_fixture::git;
use concats_message::Turn as TurnMessage;
use tempfile::TempDir;

const SESSION_REF_PREFIX: &str = "refs/agent/sessions/";
const SNAPSHOT_REF_PREFIX: &str = "refs/agent/snapshots/";

pub fn init_repo_with_commit(dir: &Path) -> gix::Repository {
    concats_fixture::init_repo_with_commit(dir);
    gix::open(dir).unwrap()
}

/// A session `id` started at the head of a one-commit repo.
pub fn session(id: &str) -> (TempDir, Rc<gix::Repository>, Session, Oid) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), id, head).unwrap();
    (dir, repo, session, head)
}

pub fn turn_message(session_id: &str) -> TurnMessage {
    TurnMessage::new(session_id.parse().unwrap())
        .with_agent_name("test-agent")
        .unwrap()
}

pub fn commit_head(
    repo: &gix::Repository,
    worktree_root: &Path,
    file_name: &str,
    contents: &str,
) -> Oid {
    std::fs::write(worktree_root.join(file_name), contents).unwrap();
    let message = format!("commit {file_name}");
    git(worktree_root, &["add", "-A"]);
    git(worktree_root, &["commit", "-q", "-m", &message]);
    Oid::from(repo.head_id().unwrap().detach())
}

pub fn session_ref_name(session_id: &str) -> String {
    format!("{SESSION_REF_PREFIX}{session_id}")
}

pub fn snapshot_ref_name(session_id: &str) -> String {
    format!("{SNAPSHOT_REF_PREFIX}{session_id}")
}
