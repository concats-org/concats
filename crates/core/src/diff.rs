use crate::{
    error::{Error, Result},
    session::{self, Session},
    snapshot,
    turn::Turn,
};

/// A single changed file in a turn diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub status: DiffStatus,
}

/// The kind of change applied to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffStatus {
    Added,
    Modified,
    Deleted,
}

impl FileDiff {
    fn from_tree_change(change: &gix::object::tree::diff::Change<'_, '_, '_>) -> Option<Self> {
        use gix::object::tree::diff::Change;
        let status = match change {
            Change::Addition { .. } => DiffStatus::Added,
            Change::Deletion { .. } => DiffStatus::Deleted,
            Change::Modification { .. } => DiffStatus::Modified,
            // NOTE: Never produced; `for_turn` turns rewrite tracking off.
            Change::Rewrite { .. } => return None,
        };
        let mode = change.entry_mode();
        (mode.is_blob() || mode.is_link()).then(|| Self {
            path: change.location().to_string(),
            status,
        })
    }
}

/// Compute the diff introduced by a turn relative to its first-parent base.
///
/// # Errors
///
/// Returns an error if the turn, or snapshot commits cannot be loaded, or if
/// git cannot render the diff.
pub fn for_turn(session: &Session, turn: &Turn) -> Result<Vec<FileDiff>> {
    let repo = session.repo();
    let turn_commit = repo.find_commit(turn.oid).map_err(Error::git)?;
    let snapshot = snapshot::get(session, turn.oid)?;
    let snapshot_commit = repo.find_commit(snapshot.oid).map_err(Error::git)?;
    let head_tree = snapshot_commit.tree().map_err(Error::git)?;

    let base_tree = match turn_commit.parent_ids().next().map(gix::Id::detach) {
        Some(parent_oid) => {
            let parent = repo.find_commit(parent_oid).map_err(Error::git)?;
            match Turn::try_from(&parent) {
                Ok(parent_turn) => {
                    let parent_session = if parent_turn.session_id() == &session.id {
                        session.clone()
                    } else {
                        session::open(session.repo().clone(), parent_turn.session_id().as_ref())?
                    };
                    let parent_snapshot = snapshot::get(&parent_session, parent_turn.oid)?;
                    repo.find_commit(parent_snapshot.oid)
                        .map_err(Error::git)?
                        .tree()
                        .map_err(Error::git)?
                }
                Err(_) => parent.tree().map_err(Error::git)?,
            }
        }
        None => repo.empty_tree(),
    };

    let mut changes: Vec<FileDiff> = Vec::new();
    base_tree
        .changes()
        .map_err(Error::git)?
        .options(|options| {
            options.track_path();
            // NOTE: No rename detection, matching the plain tree-to-tree diff
            // this replaced.
            options.track_rewrites(None);
        })
        .for_each_to_obtain_tree(&head_tree, |change| {
            changes.extend(FileDiff::from_tree_change(&change));
            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        })
        .map_err(Error::git)?;

    Ok(changes)
}
