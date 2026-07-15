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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{message}")]
    SessionId { message: String },

    #[error("{message}")]
    Snapshot { message: String },

    #[error("{message}")]
    Turn { message: String },
}

impl Error {
    pub fn session_id(message: impl Into<String>) -> Self {
        Self::SessionId {
            message: message.into(),
        }
    }

    pub fn snapshot(message: impl Into<String>) -> Self {
        Self::Snapshot {
            message: message.into(),
        }
    }

    pub fn turn(message: impl Into<String>) -> Self {
        Self::Turn {
            message: message.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
