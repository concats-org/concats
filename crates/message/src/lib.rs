mod session_id;
mod turn;

pub use session_id::SessionId;
pub use turn::{Turn, TurnEntry, TurnEntryKind};

/// Git ref namespace for session (turn) refs: `refs/agent/sessions/<id>`.
/// Part of the wire format, so it lives beside the turn grammar rather than
/// being redefined by each reader.
pub const SESSION_REF_PREFIX: &str = "refs/agent/sessions/";
/// Trailer naming the session a turn commit belongs to.
const SESSION_TRAILER_PREFIX: &str = "Session: ";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

pub type Result<T> = std::result::Result<T, Error>;
