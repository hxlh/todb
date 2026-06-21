use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::{
    engine::{ShardId, TableId, TableOption},
    errors::{StorageError, StorageResult},
    log_service::{RgId, RgOption},
    storage_layer::{Engine, StorageLayer},
};

/// Default replication group (rg_id = 0), bootstrapped by `MetaManager::new`.
/// System tables land here.
pub const DEFAULT_RG: RgId = 0;

/// Per-shard routing metadata returned to the query layer. The query layer
/// uses `engine` + `shard_id` to read/write via [`StorageLayer`].
#[derive(Clone)]
pub struct ShardMeta {
    pub table_id: TableId,
    pub shard_id: ShardId,
    pub engine: Engine,
}

/// Replication-group metadata (replication factor, members). rf=1 for now;
/// `members` reserved for raft.
#[derive(Clone)]
pub struct ReplicaGroup {
    pub rg_id: RgId,
    pub rf: u32,
    pub members: Vec<u64>,
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
    shard_to_rg: DashMap<ShardId, RgId>,
    rg_meta: DashMap<RgId, ReplicaGroup>,
    next_table_id: AtomicU64,
    next_shard_id: AtomicU64,
    #[allow(dead_code)]
    next_rg_id: AtomicU64,
}

impl MetaManager {
    pub fn new(storage: Arc<StorageLayer>) -> Self {
        let mgr = Self {
            storage,
            table_to_id: DashMap::new(),
            table_to_shard: DashMap::new(),
            shards: DashMap::new(),
            table_options: DashMap::new(),
            shard_to_rg: DashMap::new(),
            rg_meta: DashMap::new(),
            next_table_id: AtomicU64::new(0),
            next_shard_id: AtomicU64::new(0),
            next_rg_id: AtomicU64::new(1), // 0 reserved for DEFAULT_RG
        };
        // Bootstrap DEFAULT_RG (system tables land here, rf=1).
        let _ = mgr.create_replication_group(DEFAULT_RG, RgOption::default());
        mgr
    }

    /// Create a replication group (builds its WalStore via StorageLayer).
    /// Must be called before create_table on this rg_id. Idempotent.
    pub fn create_replication_group(
        &self,
        rg_id: RgId,
        rg_option: RgOption,
    ) -> StorageResult<()> {
        if self.rg_meta.contains_key(&rg_id) {
            return Ok(());
        }
        self.storage.create_replication_group(rg_id, &rg_option)?;
        self.rg_meta.insert(
            rg_id,
            ReplicaGroup {
                rg_id,
                rf: rg_option.rf,
                members: vec![],
            },
        );
        Ok(())
    }

    /// Register a new table under `rg_id`. The RG must exist
    /// (create_replication_group first): allocate ids, record metadata +
    /// options, then build the shard under the chosen engine.
    pub fn create_table(
        &self,
        rg_id: RgId,
        name: &str,
        engine: Engine,
        table_option: TableOption,
    ) -> StorageResult<()> {
        if !self.rg_meta.contains_key(&rg_id) {
            return Err(StorageError::NotFound(format!(
                "replication group {rg_id} not created"
            )));
        }
        let table_id = self.next_table_id.fetch_add(1, Ordering::Relaxed);
        let shard_id = self.next_shard_id.fetch_add(1, Ordering::Relaxed);
        self.table_to_id.insert(name.to_string(), table_id);
        self.table_to_shard.insert(table_id, shard_id);
        self.shard_to_rg.insert(shard_id, rg_id);
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
        self.storage
            .create_shard(&engine, shard_id, rg_id, &table_option)?;
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
