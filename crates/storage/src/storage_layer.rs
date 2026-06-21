use std::ops::Bound;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;

use crate::{
    engine::{ShardId, StorageEngine, TableOption},
    errors::{StorageError, StorageResult},
    iterators::ScanIter,
    lsm_engine::LsmEngine,
    lsm_state::LsmEngineOption,
    log_service::{LogService, RgId, RgOption},
    write_batch::WriteBatch,
};

/// Engine kind. Key in the engine map; carried in `ShardMeta` so the router
/// picks the right engine per shard.
#[derive(Eq, PartialEq, Hash, Clone, Debug)]
pub enum Engine {
    LsmTree,
}

/// Engine-level option, wrapped for multi-engine dispatch. Stored in the
/// MetaManager system-config table (Plan 3); `StorageLayer::new` uses the
/// default for now.
#[derive(Clone)]
pub enum EngineOption {
    LsmTree(LsmEngineOption),
}

/// Storage data layer. Holds engines in a `DashMap<Engine, Arc<dyn StorageEngine>>`
/// and routes by engine + shard_id. Knows nothing about table names or table
/// metadata — that's [`crate::meta_manager::MetaManager`]'s job. MetaManager
/// calls into this to write system-table rows and to `create_shard`.
pub struct StorageLayer {
    engines: DashMap<Engine, Arc<dyn StorageEngine>>,
    log_service: Arc<LogService>,
}

impl StorageLayer {
    pub fn new(engine_option: LsmEngineOption) -> Self {
        let engines: DashMap<Engine, Arc<dyn StorageEngine>> = DashMap::new();
        let lsm = Arc::new(LsmEngine::new(engine_option));
        // init consumes a clone of the Arc (the flush sweep thread keeps it
        // alive); the original goes into the engines map.
        let _ = lsm.clone().init();
        engines.insert(Engine::LsmTree, lsm);
        Self {
            engines,
            log_service: Arc::new(LogService::default()),
        }
    }

    /// Build with an explicit WAL root (used by tests / when a wal_root is
    /// known at construction; production uses the default via [`new`]).
    pub fn with_wal_root(engine_option: LsmEngineOption, wal_root: PathBuf) -> Self {
        let engines: DashMap<Engine, Arc<dyn StorageEngine>> = DashMap::new();
        let lsm = Arc::new(LsmEngine::new(engine_option));
        let _ = lsm.clone().init();
        engines.insert(Engine::LsmTree, lsm);
        Self {
            engines,
            log_service: Arc::new(LogService::new(wal_root)),
        }
    }

    /// Lifecycle: create a shard under `engine` from `table_option`. Called
    /// only by `MetaManager::create_table`; read/write paths acquire an
    /// already-created shard instead.
    pub fn create_shard(
        &self,
        engine: &Engine,
        shard_id: ShardId,
        rg_id: RgId,
        table_option: &TableOption,
    ) -> StorageResult<()> {
        let eng = self.map_engine(engine)?;
        let wal_store = self.log_service.get(rg_id)?;
        eng.create_shard(shard_id, table_option, wal_store)
    }

    /// Lifecycle: create a replication group's WalStore via LogService. Must
    /// be called before `create_shard` on this rg_id.
    pub fn create_replication_group(&self, rg_id: RgId, opt: &RgOption) -> StorageResult<()> {
        self.log_service.create_rg(rg_id, opt)
    }

    /// Write a batch to `shard_id` under `engine`.
    pub fn write(
        &self,
        engine: &Engine,
        shard_id: ShardId,
        batch: WriteBatch,
    ) -> StorageResult<()> {
        let eng = self.map_engine(engine)?;
        eng.write(shard_id, batch)
    }

    /// Scan a key range on `shard_id` under `engine`.
    pub fn scan(
        &self,
        engine: &Engine,
        shard_id: ShardId,
        range: (Bound<Bytes>, Bound<Bytes>),
        reverse: bool,
    ) -> StorageResult<Box<dyn ScanIter>> {
        let eng = self.map_engine(engine)?;
        eng.scan(shard_id, range, reverse)
    }

    fn map_engine(&self, engine: &Engine) -> StorageResult<Arc<dyn StorageEngine>> {
        self.engines
            .get(engine)
            .map(|e| e.value().clone())
            .ok_or_else(|| StorageError::NotFound(format!("engine {engine:?} not registered")))
    }
}
