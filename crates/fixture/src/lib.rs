//! Git repositories for tests. A dev-dependency only.

// NOTE: pedantic asks for `# Panics` sections and `#[must_use]`. Panicking is
// how a fixture fails a test, and `git` is mostly called for its effect.
#![allow(clippy::missing_panics_doc, clippy::must_use_candidate)]

use std::{path::Path, process::Command};

/// Run git in `dir` and return its stdout, trimmed.
///
/// Hermetic: user and system git config (signing, hooks) must not leak into
/// fixtures. A global `commit.gpgsign` through the 1Password agent would hang
/// them.
// NOTE: the hermetic git every fixture runs; its fan-in is the point.
// coland-ignore: @coland/high_call_degree
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository in `dir` with one commit, of `init.txt`.
pub fn init_repo_with_commit(dir: &Path) {
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("init.txt"), "init").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "initial"]);
}
