use concats_message::SessionId;
pub use concats_message::{TurnEntry, TurnEntryKind};
use time::OffsetDateTime;

use crate::{
    error::{Error, Result},
    git::{self, Oid},
    session::Session,
    snapshot,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    message: concats_message::Turn,
    pub oid: Oid,
    pub created_at: OffsetDateTime,
}

impl Turn {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.message.entries().is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.message.entries().len()
    }

    #[must_use]
    pub fn has_response(&self) -> bool {
        self.entries()
            .iter()
            .any(|e| matches!(e.kind, TurnEntryKind::Response { .. }))
    }

    #[must_use]
    pub fn entries(&self) -> &[TurnEntry] {
        self.message.entries()
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        self.message.subject()
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        self.message.session_id()
    }

    #[must_use]
    pub fn agent_name(&self) -> Option<&str> {
        self.message.agent_name()
    }

    #[must_use]
    pub fn message(&self) -> &concats_message::Turn {
        &self.message
    }
}

impl TryFrom<&gix::Commit<'_>> for Turn {
    type Error = Error;

    fn try_from(commit: &gix::Commit<'_>) -> Result<Self> {
        let message: concats_message::Turn = commit.message_raw_sloppy().to_string().parse()?;

        Ok(Self {
            message,
            oid: Oid::from(commit.id),
            created_at: git::commit_time(commit.time().map_err(Error::git)?)?,
        })
    }
}

/// List all turns for a session in creation order.
///
/// # Errors
///
/// Returns an error if the session ref is missing, or any turn commit cannot
/// be loaded.
pub fn list(session: &Session) -> Result<Vec<Turn>> {
    let repo = session.repo();
    let tip = crate::session::resolve_session_ref(repo, &session.id)?;

    let mut turns = Vec::new();
    // We only want to follow the first parent to reconstruct the linear session history
    let revwalk = repo
        .rev_walk([tip.id])
        .first_parent_only()
        .all()
        .map_err(Error::git)?;

    for info in revwalk.filter_map(std::result::Result::ok) {
        let commit = repo.find_commit(info.id).map_err(Error::git)?;
        let Ok(turn) = Turn::try_from(&commit) else {
            break;
        };
        if turn.session_id().as_ref() != session.id.as_ref() {
            break;
        }
        turns.push(turn);
    }

    turns.reverse();
    Ok(turns)
}

/// Load a single turn by object ID.
///
/// # Errors
///
/// Returns an error if turn listing fails or the requested turn is not present
/// in the session history.
pub fn get(session: &Session, oid: Oid) -> Result<Turn> {
    let commit = session
        .repo()
        .find_commit(oid.as_gix())
        .map_err(|_| Error::session(format!("turn not found: {oid}")))?;
    let turn =
        Turn::try_from(&commit).map_err(|_| Error::session(format!("turn not found: {oid}")))?;
    if turn.session_id() != &session.id || !crate::session::contains(session, oid)? {
        return Err(Error::session(format!("turn not found: {oid}")));
    }

    Ok(turn)
}

/// Restore the turn snapshot into the repository working tree.
///
/// When `force` is false, conflicting paths are collected and returned as a
/// [`RestoreConflict`](Error::RestoreConflict) error. When true, local changes
/// are discarded.
///
/// # Errors
///
/// Returns an error if the turn snapshot cannot be loaded, the checkout
/// would overwrite local changes (safe mode), or the checkout fails.
pub fn restore(session: &Session, turn: &Turn, force: bool) -> Result<()> {
    let repo = session.repo();
    let snapshot = snapshot::get(session, turn.oid)?;
    let commit = repo
        .find_commit(snapshot.oid.as_gix())
        .map_err(Error::git)?;
    let tree_id = commit.tree_id().map_err(Error::git)?.detach();
    git::checkout_tree(repo, tree_id, force)
}
