use crate::{errors::StorageResult, write_batch::WriteBatch};

/// Write-ahead log interface.
///
/// Real implementations persist [`WriteBatch`]es before applying them to the
/// memtable so they can be replayed on recovery. [`NoopWal`] is the no-op
/// stand-in for the current phase.
pub trait WalWriter: Send + Sync {
    fn append(&self, batch: &WriteBatch) -> StorageResult<()>;
    fn sync(&self) -> StorageResult<()>;
    fn recover(&self) -> StorageResult<Vec<WriteBatch>>;
}

/// No-op WAL. Writes succeed silently; recovery returns nothing.
pub struct NoopWal;

impl WalWriter for NoopWal {
    fn append(&self, _batch: &WriteBatch) -> StorageResult<()> {
        Ok(())
    }
    fn sync(&self) -> StorageResult<()> {
        Ok(())
    }
    fn recover(&self) -> StorageResult<Vec<WriteBatch>> {
        Ok(Vec::new())
    }
}
