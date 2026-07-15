//! The review domain: what was seen, what was said, and why the change was
//! made.
//!
//! Everything here is about a diff, never about a view of one. That is why it
//! is a crate: the GUI needs it, and so does the CLI that ships next to the GUI
//! — and the CLI should not have to link a UI toolkit to say `comments add`.
//!
//! - [`store`] — the repo-local SQLite state: seen hunks, comment threads,
//!   submitted guides, and the between-runs buffer cache;
//! - [`guide`] — the agent contract: a markdown document whose file links
//!   are transclusions of the real diff;

//! - [`interchange`] — a review's comments as one markdown file, in and
//!   out;
//! - [`github`] — a pull request's review comments, mapped onto the same
//!   entries;

//! - [`sessions`] — concats sessions read natively, which is the "why"
//!   column;
//! - [`error`] — what any of it can fail with.

// NOTE: `pedantic` is off here. Not because it is wrong: this code came from a
// crate that never ran under it, and turning it on means 300+ findings (almost
// all `must_use_candidate`, numeric casts, missing `# Errors` docs). Worth
// doing, but not inside a move, where it would bury the move. Everything else
// the workspace enables (`all`, `style`, `complexity`) is on.
#![allow(clippy::pedantic, clippy::cognitive_complexity)]

pub mod error;
pub mod github;
pub mod guide;
pub mod interchange;
pub mod sessions;
pub mod store;

pub use error::Error;
