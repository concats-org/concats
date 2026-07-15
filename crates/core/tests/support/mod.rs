// NOTE: pedantic asks these helpers for `# Panics` sections and `#[must_use]`.
// Panicking is how a fixture fails a test, and none of them is called for its
// value alone. `all`, `style` and `complexity` stay on, as everywhere.
#![allow(clippy::pedantic)]

use std::{ffi::OsStr, path::Path, process::Command};

use concats_core::Oid;
use concats_message::Turn as TurnMessage;

const SESSION_REF_PREFIX: &str = "refs/agent/sessions/";
const SNAPSHOT_REF_PREFIX: &str = "refs/agent/snapshots/";

pub fn init_repo_with_commit(dir: &Path) -> gix::Repository {
    run_git(dir, ["init", "-q"]);
    std::fs::write(dir.join("init.txt"), "init").unwrap();
    run_git(dir, ["add", "-A"]);
    run_git(dir, ["commit", "-q", "-m", "initial"]);
    gix::open(dir).unwrap()
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
    run_git(worktree_root, ["add", "-A"]);
    run_git(worktree_root, ["commit", "-q", "-m", message.as_str()]);
    Oid::from(repo.head_id().unwrap().detach())
}

pub fn add_origin_remote(repo: &gix::Repository, remote_path: &Path) {
    let url = remote_path.to_string_lossy();
    run_git(
        repo.workdir().unwrap(),
        ["remote", "add", "origin", url.as_ref()],
    );
}

pub fn run_git<I, S>(repo_path: &Path, args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        // Hermetic: user/system git config must not leak into fixtures.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub fn session_ref_name(session_id: &str) -> String {
    format!("{SESSION_REF_PREFIX}{session_id}")
}

pub fn snapshot_ref_name(session_id: &str) -> String {
    format!("{SNAPSHOT_REF_PREFIX}{session_id}")
}
