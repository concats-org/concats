use std::path::{Path, PathBuf};

use time::{OffsetDateTime, UtcOffset};

use crate::{
    checkpoint::{self, Checkpoint, Draft},
    error::{Error, Result},
    git::{self, Oid},
    transcript::{self, decode_commit_message},
};

const SESSION_REF_PREFIX: &str = "refs/agent/sessions/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub name: Option<String>,
    pub repo_path: PathBuf,
}

/// Create a new session ref rooted at the given base commit.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, the session already
/// exists, the base commit cannot be loaded, or the session ref cannot be
/// written.
pub fn create(repo_path: &Path, session_id: &str, base: Oid) -> Result<Session> {
    let repo = git2::Repository::open(repo_path)?;
    let ref_name = ref_name(session_id);
    if repo.find_reference(&ref_name).is_ok() {
        return Err(Error::session(format!(
            "session already exists: {session_id}"
        )));
    }

    let base_commit = repo.find_commit(base.as_git())?;
    repo.reference(&ref_name, base_commit.id(), true, "session")?;
    build_session(repo_path.to_path_buf(), session_id)
}

/// Open an existing session by its identifier.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, the session ref does
/// not exist, or the session metadata cannot be loaded.
pub fn open(repo_path: &Path, session_id: &str) -> Result<Session> {
    let repo = git2::Repository::open(repo_path)?;
    resolve_ref(&repo, &ref_name(session_id))
        .ok_or_else(|| Error::session(format!("session not found: {session_id}")))?;
    build_session(repo_path.to_path_buf(), session_id)
}

/// List all sessions stored in the repository, newest first.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, the session refs
/// cannot be enumerated, or a discovered session cannot be loaded.
pub fn list(repo_path: &Path) -> Result<Vec<Session>> {
    let repo = git2::Repository::open(repo_path)?;
    let refs = repo.references_glob(&format!("{SESSION_REF_PREFIX}*"))?;
    let mut sessions = Vec::new();

    for reference in refs.filter_map(std::result::Result::ok) {
        let Some(name) = reference.name() else {
            continue;
        };
        let Some(session_id) = name.strip_prefix(SESSION_REF_PREFIX) else {
            continue;
        };
        let Ok(tip) = reference.peel_to_commit() else {
            continue;
        };

        sessions.push((
            build_session(repo_path.to_path_buf(), session_id)?,
            commit_time(tip.time())?,
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
    crate::git::push_ref(&session.repo_path, remote, &ref_name(&session.id))?;
    Ok(())
}

/// Resolve the current session tip commit ID.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened or the session ref does
/// not resolve to a commit.
pub fn tip(session: &Session) -> Result<Oid> {
    let repo = git2::Repository::open(&session.repo_path)?;
    let tip = resolve_tip(&repo, session)?;
    Ok(Oid::from(tip.id()))
}

/// Resolve the timestamp of the current session tip commit.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened or the session ref does
/// not resolve to a commit.
pub fn modified_at(session: &Session) -> Result<OffsetDateTime> {
    let repo = git2::Repository::open(&session.repo_path)?;
    let tip = resolve_tip(&repo, session)?;
    commit_time(tip.time())
}

/// Create a new checkpoint commit and advance the session tip to it.
///
/// Parent 0 is the current session tip (session chain). Parent 1 is the
/// current branch HEAD when it differs from parent 0, linking the checkpoint
/// to ordinary branch history.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, the session ref is
/// missing, the draft transcript is invalid, the working tree snapshot cannot
/// be captured, or the checkpoint commit cannot be written.
pub fn commit(session: &Session, draft: &Draft) -> Result<Checkpoint> {
    let repo = git2::Repository::open(&session.repo_path)?;
    let tip = resolve_tip(&repo, session)?;
    let parents = parents_with_head(&repo, &tip);
    write_checkpoint(&repo, session, draft, &parents)
}

/// Amend the current session-tip checkpoint and advance the session tip.
///
/// Preserves parent 0 (the session lineage) and refreshes parent 1 to the
/// current branch HEAD if HEAD has changed.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, the session ref is
/// missing, the current tip is not a checkpoint commit, the draft transcript is
/// invalid, the working tree snapshot cannot be captured, or the amended
/// checkpoint cannot be written.
pub fn amend(session: &Session, draft: &Draft) -> Result<Checkpoint> {
    let repo = git2::Repository::open(&session.repo_path)?;
    let tip = resolve_tip(&repo, session)?;
    if decode_commit_message(tip.message().unwrap_or("")).is_none() {
        return Err(Error::session("no checkpoint to amend"));
    }

    // Preserve parent 0 (session lineage), refresh parent 1 to current HEAD.
    let mut parents: Vec<git2::Commit<'_>> = Vec::new();
    if let Ok(first_parent) = tip.parent(0) {
        parents.push(first_parent);
    }
    // Refresh parent 1: add current HEAD if it differs from parent 0.
    if let Ok(head_ref) = repo.head()
        && let Ok(head) = head_ref.peel_to_commit()
        && !parents.iter().any(|p| p.id() == head.id())
    {
        parents.push(head);
    }
    write_checkpoint(&repo, session, draft, &parents)
}

pub(crate) fn ref_name(session_id: &str) -> String {
    format!("{SESSION_REF_PREFIX}{session_id}")
}

pub(crate) fn resolve_ref<'repo>(
    repo: &'repo git2::Repository,
    ref_name: &str,
) -> Option<git2::Commit<'repo>> {
    repo.find_reference(ref_name)
        .ok()
        .and_then(|reference| reference.peel_to_commit().ok())
}

pub(crate) fn signature(repo: &git2::Repository) -> Result<git2::Signature<'static>> {
    Ok(repo
        .signature()
        .or_else(|_| git2::Signature::now("concats", "concats@checkpoint"))?)
}

pub(crate) fn commit_time(time: git2::Time) -> Result<OffsetDateTime> {
    let timestamp = OffsetDateTime::from_unix_timestamp(time.seconds())
        .map_err(|error| Error::session(format!("invalid git commit timestamp: {error}")))?;
    let offset =
        UtcOffset::from_whole_seconds(time.offset_minutes() * 60).unwrap_or(UtcOffset::UTC);
    Ok(timestamp.to_offset(offset))
}

fn build_session(repo_path: PathBuf, session_id: &str) -> Result<Session> {
    let base = Session {
        id: session_id.to_string(),
        name: None,
        repo_path,
    };

    let name = checkpoint::list(&base)?
        .first()
        .and_then(|checkpoint| checkpoint.transcript.label(None));

    Ok(Session { name, ..base })
}

fn resolve_tip<'repo>(
    repo: &'repo git2::Repository,
    session: &Session,
) -> Result<git2::Commit<'repo>> {
    resolve_ref(repo, &ref_name(&session.id))
        .ok_or_else(|| Error::session(format!("session not found: {}", session.id)))
}

/// Build the parent list for a new checkpoint: session tip as parent 0, and
/// current branch HEAD as parent 1 when it differs from the tip.
fn parents_with_head<'repo>(
    repo: &'repo git2::Repository,
    tip: &git2::Commit<'repo>,
) -> Vec<git2::Commit<'repo>> {
    let mut parents = vec![tip.clone()];
    if let Ok(head_ref) = repo.head()
        && let Ok(head) = head_ref.peel_to_commit()
        && head.id() != tip.id()
    {
        parents.push(head);
    }
    parents
}

fn write_checkpoint(
    repo: &git2::Repository,
    session: &Session,
    draft: &Draft,
    parents: &[git2::Commit<'_>],
) -> Result<Checkpoint> {
    draft.transcript.validate()?;

    let tree_oid = git::snapshot_workdir(repo)?;
    let tree = repo.find_tree(tree_oid)?;
    let signature = signature(repo)?;
    let message =
        transcript::encode_commit_message(&draft.transcript, &session.id, draft.agent_name())?;
    let parent_refs = parents.iter().collect::<Vec<_>>();

    let oid = repo.commit(None, &signature, &signature, &message, &tree, &parent_refs)?;
    repo.reference(&ref_name(&session.id), oid, true, "session")?;
    checkpoint::load_from_commit(&repo.find_commit(oid)?, &session.id, &session.repo_path)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::{
        checkpoint::{self, Draft, TranscriptEntry, TranscriptEntryKind},
        testutil::init_repo_with_commit,
    };

    #[test]
    fn create_session_without_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let head = Oid::from(repo.head().unwrap().target().unwrap());

        let session = create(dir.path(), "session-a", head).unwrap();

        assert_eq!(session.id, "session-a");
        assert_eq!(session.name, None);
        assert!(checkpoint::list(&session).unwrap().is_empty());
    }

    #[test]
    fn open_and_list_include_empty_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let head = Oid::from(repo.head().unwrap().target().unwrap());

        create(dir.path(), "session-a", head).unwrap();

        let loaded = open(dir.path(), "session-a").unwrap();
        assert_eq!(loaded.name, None);

        let sessions = list(dir.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "session-a");
    }

    #[test]
    fn tip_and_modified_at_follow_committed_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let head = Oid::from(repo.head().unwrap().target().unwrap());
        let session = create(dir.path(), "session-a", head).unwrap();

        let mut draft = Draft::new();
        draft
            .transcript
            .append(TranscriptEntry::prompt_now("prompt"))
            .unwrap();

        let checkpoint = commit(&session, &draft).unwrap();

        assert_eq!(tip(&session).unwrap(), checkpoint.oid);
        assert_eq!(modified_at(&session).unwrap(), checkpoint.created_at);
    }

    #[test]
    fn amend_rewrites_tip_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let head = Oid::from(repo.head().unwrap().target().unwrap());
        let session = create(dir.path(), "session-a", head).unwrap();

        let mut draft = Draft::new();
        draft
            .transcript
            .append(TranscriptEntry::prompt_now("prompt"))
            .unwrap();
        let checkpoint = commit(&session, &draft).unwrap();

        std::fs::write(dir.path().join("next.txt"), "next").unwrap();

        let mut amended = Draft::from_checkpoint(&checkpoint);
        amended
            .transcript
            .append(TranscriptEntry::response_now("done"))
            .unwrap();
        let updated = amend(&session, &amended).unwrap();

        assert_ne!(updated.oid, checkpoint.oid);
        assert_eq!(tip(&session).unwrap(), updated.oid);
        assert_eq!(updated.transcript.len(), 2);
        assert!(matches!(
            updated.transcript.iter().nth(0).unwrap().kind,
            TranscriptEntryKind::Prompt { .. }
        ));
        assert!(matches!(
            updated.transcript.iter().nth(1).unwrap().kind,
            TranscriptEntryKind::Response { .. }
        ));
    }

    #[test]
    fn amend_requires_checkpoint_tip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let head = Oid::from(repo.head().unwrap().target().unwrap());
        let session = create(dir.path(), "session-a", head).unwrap();

        let error = amend(&session, &Draft::new()).unwrap_err();
        assert!(error.to_string().contains("no checkpoint to amend"));
    }
}
