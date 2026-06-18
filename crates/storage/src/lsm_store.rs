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
        map_iter::MapIter,
        concat_iter::ConcatIter,
        merge_iter::MergeIter,
        scan_iter::ScanIter,
        sst_iter::SstIter,
        storage_iter::{AsArray, ForwardIter, ReverseIter},
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

    fn scan(
        &self,
        range: (Bound<Bytes>, Bound<Bytes>),
        reverse: bool,
    ) -> StorageResult<Box<dyn ScanIter>> {
        let state = self.snapshot();
        let merged = self.build_merged_iter(&state)?;

        if reverse {
            let mut scan = LsmReverseScan::new(merged, range.0, range.1);
            scan.init()?;
            Ok(Box::new(scan))
        } else {
            let mut scan = LsmForwardScan::new(merged, range.0, range.1);
            scan.init()?;
            Ok(Box::new(scan))
        }
    }
}

impl LsmStore {
    fn build_merged_iter(&self, state: &LsmState) -> StorageResult<MergedIter> {
        let block_size = self.option.block_size;

        let mut mem_iters: Vec<MapIter<OwnedMemTableIter<Bytes, Bytes>>> =
            vec![MapIter::new(state.active_mem.iter())];
        for imm in &state.imm_memtables {
            mem_iters.push(MapIter::new(imm.iter()));
        }
        let mem_merge = MergeIter::new(mem_iters);

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

        let sst_merge = TwoMergeIter::new(l0_merge, ln_merge)?;
        TwoMergeIter::new(mem_merge, sst_merge)
    }
}

// ── Forward range-limited scan ──

/// Forward scan: lower bound positions via seek, upper bound enforced in `valid()`.
pub struct LsmForwardScan<I: ForwardIter> {
    inner: I,
    lower: Bound<Bytes>,
    upper: Bound<Bytes>,
}

impl<I> LsmForwardScan<I>
where
    I: ForwardIter,
    for<'a> I::Key<'a>: From<&'a [u8]>,
{
    fn new(inner: I, lower: Bound<Bytes>, upper: Bound<Bytes>) -> Self {
        Self { inner, lower, upper }
    }

    fn init(&mut self) -> StorageResult<()> {
        match &self.lower {
            Bound::Included(start) => self.inner.seek(&start.as_ref().into()),
            Bound::Excluded(start) => {
                self.inner.seek(&start.as_ref().into())?;
                while self.inner.valid() {
                    let is_equal = self.inner.key().map_or(false, |k| k == start.as_ref().into());
                    if is_equal {
                        self.inner.next()?;
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            Bound::Unbounded => self.inner.seek_to_first(),
        }
    }
}

impl<I> ScanIter for LsmForwardScan<I>
where
    I: ForwardIter + Send,
    for<'a> I::Key<'a>: AsArray<'a> + From<&'a [u8]>,
    for<'a> I::Value<'a>: Into<Entry<&'a [u8]>>,
{
    fn valid(&self) -> bool {
        if !self.inner.valid() {
            return false;
        }
        let key = self.inner.key().unwrap();
        match &self.upper {
            Bound::Included(end) => key <= (&end[..]).into(),
            Bound::Excluded(end) => key < (&end[..]).into(),
            Bound::Unbounded => true,
        }
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner.seek_to_first()
    }

    fn seek(&mut self, target: &[u8]) -> StorageResult<()> {
        self.inner.seek(&target.into())
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
        self.inner.value().map(|v| v.into())
    }
}

// ── Reverse range-limited scan ──

/// Reverse scan: upper bound positions via seek, lower bound enforced in `valid()`.
pub struct LsmReverseScan<I: ReverseIter> {
    inner: I,
    lower: Bound<Bytes>,
    upper: Bound<Bytes>,
}

impl<I> LsmReverseScan<I>
where
    I: ReverseIter,
    for<'a> I::Key<'a>: From<&'a [u8]>,
{
    fn new(inner: I, lower: Bound<Bytes>, upper: Bound<Bytes>) -> Self {
        Self { inner, lower, upper }
    }

    fn init(&mut self) -> StorageResult<()> {
        match &self.upper {
            Bound::Included(end) => self.inner.seek(&end.as_ref().into()),
            Bound::Excluded(end) => {
                self.inner.seek(&end.as_ref().into())?;
                while self.inner.valid() {
                    let is_equal = self.inner.key().map_or(false, |k| k == end.as_ref().into());
                    if is_equal {
                        self.inner.next()?;
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            Bound::Unbounded => self.inner.seek_to_first(),
        }
    }
}

impl<I> ScanIter for LsmReverseScan<I>
where
    I: ReverseIter + Send,
    for<'a> I::Key<'a>: AsArray<'a> + From<&'a [u8]>,
    for<'a> I::Value<'a>: Into<Entry<&'a [u8]>>,
{
    fn valid(&self) -> bool {
        if !self.inner.valid() {
            return false;
        }
        let key = self.inner.key().unwrap();
        match &self.lower {
            Bound::Included(start) => key >= (&start[..]).into(),
            Bound::Excluded(start) => key > (&start[..]).into(),
            Bound::Unbounded => true,
        }
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner.seek_to_first()
    }

    fn seek(&mut self, target: &[u8]) -> StorageResult<()> {
        self.inner.seek(&target.into())
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
        self.inner.value().map(|v| v.into())
    }
}
