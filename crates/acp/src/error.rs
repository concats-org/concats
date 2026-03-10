use std::path::PathBuf;

/// ACP error type for runtime, transport, and tool adapter failures.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error(transparent)]
    Core {
        #[from]
        source: concats_core::error::Error,
    },

    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
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
