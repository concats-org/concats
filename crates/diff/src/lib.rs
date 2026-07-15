//! The diff: a range of a git repository, as content and as an ordered stream
//! of rows.
//!
//! Four modules, read bottom up:
//!
//! - [`blob`] — a file's content at one revision, and the buffer it becomes
//!   when typed into;
//! - [`row`] — the heterogeneous stream a review is made of, and the
//!   [`FileChange`]/[`Hunk`] intermediate an agent reorders before it is
//!   lowered;
//! - [`load`] — git to blobs and rows, with per-stage timing;
//! - [`stage`] — the one write-back, seen hunks into the index;
//! - [`error`] — what either can fail with.
//!
//! Nothing here draws, on purpose. The app draws this model with makepad, a
//! terminal renderer prints it with ANSI, and the CLI needs it without any
//! renderer at all.

// NOTE: `pedantic` is off here. The code came from a crate that never ran under
// it, and turning it on means 300+ findings — mostly `must_use_candidate`,
// numeric casts and missing `# Errors` docs. Worth doing, but not inside a
// move, where it would hide the move. `all`, `style` and `complexity` are on,
// as everywhere in the workspace.
#![allow(clippy::pedantic, clippy::cognitive_complexity)]

#[cfg(test)]
mod fixture;

pub mod blob;
pub mod error;
pub mod load;
pub mod row;
pub mod stage;

// The types every consumer uses. `load` and `stage` stay behind their modules:
// one would clash with its module's name, the other is the one thing here that
// writes.
pub use blob::Blob;
pub use error::Error;
pub use load::Loaded;
pub use row::{CollapsedEnd, FileChange, Hunk, LineKind, LoadStats, Row, Side};
