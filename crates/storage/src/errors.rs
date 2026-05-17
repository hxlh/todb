pub type StorageResult<T> = Result<T, StorageError>;

#[derive(thiserror::Error, Debug)]
pub enum StorageError {
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("invalid values: {0}")]
    InvalidValue(String),
    #[error("row encode error: {0}")]
    RowEncodeError(String),
    #[error("transaction conflict: {0}")]
    TransactionConflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}
