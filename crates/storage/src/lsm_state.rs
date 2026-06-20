use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;

use crate::memtable::MemTable;
use std::sync::Arc;

/// Immutable snapshot of LSM-tree state.
///
/// Swapped atomically via `Arc<LsmState>` under a write lock — readers always
/// see a consistent snapshot.
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
    pub key_range: (Bytes, Bytes), // (smallest, largest)
    pub file_size: u64,
}

/// Engine-level config for [`crate::lsm_engine::LsmEngine`] (node-level, one
/// instance per engine). Stored in the MetaManager system-config table.
#[derive(Clone)]
pub struct LsmEngineOption {
    pub data_dir: PathBuf,
    pub block_size: usize,
    /// Flush worker/thread count for the cross-shard FlushScheduler.
    pub flush_threads: usize,
    /// Compaction thread count (reserved; compaction lands in a later plan).
    pub compaction_threads: usize,
    /// Interval of the cross-shard passive flush sweep.
    pub flush_interval: Duration,
}

impl Default for LsmEngineOption {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            block_size: 4096,
            flush_threads: 2,
            compaction_threads: 2,
            flush_interval: Duration::from_secs(10),
        }
    }
}

/// Table-level config for [`crate::lsm_store::LsmStore`] (per-table; all
/// shards of a table inherit it). Stored in the MetaManager `tables` table.
#[derive(Clone)]
pub struct LsmTableOption {
    pub partition_type: PartitionType,
    pub compression: CompressionStrategy,
    pub memtable_size_limit: usize,
    /// Max immutable memtables before write-path force-flush kicks in.
    pub max_imm_memtables: usize,
}

impl Default for LsmTableOption {
    fn default() -> Self {
        Self {
            partition_type: PartitionType::None,
            compression: CompressionStrategy::Standard,
            memtable_size_limit: 4 * 1024 * 1024, // 4 MiB
            max_imm_memtables: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionType {
    None,
    Hash,
    Range,
}

/// Compaction aggressiveness — frequent-write tables may raise this to compact
/// more often. Consumed once compaction lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressionStrategy {
    None,
    Standard,
    Aggressive,
}
