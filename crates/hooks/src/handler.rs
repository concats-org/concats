use std::rc::Rc;

use concats_core::{
    Repository, current_head_oid,
    error::{Error, Result},
    session::{self, Session},
    snapshot::{self, SnapshotReason},
    turn::{self, Turn, TurnEntry},
};
use concats_message::Turn as TurnMessage;

/// Ensure a session exists when an agent starts a session.
///
/// # Errors
///
/// Returns an error if the session cannot be opened or created.
pub fn on_session_started(repo: Rc<Repository>, session_id: &str) -> Result<()> {
    let _ = load_or_create_session(repo, session_id)?;
    Ok(())
}

/// Start a turn and record the submitted user prompt.
///
/// # Errors
///
/// Returns an error if the session cannot be opened or created, or the turn
/// cannot be committed.
pub fn on_prompt_submitted(
    repo: Rc<Repository>,
    session_id: &str,
    agent_name: &str,
    prompt: &str,
) -> Result<()> {
    let session = load_or_create_session(repo, session_id)?;
    let message = new_message(&session, agent_name)?.with_entry(TurnEntry::prompt_now(prompt));
    let subject = message
        .suggest_subject()
        .unwrap_or_else(|| "files changed".to_string());
    let message = message.with_subject(subject)?;
    let turn = session::commit(&session, &message)?;
    let _ = snapshot::capture(&session, turn.oid, SnapshotReason::TurnCommit)?;
    Ok(())
}

/// Update the open turn snapshot after file changes.
///
/// # Errors
///
/// Returns an error if the current turn cannot be loaded, the snapshot
/// commit or amendment fails.
pub fn on_files_changed(repo: Rc<Repository>, session_id: &str, agent_name: &str) -> Result<()> {
    let session = load_or_create_session(repo, session_id)?;
    let turn = open_turn(&session)?;
    if let Some(turn) = turn {
        let _ = snapshot::capture(&session, turn.oid, SnapshotReason::FilesChanged)?;
    } else {
        let message = new_message(&session, agent_name)?.with_subject("files changed")?;
        let turn = session::commit(&session, &message)?;
        let _ = snapshot::capture(&session, turn.oid, SnapshotReason::FilesChanged)?;
    }
    Ok(())
}

/// Append the assistant response and close the open turn.
///
/// # Errors
///
/// Returns an error if the current turn cannot be loaded, or the response
/// commit or amendment fails.
pub fn on_stop(
    repo: Rc<Repository>,
    session_id: &str,
    agent_name: &str,
    response: &str,
) -> Result<()> {
    let session = load_or_create_session(repo, session_id)?;
    let turn = open_turn(&session)?;
    if let Some(turn) = turn {
        let message = turn
            .message()
            .clone()
            .with_entry(TurnEntry::response_now(response));
        let updated = session::amend(&session, &message)?;
        let _ = snapshot::capture(&session, updated.oid, SnapshotReason::TurnAmend)?;
    } else {
        let message =
            new_message(&session, agent_name)?.with_entry(TurnEntry::response_now(response));
        let subject = message
            .suggest_subject()
            .unwrap_or_else(|| "files changed".to_string());
        let message = message.with_subject(subject)?;
        let turn = session::commit(&session, &message)?;
        let _ = snapshot::capture(&session, turn.oid, SnapshotReason::TurnCommit)?;
    }
    Ok(())
}

fn open_turn(session: &Session) -> Result<Option<Turn>> {
    let tip_oid = session::tip(session)?;
    match turn::get(session, tip_oid) {
        Ok(turn) => {
            if turn.has_response() {
                Ok(None)
            } else {
                Ok(Some(turn))
            }
        }
        Err(_) => Ok(None),
    }
}

fn new_message(session: &Session, agent_name: &str) -> Result<TurnMessage> {
    Ok(TurnMessage::new(session.id.clone()).with_agent_name(agent_name)?)
}

fn load_or_create_session(repo: Rc<Repository>, session_id: &str) -> Result<Session> {
    match session::open(repo.clone(), session_id) {
        Ok(session) => Ok(session),
        Err(Error::SessionNotFound { .. }) => {
            let base = current_head_oid(&repo)?;
            session::create(repo, session_id, base)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::rc::Rc;

    use concats_core::{
        session, snapshot,
        turn::{self, TurnEntry, TurnEntryKind},
    };

    use super::*;

    fn init_repo_with_commit(dir: &std::path::Path) {
        let repo = git2::Repository::init(dir).unwrap();
        let mut index = repo.index().unwrap();
        std::fs::write(dir.join("init.txt"), "init").unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@test").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }

    mod on_session_started {
        use super::*;

        #[test]
        fn creates_session() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(Repository::open(dir.path()).unwrap());
            super::on_session_started(repo.clone(), "session-a").unwrap();

            assert!(session::open(repo, "session-a").is_ok());
        }
    }

    mod load_or_create_session {
        use super::*;

        #[test]
        fn creates_missing_session() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(Repository::open(dir.path()).unwrap());
            let session = super::load_or_create_session(repo.clone(), "session-a").unwrap();

            assert_eq!(session.id, "session-a");
            assert!(session::open(repo, "session-a").is_ok());
        }

        #[test]
        fn propagates_non_not_found_session_errors() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(Repository::open(dir.path()).unwrap());
            let error = super::load_or_create_session(repo.clone(), "bad\nsession").unwrap_err();

            assert!(matches!(error, Error::Session { .. }));
            assert!(session::list(&repo).unwrap().is_empty());
        }
    }

    mod on_prompt_submitted {
        use super::*;

        #[test]
        fn starts_turn() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(Repository::open(dir.path()).unwrap());
            super::on_prompt_submitted(repo.clone(), "session-a", "Test", "hello").unwrap();

            let session = session::open(repo, "session-a").unwrap();
            let turns = turn::list(&session).unwrap();
            let snapshot = snapshot::get(&session, turns[0].oid).unwrap();
            assert_eq!(turns[0].subject(), "hello");
            assert_eq!(turns[0].agent_name(), Some("Test"));
            assert_eq!(snapshot.reason(), Some(SnapshotReason::TurnCommit));
            assert!(matches!(
                turns[0].entries(),
                [TurnEntry {
                    kind: TurnEntryKind::Prompt { text }
                }] if text == "hello"
            ));
        }
    }

    mod on_files_changed {
        use super::*;

        #[test]
        fn refreshes_active_turn_snapshot() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(Repository::open(dir.path()).unwrap());
            super::on_prompt_submitted(repo.clone(), "session-a", "Test", "hello").unwrap();

            std::fs::write(dir.path().join("changed.txt"), "updated").unwrap();
            super::on_files_changed(repo.clone(), "session-a", "Test").unwrap();

            let session = session::open(repo, "session-a").unwrap();
            let turns = turn::list(&session).unwrap();
            let snapshots = snapshot::list(&session).unwrap();

            assert_eq!(turns.len(), 1);
            assert_eq!(snapshots.len(), 2);
            assert_eq!(
                snapshots.last().unwrap().reason(),
                Some(SnapshotReason::FilesChanged)
            );
        }

        #[test]
        fn without_active_turn_uses_files_changed_subject() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(Repository::open(dir.path()).unwrap());
            std::fs::write(dir.path().join("changed.txt"), "updated").unwrap();
            super::on_files_changed(repo.clone(), "session-a", "Test").unwrap();

            let session = session::open(repo, "session-a").unwrap();
            let turns = turn::list(&session).unwrap();
            let snapshot = snapshot::get(&session, turns[0].oid).unwrap();

            assert_eq!(turns.len(), 1);
            assert_eq!(turns[0].subject(), "files changed");
            assert_eq!(snapshot.reason(), Some(SnapshotReason::FilesChanged));
            assert!(turns[0].entries().is_empty());
        }
    }

    mod on_stop {
        use super::*;

        #[test]
        fn closes_active_turn() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(Repository::open(dir.path()).unwrap());
            super::on_prompt_submitted(repo.clone(), "session-a", "Test", "hello").unwrap();
            super::on_stop(repo.clone(), "session-a", "Test", "done").unwrap();

            let session = session::open(repo, "session-a").unwrap();
            let turns = turn::list(&session).unwrap();
            let snapshot = snapshot::get(&session, turns[0].oid).unwrap();

            assert_eq!(snapshot.reason(), Some(SnapshotReason::TurnAmend));
            assert!(matches!(
                turns[0].entries(),
                [
                    TurnEntry {
                        kind: TurnEntryKind::Prompt { text: prompt }
                    },
                    TurnEntry {
                        kind: TurnEntryKind::Response { text: response }
                    }
                ] if prompt == "hello" && response == "done"
            ));
        }

        #[test]
        fn without_active_turn_uses_response_subject() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(Repository::open(dir.path()).unwrap());
            super::on_stop(repo.clone(), "session-a", "Test", "done now").unwrap();

            let session = session::open(repo, "session-a").unwrap();
            let turns = turn::list(&session).unwrap();
            let snapshot = snapshot::get(&session, turns[0].oid).unwrap();

            assert_eq!(turns.len(), 1);
            assert_eq!(turns[0].subject(), "done now");
            assert_eq!(snapshot.reason(), Some(SnapshotReason::TurnCommit));
            assert!(matches!(
                turns[0].entries(),
                [TurnEntry {
                    kind: TurnEntryKind::Response { text }
                }] if text == "done now"
            ));
        }
    }
}
