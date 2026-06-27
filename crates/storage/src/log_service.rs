use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use crate::{
    errors::{StorageError, StorageResult},
    wal::{DynWalStore, file_wal_store::FileWalStore},
};

pub type RgId = u64;

/// Per-replication-group options. Carried into `LogService::create_rg` and
/// applied to the `WalStore` of that RG. (Engine-level; deliberately not on
/// `LsmEngineOption` — WAL config belongs to the log service, not the engine.)
#[derive(Clone)]
pub struct RgOption {
    pub rf: u32,
    pub wal_buffer_size: usize,
    pub wal_sync_interval: Duration,
    pub wal_segment_size: u64,
}

impl Default for RgOption {
    fn default() -> Self {
        Self {
            rf: 1,
            wal_buffer_size: 1024 * 1024,        // 1 MiB
            wal_sync_interval: Duration::from_millis(100),
            wal_segment_size: 64 * 1024 * 1024,  // 64 MiB
        }
    }
}

/// Engine-wide WAL service. Owns one `WalStore` per replication group,
/// physically isolated under `{wal_root}/{rg_id}/`. Independent of any storage
/// engine (LSM, future B+tree all share a RG's WalStore).
pub struct LogService {
    wal_root: PathBuf,
    stores: DashMap<RgId, Arc<DynWalStore>>,
}

impl LogService {
    pub fn new(wal_root: PathBuf) -> Self {
        Self {
            wal_root,
            stores: DashMap::new(),
        }
    }

    /// Convenience: LogService over the default `./data/wal` root. Used by
    /// `StorageLayer::new` (production); tests pass an explicit root.
    pub fn default() -> Self {
        Self::new(PathBuf::from("./data/wal"))
    }

    /// Create the WalStore for `rg_id` (builds a `FileWalStore` and starts its
    /// sync thread). Idempotent; returns existing store if any.
    pub fn create_rg(&self, rg_id: RgId, opt: &RgOption) -> StorageResult<()> {
        if self.stores.contains_key(&rg_id) {
            return Ok(());
        }
        let dir = self.wal_root.join(rg_id.to_string());
        let store: Arc<FileWalStore> = Arc::new(FileWalStore::open_with(
            dir,
            rg_id,
            opt.wal_buffer_size,
            opt.wal_segment_size,
        ));
        store.start_sync(opt.wal_sync_interval);
        self.stores.insert(rg_id, store as Arc<DynWalStore>);
        Ok(())
    }

    /// Get the WalStore for `rg_id`. NotFound if not created.
    pub fn get(&self, rg_id: RgId) -> StorageResult<Arc<DynWalStore>> {
        self.stores
            .get(&rg_id)
            .map(|r| r.value().clone())
            .ok_or_else(|| StorageError::NotFound(format!("replication group {rg_id}")))
    }
}
