mod session_id;
mod snapshot;
mod turn;

pub use session_id::SessionId;
pub use snapshot::{Snapshot, SnapshotReason};
pub use turn::{Turn, TurnEntry, TurnEntryKind, TurnToolKind};

/// Git ref namespace for session (turn) refs: `refs/agent/sessions/<id>`.
/// Part of the wire format, so it lives beside the turn/snapshot grammar
/// rather than being redefined by each reader.
pub const SESSION_REF_PREFIX: &str = "refs/agent/sessions/";
/// Git ref namespace for snapshot refs: `refs/agent/snapshots/<id>`.
pub const SNAPSHOT_REF_PREFIX: &str = "refs/agent/snapshots/";
/// Trailer naming the session a turn or snapshot commit belongs to.
const SESSION_TRAILER_PREFIX: &str = "Session: ";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

pub type Result<T> = std::result::Result<T, Error>;
