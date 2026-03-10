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

    #[error("session error: {message}")]
    Session { message: String },
}

impl Error {
    pub fn session(message: impl Into<String>) -> Self {
        Self::Session {
            message: message.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
