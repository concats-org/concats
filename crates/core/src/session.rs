use std::rc::Rc;

use concats_message::{SESSION_REF_PREFIX, SessionId};
use time::OffsetDateTime;

use crate::{
    error::{Error, Result},
    git::{self, CommitParts, Oid},
    turn::{self, Turn},
};

#[derive(Clone)]
pub struct Session {
    pub id: SessionId,
    pub name: Option<String>,
    repo: Rc<gix::Repository>,
}

impl Session {
    #[must_use]
    pub fn repo(&self) -> &Rc<gix::Repository> {
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
pub fn create(repo: Rc<gix::Repository>, session_id: &str, base: Oid) -> Result<Session> {
    let session_id = parse_session_id(session_id)?;
    let ref_name = ref_name(&session_id);
    if repo.find_reference(&ref_name).is_ok() {
        return Err(Error::session(format!(
            "session already exists: {session_id}"
        )));
    }

    let base_oid = repo.find_commit(base.as_gix()).map_err(Error::git)?.id;
    repo.reference(
        ref_name.as_str(),
        base_oid,
        gix::refs::transaction::PreviousValue::Any,
        "session",
    )
    .map_err(Error::git)?;
    load_session(repo, session_id)
}

/// Open an existing session by its identifier.
///
/// # Errors
///
/// Returns an error if the session ref does not exist, or the session metadata
/// cannot be loaded.
pub fn open(repo: Rc<gix::Repository>, session_id: &str) -> Result<Session> {
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
pub fn list(repo: &Rc<gix::Repository>) -> Result<Vec<Session>> {
    let platform = repo.references().map_err(Error::git)?;
    let refs = platform.prefixed(SESSION_REF_PREFIX).map_err(Error::git)?;
    let mut sessions: Vec<(Session, OffsetDateTime)> = Vec::new();

    for mut reference in refs.filter_map(std::result::Result::ok) {
        let name = reference.name().as_bstr().to_string();
        let Some(session_id) = name.strip_prefix(SESSION_REF_PREFIX) else {
            continue;
        };
        let Ok(session_id) = parse_session_id(session_id) else {
            continue;
        };
        let Ok(tip_id) = reference.peel_to_id() else {
            continue;
        };
        let tip_id = tip_id.detach();
        let Ok(tip) = repo.find_commit(tip_id) else {
            continue;
        };
        let time = tip.time().map_err(Error::git)?;

        sessions.push((
            load_session(repo.clone(), session_id)?,
            git::commit_time(time)?,
        ));
    }

    sessions.sort_by(|left, right| right.1.cmp(&left.1));
    Ok(sessions.into_iter().map(|(session, _)| session).collect())
}

/// Resolve the current session tip commit ID.
///
/// # Errors
///
/// Returns an error if the session ref does not resolve to a commit.
pub fn tip(session: &Session) -> Result<Oid> {
    let tip = resolve_tip(session)?;
    Ok(Oid::from(tip.id))
}

/// Resolve the timestamp of the current session tip commit.
///
/// # Errors
///
/// Returns an error if the session ref does not resolve to a commit.
pub fn modified_at(session: &Session) -> Result<OffsetDateTime> {
    let tip = resolve_tip(session)?;
    git::commit_time(tip.time().map_err(Error::git)?)
}

/// Return whether the session ref tip can reach the given commit.
///
/// # Errors
///
/// Returns an error if the session ref is missing or the git graph query fails.
pub fn contains(session: &Session, oid: Oid) -> Result<bool> {
    let repo = session.repo();
    if repo.find_commit(oid.as_gix()).is_err() {
        return Ok(false);
    }

    let tip = resolve_tip(session)?;
    if tip.id == oid.as_gix() {
        return Ok(true);
    }

    git::reachable_from(repo, tip.id, oid.as_gix())
}

/// List the sessions whose refs can reach the given commit, newest first.
///
/// # Errors
///
/// Returns an error if session enumeration fails or a graph query fails.
pub fn containing(repo: &Rc<gix::Repository>, oid: Oid) -> Result<Vec<Session>> {
    if repo.find_commit(oid.as_gix()).is_err() {
        return Ok(Vec::new());
    }

    let mut sessions: Vec<(Session, OffsetDateTime, Oid)> = Vec::new();
    for session in list(repo)? {
        if contains(&session, oid)? {
            let tip = resolve_tip(&session)?;
            let modified_at = git::commit_time(tip.time().map_err(Error::git)?)?;
            let tip_oid = Oid::from(tip.id);
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

                if git::reachable_from(repo, tip_oid.as_gix(), other.2.as_gix())? {
                    index = position;
                    break;
                }
                if git::reachable_from(repo, other.2.as_gix(), tip_oid.as_gix())? {
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
    let turn_parents = turn_parents(repo, tip.id);
    write_turn(session, message, turn_parents, None)
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
        .parent_ids()
        .next()
        .ok_or_else(|| Error::session("no turn to amend"))?
        .detach();
    let turn_parents = turn_parents(repo, session_parent);
    let minimum_time_seconds = tip.time().map_err(Error::git)?.seconds;

    write_turn(session, message, turn_parents, Some(minimum_time_seconds))
}

pub(crate) fn ref_name(session_id: &SessionId) -> String {
    format!("{SESSION_REF_PREFIX}{session_id}")
}

pub(crate) fn resolve_session_ref<'repo>(
    repo: &'repo gix::Repository,
    session_id: &SessionId,
) -> Result<gix::Commit<'repo>> {
    git::resolve_ref(repo, &ref_name(session_id))
        .ok_or_else(|| Error::session_not_found(session_id.to_string()))
}

fn load_session(repo: Rc<gix::Repository>, session_id: SessionId) -> Result<Session> {
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

fn resolve_tip(session: &Session) -> Result<gix::Commit<'_>> {
    resolve_session_ref(session.repo(), &session.id)
}

fn write_turn(
    session: &Session,
    message: &concats_message::Turn,
    parents: Vec<gix::ObjectId>,
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

    let tree = repo
        .write_object(gix::objs::Tree::empty())
        .map_err(Error::git)?
        .detach();
    let oid = git::commit(
        repo,
        &ref_name(&session.id),
        &message.to_string(),
        CommitParts {
            tree,
            parents,
            minimum_time_seconds,
            log_message: "session",
        },
    )?;
    let turn_commit = repo.find_commit(oid).map_err(Error::git)?;
    Turn::try_from(&turn_commit)
}

fn turn_parents(repo: &gix::Repository, session_parent: gix::ObjectId) -> Vec<gix::ObjectId> {
    let mut parents = vec![session_parent];
    if let Ok(head) = repo.head_id()
        && head.detach() != parents[0]
    {
        parents.push(head.detach());
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
