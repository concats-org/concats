use std::{path::Path, rc::Rc};

use concats_core::{
    Repository, current_head_oid,
    error::{Error, Result},
    session::{self, Session},
    snapshot::{self, SnapshotReason},
    turn::{self, Turn, TurnEntry},
};
use concats_message::Turn as TurnMessage;

use crate::state::{ClaudeLifecycleState, ClaudeStateStore};

const CLAUDE_AGENT_NAME: &str = "Claude";

/// Ensure a session and Claude lifecycle state exist when Claude starts a session.
///
/// # Errors
///
/// Returns an error if the session cannot be opened or created, or the Claude
/// lifecycle state cannot be loaded or initialized.
pub fn on_session_started(worktree_root: &Path, session_id: &str) -> Result<()> {
    let _ = ClaudeSessionLifecycle::load(worktree_root, session_id)?;
    Ok(())
}

/// Start a turn and record the submitted user prompt.
///
/// # Errors
///
/// Returns an error if the session cannot be opened or created, the turn
/// cannot be committed, or the Claude lifecycle state cannot be saved.
pub fn on_prompt_submitted(worktree_root: &Path, session_id: &str, prompt: &str) -> Result<()> {
    ClaudeSessionLifecycle::load(worktree_root, session_id)?.start_prompt(prompt)
}

/// Update the open turn snapshot after file changes.
///
/// # Errors
///
/// Returns an error if the current turn cannot be loaded, the snapshot
/// commit or amendment fails, or the Claude lifecycle state cannot be saved.
pub fn on_files_changed(worktree_root: &Path, session_id: &str) -> Result<()> {
    ClaudeSessionLifecycle::load(worktree_root, session_id)?.record_files_changed()
}

/// Append the assistant response and close the open turn.
///
/// # Errors
///
/// Returns an error if the current turn cannot be loaded, the response
/// commit or amendment fails, or the Claude lifecycle state cannot be saved.
pub fn on_stop(worktree_root: &Path, session_id: &str, response: &str) -> Result<()> {
    ClaudeSessionLifecycle::load(worktree_root, session_id)?.finish_response(response)
}

struct ClaudeSessionLifecycle {
    session: Session,
    state_store: ClaudeStateStore,
    state: ClaudeLifecycleState,
}

impl ClaudeSessionLifecycle {
    fn load(worktree_root: &Path, session_id: &str) -> Result<Self> {
        let repo = Rc::new(Repository::open(worktree_root)?);
        let session = load_or_create_session(repo, session_id)?;
        let state_store = ClaudeStateStore::new(worktree_root);
        let state = state_store.load_or_init(session_id)?;
        Ok(Self {
            session,
            state_store,
            state,
        })
    }

    fn start_prompt(mut self, prompt: &str) -> Result<()> {
        let message = self
            .new_message()?
            .with_entry(TurnEntry::prompt_now(prompt));
        let subject = message
            .suggest_subject()
            .unwrap_or_else(|| "files changed".to_string());
        let message = message.with_subject(subject)?;
        let turn = session::commit(&self.session, &message)?;
        let _ = snapshot::capture(&self.session, turn.oid, SnapshotReason::TurnCommit)?;
        self.save_state(ClaudeLifecycleState::ActiveTurn { turn_oid: turn.oid })
    }

    fn record_files_changed(mut self) -> Result<()> {
        let turn = self.active_turn()?;
        let turn = if let Some(turn) = turn {
            let _ = snapshot::capture(&self.session, turn.oid, SnapshotReason::FilesChanged)?;
            turn
        } else {
            let message = self.new_message()?.with_subject("files changed")?;
            let turn = session::commit(&self.session, &message)?;
            let _ = snapshot::capture(&self.session, turn.oid, SnapshotReason::FilesChanged)?;
            turn
        };
        self.save_state(ClaudeLifecycleState::ActiveTurn { turn_oid: turn.oid })
    }

    fn finish_response(mut self, response: &str) -> Result<()> {
        let turn = self.active_turn()?;
        if let Some(turn) = turn {
            let message = turn
                .message()
                .clone()
                .with_entry(TurnEntry::response_now(response));
            let updated = session::amend(&self.session, &message)?;
            let _ = snapshot::capture(&self.session, updated.oid, SnapshotReason::TurnAmend)?;
        } else {
            let message = self
                .new_message()?
                .with_entry(TurnEntry::response_now(response));
            let subject = message
                .suggest_subject()
                .unwrap_or_else(|| "files changed".to_string());
            let message = message.with_subject(subject)?;
            let turn = session::commit(&self.session, &message)?;
            let _ = snapshot::capture(&self.session, turn.oid, SnapshotReason::TurnCommit)?;
        }
        self.save_state(ClaudeLifecycleState::Idle)
    }

    fn active_turn(&self) -> Result<Option<Turn>> {
        match &self.state {
            ClaudeLifecycleState::Idle => Ok(None),
            ClaudeLifecycleState::ActiveTurn { turn_oid } => {
                turn::get(&self.session, *turn_oid).map(Some)
            }
        }
    }

    fn new_message(&self) -> Result<TurnMessage> {
        Ok(TurnMessage::new(self.session.id.clone()).with_agent_name(CLAUDE_AGENT_NAME)?)
    }

    fn save_state(&mut self, state: ClaudeLifecycleState) -> Result<()> {
        self.state_store.save(self.session.id.as_ref(), &state)?;
        self.state = state;
        Ok(())
    }
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
        turn::{TurnEntry, TurnEntryKind},
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

    #[test]
    fn session_start_initializes_idle_state() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        on_session_started(dir.path(), "session-a").unwrap();

        let store = ClaudeStateStore::new(dir.path());
        assert_eq!(
            store.load("session-a").unwrap(),
            Some(ClaudeLifecycleState::Idle)
        );
    }

    #[test]
    fn load_or_create_session_creates_missing_session() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        let repo = Rc::new(Repository::open(dir.path()).unwrap());
        let session = load_or_create_session(repo.clone(), "session-a").unwrap();

        assert_eq!(session.id, "session-a");
        assert!(session::open(repo, "session-a").is_ok());
    }

    #[test]
    fn load_or_create_session_propagates_non_not_found_session_errors() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        let repo = Rc::new(Repository::open(dir.path()).unwrap());
        let error = load_or_create_session(repo.clone(), "bad\nsession").unwrap_err();

        assert!(matches!(error, Error::Session { .. }));
        assert!(session::list(&repo).unwrap().is_empty());
    }

    #[test]
    fn prompt_starts_active_turn() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        on_prompt_submitted(dir.path(), "session-a", "hello").unwrap();

        let repo = Rc::new(Repository::open(dir.path()).unwrap());
        let store = ClaudeStateStore::new(dir.path());
        let state = store.load("session-a").unwrap().unwrap();
        let turn_oid = match state {
            ClaudeLifecycleState::ActiveTurn { turn_oid } => turn_oid,
            ClaudeLifecycleState::Idle => panic!("expected active Claude turn"),
        };
        let session = session::open(repo, "session-a").unwrap();
        let turn = turn::get(&session, turn_oid).unwrap();
        let snapshot = snapshot::get(&session, turn_oid).unwrap();
        assert_eq!(turn.subject(), "hello");
        assert_eq!(snapshot.reason(), Some(SnapshotReason::TurnCommit));
        assert!(matches!(
            turn.entries(),
            [TurnEntry {
                kind: TurnEntryKind::Prompt { text }
            }] if text == "hello"
        ));
    }

    #[test]
    fn files_changed_refreshes_active_turn_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        on_prompt_submitted(dir.path(), "session-a", "hello").unwrap();
        let store = ClaudeStateStore::new(dir.path());
        let initial_oid = match store.load("session-a").unwrap().unwrap() {
            ClaudeLifecycleState::ActiveTurn { turn_oid } => turn_oid,
            ClaudeLifecycleState::Idle => panic!("expected active Claude turn"),
        };

        std::fs::write(dir.path().join("changed.txt"), "updated").unwrap();
        on_files_changed(dir.path(), "session-a").unwrap();

        let repo = Rc::new(Repository::open(dir.path()).unwrap());
        let updated_oid = match store.load("session-a").unwrap().unwrap() {
            ClaudeLifecycleState::ActiveTurn { turn_oid } => turn_oid,
            ClaudeLifecycleState::Idle => panic!("expected active Claude turn"),
        };
        let session = session::open(repo, "session-a").unwrap();
        let turns = turn::list(&session).unwrap();
        let snapshots = snapshot::list(&session).unwrap();

        assert_eq!(updated_oid, initial_oid);
        assert_eq!(turns.len(), 1);
        assert_eq!(snapshots.len(), 2);
        assert_eq!(
            snapshots.last().unwrap().reason(),
            Some(SnapshotReason::FilesChanged)
        );
    }

    #[test]
    fn stop_closes_active_turn() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        on_prompt_submitted(dir.path(), "session-a", "hello").unwrap();
        on_stop(dir.path(), "session-a", "done").unwrap();

        let repo = Rc::new(Repository::open(dir.path()).unwrap());
        let store = ClaudeStateStore::new(dir.path());
        let session = session::open(repo, "session-a").unwrap();
        let turns = turn::list(&session).unwrap();
        let snapshot = snapshot::get(&session, turns[0].oid).unwrap();

        assert_eq!(
            store.load("session-a").unwrap(),
            Some(ClaudeLifecycleState::Idle)
        );
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
    fn files_changed_without_active_turn_uses_files_changed_subject() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        std::fs::write(dir.path().join("changed.txt"), "updated").unwrap();
        on_files_changed(dir.path(), "session-a").unwrap();

        let repo = Rc::new(Repository::open(dir.path()).unwrap());
        let session = session::open(repo, "session-a").unwrap();
        let turns = turn::list(&session).unwrap();
        let snapshot = snapshot::get(&session, turns[0].oid).unwrap();

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].subject(), "files changed");
        assert_eq!(snapshot.reason(), Some(SnapshotReason::FilesChanged));
        assert!(turns[0].entries().is_empty());
    }

    #[test]
    fn stop_without_active_turn_uses_response_subject() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        on_stop(dir.path(), "session-a", "done now").unwrap();

        let repo = Rc::new(Repository::open(dir.path()).unwrap());
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
