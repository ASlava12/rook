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
    // Not "probably `rookd`": a reachable daemon is exactly the case that never
    // reaches this, because everything routes over its API instead. Whoever
    // holds it is a `tui`, a `chat`, a `run` — each of which takes the store
    // for itself and serves nobody — or a daemon that is not answering where
    // it said it would. So the message names the one program that shares.
    #[error(
        "the store at {path} is already open in another process.\n\
         The index allows one writer at a time, and only `rookd` shares it: start that \
         first and every window works beside it, the browser included. A `rook tui`, \
         `chat` or `run` takes it alone — close that one, or start `rookd` before them."
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
