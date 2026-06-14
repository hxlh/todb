use std::path::PathBuf;

use bytes::Bytes;

use crate::{builder::SstFooter, memtable::MemTable};
use std::sync::Arc;

/// Immutable snapshot of LSM-tree state.
///
/// Swapped atomically via `Arc<LsmState>` under a write lock — readers always
/// see a consistent snapshot.
/// 
pub struct LsmState {
    pub active_mem: Arc<MemTable<Bytes, Bytes>>,
    /// Immutable memtables awaiting flush. Newest first (`imm[0]` = most recent
    /// switch), oldest last (`imm.last()` = next to flush, FIFO).
    pub imm_memtables: Vec<Arc<MemTable<Bytes, Bytes>>>,
    pub levels: Vec<LevelMeta>,
}

impl LsmState {
    /// Initial state: empty active memtable, single L0 level.
    pub fn new() -> Self {
        Self {
            active_mem: Arc::new(MemTable::new()),
            imm_memtables: Vec::new(),
            levels: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct LevelMeta {
    pub level: u32,
    pub ssts: Vec<SstMeta>,
}

/// Metadata for a flushed SST file.
#[derive(Clone)]
pub struct SstMeta {
    pub id: u64,
    pub file_path: PathBuf,
    pub footer: SstFooter,
    pub key_range: (Bytes, Bytes), // (smallest, largest)
    pub file_size: u64,
}

/// Configuration for [`crate::lsm_store::LsmStore`].
#[derive(Clone)]
pub struct LsmOption {
    pub memtable_size_limit: usize,
    pub block_size: usize,
    pub data_dir: PathBuf,
}

impl Default for LsmOption {
    fn default() -> Self {
        Self {
            memtable_size_limit: 4 * 1024 * 1024, // 4 MiB
            block_size: 4096,
            data_dir: PathBuf::from("./data"),
        }
    }
}
