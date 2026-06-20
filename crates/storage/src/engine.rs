use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;

use crate::{errors::StorageResult, iterators::ScanIter, lsm_state::LsmTableOption, write_batch::WriteBatch};

/// Identifies a shard within the storage layer.
pub type ShardId = u64;
/// Identifies a table.
pub type TableId = u64;

pub const DEFAULT_SHARD: ShardId = 0;

/// Per-table option, wrapped for multi-engine dispatch. Implemented engines
/// destructure their variant in `create_shard`.
#[derive(Clone)]
pub enum TableOption {
    LsmTree(LsmTableOption),
}

/// Storage engine interface (engine-level). Implemented by `LsmEngine`.
/// Routes by shard_id to the per-shard [`TableStore`].
///
/// `create_shard` is a **lifecycle** method: it explicitly creates a shard,
/// loading `table_option` into the per-shard store (eager, OB-style — mirrors
/// `ObLSTabletService::create_tablet`). Only `MetaManager::create_table` calls
/// it. The read/write paths (`write`/`scan`) do NOT create shards — they
/// acquire an already-created shard and surface `NotFound` when absent
/// (OB-style `get_tablet`).
pub trait StorageEngine: Send + Sync {
    /// Initialize the engine and start its background services (flush sweep).
    /// Consumes an `Arc<Self>` so spawned threads keep the engine alive
    /// (OB-style: background pools start during init, not at construction).
    fn init(self: Arc<Self>) -> StorageResult<()>;
    fn create_shard(&self, shard_id: ShardId, table_option: &TableOption) -> StorageResult<()>;
    fn write(&self, shard_id: ShardId, batch: WriteBatch) -> StorageResult<()>;
    fn scan(
        &self,
        shard_id: ShardId,
        range: (Bound<Bytes>, Bound<Bytes>),
        reverse: bool,
    ) -> StorageResult<Box<dyn ScanIter>>;
}

/// Row storage engine interface (per-shard). key = primary key bytes,
/// value = encoded non-primary columns. `LsmStore` is the sole implementation
/// for now; the instance is bound to one shard, so write/scan take no shard_id.
pub trait TableStore: Send + Sync + 'static {
    fn write(&self, batch: WriteBatch) -> StorageResult<()>;
    fn scan(
        &self,
        range: (Bound<Bytes>, Bound<Bytes>),
        reverse: bool,
    ) -> StorageResult<Box<dyn ScanIter>>;
}
