#![cfg(feature = "review")]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn comments(dir: &Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_concats"))
        .current_dir(dir)
        .arg("comments")
        .args(args)
        .args(["--repo", ".", "--base", "HEAD", "--head", "WORKTREE"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Every worktree of a repository shares one store, so an export is narrowed
/// to the threads its own range shows — another checkout's threads are not
/// this agent's to answer unless asked for.
#[test]
fn an_export_leaves_out_threads_made_in_another_worktree() {
    let sandbox = tempfile::tempdir().unwrap();
    let (a, b) = (sandbox.path().join("a"), sandbox.path().join("b"));
    fs::create_dir(&a).unwrap();
    git(&a, &["init", "-q"]);
    fs::write(a.join("file.rs"), "old\n").unwrap();
    git(&a, &["add", "file.rs"]);
    git(
        &a,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    git(
        &a,
        &["worktree", "add", "-q", "-b", "b", b.to_str().unwrap()],
    );

    fs::write(a.join("file.rs"), "made in a\n").unwrap();
    comments(&a, &["add", "file.rs:1", "-m", "From a"]);
    fs::write(b.join("file.rs"), "made in b\n").unwrap();
    comments(&b, &["add", "file.rs:1", "-m", "From b"]);

    let narrowed = comments(&a, &["export"]);
    let stdout = String::from_utf8_lossy(&narrowed.stdout);
    assert!(stdout.contains("From a"), "{stdout}");
    assert!(!stdout.contains("From b"), "{stdout}");
    assert!(String::from_utf8_lossy(&narrowed.stderr).contains("left out 1 thread"));

    let everything =
        String::from_utf8_lossy(&comments(&a, &["export", "--all"]).stdout).into_owned();
    assert!(
        everything.contains("From a") && everything.contains("From b"),
        "{everything}"
    );
}
