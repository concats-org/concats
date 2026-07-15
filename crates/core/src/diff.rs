use imara_diff::{Algorithm, Diff, InternedInput};

use crate::{
    error::{Error, Result},
    git::Oid,
    session::{self, Session},
    snapshot,
    turn::Turn,
};

/// A single changed file in a turn diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub status: DiffStatus,
    pub hunks: Vec<DiffHunk>,
}

/// The kind of change applied to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffStatus {
    Added,
    Modified,
    Deleted,
    Renamed { old_path: String },
}

/// A contiguous hunk within a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// A single line within a diff hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
}

/// Whether a diff line is context, an addition, or a removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Add,
    Remove,
}

/// Unchanged lines kept either side of a change, matching git's default.
const CONTEXT: usize = 3;

/// One changed path from the tree diff, before its hunks are computed.
struct Changed {
    path: String,
    status: DiffStatus,
    old: Option<Oid>,
    new: Option<Oid>,
}

impl Changed {
    fn from_tree_change(change: &gix::object::tree::diff::Change<'_, '_, '_>) -> Option<Self> {
        use gix::object::tree::diff::Change;
        match *change {
            Change::Addition {
                location,
                entry_mode,
                id,
                ..
            } if entry_mode.is_blob() || entry_mode.is_link() => Some(Self {
                path: location.to_string(),
                status: DiffStatus::Added,
                old: None,
                new: Some(Oid::from(id.detach())),
            }),
            Change::Deletion {
                location,
                entry_mode,
                id,
                ..
            } if entry_mode.is_blob() || entry_mode.is_link() => Some(Self {
                path: location.to_string(),
                status: DiffStatus::Deleted,
                old: Some(Oid::from(id.detach())),
                new: None,
            }),
            Change::Modification {
                location,
                previous_id,
                id,
                entry_mode,
                ..
            } if entry_mode.is_blob() || entry_mode.is_link() => Some(Self {
                path: location.to_string(),
                status: DiffStatus::Modified,
                old: Some(Oid::from(previous_id.detach())),
                new: Some(Oid::from(id.detach())),
            }),
            Change::Rewrite {
                source_location,
                location,
                source_id,
                id,
                entry_mode,
                ..
            } if entry_mode.is_blob() || entry_mode.is_link() => Some(Self {
                path: location.to_string(),
                status: DiffStatus::Renamed {
                    old_path: source_location.to_string(),
                },
                old: Some(Oid::from(source_id.detach())),
                new: Some(Oid::from(id.detach())),
            }),
            _ => None,
        }
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
    let turn_commit = repo.find_commit(turn.oid.as_gix()).map_err(Error::git)?;
    let snapshot = snapshot::get(session, turn.oid)?;
    let snapshot_commit = repo
        .find_commit(snapshot.oid.as_gix())
        .map_err(Error::git)?;
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
                    repo.find_commit(parent_snapshot.oid.as_gix())
                        .map_err(Error::git)?
                        .tree()
                        .map_err(Error::git)?
                }
                Err(_) => parent.tree().map_err(Error::git)?,
            }
        }
        None => repo.empty_tree(),
    };

    let mut changes: Vec<Changed> = Vec::new();
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
            changes.extend(Changed::from_tree_change(&change));
            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        })
        .map_err(Error::git)?;

    changes
        .into_iter()
        .map(|change| {
            let old_bytes = read_blob(repo, change.old)?;
            let new_bytes = read_blob(repo, change.new)?;
            // NOTE: Binary content gets a file entry but no hunks, like
            // git's "Binary files differ".
            let hunks = if old_bytes.contains(&0) || new_bytes.contains(&0) {
                Vec::new()
            } else {
                hunks(&old_bytes, &new_bytes)
            };
            Ok(FileDiff {
                path: change.path,
                status: change.status,
                hunks,
            })
        })
        .collect()
}

fn read_blob(repo: &gix::Repository, oid: Option<Oid>) -> Result<Vec<u8>> {
    oid.map_or_else(
        || Ok(Vec::new()),
        |oid| {
            Ok(repo
                .find_blob(oid.as_gix())
                .map_err(Error::git)?
                .take_data())
        },
    )
}

/// Diff two text blobs into unified hunks: histogram diff, three context
/// lines, and adjacent changes merged exactly as `git diff` groups them.
fn hunks(old: &[u8], new: &[u8]) -> Vec<DiffHunk> {
    let old_text = String::from_utf8_lossy(old).into_owned();
    let new_text = String::from_utf8_lossy(new).into_owned();
    let input = InternedInput::new(old_text.as_str(), new_text.as_str());
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);

    let changes: Vec<imara_diff::Hunk> = diff.hunks().collect();
    let (old_len, new_len) = (input.before.len(), input.after.len());

    // Two changes whose context windows touch belong to one displayed hunk.
    let mut groups: Vec<Vec<imara_diff::Hunk>> = Vec::new();
    for change in changes {
        match groups.last_mut() {
            Some(group)
                if change.before.start as usize
                    <= group.last().expect("groups are never empty").before.end as usize
                        + 2 * CONTEXT =>
            {
                group.push(change);
            }
            _ => groups.push(vec![change]),
        }
    }

    groups
        .into_iter()
        .map(|group| {
            let first = group.first().expect("groups are never empty");
            let last = group.last().expect("groups are never empty");
            let old_start = (first.before.start as usize).saturating_sub(CONTEXT);
            let old_end = (last.before.end as usize + CONTEXT).min(old_len);
            let new_start = (first.after.start as usize).saturating_sub(CONTEXT);
            let new_end = (last.after.end as usize + CONTEXT).min(new_len);

            let mut lines = Vec::new();
            let mut old_at = old_start;
            for change in &group {
                while old_at < change.before.start as usize {
                    lines.push(line(
                        DiffLineKind::Context,
                        input.interner[input.before[old_at]],
                    ));
                    old_at += 1;
                }
                for i in change.before.clone() {
                    lines.push(line(
                        DiffLineKind::Remove,
                        input.interner[input.before[i as usize]],
                    ));
                }
                for i in change.after.clone() {
                    lines.push(line(
                        DiffLineKind::Add,
                        input.interner[input.after[i as usize]],
                    ));
                }
                old_at = change.before.end as usize;
            }
            while old_at < old_end {
                lines.push(line(
                    DiffLineKind::Context,
                    input.interner[input.before[old_at]],
                ));
                old_at += 1;
            }

            DiffHunk {
                header: header(
                    (old_start, old_end - old_start),
                    (new_start, new_end - new_start),
                ),
                lines,
            }
        })
        .collect()
}

fn line(kind: DiffLineKind, content: &str) -> DiffLine {
    DiffLine {
        kind,
        content: content.trim_end().to_string(),
    }
}

/// Render `@@ -a,b +c,d @@` the way git does: starts are 1-based, an empty
/// range shows the line before it, and a count of one is omitted.
fn header(
    (old_start, old_count): (usize, usize),
    (new_start, new_count): (usize, usize),
) -> String {
    let side = |start: usize, count: usize| match count {
        0 => format!("{start},0"),
        1 => format!("{}", start + 1),
        _ => format!("{},{count}", start + 1),
    };
    format!(
        "@@ -{} +{} @@",
        side(old_start, old_count),
        side(new_start, new_count)
    )
}
