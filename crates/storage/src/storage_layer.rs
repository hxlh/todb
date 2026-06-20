use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;

use crate::{
    engine::{ShardId, StorageEngine, TableOption},
    errors::{StorageError, StorageResult},
    iterators::ScanIter,
    lsm_engine::LsmEngine,
    lsm_state::LsmEngineOption,
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
}

impl StorageLayer {
    pub fn new(engine_option: LsmEngineOption) -> Self {
        let engines: DashMap<Engine, Arc<dyn StorageEngine>> = DashMap::new();
        let lsm = Arc::new(LsmEngine::new(engine_option));
        // init consumes a clone of the Arc (the flush sweep thread keeps it
        // alive); the original goes into the engines map.
        let _ = lsm.clone().init();
        engines.insert(Engine::LsmTree, lsm);
        Self { engines }
    }

    /// Lifecycle: create a shard under `engine` from `table_option`. Called
    /// only by `MetaManager::create_table`; read/write paths acquire an
    /// already-created shard instead.
    pub fn create_shard(
        &self,
        engine: &Engine,
        shard_id: ShardId,
        table_option: &TableOption,
    ) -> StorageResult<()> {
        let eng = self.map_engine(engine)?;
        eng.create_shard(shard_id, table_option)
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
