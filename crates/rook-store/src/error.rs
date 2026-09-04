use std::path::PathBuf;

/// Every fallible operation in the store funnels through this type.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Db(String),
    #[error("object {0} not found")]
    MissingObject(String),
    #[error("session {0} not found")]
    MissingSession(String),
    #[error("corrupt object {id}: {reason}")]
    Corrupt { id: String, reason: String },
    #[error("encoding error: {0}")]
    Encoding(String),
    #[error("store format v{found} is newer than this build supports (v{supported}); upgrade rook")]
    FormatTooNew { found: u32, supported: u32 },
    // Names what does work beside a daemon rather than only what does not:
    // every `store`, `session`, `skills`, `memory` and `checkpoint` command
    // routes over its API, and `rook tui` runs its turns over its socket. What
    // is left holding the lock is a turn started from here — `run`, `chat`,
    // `acp` — and the answer for those is the daemon's own chat or stopping it.
    #[error(
        "the store at {path} is already open in another process (probably `rookd`).\n\
         The index allows one writer at a time. `rook tui` works beside it, and so does \
         its own page; a turn started here needs the store, so stop the daemon for one."
    )]
    Locked { path: PathBuf },
}

pub type Result<T> = std::result::Result<T, StoreError>;

impl StoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }
}

macro_rules! db_err {
    ($($t:ty),* $(,)?) => {$(
        impl From<$t> for StoreError {
            fn from(e: $t) -> Self { StoreError::Db(e.to_string()) }
        }
    )*};
}

db_err!(
    redb::Error,
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
);

impl From<postcard::Error> for StoreError {
    fn from(e: postcard::Error) -> Self {
        StoreError::Encoding(e.to_string())
    }
}
