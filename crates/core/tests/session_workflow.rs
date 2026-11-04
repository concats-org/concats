#![allow(clippy::cognitive_complexity)]

pub mod support;

use std::rc::Rc;

use concats_core::{error::Error, session, turn, turn::TurnEntry};

#[test]
fn create_session_without_turns() {
    let (_dir, _, session, _) = support::session("session-a");

    assert_eq!(session.id, "session-a");
    assert_eq!(turn::list(&session).unwrap(), [] as [turn::Turn; 0]);
}

#[test]
fn open_and_list_include_empty_sessions() {
    let (_dir, repo, _, _) = support::session("session-a");

    let loaded = session::open(repo.clone(), "session-a").unwrap();
    assert_eq!(loaded.id, "session-a");

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
fn tip_and_modified_at_follow_committed_turn() {
    let (_dir, _, session, _) = support::session("session-a");

    let message = support::turn_message("session-a").with_entry(TurnEntry::prompt_now("prompt"));
    let turn = session::commit(&session, &message).unwrap();

    assert_eq!(session::tip(&session).unwrap(), turn.oid);
    assert_eq!(session::modified_at(&session).unwrap(), turn.created_at);
}

#[test]
fn amend_rewrites_tip_turn() {
    let (_dir, _, session, _) = support::session("session-a");

    let message = support::turn_message("session-a").with_entry(TurnEntry::prompt_now("prompt"));
    let turn = session::commit(&session, &message).unwrap();

    let amended = turn
        .message()
        .clone()
        .with_entry(TurnEntry::response_now("done"));
    let updated = session::amend(&session, &amended).unwrap();

    assert_ne!(updated.oid, turn.oid);
    assert_eq!(session::tip(&session).unwrap(), updated.oid);
    assert_eq!(
        updated.entries(),
        [
            TurnEntry::prompt_now("prompt"),
            TurnEntry::response_now("done")
        ]
    );
}

#[test]
fn amend_requires_turn_tip() {
    let (_dir, _, session, _) = support::session("session-a");

    let error = session::amend(&session, &support::turn_message("session-a")).unwrap_err();
    assert!(error.to_string().contains("no turn to amend"));
}
