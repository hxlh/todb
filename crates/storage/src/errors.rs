pub type StorageResult<T> = Result<T, StorageError>;

#[derive(thiserror::Error, Debug)]
pub enum StorageError {
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("invalid values: {0}")]
    InvalidValue(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("row encode error: {0}")]
    RowEncodeError(String),
    #[error("transaction conflict: {0}")]
    TransactionConflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal error: {0}")]
    InternalError(String),
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("wal corrupted: {0}")]
    WalCorrupted(String),
}

impl From<crate::wal::WalError> for StorageError {
    /// Bridge WAL errors into the storage error type so WAL primitives can back
    /// storage traits (e.g. `BlockReader`) that return `StorageResult`.
    fn from(e: crate::wal::WalError) -> Self {
        match e {
            crate::wal::WalError::Io(io) => StorageError::IoError(io),
            crate::wal::WalError::CrcMismatch { lsn, .. } => {
                StorageError::WalCorrupted(format!("crc mismatch at lsn {lsn}"))
            }
            crate::wal::WalError::HeaderCorrupt { seg_id } => {
                StorageError::WalCorrupted(format!("segment {seg_id} header corrupt"))
            }
            crate::wal::WalError::InvalidConfig(s) => StorageError::InvalidConfig(s),
            crate::wal::WalError::Closed => StorageError::InternalError("wal closed".into()),
        }
    }
}
