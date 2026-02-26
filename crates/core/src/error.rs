use std::path::PathBuf;

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
        #[from]
        source: git2::Error,
    },

    #[error("protocol error: {message}")]
    Protocol { message: String },

    #[error("process error: {message}")]
    Process { message: String },

    #[error("terminal error: {message}")]
    Terminal { message: String },

    #[error("session error: {message}")]
    Session { message: String },

    #[error("path {path:?} escapes workspace root {root:?}")]
    PathEscape { path: PathBuf, root: PathBuf },
}

impl Error {
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol {
            message: message.into(),
        }
    }

    pub fn process(message: impl Into<String>) -> Self {
        Self::Process {
            message: message.into(),
        }
    }

    pub fn terminal(message: impl Into<String>) -> Self {
        Self::Terminal {
            message: message.into(),
        }
    }

    pub fn session(message: impl Into<String>) -> Self {
        Self::Session {
            message: message.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
