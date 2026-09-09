#![cfg(feature = "review")]

use std::{fs, process::Command};

use concats_review::store::Store;

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "The assertions verify one import sequence and its persisted roots and replies."
)]
fn import_policies_preserve_atomicity_dry_runs_and_thread_identity() {
    let sandbox = tempfile::tempdir().unwrap();
    let repo = sandbox.path();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.com"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    fs::write(repo.join("file.rs"), "old\n").unwrap();
    assert!(
        Command::new("git")
            .current_dir(repo)
            .args(["add", "file.rs"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(repo)
            .args(["commit", "-qm", "fixture"])
            .status()
            .unwrap()
            .success()
    );
    fs::write(repo.join("file.rs"), "new\n").unwrap();
    let import = |name: &str, flags: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_concats"))
            .current_dir(repo)
            .args([
                "comments", "import", name, "--repo", ".", "--base", "HEAD", "--head", "WORKTREE",
                "--author", "Fallback",
            ])
            .args(flags)
            .output()
            .unwrap()
    };
    fs::write(
        repo.join("review.md"),
        "## `file.rs`\n\n### L1\nValid\n\n### L999\nInvalid\n",
    )
    .unwrap();
    let rejected = import("review.md", &[]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("nothing imported"));
    assert!(Store::open(&repo.join(".git")).comments.is_empty());

    let payload = serde_json::json!([
        {"id": 1, "path": "file.rs", "line": 1, "side": "RIGHT", "body": "Root"},
        {"id": 2, "in_reply_to_id": 1, "path": "file.rs", "line": 999, "side": "RIGHT", "body": "Reply"},
        {"id": 3, "path": "file.rs", "line": 999, "side": "RIGHT", "body": "Outdated"}
    ]);
    fs::write(repo.join("review.json"), payload.to_string()).unwrap();
    assert!(import("review.json", &["--dry-run"]).status.success());
    assert!(Store::open(&repo.join(".git")).comments.is_empty());
    let accepted = import("review.json", &[]);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert!(
        String::from_utf8_lossy(&accepted.stderr)
            .contains("1 thread(s) do not anchor in this range — skipped")
    );
    let comments = Store::open(&repo.join(".git")).comments;
    assert_eq!(comments.len(), 2);
    let root = comments.iter().find(|c| c.body == "Root").unwrap();
    let reply = comments.iter().find(|c| c.body == "Reply").unwrap();
    assert_eq!(reply.parent, Some(root.id));
    assert_eq!(root.author.as_deref(), Some("Fallback"));
    assert!(import("review.json", &[]).status.success());
    assert_eq!(Store::open(&repo.join(".git")).comments.len(), 2);

    fs::write(repo.join("valid.md"), "## `file.rs`\n\n### L1\nMarkdown\n").unwrap();
    assert!(import("valid.md", &[]).status.success());
    assert_eq!(Store::open(&repo.join(".git")).comments.len(), 3);
}
