//! Error types shared by every susnotes crate.

use std::path::PathBuf;

/// Convenience alias: susnotes code never panics on user data.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while talking to a vault.
///
/// The variants are deliberately coarse enough to be reported to a user
/// verbatim and specific enough to be actionable in a log line.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading or writing the vault on disk failed.
    ///
    /// The operating system's own reason is part of the message, not only of the
    /// error chain: `Permission denied`, `No space left` and `Read-only file
    /// system` are the difference between a useful and a useless bug report,
    /// and every layer (CLI, API, log file) renders only `Display`.
    #[error("could not {} ({}): {}", .action, .path.display(), .source)]
    Io {
        action: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The folder does not contain a susnotes vault.
    #[error("no susnotes vault found in {path}")]
    NotAVault { path: PathBuf },

    /// The folder exists but is not usable as a vault.
    #[error("{path} cannot be used as a vault: {reason}")]
    InvalidVault { path: PathBuf, reason: String },

    /// The password does not unlock this vault.
    #[error("incorrect password")]
    WrongPassword,

    /// A vault file does not match the expected on-disk format.
    #[error("malformed {what}: {reason}")]
    Format { what: String, reason: String },

    /// A primitive in the crypto layer refused to run.
    #[error("crypto failure: {0}")]
    Crypto(String),

    /// The requested note name cannot be used.
    #[error("invalid note name {name:?}: {reason}")]
    InvalidName { name: String, reason: String },

    /// A note with this name does not exist.
    #[error("no note named {0}")]
    NoteNotFound(String),

    /// A note (or asset) with this name already exists.
    #[error("a note named {0} already exists")]
    AlreadyExists(String),

    /// An asset is not in the vault.
    #[error("no asset at {0}")]
    AssetNotFound(String),

    /// A configuration file could not be read or written.
    #[error("configuration error in {file}: {reason}")]
    Config { file: PathBuf, reason: String },

    /// A request to the local API was not understood.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// The local API is not reachable.
    #[error("API unreachable at {addr}: {reason}")]
    ApiUnreachable { addr: String, reason: String },

    /// The local API answered with an error.
    #[error("API error {status}: {message}")]
    ApiError { status: u16, message: String },

    /// JSON could not be read or produced.
    #[error("JSON error: {0}")]
    Json(String),

    /// Anything the layers above have no specific handling for.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Wrap an IO failure with what was being attempted and where.
    pub fn io(action: impl Into<String>, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            action: action.into(),
            path: path.into(),
            source,
        }
    }

    /// Whether the error means the vault is simply not there yet.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
            || matches!(self, Self::NotAVault { .. })
    }

    /// Which file or folder this error is about, when there is one.
    #[must_use]
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Io { path, .. }
            | Self::NotAVault { path }
            | Self::InvalidVault { path, .. }
            | Self::Config { file: path, .. } => Some(path),
            _ => None,
        }
    }

    /// A short, stable identifier for the error, used in logs and in the API.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Io { .. } => "io",
            Self::NotAVault { .. } => "not_a_vault",
            Self::InvalidVault { .. } => "invalid_vault",
            Self::WrongPassword => "wrong_password",
            Self::Format { .. } => "format",
            Self::Crypto(_) => "crypto",
            Self::InvalidName { .. } => "invalid_name",
            Self::NoteNotFound(_) => "note_not_found",
            Self::AlreadyExists(_) => "already_exists",
            Self::AssetNotFound(_) => "asset_not_found",
            Self::Config { .. } => "config",
            Self::BadRequest(_) => "bad_request",
            Self::ApiUnreachable { .. } => "api_unreachable",
            Self::ApiError { .. } => "api_error",
            Self::Json(_) => "json",
            Self::Other(_) => "other",
        }
    }
}

impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_io_failure_says_why_the_os_said_no() {
        let error = Error::io(
            "create",
            "/vault/pages/notes.md",
            std::io::Error::from_raw_os_error(13),
        );
        let text = error.to_string();
        assert!(
            text.contains("Permission denied"),
            "the reason was missing from {text:?}"
        );
        assert!(text.contains("/vault/pages/notes.md"), "{text:?}");
        assert!(text.contains("create"), "{text:?}");
        assert!(
            std::error::Error::source(&error).is_some(),
            "the io error must stay in the chain"
        );
    }
}
