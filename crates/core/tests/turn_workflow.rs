pub mod support;

use concats_core::{session, turn, turn::TurnEntry};

#[test]
fn list_and_get_turns() {
    let (_dir, _, session, _) = support::session("session-a");

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
    let (_dir, _, session, _) = support::session("session-a");

    let message = support::turn_message("session-a")
        .with_entry(TurnEntry::prompt_now("prompt"))
        .with_entry(TurnEntry::response_now("first"));
    let created = session::commit(&session, &message).unwrap();
    let copied = created.message().clone();

    assert_eq!(copied.subject(), "turn");
    assert_eq!(copied.agent_name(), Some("test-agent"));
    assert_eq!(
        copied.entries(),
        [
            TurnEntry::prompt_now("prompt"),
            TurnEntry::response_now("first")
        ]
    );
}

#[test]
fn forked_session_list_stops_at_mismatched_session_trailers() {
    let (dir, repo, source_session, _) = support::session("session-a");

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
    let (dir, repo, source_session, _) = support::session("session-a");

    let source_turn =
        session::commit(&source_session, &support::turn_message("session-a")).unwrap();

    support::commit_head(&repo, dir.path(), "branch.txt", "branch");

    let fork_session = session::create(repo.clone(), "session-b", source_turn.oid).unwrap();
    let _fork_turn = session::commit(&fork_session, &support::turn_message("session-b")).unwrap();

    let error = turn::get(&fork_session, source_turn.oid).unwrap_err();
    assert!(error.to_string().contains("turn not found"));
}
