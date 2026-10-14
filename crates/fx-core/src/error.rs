use std::io;

/// What went wrong inside the file system code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    NotFatx,
    Corrupt,
    NotFound,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    InvalidName,
    PathTooLong,
    DirectoryFull,
    DiskFull,
    FileTooLarge,
    ReadOnly,
    InvalidOperation,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{message}")]
    Fatx { kind: ErrorKind, message: String },

    /// A backup or restore that can't go ahead. The message is meant to be shown as it is.
    #[error("{0}")]
    Backup(String),

    /// Something asked for doesn't make sense, like a partition that isn't there.
    #[error("{0}")]
    Invalid(String),

    #[error("not supported here: {0}")]
    Unsupported(String),

    #[error("stopped")]
    Cancelled,

    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn fatx(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error::Fatx {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Error::Invalid(message.into())
    }

    pub fn kind(&self) -> Option<ErrorKind> {
        match self {
            Error::Fatx { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}
