mod session_id;
mod snapshot;
mod turn;

pub use session_id::SessionId;
pub use snapshot::{Snapshot, SnapshotReason};
pub use turn::{Turn, TurnEntry, TurnEntryKind, TurnToolKind};

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
