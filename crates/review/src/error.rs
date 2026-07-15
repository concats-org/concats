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
}
