use std::ops::Bound;

use bytes::Bytes;

use crate::{errors::StorageResult, iterators::ScanIter, write_batch::WriteBatch};

/// Identifies a shard within the storage layer.
pub type ShardId = u64;

pub const DEFAULT_SHARD: ShardId = 0;

/// Row storage engine interface.
///
/// key = primary key bytes, value = encoded non-primary columns.
/// `LsmStore` is the sole implementation for now. The query engine routes
/// requests to shards; the storage layer does not perceive shard boundaries —
/// it accepts a key range and returns data.
pub trait StorageEngine: Send + Sync + 'static {
    fn write(&self, batch: WriteBatch) -> StorageResult<()>;
    fn scan(&self, range: (Bound<Bytes>, Bound<Bytes>)) -> StorageResult<Box<dyn ScanIter>>;
}
