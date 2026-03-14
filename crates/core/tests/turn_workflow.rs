pub mod support;

use std::rc::Rc;

use concats_core::{
    Oid, diff, session, snapshot,
    snapshot::SnapshotReason,
    turn,
    turn::{TurnEntry, TurnEntryKind},
};

#[test]
fn list_and_get_turns() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());
    let session = session::create(repo.clone(), "session-a", base).unwrap();

    let message = support::turn_message("session-a")
        .with_entry(TurnEntry::prompt_now("prompt"))
        .with_entry(TurnEntry::response_now("done"));
    let created = session::commit(&session, &message).unwrap();

    let turns = turn::list(&session).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turn::get(&session, created.oid).unwrap().oid, created.oid);
}

#[test]
fn turn_message_accessor_exposes_composed_message() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());
    let session = session::create(repo.clone(), "session-a", base).unwrap();

    let message = support::turn_message("session-a")
        .with_entry(TurnEntry::prompt_now("prompt"))
        .with_entry(TurnEntry::response_now("first"));
    let created = session::commit(&session, &message).unwrap();
    let copied = created.message().clone();

    assert_eq!(copied.subject(), "turn");
    assert_eq!(copied.agent_name(), Some("test-agent"));
    assert_eq!(copied.entries().len(), 2);
    assert!(matches!(
        copied.entries()[0].kind,
        TurnEntryKind::Prompt { .. }
    ));
    assert!(matches!(
        copied.entries()[1].kind,
        TurnEntryKind::Response { .. }
    ));
}

#[test]
fn restore_uses_turn_snapshot_tree() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());
    let session = session::create(repo.clone(), "session-a", base).unwrap();

    std::fs::write(dir.path().join("src.txt"), "hello").unwrap();
    let created = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, created.oid, SnapshotReason::TurnCommit).unwrap();

    std::fs::remove_file(dir.path().join("src.txt")).unwrap();
    turn::restore(&session, &created).unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("src.txt")).unwrap(),
        "hello"
    );
    let turn_commit = repo.find_commit(created.oid.as_git()).unwrap();
    assert_eq!(turn_commit.tree().unwrap().len(), 0);
}

#[test]
fn restore_rejects_turns_from_other_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());

    let source_session = session::create(repo.clone(), "session-a", base).unwrap();
    std::fs::write(dir.path().join("src.txt"), "hello").unwrap();
    let source_turn =
        session::commit(&source_session, &support::turn_message("session-a")).unwrap();
    let _ =
        snapshot::capture(&source_session, source_turn.oid, SnapshotReason::TurnCommit).unwrap();

    let fork_session = session::create(repo.clone(), "session-b", source_turn.oid).unwrap();
    let error = turn::restore(&fork_session, &source_turn).unwrap_err();

    assert!(error.to_string().contains("snapshot not found"));
}

#[test]
fn diff_for_turn_uses_parent_relative_snapshot_patch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());
    let session = session::create(repo.clone(), "session-a", base).unwrap();

    let created = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, created.oid, SnapshotReason::TurnCommit).unwrap();
    std::fs::write(dir.path().join("src.txt"), "hello").unwrap();
    let created = session::amend(&session, created.message()).unwrap();
    let _ = snapshot::capture(&session, created.oid, SnapshotReason::TurnAmend).unwrap();

    let diffs = diff::for_turn(&session, &created).unwrap();
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, "src.txt");
}

#[test]
fn snapshot_ignores_nested_git_roots() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());
    let session = session::create(repo.clone(), "session-a", base).unwrap();

    let created = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, created.oid, SnapshotReason::TurnCommit).unwrap();
    std::fs::write(dir.path().join("src.txt"), "hello").unwrap();
    let nested = dir.path().join("vendor/nested-repo");
    std::fs::create_dir_all(&nested).unwrap();
    git2::Repository::init(&nested).unwrap();
    std::fs::write(nested.join("ignored.txt"), "ignore me").unwrap();

    let created = session::amend(&session, created.message()).unwrap();
    let _ = snapshot::capture(&session, created.oid, SnapshotReason::TurnAmend).unwrap();
    let diffs = diff::for_turn(&session, &created).unwrap();

    assert!(diffs.iter().any(|diff| diff.path == "src.txt"));
    assert!(diffs.iter().all(|diff| !diff.path.starts_with("vendor/")));
}

#[test]
fn snapshot_includes_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());
    let session = session::create(repo.clone(), "session-a", base).unwrap();

    let created = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, created.oid, SnapshotReason::TurnCommit).unwrap();
    std::fs::write(dir.path().join("src.txt"), "hello").unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink("src.txt", dir.path().join("link.txt")).unwrap();

    #[cfg(windows)]
    std::os::windows::fs::symlink_file("src.txt", dir.path().join("link.txt")).unwrap();

    let created = session::amend(&session, created.message()).unwrap();
    let _ = snapshot::capture(&session, created.oid, SnapshotReason::TurnAmend).unwrap();
    let diffs = diff::for_turn(&session, &created).unwrap();

    assert!(diffs.iter().any(|diff| diff.path == "src.txt"));
    assert!(diffs.iter().any(|diff| diff.path == "link.txt"));
}

#[test]
fn forked_session_list_stops_at_mismatched_session_trailers() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());

    let source_session = session::create(repo.clone(), "session-a", base).unwrap();
    let source_turn =
        session::commit(&source_session, &support::turn_message("session-a")).unwrap();

    support::commit_head(&repo, dir.path(), "branch.txt", "branch");

    let fork_session = session::create(repo.clone(), "session-b", source_turn.oid).unwrap();
    let fork_turn = session::commit(&fork_session, &support::turn_message("session-b")).unwrap();

    let turns = turn::list(&fork_session).unwrap();

    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].oid, fork_turn.oid);
    assert_eq!(turns[0].session_id().as_ref(), "session-b");
}

#[test]
fn get_rejects_turns_from_other_sessions_even_when_ancestral() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());

    let source_session = session::create(repo.clone(), "session-a", base).unwrap();
    let source_turn =
        session::commit(&source_session, &support::turn_message("session-a")).unwrap();

    support::commit_head(&repo, dir.path(), "branch.txt", "branch");

    let fork_session = session::create(repo.clone(), "session-b", source_turn.oid).unwrap();
    let _fork_turn = session::commit(&fork_session, &support::turn_message("session-b")).unwrap();

    let error = turn::get(&fork_session, source_turn.oid).unwrap_err();
    assert!(error.to_string().contains("turn not found"));
}

#[test]
fn snapshot_lookup_finds_historical_turns() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());
    let session = session::create(repo.clone(), "session-a", base).unwrap();

    let first_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, first_turn.oid, SnapshotReason::TurnCommit).unwrap();
    std::fs::write(dir.path().join("file.txt"), "first").unwrap();
    let second_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, second_turn.oid, SnapshotReason::TurnCommit).unwrap();

    let first_snapshot = snapshot::get(&session, first_turn.oid).unwrap();
    let second_snapshot = snapshot::get(&session, second_turn.oid).unwrap();

    assert_ne!(first_snapshot.oid, second_snapshot.oid);
    assert_eq!(first_snapshot.turn_oid, first_turn.oid);
    assert_eq!(second_snapshot.turn_oid, second_turn.oid);
}
