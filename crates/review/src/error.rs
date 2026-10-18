//! What the review domain can fail with, as one type.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "unsupported schema `{found}` — this version reads {}",
        crate::interchange::SCHEMA
    )]
    UnsupportedSchema { found: String },
    #[error("not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{op}: {source}")]
    Store {
        op: &'static str,
        source: rusqlite::Error,
    },
    #[error("anchor must be <path>:<start>[-<end>]")]
    AnchorSyntax,
    #[error("cannot parse line range `{0}` — lines are 1-based new-side numbers")]
    AnchorLines(String),
    #[error("lines are 1-based")]
    AnchorZero,
    #[error(
        "`{0}` is not part of this diff\n       run `concats manifest` for the reviewable files"
    )]
    NotInDiff(String),
    #[error("line(s) {lines} of {path} are not part of this diff\n\n{ranges}")]
    LinesNotInDiff {
        lines: String,
        path: String,
        ranges: String,
    },
    #[error("empty comment body")]
    EmptyBody,
    #[error("no comment with id {0}")]
    NoSuchComment(u64),
}
