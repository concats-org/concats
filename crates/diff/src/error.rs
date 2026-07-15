//! What loading or staging can fail with, as one type: a caller matches on the
//! case it cares about, and every message has one owner.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no git repository at {}", .0.display())]
    NoRepository(PathBuf),
    #[error(
        "{rev} is only valid in a WORKTREE diff — load INDEX...WORKTREE (unstaged) \
         or HEAD...WORKTREE (all uncommitted changes)"
    )]
    WorktreeOnly { rev: String },
    #[error("empty revision")]
    EmptyRevision,
    #[error("cannot resolve revision: {rev} ({source})")]
    UnknownRevision {
        rev: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("not a commit: {rev}")]
    NotACommit { rev: String },
    #[error("no merge base between {base} and {head}")]
    NoMergeBase { base: String, head: String },
    #[error("{path} is not in the tree at the head")]
    NotInTree { path: String },
    #[error("{path} is binary")]
    Binary { path: String },
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A git operation failed. gix has one error type per operation, so the
    /// source is boxed and `op` says which one ("commit", "tree", "blob", …).
    #[error("{op}: {source}")]
    Git {
        op: &'static str,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl Error {
    pub fn git(op: &'static str, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Git {
            op,
            source: Box::new(source),
        }
    }
}
