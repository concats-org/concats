/// Core error type for all operations.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error("git error: {source}")]
    Git {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("session error: {message}")]
    Session { message: String },

    #[error("session not found: {session_id}")]
    SessionNotFound { session_id: String },

    #[error("turn error: {message}")]
    Turn { message: String },

    #[error("snapshot error: {message}")]
    Snapshot { message: String },

    #[error("checkout would overwrite local changes")]
    RestoreConflict { paths: Vec<String> },
}

impl Error {
    // NOTE: gix has one error type per operation, so the Git variant boxes
    // its source rather than naming one concrete error type.
    pub fn git(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Git {
            source: Box::new(source),
        }
    }

    pub fn session(message: impl Into<String>) -> Self {
        Self::Session {
            message: message.into(),
        }
    }

    pub fn turn(message: impl Into<String>) -> Self {
        Self::Turn {
            message: message.into(),
        }
    }

    pub fn session_not_found(session_id: impl Into<String>) -> Self {
        Self::SessionNotFound {
            session_id: session_id.into(),
        }
    }

    pub fn snapshot(message: impl Into<String>) -> Self {
        Self::Snapshot {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn restore_conflict(paths: Vec<String>) -> Self {
        Self::RestoreConflict { paths }
    }
}

impl From<concats_message::Error> for Error {
    fn from(error: concats_message::Error) -> Self {
        Self::Turn {
            message: error.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
