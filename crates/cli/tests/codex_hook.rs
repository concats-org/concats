use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
    rc::Rc,
};

use concats_core::{session, snapshot, turn};
use concats_fixture::git;

fn hook(root: &Path, payload: &serde_json::Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_concats"))
        .current_dir(root)
        .args(["hook", "codex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"");
}

fn recorded_worktree() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "user.email", "test@example.com"]);
    std::fs::write(root.join("file.txt"), "base\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base"]);
    let worktree = root.join("linked");
    git(
        root,
        &[
            "worktree",
            "add",
            "-qb",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    #[cfg(feature = "review")]
    let fingerprint =
        concats_review::sessions::worktree_fingerprint(&gix::open(&worktree).unwrap()).unwrap();
    for id in ["first", "second", "first"] {
        std::fs::write(worktree.join("file.txt"), "base\n").unwrap();
        hook(
            root,
            &serde_json::json!({
                "hook_event_name": "UserPromptSubmit", "session_id": id, "cwd": worktree,
                "prompt": "change the file"
            }),
        );
        std::fs::write(worktree.join("file.txt"), "changed\n").unwrap();
        hook(
            root,
            &serde_json::json!({
                "hook_event_name": "PostToolUse", "session_id": id, "cwd": worktree,
                "tool_name": "Bash", "tool_input": {"command": "write file"}, "tool_response": "ok"
            }),
        );
        if id == "first" {
            hook(
                root,
                &serde_json::json!({
                    "hook_event_name": "Stop", "session_id": id, "cwd": worktree,
                    "last_assistant_message": "changed it", "stop_hook_active": false
                }),
            );
        }
    }

    #[cfg(feature = "review")]
    {
        let repo = gix::open(&worktree).unwrap();
        assert_ne!(
            concats_review::sessions::worktree_fingerprint(&repo).unwrap(),
            fingerprint
        );
    }
    tmp
}

#[test]
fn records_codex_turns_from_a_linked_worktree() {
    let tmp = recorded_worktree();
    let root = tmp.path();
    let repo = Rc::new(gix::open(root).unwrap());
    for (id, count) in [("first", 2), ("second", 1)] {
        let session = session::open(repo.clone(), id).unwrap();
        let turns = turn::list(&session).unwrap();
        assert_eq!(turns.len(), count);
        assert_eq!(turns[0].subject(), "change the file");
        assert_eq!(turns[0].entries().len(), if id == "first" { 2 } else { 1 });
        assert_eq!(turns[0].has_response(), id == "first");
        assert_eq!(
            snapshot::list(&session).unwrap().len(),
            if id == "first" { count * 3 } else { 2 }
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.join("file.txt")).unwrap(),
        "base\n"
    );
}

#[test]
#[cfg(feature = "review")]
fn links_multiple_sessions_to_worktree_changes() {
    let tmp = recorded_worktree();
    let worktree = tmp.path().join("linked");
    for base in ["HEAD", "INDEX"] {
        let mut loaded = concats_diff::load::load(&worktree, base, "WORKTREE").unwrap();
        let mined = concats_review::sessions::mine(&mut loaded);
        for id in ["first", "second"] {
            assert!(mined.sessions.iter().any(|row| matches!(row,
                concats_diff::Row::Title { text } if text == &format!("## Session `{id}`"))));
        }
        assert!(mined.commits.is_empty());
    }

    {
        std::fs::write(worktree.join("file.txt"), "unrelated\n").unwrap();
        let mut loaded = concats_diff::load::load(&worktree, "HEAD", "WORKTREE").unwrap();
        assert!(
            concats_review::sessions::mine(&mut loaded)
                .sessions
                .is_empty()
        );
    }
}

#[test]
fn installs_and_removes_lifecycle_hooks_without_replacing_notify() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    std::fs::write(&path, "notify = ['computer-use', 'turn-ended']\n").unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_concats"))
            .args(args)
            .env("CODEX_HOME", home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    for _ in 0..2 {
        run(&["hooks", "install", "codex", "--global"]);
    }
    let installed = concats_hooks::toml_config::read(&path).unwrap();
    assert_eq!(installed["notify"][0].as_str(), Some("computer-use"));
    for event in ["UserPromptSubmit", "PostToolUse", "Stop"] {
        assert_eq!(installed["hooks"][event].as_array().unwrap().len(), 1);
    }
    let status = run(&["hooks", "status", "--global"]);
    assert!(
        String::from_utf8_lossy(&status.stderr)
            .lines()
            .any(|line| { line.split_whitespace().collect::<Vec<_>>() == ["codex", "yes", "yes"] })
    );
    run(&["hooks", "uninstall", "codex", "--global"]);
    let removed = concats_hooks::toml_config::read(&path).unwrap();
    assert_eq!(removed["notify"], installed["notify"]);
    let status = run(&["hooks", "status", "--global"]);
    assert!(
        String::from_utf8_lossy(&status.stderr)
            .lines()
            .any(|line| { line.split_whitespace().collect::<Vec<_>>() == ["codex", "yes", "no"] })
    );
}
