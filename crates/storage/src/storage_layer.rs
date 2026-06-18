use std::ops::Bound;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use tracing::debug;

use crate::{
    engine::{StorageEngine, DEFAULT_SHARD, ShardId},
    errors::{StorageError, StorageResult},
    iterators::ScanIter,
    lsm_store::LsmStore,
    lsm_state::LsmOption,
    write_batch::WriteBatch,
};

/// Global storage singleton. Routes table-level requests to shards.
///
/// Table → shard routing lives here (via [`MetaManager`]); the storage layer
/// does not perceive shard boundaries — each shard is a routing target, not a
/// router (OB/TiDB model: the query engine decides ranges).
pub struct StorageLayer {
    meta: MetaManager,
    shards: DashMap<ShardId, Arc<dyn StorageEngine>>,
    option: LsmOption,
}

impl StorageLayer {
    pub fn new(option: LsmOption) -> Self {
        let layer = Self {
            meta: MetaManager::new(),
            shards: DashMap::new(),
            option,
        };
        layer.ensure_shard(DEFAULT_SHARD);
        layer
    }

    /// Ensure a shard exists, creating its [`LsmStore`] if needed.
    fn ensure_shard(&self, id: ShardId) {
        self.shards
            .entry(id)
            .or_insert_with(|| Arc::new(LsmStore::new(self.option.clone())) as Arc<dyn StorageEngine>);
    }

    /// Register a new table. Maps to [`DEFAULT_SHARD`] for now.
    pub fn create_table(&self, table_name: &str) -> StorageResult<()> {
        self.meta.create_table(table_name);
        debug!("table registered: {table_name}");
        Ok(())
    }

    /// Write a batch to the shard backing `table_name`.
    pub fn write(&self, table_name: &str, batch: WriteBatch) -> StorageResult<()> {
        let shard_id = self.meta.shard_for(table_name)?;
        let engine = self.acquire_engine(shard_id)?;
        engine.write(batch)
    }

    /// Scan a key range on the shard backing `table_name`.
    pub fn scan(
        &self,
        table_name: &str,
        range: (Bound<Bytes>, Bound<Bytes>),
        reverse: bool,
    ) -> StorageResult<Box<dyn ScanIter>> {
        let shard_id = self.meta.shard_for(table_name)?;
        let engine = self.acquire_engine(shard_id)?;
        engine.scan(range, reverse)
    }

    /// Look up the engine for a shard, cloning the Arc to release the DashMap
    /// guard before the (potentially slow) engine call.
    fn acquire_engine(&self, shard_id: ShardId) -> StorageResult<Arc<dyn StorageEngine>> {
        self.shards
            .get(&shard_id)
            .map(|r| r.value().clone())
            .ok_or_else(|| StorageError::NotFound(format!("shard {shard_id}")))
    }
}

/// Table metadata catalog: table_name → [`TableMeta`].
struct MetaManager {
    tables: DashMap<String, TableMeta>,
    next_table_id: AtomicU64,
}

#[allow(dead_code)]
struct TableMeta {
    table_id: u64,
    shard_ids: Vec<ShardId>, // always [DEFAULT_SHARD] for now
}

impl MetaManager {
    fn new() -> Self {
        Self {
            tables: DashMap::new(),
            next_table_id: AtomicU64::new(1),
        }
    }

    fn create_table(&self, name: &str) {
        self.tables.entry(name.to_string()).or_insert_with(|| {
            let table_id = self.next_table_id.fetch_add(1, Ordering::Relaxed);
            TableMeta {
                table_id,
                shard_ids: vec![DEFAULT_SHARD],
            }
        });
    }

    fn shard_for(&self, name: &str) -> StorageResult<ShardId> {
        self.tables
            .get(name)
            .map(|t| t.shard_ids[0])
            .ok_or_else(|| StorageError::NotFound(format!("table {name}")))
    }
}
