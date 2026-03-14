use std::rc::Rc;

use concats_message::SessionId;
use time::OffsetDateTime;

use crate::{
    error::{Error, Result},
    git::{self, Oid},
    turn::{self, Turn},
};

const SESSION_REF_PREFIX: &str = "refs/agent/sessions/";

#[derive(Clone)]
pub struct Session {
    pub id: SessionId,
    pub name: Option<String>,
    repo: Rc<git2::Repository>,
}

impl Session {
    #[must_use]
    pub fn repo(&self) -> &Rc<git2::Repository> {
        &self.repo
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.name == other.name
    }
}

impl Eq for Session {}

/// Create a new session ref rooted at the given base commit.
///
/// # Errors
///
/// Returns an error if the session already exists, the base commit cannot be
/// loaded, or the session ref cannot be written.
pub fn create(repo: Rc<git2::Repository>, session_id: &str, base: Oid) -> Result<Session> {
    let session_id = parse_session_id(session_id)?;
    let ref_name = ref_name(&session_id);
    if repo.find_reference(&ref_name).is_ok() {
        return Err(Error::session(format!(
            "session already exists: {session_id}"
        )));
    }

    let base_commit = repo.find_commit(base.as_git())?;
    repo.reference(&ref_name, base_commit.id(), true, "session")?;
    drop(base_commit);
    load_session(repo, session_id)
}

/// Open an existing session by its identifier.
///
/// # Errors
///
/// Returns an error if the session ref does not exist, or the session metadata
/// cannot be loaded.
pub fn open(repo: Rc<git2::Repository>, session_id: &str) -> Result<Session> {
    let session_id = parse_session_id(session_id)?;
    resolve_session_ref(&repo, &session_id)?;
    load_session(repo, session_id)
}

/// List all sessions stored in the repository, newest first.
///
/// # Errors
///
/// Returns an error if the session refs cannot be enumerated, or a discovered
/// session cannot be loaded.
pub fn list(repo: &Rc<git2::Repository>) -> Result<Vec<Session>> {
    let refs = repo.references_glob(&format!("{SESSION_REF_PREFIX}*"))?;
    let mut sessions: Vec<(Session, OffsetDateTime)> = Vec::new();

    for reference in refs.filter_map(std::result::Result::ok) {
        let Some(name) = reference.name() else {
            continue;
        };
        let Some(session_id) = name.strip_prefix(SESSION_REF_PREFIX) else {
            continue;
        };
        let Ok(session_id) = parse_session_id(session_id) else {
            continue;
        };
        let Ok(tip) = reference.peel_to_commit() else {
            continue;
        };

        sessions.push((
            load_session(repo.clone(), session_id)?,
            git::commit_time(tip.time())?,
        ));
    }

    sessions.sort_by(|left, right| right.1.cmp(&left.1));
    Ok(sessions.into_iter().map(|(session, _)| session).collect())
}

/// Push the session ref to the named remote.
///
/// # Errors
///
/// Returns an error if the session ref cannot be pushed to the remote.
pub fn push(session: &Session, remote: &str) -> Result<()> {
    crate::git::push_ref(session.repo(), remote, &ref_name(&session.id))?;
    Ok(())
}

/// Resolve the current session tip commit ID.
///
/// # Errors
///
/// Returns an error if the session ref does not resolve to a commit.
pub fn tip(session: &Session) -> Result<Oid> {
    let tip = resolve_tip(session)?;
    Ok(Oid::from(tip.id()))
}

/// Resolve the timestamp of the current session tip commit.
///
/// # Errors
///
/// Returns an error if the session ref does not resolve to a commit.
pub fn modified_at(session: &Session) -> Result<OffsetDateTime> {
    let tip = resolve_tip(session)?;
    git::commit_time(tip.time())
}

/// Return whether the session ref tip can reach the given commit.
///
/// # Errors
///
/// Returns an error if the session ref is missing or the git graph query fails.
pub fn contains(session: &Session, oid: Oid) -> Result<bool> {
    let repo = session.repo();
    if repo.find_commit(oid.as_git()).is_err() {
        return Ok(false);
    }

    let tip = resolve_tip(session)?;
    if tip.id() == oid.as_git() {
        return Ok(true);
    }

    Ok(repo.graph_descendant_of(tip.id(), oid.as_git())?)
}

/// List the sessions whose refs can reach the given commit, newest first.
///
/// # Errors
///
/// Returns an error if session enumeration fails or a graph query fails.
pub fn containing(repo: &Rc<git2::Repository>, oid: Oid) -> Result<Vec<Session>> {
    if repo.find_commit(oid.as_git()).is_err() {
        return Ok(Vec::new());
    }

    let mut sessions: Vec<(Session, OffsetDateTime, Oid)> = Vec::new();
    for session in list(repo)? {
        if contains(&session, oid)? {
            let tip = resolve_tip(&session)?;
            let modified_at = git::commit_time(tip.time())?;
            let tip_oid = Oid::from(tip.id());
            drop(tip);
            let mut index = sessions.len();
            for (position, other) in sessions.iter().enumerate() {
                if modified_at > other.1 {
                    index = position;
                    break;
                }
                if modified_at < other.1 {
                    continue;
                }

                if repo.graph_descendant_of(tip_oid.as_git(), other.2.as_git())? {
                    index = position;
                    break;
                }
                if repo.graph_descendant_of(other.2.as_git(), tip_oid.as_git())? {
                    continue;
                }
                if session.id.as_ref() > other.0.id.as_ref() {
                    index = position;
                    break;
                }
            }
            sessions.insert(index, (session, modified_at, tip_oid));
        }
    }

    Ok(sessions
        .into_iter()
        .map(|(session, _, _)| session)
        .collect())
}

/// Create a new turn commit and advance the session ref.
///
/// # Errors
///
/// Returns an error if the session ref is missing, the message does not belong
/// to the session, or the turn commit cannot be written.
pub fn commit(session: &Session, message: &concats_message::Turn) -> Result<Turn> {
    let repo = session.repo();
    let tip = resolve_tip(session)?;
    let turn_parents = turn_parents(repo, tip);
    write_turn(session, message, &turn_parents, None)
}

/// Amend the current session-tip turn and advance the session ref.
///
/// # Errors
///
/// Returns an error if the session ref is missing, the current tip is not a
/// turn commit, the message does not belong to the session, or the rewritten
/// turn commit cannot be written.
pub fn amend(session: &Session, message: &concats_message::Turn) -> Result<Turn> {
    let repo = session.repo();
    let tip = resolve_tip(session)?;
    let turn = Turn::try_from(&tip).map_err(|_| Error::session("no turn to amend"))?;
    if turn.session_id() != &session.id {
        return Err(Error::session("no turn to amend"));
    }

    let session_parent = tip
        .parent(0)
        .map_err(|_| Error::session("no turn to amend"))?;
    let turn_parents = turn_parents(repo, session_parent);
    let minimum_time_seconds = tip.time().seconds();

    write_turn(session, message, &turn_parents, Some(minimum_time_seconds))
}

pub(crate) fn ref_name(session_id: &SessionId) -> String {
    format!("{SESSION_REF_PREFIX}{session_id}")
}

pub(crate) fn resolve_session_ref<'repo>(
    repo: &'repo git2::Repository,
    session_id: &SessionId,
) -> Result<git2::Commit<'repo>> {
    git::resolve_ref(repo, &ref_name(session_id))
        .ok_or_else(|| Error::session_not_found(session_id.to_string()))
}

fn load_session(repo: Rc<git2::Repository>, session_id: SessionId) -> Result<Session> {
    let base = Session {
        id: session_id,
        name: None,
        repo,
    };

    let name = turn::list(&base)?
        .first()
        .map(|turn| turn.subject().to_string());

    Ok(Session { name, ..base })
}

fn parse_session_id(value: &str) -> Result<SessionId> {
    value
        .parse()
        .map_err(|error: concats_message::Error| Error::session(error.to_string()))
}

fn resolve_tip(session: &Session) -> Result<git2::Commit<'_>> {
    resolve_session_ref(session.repo(), &session.id)
}

fn write_turn(
    session: &Session,
    message: &concats_message::Turn,
    parents: &[git2::Commit<'_>],
    minimum_time_seconds: Option<i64>,
) -> Result<Turn> {
    let repo = session.repo();
    if message.session_id() != &session.id {
        return Err(Error::session(format!(
            "turn message belongs to {}, expected {}",
            message.session_id(),
            session.id
        )));
    }

    let tree = repo.find_tree(git::empty_tree(repo)?)?;
    let sig = git::signature(repo, minimum_time_seconds)?;
    let parent_refs = parents.iter().collect::<Vec<_>>();
    let oid = repo.commit(None, &sig, &sig, &message.to_string(), &tree, &parent_refs)?;
    let turn_commit = repo.find_commit(oid)?;

    repo.reference(&ref_name(&session.id), turn_commit.id(), true, "session")?;
    Turn::try_from(&turn_commit)
}

fn turn_parents<'repo>(
    repo: &'repo git2::Repository,
    session_parent: git2::Commit<'repo>,
) -> Vec<git2::Commit<'repo>> {
    let mut parents = vec![session_parent];
    if let Some(head) = repo.head().ok().and_then(|h| h.peel_to_commit().ok())
        && head.id() != parents[0].id()
    {
        parents.push(head);
    }
    parents
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn session_id_parses_bare_value() {
        let session_id: SessionId = "session-a".parse().unwrap();

        assert_eq!(session_id.as_ref(), "session-a");
        assert_eq!(session_id.to_string(), "session-a");
    }

    #[test]
    fn session_id_rejects_empty_value() {
        let error = "".parse::<SessionId>().unwrap_err();

        assert!(error.to_string().contains("must not be empty"));
    }

    #[test]
    fn session_id_rejects_newlines() {
        let error = "session-a\nsession-b".parse::<SessionId>().unwrap_err();

        assert!(error.to_string().contains("newline"));
    }
}
