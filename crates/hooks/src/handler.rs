use std::rc::Rc;

use concats_core::{
    Repository, current_head_oid,
    error::{Error, Result},
    session::{self, Session},
    snapshot::{self, SnapshotReason},
    turn::{self, Turn, TurnEntry},
};
use concats_message::Turn as TurnMessage;

/// Start a turn and record the submitted user prompt.
///
/// # Errors
///
/// Returns an error if the turn cannot be committed.
pub fn on_prompt_submitted(session: &Session, agent_name: &str, prompt: &str) -> Result<()> {
    let turn = commit_turn(session, agent_name, &[TurnEntry::prompt_now(prompt)])?;
    let _ = snapshot::capture(session, turn.oid, SnapshotReason::TurnCommit)?;
    Ok(())
}

/// Update the open turn snapshot after file changes.
///
/// # Errors
///
/// Returns an error if the current turn cannot be loaded, the snapshot
/// commit or amendment fails.
pub fn on_files_changed(session: &Session, agent_name: &str) -> Result<()> {
    let turn = match open_turn(session)? {
        Some(turn) => turn,
        None => commit_turn(session, agent_name, &[])?,
    };
    let _ = snapshot::capture(session, turn.oid, SnapshotReason::FilesChanged)?;
    Ok(())
}

/// Append one or more transcript entries and close the open turn.
///
/// Accepts a slice so adapters that extract multiple artifacts from a single
/// agent Stop (e.g. Claude's plan-mode plans plus user feedback) can land them
/// as separate entries on the same turn.
///
/// # Errors
///
/// Returns an error if the current turn cannot be loaded, or the entry commit
/// or amendment fails.
pub fn on_stop(session: &Session, agent_name: &str, entries: &[TurnEntry]) -> Result<()> {
    let (turn, reason) = match open_turn(session)? {
        Some(turn) => {
            let message = entries
                .iter()
                .cloned()
                .fold(turn.message().clone(), TurnMessage::with_entry);
            (
                session::amend(session, &message)?,
                SnapshotReason::TurnAmend,
            )
        }
        None => (
            commit_turn(session, agent_name, entries)?,
            SnapshotReason::TurnCommit,
        ),
    };
    let _ = snapshot::capture(session, turn.oid, reason)?;
    Ok(())
}

/// Open the session, creating it at `HEAD` on its first hook.
///
/// # Errors
///
/// Returns an error if the session cannot be opened or created.
pub fn load_or_create_session(repo: Rc<Repository>, session_id: &str) -> Result<Session> {
    match session::open(repo.clone(), session_id) {
        Ok(session) => Ok(session),
        Err(Error::SessionNotFound { .. }) => {
            let base = current_head_oid(&repo)?;
            session::create(repo, session_id, base)
        }
        Err(error) => Err(error),
    }
}

/// Commit a new turn holding `entries`, titled after the first one with a
/// summary.
fn commit_turn(session: &Session, agent_name: &str, entries: &[TurnEntry]) -> Result<Turn> {
    let message = entries.iter().cloned().fold(
        TurnMessage::new(session.id.clone()).with_agent_name(agent_name)?,
        TurnMessage::with_entry,
    );
    let subject = message
        .suggest_subject()
        .unwrap_or_else(|| "files changed".to_string());
    session::commit(session, &message.with_subject(subject)?)
}

fn open_turn(session: &Session) -> Result<Option<Turn>> {
    let tip_oid = session::tip(session)?;
    // NOTE: A tip that is not one of this session's turns, like a new
    // session's base commit, means no turn is open.
    Ok(turn::get(session, tip_oid)
        .ok()
        .filter(|turn| !turn.has_response()))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::rc::Rc;

    use concats_core::{
        session, snapshot,
        turn::{self, TurnEntry},
    };
    use concats_fixture::init_repo_with_commit;

    use super::*;

    mod load_or_create_session {
        use super::*;

        #[test]
        fn creates_missing_session() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(gix::open(dir.path()).unwrap());
            let session = super::load_or_create_session(repo.clone(), "session-a").unwrap();

            assert_eq!(session.id, "session-a");
            assert!(session::open(repo, "session-a").is_ok());
        }

        #[test]
        fn propagates_non_not_found_session_errors() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(gix::open(dir.path()).unwrap());
            let error = super::load_or_create_session(repo.clone(), "bad\nsession").unwrap_err();

            assert!(matches!(error, Error::Session { .. }));
            assert_eq!(session::list(&repo).unwrap(), [] as [session::Session; 0]);
        }
    }

    mod on_prompt_submitted {
        use super::*;

        #[test]
        fn starts_turn() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(gix::open(dir.path()).unwrap());
            let session = super::load_or_create_session(repo, "session-a").unwrap();
            super::on_prompt_submitted(&session, "Test", "hello").unwrap();

            let turns = turn::list(&session).unwrap();
            let snapshot = snapshot::get(&session, turns[0].oid).unwrap();
            assert_eq!(turns[0].subject(), "hello");
            assert_eq!(turns[0].message().agent_name(), Some("Test"));
            assert_eq!(snapshot.reason(), Some(SnapshotReason::TurnCommit));
            assert_eq!(turns[0].entries(), [TurnEntry::prompt_now("hello")]);
        }
    }

    mod on_files_changed {
        use super::*;

        #[test]
        fn refreshes_active_turn_snapshot() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(gix::open(dir.path()).unwrap());
            let session = super::load_or_create_session(repo, "session-a").unwrap();
            super::on_prompt_submitted(&session, "Test", "hello").unwrap();

            std::fs::write(dir.path().join("changed.txt"), "updated").unwrap();
            super::on_files_changed(&session, "Test").unwrap();

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

            let repo = Rc::new(gix::open(dir.path()).unwrap());
            let session = super::load_or_create_session(repo, "session-a").unwrap();
            std::fs::write(dir.path().join("changed.txt"), "updated").unwrap();
            super::on_files_changed(&session, "Test").unwrap();

            let turns = turn::list(&session).unwrap();
            let snapshot = snapshot::get(&session, turns[0].oid).unwrap();

            assert_eq!(turns.len(), 1);
            assert_eq!(turns[0].subject(), "files changed");
            assert_eq!(snapshot.reason(), Some(SnapshotReason::FilesChanged));
            assert_eq!(turns[0].entries(), []);
        }
    }

    mod on_stop {
        use super::*;

        #[test]
        fn closes_active_turn() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(gix::open(dir.path()).unwrap());
            let session = super::load_or_create_session(repo, "session-a").unwrap();
            super::on_prompt_submitted(&session, "Test", "hello").unwrap();
            super::on_stop(&session, "Test", &[TurnEntry::response_now("done")]).unwrap();

            let turns = turn::list(&session).unwrap();
            let snapshot = snapshot::get(&session, turns[0].oid).unwrap();

            assert_eq!(snapshot.reason(), Some(SnapshotReason::TurnAmend));
            assert_eq!(
                turns[0].entries(),
                [
                    TurnEntry::prompt_now("hello"),
                    TurnEntry::response_now("done")
                ]
            );
        }

        #[test]
        fn closes_active_turn_with_multiple_responses() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(gix::open(dir.path()).unwrap());
            let session = super::load_or_create_session(repo, "session-a").unwrap();
            super::on_prompt_submitted(&session, "Test", "hello").unwrap();
            super::on_stop(
                &session,
                "Test",
                &[
                    TurnEntry::response_now("first"),
                    TurnEntry::response_now("second"),
                ],
            )
            .unwrap();

            let turns = turn::list(&session).unwrap();
            let snapshot = snapshot::get(&session, turns[0].oid).unwrap();

            assert_eq!(snapshot.reason(), Some(SnapshotReason::TurnAmend));
            assert_eq!(
                turns[0].entries(),
                [
                    TurnEntry::prompt_now("hello"),
                    TurnEntry::response_now("first"),
                    TurnEntry::response_now("second")
                ]
            );
        }

        #[test]
        fn without_active_turn_uses_response_subject() {
            let dir = tempfile::tempdir().unwrap();
            init_repo_with_commit(dir.path());

            let repo = Rc::new(gix::open(dir.path()).unwrap());
            let session = super::load_or_create_session(repo, "session-a").unwrap();
            super::on_stop(&session, "Test", &[TurnEntry::response_now("done now")]).unwrap();

            let turns = turn::list(&session).unwrap();
            let snapshot = snapshot::get(&session, turns[0].oid).unwrap();

            assert_eq!(turns.len(), 1);
            assert_eq!(turns[0].subject(), "done now");
            assert_eq!(snapshot.reason(), Some(SnapshotReason::TurnCommit));
            assert_eq!(turns[0].entries(), [TurnEntry::response_now("done now")]);
        }
    }
}
