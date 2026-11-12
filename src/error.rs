//! The CLI's error type and its mapping to process exit codes.

/// Every way a command can fail, grouped by exit code.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Bad flags, unreadable input files, declined confirmation (exit 2).
    #[error("{0}")]
    Usage(String),
    /// Missing or rejected credentials (exit 3).
    #[error("{message}")]
    Auth {
        message: String,
        /// HTTP status, when the server produced the failure.
        status: Option<u16>,
        /// Raw response body, when there is one.
        body: Vec<u8>,
    },
    /// Any other 4xx/5xx answer from the API (exit 4).
    #[error("the API answered with status {status}")]
    Api { status: u16, body: Vec<u8> },
    /// The request never got an HTTP answer (exit 5).
    #[error("{0}")]
    Network(String),
}

impl CliError {
    /// Exit code reported to the shell.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Auth { .. } => 3,
            Self::Api { .. } => 4,
            Self::Network(_) => 5,
        }
    }

    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }
}

pub type Result<T> = std::result::Result<T, CliError>;
