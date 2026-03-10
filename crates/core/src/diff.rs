use std::path::Path;

use crate::{
    checkpoint::{Checkpoint, Snapshot},
    error::Result,
};

/// A single changed file in a checkpoint diff.
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

/// Compute the diff introduced by a checkpoint relative to its parent commit.
///
/// # Errors
///
/// Returns an error if the repository, checkpoint commit, or git trees cannot
/// be loaded, or if git cannot render the diff.
pub fn for_checkpoint(checkpoint: &Checkpoint) -> Result<Vec<FileDiff>> {
    let repo = git2::Repository::open(&checkpoint.repo_path)?;
    let commit = repo.find_commit(checkpoint.oid.as_git())?;
    let commit_tree = commit.tree()?;
    let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
    load_tree_diff(&repo, parent_tree.as_ref(), &commit_tree)
}

/// Compute the diff between two stored snapshots.
///
/// # Errors
///
/// Returns an error if the repository or either snapshot tree cannot be
/// loaded, or if git cannot render the diff.
pub fn between(repo_path: &Path, base: &Snapshot, head: &Snapshot) -> Result<Vec<FileDiff>> {
    let repo = git2::Repository::open(repo_path)?;
    let base_tree = repo.find_tree(base.tree.as_git())?;
    let head_tree = repo.find_tree(head.tree.as_git())?;
    load_tree_diff(&repo, Some(&base_tree), &head_tree)
}

fn load_tree_diff(
    repo: &git2::Repository,
    base_tree: Option<&git2::Tree<'_>>,
    head_tree: &git2::Tree<'_>,
) -> Result<Vec<FileDiff>> {
    let diff = repo.diff_tree_to_tree(base_tree, Some(head_tree), None)?;
    let mut files = Vec::new();

    diff.print(git2::DiffFormat::Patch, |delta, maybe_hunk, line| {
        match line.origin() {
            'F' | 'H' => {
                if line.origin() == 'F' {
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

                if line.origin() == 'H'
                    && let Some(hunk) = maybe_hunk
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
            '+' | '-' | ' ' => {
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
            _ => {}
        }

        true
    })?;

    Ok(files)
}
