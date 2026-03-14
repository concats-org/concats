use crate::{
    error::Result,
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

/// Compute the diff introduced by a turn relative to its first-parent base.
///
/// # Errors
///
/// Returns an error if the turn, or snapshot commits cannot be loaded, or if
/// git cannot render the diff.
pub fn for_turn(session: &Session, turn: &Turn) -> Result<Vec<FileDiff>> {
    let repo = session.repo();
    let turn_commit = repo.find_commit(turn.oid.as_git())?;
    let snapshot = snapshot::get(session, turn.oid)?;
    let snapshot_commit = repo.find_commit(snapshot.oid.as_git())?;
    let head_tree = snapshot_commit.tree()?;

    let base_tree = match turn_commit.parent(0) {
        Ok(parent) => match crate::turn::Turn::try_from(&parent) {
            Ok(parent_turn) => {
                let parent_session = if parent_turn.session_id() == &session.id {
                    session.clone()
                } else {
                    session::open(session.repo().clone(), parent_turn.session_id().as_ref())?
                };
                let parent_snapshot = snapshot::get(&parent_session, parent_turn.oid)?;
                Some(repo.find_commit(parent_snapshot.oid.as_git())?.tree()?)
            }
            Err(_) => Some(parent.tree()?),
        },
        Err(_) => None,
    };

    let diff = repo.diff_tree_to_tree(base_tree.as_ref(), Some(&head_tree), None)?;
    let mut files = Vec::new();

    diff.print(git2::DiffFormat::Patch, |delta, maybe_hunk, line| {
        match line.origin() {
            'F' => push_file_entry(&delta, &mut files),
            'H' => push_hunk_entry(maybe_hunk.as_ref(), &mut files),
            '+' | '-' | ' ' => push_line_entry(&line, maybe_hunk.as_ref(), &mut files),
            _ => {}
        }

        true
    })?;

    Ok(files)
}

fn push_file_entry(delta: &git2::DiffDelta<'_>, files: &mut Vec<FileDiff>) {
    let path = delta
        .new_file()
        .path()
        .or_else(|| delta.old_file().path())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let duplicate = files.last().is_some_and(|f: &FileDiff| f.path == path);
    if !duplicate {
        let status = match delta.status() {
            git2::Delta::Added => DiffStatus::Added,
            git2::Delta::Deleted => DiffStatus::Deleted,
            git2::Delta::Renamed => {
                let old_path = delta
                    .old_file()
                    .path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                DiffStatus::Renamed { old_path }
            }
            _ => DiffStatus::Modified,
        };
        files.push(FileDiff {
            path,
            status,
            hunks: Vec::new(),
        });
    }
}

fn push_hunk_entry(maybe_hunk: Option<&git2::DiffHunk<'_>>, files: &mut [FileDiff]) {
    if let Some(hunk) = maybe_hunk
        && let Some(file) = files.last_mut()
    {
        file.hunks.push(DiffHunk {
            header: String::from_utf8_lossy(hunk.header())
                .trim_end()
                .to_string(),
            lines: Vec::new(),
        });
    }
}

fn push_line_entry(
    line: &git2::DiffLine<'_>,
    maybe_hunk: Option<&git2::DiffHunk<'_>>,
    files: &mut [FileDiff],
) {
    let kind = match line.origin() {
        '+' => DiffLineKind::Add,
        '-' => DiffLineKind::Remove,
        _ => DiffLineKind::Context,
    };

    if let Some(file) = files.last_mut() {
        if file.hunks.is_empty() {
            let header = maybe_hunk
                .map(|h| String::from_utf8_lossy(h.header()).trim_end().to_string())
                .unwrap_or_default();
            file.hunks.push(DiffHunk {
                header,
                lines: Vec::new(),
            });
        }

        if let Some(hunk) = file.hunks.last_mut() {
            hunk.lines.push(DiffLine {
                kind,
                content: String::from_utf8_lossy(line.content())
                    .trim_end()
                    .to_string(),
            });
        }
    }
}
