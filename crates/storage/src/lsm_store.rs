use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::RwLock;

use crate::{
    block::FileBlockReader,
    builder::SstOption,
    engine::StorageEngine,
    errors::StorageResult,
    iterators::{
        EntryValue,
        map_iter::MapIter,
        concat_iter::ConcatIter,
        merge_iter::MergeIter,
        scan_iter::ScanIter,
        sst_iter::SstIter,
        storage_iter::{AsArray, ForwardIter, StorageIter},
        two_merge_iter::TwoMergeIter,
    },
    lsm_state::{LsmOption, LsmState},
    memtable::{Entry, MemTable, OwnedMemTableIter},
    write_batch::{WriteBatch, WriteEntry},
};

// ── Iterator type aliases for the scan path ──

type MemSide = MergeIter<MapIter<OwnedMemTableIter<Bytes, Bytes>>>;
type L0Side = MergeIter<SstIter<FileBlockReader>>;
type LnSide = MergeIter<ConcatIter<SstIter<FileBlockReader>>>;
// TwoMergeIter: L0 (A) wins over L1+ (B) on key overlap — L0 数据更新。
type SstSide = TwoMergeIter<L0Side, LnSide>;
type MergedIter = TwoMergeIter<MemSide, SstSide>;

/// LSM-tree storage engine.
///
/// State is swapped atomically via `Arc<LsmState>` under a write lock.
/// Readers clone the Arc and see a consistent snapshot.
pub struct LsmStore {
    state: Arc<RwLock<Arc<LsmState>>>,
    option: LsmOption,
}

impl LsmStore {
    pub fn new(option: LsmOption) -> Self {
        std::fs::create_dir_all(&option.data_dir).ok();
        Self {
            state: Arc::new(RwLock::new(Arc::new(LsmState::new()))),
            option,
        }
    }

    /// Read-lock the state and clone the Arc snapshot.
    fn snapshot(&self) -> Arc<LsmState> {
        self.state.read().clone()
    }

    /// Atomically switch the active memtable to a fresh one, pushing the old
    /// active into `imm_memtables` (front = newest).
    fn switch_memtable(&self) {
        let mut guard = self.state.write();
        let old = guard.clone();

        if old.active_mem.estimate_memory() < self.option.memtable_size_limit {
            // Another thread already switched.
            return;
        }

        let mut new_imms = Vec::with_capacity(1 + old.imm_memtables.len());
        new_imms.push(old.active_mem.clone());
        new_imms.extend_from_slice(&old.imm_memtables);

        *guard = Arc::new(LsmState {
            active_mem: Arc::new(MemTable::new()),
            imm_memtables: new_imms,
            levels: old.levels.clone(),
        });
    }
}

impl StorageEngine for LsmStore {
    fn write(&self, batch: WriteBatch) -> StorageResult<()> {
        let need_switch = {
            let state = self.snapshot();
            let active = &state.active_mem;
            for entry in batch.entries {
                match entry {
                    WriteEntry::Put { key, value } => active.put(key, value),
                    WriteEntry::Delete { key } => active.delete(key),
                }
            }
            active.estimate_memory() >= self.option.memtable_size_limit
        };

        if need_switch {
            self.switch_memtable();
        }
        Ok(())
    }

    fn scan(&self, range: (Bound<Bytes>, Bound<Bytes>)) -> StorageResult<Box<dyn ScanIter>> {
        let state = self.snapshot();

        // Build memtable iterators (active + all imms).
        let mut mem_iters: Vec<MapIter<OwnedMemTableIter<Bytes, Bytes>>> =
            vec![MapIter::new(state.active_mem.iter())];
        for imm in &state.imm_memtables {
            mem_iters.push(MapIter::new(imm.iter()));
        }
        let mem_merge = MergeIter::new(mem_iters);

        // SST 侧：
        //   L0   → MergeIter<SstIter>           (多次 flush 可能 overlap，堆合并)
        //   L1+  → MergeIter<ConcatIter<SstIter>> (同层不重叠用 ConcatIter，跨层 merge)
        //   合并 → TwoMergeIter(L0, L1+)         (A 侧 L0 更新，同 key 胜出)
        //
        // 读优先级（从新到旧）：
        //   active_mem > imm[0..n] > L0 SSTs > L1 SSTs > ... > LN SSTs
        let block_size = self.option.block_size;

        // L0: MergeIter (SST 间可能 overlap)
        let l0_iters: Vec<SstIter<FileBlockReader>> = state
            .levels
            .first()
            .into_iter()
            .flat_map(|lvl| &lvl.ssts)
            .map(|sst| {
                let reader = Arc::new(FileBlockReader::open(&sst.file_path, block_size)?);
                let opt = SstOption::default().block_size(block_size);
                SstIter::new(reader, sst.footer, opt)
            })
            .collect::<StorageResult<_>>()?;
        let l0_merge = MergeIter::new(l0_iters);

        // L1+: 每层 ConcatIter（同层有序不重叠），跨层 MergeIter
        let ln_iters: Vec<ConcatIter<SstIter<FileBlockReader>>> = state
            .levels
            .iter()
            .skip(1)
            .map(|level| -> StorageResult<_> {
                let iters: Vec<_> = level
                    .ssts
                    .iter()
                    .map(|sst| {
                        let reader = Arc::new(FileBlockReader::open(&sst.file_path, block_size)?);
                        let opt = SstOption::default().block_size(block_size);
                        SstIter::new(reader, sst.footer, opt)
                    })
                    .collect::<StorageResult<_>>()?;
                Ok(ConcatIter::new(iters))
            })
            .collect::<StorageResult<_>>()?;
        let ln_merge = MergeIter::new(ln_iters);

        // L0 (A) wins over L1+ (B) on key overlap.
        let sst_merge = TwoMergeIter::new(l0_merge, ln_merge)?;

        let merged = TwoMergeIter::new(mem_merge, sst_merge)?;

        let mut scanner = LsmScanIter {
            inner: merged,
            upper: range.1,
        };

        // Position at the lower bound.
        match &range.0 {
            Bound::Included(start) => scanner.seek(start)?,
            Bound::Excluded(start) => {
                scanner.seek(start)?;
                // Skip past any entries equal to the excluded key.
                while scanner.raw_valid() {
                    match scanner.inner.key() {
                        Some(k) if k.as_bytes() == start.as_ref() => scanner.inner.next()?,
                        _ => break,
                    }
                }
            }
            Bound::Unbounded => {
                scanner.seek_to_first()?;
            }
        }

        Ok(Box::new(scanner))
    }
}

/// Range-limited scanner wrapping the merged LSM iterator.
///
/// Enforces the upper bound by returning `valid() == false` once the iterator
/// passes the end of the scan range.
pub struct LsmScanIter {
    inner: MergedIter,
    upper: Bound<Bytes>,
}

impl LsmScanIter {
    /// Whether the inner iterator has a current element (ignoring range).
    fn raw_valid(&self) -> bool {
        self.inner.valid()
    }
}

impl ScanIter for LsmScanIter {
    fn valid(&self) -> bool {
        if !self.raw_valid() {
            return false;
        }
        let key = self.inner.key().unwrap();
        match &self.upper {
            Bound::Included(end) => key.as_bytes() <= end.as_ref(),
            Bound::Excluded(end) => key.as_bytes() < end.as_ref(),
            Bound::Unbounded => true,
        }
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner.seek_to_first()
    }

    fn seek(&mut self, target: &[u8]) -> StorageResult<()> {
        self.inner.seek(&crate::row_key::BinaryKey::from(target))
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner.next()
    }

    fn key(&self) -> Option<&[u8]> {
        if !self.valid() {
            return None;
        }
        self.inner.key().map(|k| k.as_array())
    }

    fn value(&self) -> Option<Entry<&[u8]>> {
        if !self.valid() {
            return None;
        }
        self.inner.value().map(|v: EntryValue| v.into())
    }
}
