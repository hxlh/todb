use storage::errors::{StorageError, StorageResult};

#[test]
fn invalid_key_error_display() {
    assert_eq!(
        StorageError::InvalidKey("empty key".into()).to_string(),
        "invalid key: empty key"
    );
}

#[test]
fn row_encode_error_display() {
    assert_eq!(
        StorageError::RowEncodeError("row id 1".into()).to_string(),
        "row encode error: row id 1"
    );
}

#[test]
fn transaction_conflict_error_display() {
    assert_eq!(
        StorageError::TransactionConflict("txn 1".into()).to_string(),
        "transaction conflict: txn 1"
    )
}

#[test]
fn not_found_error_display() {
    assert_eq!(
        StorageError::NotFound("txn 1".into()).to_string(),
        "not found: txn 1"
    )
}

#[test]
fn io_error_display() {
    let _ = std_io_to_storage_error().unwrap_err();
    assert_eq!(
        StorageError::IoError(std::io::ErrorKind::OutOfMemory.into()).to_string(),
        "I/O error: out of memory"
    );
}

fn std_io_to_storage_error() -> StorageResult<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::OutOfMemory,
        "out of memory",
    ))?;
    Ok(())
}
