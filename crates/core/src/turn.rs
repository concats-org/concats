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

impl TryFrom<&git2::Commit<'_>> for Turn {
    type Error = Error;

    fn try_from(commit: &git2::Commit<'_>) -> Result<Self> {
        let message: concats_message::Turn = commit.message_raw().unwrap_or("").parse()?;

        Ok(Self {
            message,
            oid: Oid::from(commit.id()),
            created_at: git::commit_time(commit.time())?,
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
    let mut revwalk = repo.revwalk()?;
    revwalk.push(tip.id())?;
    // We only want to follow the first parent to reconstruct the linear session history
    revwalk.simplify_first_parent()?;

    for oid in revwalk.filter_map(std::result::Result::ok) {
        let commit = repo.find_commit(oid)?;
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
        .find_commit(oid.as_git())
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
/// When `force` is false, the checkout collects conflicting paths and returns
/// them as a [`RestoreConflict`] error. When true, local changes are discarded.
///
/// # Errors
///
/// Returns an error if the turn snapshot cannot be loaded, the checkout
/// would overwrite local changes (safe mode), or the checkout fails.
pub fn restore(session: &Session, turn: &Turn, force: bool) -> Result<()> {
    let repo = session.repo();
    let snapshot = snapshot::get(session, turn.oid)?;
    let commit = repo.find_commit(snapshot.oid.as_git())?;
    let tree = commit.tree()?;
    let mut builder = git2::build::CheckoutBuilder::new();
    if force {
        builder.force();
    } else {
        builder.safe();
        let conflicts: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let conflicts_cb = std::rc::Rc::clone(&conflicts);
        builder.notify_on(git2::CheckoutNotificationType::CONFLICT);
        builder.notify(move |_why, path, _a, _b, _c| {
            if let Some(p) = path {
                conflicts_cb
                    .borrow_mut()
                    .push(p.to_string_lossy().into_owned());
            }
            true
        });
        let result = repo.checkout_tree(tree.as_object(), Some(&mut builder));
        let paths = conflicts.borrow();
        if !paths.is_empty() {
            return Err(Error::restore_conflict(paths.clone()));
        }
        result?;
        return Ok(());
    }
    repo.checkout_tree(tree.as_object(), Some(&mut builder))?;
    Ok(())
}
