#![allow(clippy::cognitive_complexity)]

pub mod support;

use std::rc::Rc;

use concats_core::{
    Oid,
    error::Error,
    session, snapshot,
    snapshot::SnapshotReason,
    turn,
    turn::{TurnEntry, TurnEntryKind},
};

#[test]
fn create_session_without_turns() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());

    let session = session::create(repo.clone(), "session-a", head).unwrap();

    assert_eq!(session.id, "session-a");
    assert_eq!(session.name, None);
    assert!(turn::list(&session).unwrap().is_empty());
    assert!(snapshot::list(&session).unwrap().is_empty());
}

#[test]
fn open_and_list_include_empty_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());

    session::create(repo.clone(), "session-a", head).unwrap();

    let loaded = session::open(repo.clone(), "session-a").unwrap();
    assert_eq!(loaded.name, None);

    let sessions = session::list(&repo).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "session-a");
}

#[test]
fn open_missing_session_returns_session_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));

    let error = session::open(repo, "missing-session").unwrap_err();

    assert!(matches!(
        error,
        Error::SessionNotFound { session_id } if session_id == "missing-session"
    ));
}

#[test]
fn session_name_uses_first_turn_subject() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let message = support::turn_message("session-a")
        .with_subject("hello world from prompt")
        .unwrap()
        .with_entry(TurnEntry::prompt_now("  hello \n world from   prompt "));
    session::commit(&session, &message).unwrap();

    let loaded = session::open(repo.clone(), "session-a").unwrap();
    assert_eq!(loaded.name.as_deref(), Some("hello world from prompt"));
}

#[test]
fn session_name_falls_back_to_turn_without_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    session::commit(&session, &support::turn_message("session-a")).unwrap();

    let loaded = session::open(repo.clone(), "session-a").unwrap();
    assert_eq!(loaded.name.as_deref(), Some("turn"));
}

#[test]
fn tip_and_modified_at_follow_committed_turn() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let message = support::turn_message("session-a").with_entry(TurnEntry::prompt_now("prompt"));
    let turn = session::commit(&session, &message).unwrap();

    assert_eq!(session::tip(&session).unwrap(), turn.oid);
    assert_eq!(session::modified_at(&session).unwrap(), turn.created_at);
}

#[test]
fn commit_does_not_create_snapshot_until_capture() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let turn = session::commit(&session, &support::turn_message("session-a")).unwrap();

    assert!(snapshot::get(&session, turn.oid).is_err());
    assert!(snapshot::list(&session).unwrap().is_empty());
}

#[test]
fn amend_rewrites_tip_turn_without_touching_snapshot_ref() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let message = support::turn_message("session-a").with_entry(TurnEntry::prompt_now("prompt"));
    let turn = session::commit(&session, &message).unwrap();
    assert!(snapshot::get(&session, turn.oid).is_err());
    let initial_snapshot = snapshot::capture(&session, turn.oid, SnapshotReason::TurnCommit)
        .unwrap()
        .unwrap();

    let amended = turn
        .message()
        .clone()
        .with_entry(TurnEntry::response_now("done"));
    let updated = session::amend(&session, &amended).unwrap();

    assert_ne!(updated.oid, turn.oid);
    assert_eq!(session::tip(&session).unwrap(), updated.oid);
    assert_eq!(
        snapshot::get(&session, turn.oid).unwrap().oid,
        initial_snapshot.oid
    );
    assert!(snapshot::get(&session, updated.oid).is_err());
    assert_eq!(updated.len(), 2);
    assert!(matches!(
        updated.entries()[0].kind,
        TurnEntryKind::Prompt { .. }
    ));
    assert!(matches!(
        updated.entries()[1].kind,
        TurnEntryKind::Response { .. }
    ));
}

#[test]
fn capture_after_amend_creates_new_snapshot_even_when_tree_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let message = support::turn_message("session-a").with_entry(TurnEntry::prompt_now("prompt"));
    let turn = session::commit(&session, &message).unwrap();
    let initial_snapshot = snapshot::capture(&session, turn.oid, SnapshotReason::TurnCommit)
        .unwrap()
        .unwrap();

    let amended = turn
        .message()
        .clone()
        .with_entry(TurnEntry::response_now("done"));
    let updated = session::amend(&session, &amended).unwrap();
    let updated_snapshot = snapshot::capture(&session, updated.oid, SnapshotReason::TurnAmend)
        .unwrap()
        .unwrap();

    assert_ne!(updated_snapshot.oid, initial_snapshot.oid);
    assert_eq!(
        snapshot::get(&session, updated.oid).unwrap().oid,
        updated_snapshot.oid
    );
    assert_eq!(snapshot::list(&session).unwrap().len(), 2);
}

#[test]
fn capture_skips_same_turn_noop_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let snapshot = snapshot::capture(&session, turn.oid, SnapshotReason::TurnCommit)
        .unwrap()
        .unwrap();

    assert!(
        snapshot::capture(&session, turn.oid, SnapshotReason::FilesChanged)
            .unwrap()
            .is_none()
    );
    assert_eq!(snapshot::get(&session, turn.oid).unwrap().oid, snapshot.oid);
    assert_eq!(snapshot::list(&session).unwrap().len(), 1);
}

#[test]
fn get_returns_latest_snapshot_for_turn() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let first = snapshot::capture(&session, turn.oid, SnapshotReason::TurnCommit)
        .unwrap()
        .unwrap();

    std::fs::write(dir.path().join("next.txt"), "next").unwrap();
    let latest = snapshot::capture(&session, turn.oid, SnapshotReason::FilesChanged)
        .unwrap()
        .unwrap();

    assert_ne!(first.oid, latest.oid);
    assert_eq!(snapshot::get(&session, turn.oid).unwrap().oid, latest.oid);
    assert_eq!(snapshot::list(&session).unwrap().len(), 2);
}

#[test]
fn older_snapshots_without_reason_still_parse() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let turn_commit = repo.find_commit(turn.oid.as_gix()).unwrap();
    let sig = gix::actor::Signature {
        name: "test".into(),
        email: "test@test".into(),
        time: gix::date::Time {
            seconds: 0,
            offset: 0,
        },
    };
    let commit = gix::objs::Commit {
        tree: repo.head_commit().unwrap().tree_id().unwrap().detach(),
        parents: [turn_commit.id].into_iter().collect(),
        author: sig.clone(),
        committer: sig,
        encoding: None,
        message: "snapshot\n\nSession: session-a".into(),
        extra_headers: Vec::new(),
    };
    let oid = repo.write_object(&commit).unwrap().detach();
    repo.reference(
        support::snapshot_ref_name("session-a").as_str(),
        oid,
        gix::refs::transaction::PreviousValue::Any,
        "snapshot",
    )
    .unwrap();

    let snapshot = snapshot::get(&session, turn.oid).unwrap();

    assert_eq!(snapshot.reason(), None);
}

#[test]
fn amend_requires_turn_tip() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let error = session::amend(&session, &support::turn_message("session-a")).unwrap_err();
    assert!(error.to_string().contains("no turn to amend"));
}

#[test]
fn list_returns_empty_when_session_has_no_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    assert!(snapshot::list(&session).unwrap().is_empty());
}
