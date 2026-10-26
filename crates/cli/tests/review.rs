#![cfg(feature = "review")]

use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Output},
};

use concats_fixture::git;
use concats_review::store::{Comment, Store};

/// A repo with `file.rs` committed as "old" and edited to "new", so the
/// worktree range has a diff.
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q"]);
    fs::write(repo.join("file.rs"), "old\n").unwrap();
    git(repo, &["add", "file.rs"]);
    git(repo, &["commit", "-qm", "fixture"]);
    fs::write(repo.join("file.rs"), "new\n").unwrap();
    tmp
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

fn manifest(repo: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_concats"))
        .current_dir(repo)
        .arg("manifest")
        .output()
        .unwrap()
}

/// With no app, a bare command reviews the branch so far: from where HEAD
/// left the default branch to the working tree.
#[test]
fn a_bare_command_without_an_app_reviews_the_branch_so_far() {
    let sandbox = fixture();
    let repo = sandbox.path();
    git(repo, &["branch", "-M", "main"]);
    git(repo, &["checkout", "-q", "-b", "feature"]);
    fs::write(repo.join("other.rs"), "committed on the branch\n").unwrap();
    git(repo, &["add", "other.rs"]);
    git(repo, &["commit", "-qm", "branch work"]);

    let alone = manifest(repo);
    let stdout = String::from_utf8_lossy(&alone.stdout);
    assert!(
        alone.status.success(),
        "{}",
        String::from_utf8_lossy(&alone.stderr)
    );
    assert!(
        stdout.contains("other.rs") && stdout.contains("file.rs"),
        "{stdout}"
    );
}

/// The list prints a comment on deleted lines as the old-side anchor `add`
/// takes back, measured against the default range.
#[test]
fn the_list_prints_old_side_anchors_against_the_default_range() {
    let sandbox = fixture();
    let repo = sandbox.path();
    let rev = git(repo, &["rev-parse", "HEAD"])[..7].to_string();
    comments(repo, &["add", &format!("file.rs@{rev}:1"), "-m", "gone"]);
    let list = || {
        let output = Command::new(env!("CARGO_BIN_EXE_concats"))
            .current_dir(repo)
            .arg("comments")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    let stdout = list();
    assert!(
        stdout.contains(&format!("#1  file.rs@{rev}:1-1  ")),
        "{stdout}"
    );

    // Before a branch's first commit the default range is the index against
    // the working tree, and the comment's lines are the index's.
    git(repo, &["checkout", "-q", "--orphan", "unborn"]);
    let stdout = list();
    assert!(stdout.contains("#1  file.rs@INDEX:1-1  "), "{stdout}");
}

/// A comment left without `--author` is signed with `git config user.name`,
/// the way the app signs its own.
#[test]
fn a_comment_without_an_author_takes_the_git_user_name() {
    let sandbox = fixture();
    let repo = sandbox.path();
    git(repo, &["config", "user.name", "Ada Lovelace"]);
    comments(repo, &["add", "file.rs:1", "-m", "why?"]);
    let id = Store::open(&repo.join(".git")).comments[0].id.to_string();
    comments(repo, &["reply", &id, "-m", "because"]);
    let authors: Vec<Option<String>> = Store::open(&repo.join(".git"))
        .comments
        .into_iter()
        .map(|c| c.author)
        .collect();
    assert_eq!(
        authors,
        [Some("Ada Lovelace".into()), Some("Ada Lovelace".into())]
    );
}

/// Every worktree of a repository shares one store, so an export is narrowed
/// to the threads its own range shows — another checkout's threads are not
/// this agent's to answer unless asked for.
#[test]
fn an_export_leaves_out_threads_made_in_another_worktree() {
    let (repo, sandbox) = (fixture(), tempfile::tempdir().unwrap());
    let (a, b) = (repo.path(), sandbox.path().join("b"));
    git(
        a,
        &["worktree", "add", "-q", "-b", "b", b.to_str().unwrap()],
    );

    fs::write(a.join("file.rs"), "made in a\n").unwrap();
    comments(a, &["add", "file.rs:1", "-m", "From a"]);
    fs::write(b.join("file.rs"), "made in b\n").unwrap();
    comments(&b, &["add", "file.rs:1", "-m", "From b"]);

    let narrowed = comments(a, &["export"]);
    let stdout = String::from_utf8_lossy(&narrowed.stdout);
    assert!(stdout.contains("From a"), "{stdout}");
    assert!(!stdout.contains("From b"), "{stdout}");
    assert!(String::from_utf8_lossy(&narrowed.stderr).contains("left out 1 thread"));

    let everything =
        String::from_utf8_lossy(&comments(a, &["export", "--all"]).stdout).into_owned();
    assert!(
        everything.contains("From a") && everything.contains("From b"),
        "{everything}"
    );
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "The assertions verify one import sequence and its persisted roots and replies."
)]
fn import_policies_preserve_atomicity_dry_runs_and_thread_identity() {
    let sandbox = fixture();
    let repo = sandbox.path();
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
    let document = |comments| serde_json::json!({"schema": "concats-app/2", "comments": comments});
    let invalid = document(serde_json::json!([
        {"id": 1, "path": "file.rs", "line": 1, "side": "RIGHT", "body": "Valid"},
        {"id": 2, "path": "file.rs", "line": 999, "side": "RIGHT", "body": "Invalid"}
    ]));
    fs::write(repo.join("review.json"), invalid.to_string()).unwrap();
    let rejected = import("review.json", &[]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("nothing imported"));
    assert_eq!(Store::open(&repo.join(".git")).comments, [] as [Comment; 0]);

    let payload = serde_json::json!([
        {"id": 1, "path": "file.rs", "line": 1, "side": "RIGHT", "body": "Root"},
        {"id": 2, "in_reply_to_id": 1, "path": "file.rs", "line": 999, "side": "RIGHT", "body": "Reply"},
        {"id": 3, "path": "file.rs", "line": 999, "side": "RIGHT", "body": "Outdated"}
    ]);
    fs::write(repo.join("pulls.json"), payload.to_string()).unwrap();
    assert!(import("pulls.json", &["--dry-run"]).status.success());
    assert_eq!(Store::open(&repo.join(".git")).comments, [] as [Comment; 0]);
    let accepted = import("pulls.json", &[]);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert!(
        String::from_utf8_lossy(&accepted.stderr)
            .contains("1 thread(s) do not anchor in this range — skipped")
    );
    let stored = Store::open(&repo.join(".git")).comments;
    assert_eq!(stored.len(), 2);
    let root = stored.iter().find(|c| c.body == "Root").unwrap();
    let reply = stored.iter().find(|c| c.body == "Reply").unwrap();
    assert_eq!(reply.parent, Some(root.id));
    assert_eq!(root.author.as_deref(), Some("Fallback"));
    assert!(import("pulls.json", &[]).status.success());
    assert_eq!(Store::open(&repo.join(".git")).comments.len(), 2);

    let valid = document(serde_json::json!([
        {"id": 1, "path": "file.rs", "line": 1, "side": "RIGHT", "body": "Document"}
    ]));
    fs::write(repo.join("valid.json"), valid.to_string()).unwrap();
    assert!(import("valid.json", &[]).status.success());
    assert_eq!(Store::open(&repo.join(".git")).comments.len(), 3);

    // An export imports back as a no-op.
    let export = comments(repo, &["export"]);
    fs::write(repo.join("export.json"), &export.stdout).unwrap();
    let again = import("export.json", &[]);
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("imported 0 comment(s)"),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(Store::open(&repo.join(".git")).comments.len(), 3);
}

/// A bare command takes its range from the app open on the checkout.
#[test]
fn a_bare_command_reviews_what_the_app_shows_for_the_checkout() {
    let sandbox = fixture();
    let repo = sandbox.path().canonicalize().unwrap();

    // The app's side of it: one answer, the way it answers `GET /range`.
    let pipe = concats_state::listen(&repo).unwrap().unwrap();
    let root = repo.clone();
    let app = std::thread::spawn(move || {
        let mut id = String::new();
        BufReader::new(pipe).read_line(&mut id).unwrap();
        let id = id.trim_end();
        let request = concats_state::read_request(&root, id).unwrap();
        let range = r#"{"repo":".","base":"HEAD","head":"WORKTREE"}"#;
        let answer = concats_state::Answer {
            status: 200,
            body: range.to_string(),
        };
        concats_state::answer(&root, id, &answer).unwrap();
        request
    });

    let shown = manifest(&repo);
    let asked = app.join().unwrap();
    assert_eq!(
        (asked.method, asked.target.as_str()),
        (concats_state::Method::Get, concats_state::RANGE)
    );
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert!(stdout.contains("file.rs"), "{stdout}");
}
