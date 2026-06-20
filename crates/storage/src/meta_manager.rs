use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::{
    engine::{ShardId, TableId, TableOption},
    errors::{StorageError, StorageResult},
    storage_layer::{Engine, StorageLayer},
};

/// Per-shard routing metadata returned to the query layer. The query layer
/// uses `engine` + `shard_id` to read/write via [`StorageLayer`].
#[derive(Clone)]
pub struct ShardMeta {
    pub table_id: TableId,
    pub shard_id: ShardId,
    pub engine: Engine,
}

/// Metadata facade. The query layer creates tables / looks up shards here.
/// Metadata itself is persisted as rows in system tables (written via
/// [`StorageLayer`]); for now it is held in-memory (Plan 3 adds persistence).
///
/// Holds an `Arc<StorageLayer>` to write system-table rows and to `create_shard`.
/// `StorageLayer` does NOT hold MetaManager — dependency is one-directional.
pub struct MetaManager {
    storage: Arc<StorageLayer>,
    table_to_id: DashMap<String, TableId>,
    table_to_shard: DashMap<TableId, ShardId>,
    shards: DashMap<ShardId, ShardMeta>,
    table_options: DashMap<TableId, TableOption>,
    next_table_id: AtomicU64,
    next_shard_id: AtomicU64,
}

impl MetaManager {
    pub fn new(storage: Arc<StorageLayer>) -> Self {
        Self {
            storage,
            table_to_id: DashMap::new(),
            table_to_shard: DashMap::new(),
            shards: DashMap::new(),
            table_options: DashMap::new(),
            next_table_id: AtomicU64::new(0),
            next_shard_id: AtomicU64::new(0),
        }
    }

    /// Register a new table: allocate ids, record metadata + options, then
    /// build the shard under the chosen engine.
    pub fn create_table(
        &self,
        name: &str,
        engine: Engine,
        table_option: TableOption,
    ) -> StorageResult<()> {
        let table_id = self.next_table_id.fetch_add(1, Ordering::Relaxed);
        let shard_id = self.next_shard_id.fetch_add(1, Ordering::Relaxed);
        self.table_to_id.insert(name.to_string(), table_id);
        self.table_to_shard.insert(table_id, shard_id);
        self.shards.insert(
            shard_id,
            ShardMeta {
                table_id,
                shard_id,
                engine: engine.clone(),
            },
        );
        self.table_options.insert(table_id, table_option.clone());
        // TODO(Plan 3): persist this row to the `tables` system table via
        // self.storage.write(...). Metadata is in-memory for now.
        self.storage.create_shard(&engine, shard_id, &table_option)?;
        Ok(())
    }

    /// Look up the shard routing a table (by name).
    pub fn shard_for(&self, name: &str) -> StorageResult<ShardMeta> {
        let table_id = self
            .table_to_id
            .get(name)
            .map(|t| *t)
            .ok_or_else(|| StorageError::NotFound(format!("table {name}")))?;
        let shard_id = self
            .table_to_shard
            .get(&table_id)
            .map(|t| *t)
            .ok_or_else(|| StorageError::NotFound(format!("table {table_id}")))?;
        self.shards
            .get(&shard_id)
            .map(|s| s.clone())
            .ok_or_else(|| StorageError::NotFound(format!("shard {shard_id}")))
    }

    /// Look up a table's option (engine-specific).
    pub fn table_option(&self, table_id: TableId) -> StorageResult<TableOption> {
        self.table_options
            .get(&table_id)
            .map(|o| o.clone())
            .ok_or_else(|| StorageError::NotFound(format!("table_option {table_id}")))
    }

    /// Borrow the storage layer (for the query layer to write/scan user data).
    pub fn storage(&self) -> &Arc<StorageLayer> {
        &self.storage
    }
}
