use std::{ffi::OsStr, path::Path, process::Command};

use concats_core::Oid;
use concats_message::Turn as TurnMessage;

const SESSION_REF_PREFIX: &str = "refs/agent/sessions/";
const SNAPSHOT_REF_PREFIX: &str = "refs/agent/snapshots/";

pub fn init_repo_with_commit(dir: &Path) -> git2::Repository {
    let repo = git2::Repository::init(dir).unwrap();
    {
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
    repo
}

pub fn turn_message(session_id: &str) -> TurnMessage {
    TurnMessage::new(session_id.parse().unwrap())
        .with_agent_name("test-agent")
        .unwrap()
}

pub fn commit_head(
    repo: &git2::Repository,
    worktree_root: &Path,
    file_name: &str,
    contents: &str,
) -> Oid {
    std::fs::write(worktree_root.join(file_name), contents).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("test", "test@test").unwrap();

    Oid::from(
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("commit {file_name}"),
            &tree,
            &[&parent],
        )
        .unwrap(),
    )
}

pub fn add_origin_remote(repo: &git2::Repository, remote_path: &Path) {
    repo.remote("origin", remote_path.to_string_lossy().as_ref())
        .unwrap();
}

pub fn run_git<I, S>(repo_path: &Path, args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
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
