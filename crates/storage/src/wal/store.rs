use crate::{engine::ShardId, errors::StorageResult, write_batch::WriteBatch};

/// Per-WalStore monotonically increasing log sequence number. Reserved as the
/// raft log index when raft lands.
pub type Lsn = u64;

/// One WAL record. `recover` streams these. `payload` is an ADT so new record
/// kinds (Checkpoint, Raft metadata, ...) can be added without changing the
/// on-disk framing.
pub struct WalEntry {
    pub shard_id: ShardId,
    pub lsn: Lsn,
    pub payload: WalPayload,
}

/// Kinds of records stored in the WAL.
pub enum WalPayload {
    /// User write batch (put/delete), replayed to a shard's memtable.
    Write(WriteBatch),
    // Reserved for future: Checkpoint, Raft metadata, ...
}

/// Per-replication-group WAL. Shared across all shards (and engines) under
/// one RG. Entries carry `shard_id` so recovery can dispatch per shard.
pub trait WalStore: Send + Sync {
    /// Append a user batch for `shard_id` to the in-memory buffer; persistence
    /// is driven by the sync thread (interval / buffer-full). Returns the
    /// assigned lsn. Takes `&WriteBatch` because the caller also needs the
    /// batch to apply the memtable (no ownership move).
    fn append(&self, shard_id: ShardId, batch: &WriteBatch) -> StorageResult<Lsn>;
    /// Flush + fsync the buffer immediately.
    fn sync(&self) -> StorageResult<()>;
    /// Stream entries in lsn order for replay. An iterator (not a Vec) so a
    /// large WAL is not materialized into memory at once.
    fn recover(
        &self,
    ) -> StorageResult<Box<dyn Iterator<Item = StorageResult<WalEntry>> + Send>>;
}

/// No-op WAL. Writes succeed silently; recovery yields nothing. Useful for
/// tests and shards that opt out of durability.
pub struct NoopWal;

impl WalStore for NoopWal {
    fn append(&self, _shard_id: ShardId, _batch: &WriteBatch) -> StorageResult<Lsn> {
        Ok(0)
    }
    fn sync(&self) -> StorageResult<()> {
        Ok(())
    }
    fn recover(
        &self,
    ) -> StorageResult<Box<dyn Iterator<Item = StorageResult<WalEntry>> + Send>> {
        Ok(Box::new(std::iter::empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write_batch::WriteBatch;
    use bytes::Bytes;

    #[test]
    fn noop_wal_returns_empty_recover() {
        let wal = NoopWal;
        let mut b = WriteBatch::new();
        b.put(Bytes::from_static(b"k"), Bytes::from_static(b"v"));
        let lsn = wal.append(0, &b).unwrap();
        assert_eq!(lsn, 0);
        wal.sync().unwrap();
        let mut iter = wal.recover().unwrap();
        assert!(iter.next().is_none());
    }
}
