use std::ops::Bound;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use dashmap::DashMap;

use crate::wal::DynWalStore;
use crate::{
    disk_manager::DiskManager,
    engine::{ShardId, StorageEngine, TableOption, TableStore},
    errors::{StorageError, StorageResult},
    flush_scheduler::FlushScheduler,
    iterators::ScanIter,
    lsm_state::LsmEngineOption,
    lsm_store::LsmStore,
    write_batch::WriteBatch,
};

/// LSM storage engine. Owns the global `DiskManager` and the cross-shard
/// `FlushScheduler`; holds all its shards in a `shard_id -> LsmStore` map.
/// Implements [`StorageEngine`]: `create_shard` eagerly builds a shard from a
/// [`TableOption`] (lifecycle, called by create_table); write/scan acquire an
/// existing shard and route by shard_id.
pub struct LsmEngine {
    engine_option: LsmEngineOption,
    disk_manager: Arc<DiskManager>,
    shards: DashMap<ShardId, Arc<LsmStore>>,
    flush_scheduler: OnceLock<FlushScheduler>,
}

impl LsmEngine {
    pub fn new(engine_option: LsmEngineOption) -> Self {
        let disk_manager = Arc::new(DiskManager::new(
            engine_option.data_dir.clone(),
            engine_option.block_size,
        ));
        Self {
            engine_option,
            disk_manager,
            shards: DashMap::new(),
            flush_scheduler: OnceLock::new(),
        }
    }

    pub fn disk_manager(&self) -> &Arc<DiskManager> {
        &self.disk_manager
    }

    pub fn engine_option(&self) -> &LsmEngineOption {
        &self.engine_option
    }

    pub fn acquire(&self, shard_id: ShardId) -> StorageResult<Arc<LsmStore>> {
        self.shards
            .get(&shard_id)
            .map(|r| r.value().clone())
            .ok_or_else(|| StorageError::NotFound(format!("shard {shard_id}")))
    }
}

impl StorageEngine for LsmEngine {
    /// Initialize the engine and start the cross-shard flush scheduler. The
    /// background sweep thread holds the consumed `Arc<Self>`, keeping the
    /// engine alive independent of the engines map. OB-style: flush starts as
    /// part of init (was a separate `start_flush` call).
    fn init(self: Arc<Self>) -> StorageResult<()> {
        // Idempotent: a re-init leaves the existing scheduler in place rather
        // than spawning a second sweep thread.
        if self.flush_scheduler.get().is_some() {
            return Ok(());
        }
        let interval = self.engine_option.flush_interval;
        let this = self.clone();
        let scheduler = FlushScheduler::start(interval, move || {
            for entry in this.shards.iter() {
                let _ = entry.flush_oldest_imm();
            }
        });
        let _ = self.flush_scheduler.set(scheduler);
        Ok(())
    }

    /// Eagerly create a shard: build its [`LsmStore`] from `table_option` and
    /// register it (OB-style `create_tablet`). Idempotent — an existing shard
    /// is left as-is. Called only by the create_table lifecycle path; read/
    /// write use `acquire`.
    fn create_shard(
        &self,
        shard_id: ShardId,
        table_option: &TableOption,
        wal_store: Arc<DynWalStore>,
    ) -> StorageResult<()> {
        let opt = match table_option {
            TableOption::LsmTree(o) => o.clone(),
        };
        self.shards
            .entry(shard_id)
            .or_insert_with(|| {
                Arc::new(LsmStore::new(
                    opt,
                    self.disk_manager.clone(),
                    shard_id,
                    wal_store,
                ))
            });
        Ok(())
    }

    fn write(&self, shard_id: ShardId, batch: WriteBatch) -> StorageResult<()> {
        let store = self.acquire(shard_id)?;
        store.write(batch)
    }

    fn scan(
        &self,
        shard_id: ShardId,
        range: (Bound<Bytes>, Bound<Bytes>),
        reverse: bool,
    ) -> StorageResult<Box<dyn ScanIter>> {
        let store = self.acquire(shard_id)?;
        store.scan(range, reverse)
    }
}
