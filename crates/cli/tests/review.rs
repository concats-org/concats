#![cfg(feature = "review")]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
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
    fs::write(
        repo.join("review.md"),
        "## `file.rs`\n\n### L1\nValid\n\n### L999\nInvalid\n",
    )
    .unwrap();
    let rejected = import("review.md", &[]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("nothing imported"));
    assert_eq!(Store::open(&repo.join(".git")).comments, [] as [Comment; 0]);

    let payload = serde_json::json!([
        {"id": 1, "path": "file.rs", "line": 1, "side": "RIGHT", "body": "Root"},
        {"id": 2, "in_reply_to_id": 1, "path": "file.rs", "line": 999, "side": "RIGHT", "body": "Reply"},
        {"id": 3, "path": "file.rs", "line": 999, "side": "RIGHT", "body": "Outdated"}
    ]);
    fs::write(repo.join("review.json"), payload.to_string()).unwrap();
    assert!(import("review.json", &["--dry-run"]).status.success());
    assert_eq!(Store::open(&repo.join(".git")).comments, [] as [Comment; 0]);
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

/// A bare command takes its range from the app open on the checkout.
#[test]
fn a_bare_command_reviews_what_the_app_shows_for_the_checkout() {
    let sandbox = fixture();
    let repo = sandbox.path().canonicalize().unwrap();

    // The app's side of it: one answer, the way it answers `GET /range`.
    let socket = repo.join(concats_state::SOCKET);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let app = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        let range = r#"{"repo":".","base":"HEAD","head":"WORKTREE"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{range}",
            range.len()
        )
        .unwrap();
        request
    });

    let shown = manifest(&repo);
    assert_eq!(
        app.join().unwrap().trim(),
        format!("GET {} HTTP/1.1", concats_state::RANGE)
    );
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert!(stdout.contains("file.rs"), "{stdout}");
}
