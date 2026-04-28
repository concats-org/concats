use std::{collections::HashMap, path::PathBuf, rc::Rc};

use concats_message::SessionId;
pub use concats_message::SnapshotReason;
use time::OffsetDateTime;

use crate::{
    error::{Error, Result},
    git::{self, Oid},
    session::{self, Session},
    turn::{self, Turn},
};

const SNAPSHOT_REF_PREFIX: &str = "refs/agent/snapshots/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    message: concats_message::Snapshot,
    pub turn_oid: Oid,
    pub oid: Oid,
    pub created_at: OffsetDateTime,
}

impl Snapshot {
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        self.message.session_id()
    }

    #[must_use]
    pub fn reason(&self) -> Option<SnapshotReason> {
        self.message.reason()
    }
}

impl TryFrom<&git2::Commit<'_>> for Snapshot {
    type Error = Error;

    fn try_from(commit: &git2::Commit<'_>) -> Result<Self> {
        let message: concats_message::Snapshot = commit
            .message_raw()
            .unwrap_or("")
            .parse()
            .map_err(|error: concats_message::Error| Error::snapshot(error.to_string()))?;

        let turn_oid = match commit.parent_count() {
            1 => commit
                .parent_id(0)
                .map(Oid::from)
                .map_err(|_| Error::snapshot("snapshot is missing its turn parent")),
            2 => commit
                .parent_id(1)
                .map(Oid::from)
                .map_err(|_| Error::snapshot("snapshot is missing its turn parent")),
            count => Err(Error::snapshot(format!(
                "snapshot has unsupported parent count: {count}"
            ))),
        }?;

        Ok(Self {
            message,
            turn_oid,
            oid: Oid::from(commit.id()),
            created_at: git::commit_time(commit.time())?,
        })
    }
}

/// List all snapshots for a session in creation order.
///
/// # Errors
///
/// Returns an error if the snapshot ref points to an invalid commit, or any
/// snapshot commit cannot be decoded.
pub fn list(session: &Session) -> Result<Vec<Snapshot>> {
    let repo = session.repo();
    let Some(tip) = git::resolve_ref(repo, &ref_name(&session.id)) else {
        return Ok(Vec::new());
    };
    load_from_tip(repo, &session.id, &tip)
}

/// Resolve the snapshot recorded for a specific session turn.
///
/// # Errors
///
/// Returns an error if the session's snapshot ref is missing, or no snapshot
/// in that chain points to the requested turn.
pub fn get(session: &Session, turn_oid: Oid) -> Result<Snapshot> {
    let repo = session.repo();
    let mut current = git::resolve_ref(repo, &ref_name(&session.id));
    while let Some(commit) = current {
        let snapshot = Snapshot::try_from(&commit)?;
        if snapshot.session_id() != &session.id {
            break;
        }
        if snapshot.turn_oid == turn_oid {
            return Ok(snapshot);
        }
        current =
            if commit.parent_count() == 2 {
                Some(commit.parent(0).map_err(|_| {
                    Error::snapshot("snapshot is missing its previous-snapshot parent")
                })?)
            } else {
                None
            };
    }

    Err(Error::snapshot(format!(
        "snapshot not found for turn {} in session {}",
        turn_oid, session.id
    )))
}

/// Capture the current worktree state and advance the snapshot ref.
///
/// # Errors
///
/// Returns an error if the target turn cannot be loaded, the target turn does
/// not belong to the session, the worktree snapshot cannot be captured, or the
/// snapshot commit cannot be written.
pub fn capture(
    session: &Session,
    turn_oid: Oid,
    reason: SnapshotReason,
) -> Result<Option<Snapshot>> {
    let repo = session.repo();
    let turn = repo
        .find_commit(turn_oid.as_git())
        .map_err(|_| Error::snapshot(format!("turn not found: {turn_oid}")))?;
    let turn_metadata = Turn::try_from(&turn)
        .map_err(|_| Error::snapshot(format!("turn not found: {turn_oid}")))?;
    if turn_metadata.session_id() != &session.id {
        return Err(Error::snapshot(format!(
            "turn {turn_oid} does not belong to session {}",
            session.id
        )));
    }

    let tree_oid = git::snapshot_workdir(repo)?;
    let current_snapshot = git::resolve_ref(repo, &ref_name(&session.id));
    if let Some(current_snapshot) = current_snapshot.as_ref() {
        let current_metadata = Snapshot::try_from(current_snapshot)?;
        if current_metadata.session_id() != &session.id {
            return Err(Error::snapshot(format!(
                "snapshot ref belongs to session {}, expected {}",
                current_metadata.session_id(),
                session.id
            )));
        }
        if current_metadata.turn_oid == turn_oid && current_snapshot.tree_id() == tree_oid {
            return Ok(None);
        }
    }

    let tree = repo.find_tree(tree_oid)?;
    let signature = git::signature(repo, None)?;
    let snapshot_message = concats_message::Snapshot::new(session.id.clone(), reason);

    let parents = match current_snapshot.as_ref() {
        Some(current_snapshot) => vec![current_snapshot, &turn],
        None => vec![&turn],
    };

    let oid = repo.commit(
        None,
        &signature,
        &signature,
        &snapshot_message.to_string(),
        &tree,
        &parents,
    )?;
    repo.reference(&ref_name(&session.id), oid, true, "snapshot")?;
    Ok(Some(Snapshot::try_from(&repo.find_commit(oid)?)?))
}

/// Push the snapshot ref to the named remote.
///
/// # Errors
///
/// Returns an error if the snapshot ref cannot be pushed to the remote.
pub fn push(session: &Session, remote: &str) -> Result<()> {
    crate::git::push_ref(session.repo(), remote, &ref_name(&session.id))?;
    Ok(())
}

pub(crate) fn ref_name(session_id: &SessionId) -> String {
    format!("{SNAPSHOT_REF_PREFIX}{session_id}")
}

/// The set of `(path, blob)` pairs the agent introduced in this snapshot.
///
/// Computed as the diff between the snapshot's tree and its baseline:
/// - 2-parent snapshot: baseline = previous snapshot's tree.
/// - 1-parent snapshot: baseline = the turn's anchor-parent tree (the parent
///   of the turn commit whose message does not parse as a Turn).
///
/// Returns `Ok(None)` when no unambiguous baseline exists (e.g. a turn with
/// zero or multiple non-turn parents). Only added/modified/renamed/copied
/// entries are returned; deletions are dropped.
///
/// # Errors
///
/// Returns an error if the snapshot or its parents cannot be loaded, or the
/// diff cannot be computed.
pub fn diff_against_baseline(
    repo: &git2::Repository,
    snapshot: &Snapshot,
) -> Result<Option<HashMap<PathBuf, Oid>>> {
    let snapshot_commit = repo.find_commit(snapshot.oid.as_git())?;
    let snapshot_tree = snapshot_commit.tree()?;

    let baseline_tree = match snapshot_commit.parent_count() {
        2 => snapshot_commit.parent(0)?.tree()?,
        1 => {
            let turn_commit = snapshot_commit.parent(0)?;
            let mut anchors = turn_commit
                .parents()
                .filter(|parent| !turn::is_turn_commit(parent));
            let Some(anchor) = anchors.next() else {
                return Ok(None);
            };
            if anchors.next().is_some() {
                return Ok(None);
            }
            anchor.tree()?
        }
        _ => return Ok(None),
    };

    let diff = repo.diff_tree_to_tree(Some(&baseline_tree), Some(&snapshot_tree), None)?;
    let mut changes: HashMap<PathBuf, Oid> = HashMap::new();
    for delta in diff.deltas() {
        match delta.status() {
            git2::Delta::Added
            | git2::Delta::Modified
            | git2::Delta::Renamed
            | git2::Delta::Copied
            | git2::Delta::Typechange => {}
            _ => continue,
        }
        let new_file = delta.new_file();
        let blob = new_file.id();
        if blob.is_zero() {
            continue;
        }
        let Some(path) = new_file.path() else {
            continue;
        };
        changes.insert(path.to_path_buf(), Oid::from(blob));
    }
    Ok(Some(changes))
}

/// Find the most-recently-modified session whose latest matching snapshot
/// shares at least one `(path, blob)` with `commit_changes`.
///
/// Sessions are walked newest-first; within each, snapshots are walked
/// newest-first. The first snapshot whose `diff_against_baseline` shares any
/// `(path, blob)` with `commit_changes` wins.
///
/// # Errors
///
/// Returns an error if session enumeration fails. Snapshots that fail to load
/// or diff are skipped silently.
pub fn find_overlapping<S: std::hash::BuildHasher>(
    repo: &Rc<git2::Repository>,
    commit_changes: &HashMap<PathBuf, Oid, S>,
) -> Result<Option<(Session, Snapshot)>> {
    if commit_changes.is_empty() {
        return Ok(None);
    }

    for session_candidate in session::list(repo)? {
        let Ok(snapshots) = list(&session_candidate) else {
            continue;
        };
        for snapshot in snapshots.iter().rev() {
            let Ok(Some(diff)) = diff_against_baseline(repo, snapshot) else {
                continue;
            };
            if has_overlap(commit_changes, &diff) {
                return Ok(Some((session_candidate, snapshot.clone())));
            }
        }
    }
    Ok(None)
}

fn has_overlap<S1, S2>(
    left: &HashMap<PathBuf, Oid, S1>,
    right: &HashMap<PathBuf, Oid, S2>,
) -> bool
where
    S1: std::hash::BuildHasher,
    S2: std::hash::BuildHasher,
{
    // Iterate the smaller map; lookups in the larger one are O(1).
    if left.len() <= right.len() {
        left.iter()
            .any(|(path, blob)| right.get(path).is_some_and(|other| other == blob))
    } else {
        right
            .iter()
            .any(|(path, blob)| left.get(path).is_some_and(|other| other == blob))
    }
}

fn load_from_tip(
    repo: &git2::Repository,
    session_id: &SessionId,
    tip: &git2::Commit<'_>,
) -> Result<Vec<Snapshot>> {
    let mut snapshots = Vec::new();
    let first = Snapshot::try_from(tip)?;
    if first.session_id() != session_id {
        return Ok(snapshots);
    }
    snapshots.push(first);

    let mut next_oid = if tip.parent_count() == 2 {
        tip.parent_id(0).ok()
    } else {
        None
    };

    while let Some(oid) = next_oid {
        let commit = repo.find_commit(oid)?;
        let snapshot = Snapshot::try_from(&commit)?;
        if snapshot.session_id() != session_id {
            break;
        }
        next_oid = if commit.parent_count() == 2 {
            commit.parent_id(0).ok()
        } else {
            None
        };
        snapshots.push(snapshot);
    }

    snapshots.reverse();
    Ok(snapshots)
}
