use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};

use crate::{
    block::FileBlockReader, builder::{DefaultSstWriter, SstBuilder, SstOption}, disk_manager::DiskManager, engine::{ShardId, TableStore}, errors::StorageResult, iterators::{
        concat_iter::ConcatIter,
        map_iter::MapIter,
        merge_iter::MergeIter,
        scan_iter::ScanIter,
        sst_iter::SstIter,
        storage_iter::{ForwardIter, IterRead},
        two_merge_iter::TwoMergeIter,
    }, lsm_iter::{LsmForwardScan, LsmReverseScan}, lsm_state::{LevelMeta, LsmState, LsmTableOption, SstMeta}, memtable::{Entry, MemTable, OwnedMemTableIter}, wal::DynWalStore, write_batch::{WriteBatch, WriteEntry}
};

// ── Iterator type aliases for the scan path ──

type MemSide = MergeIter<MapIter<OwnedMemTableIter<Bytes, Bytes>>>;
type L0Side = MergeIter<SstIter<FileBlockReader>>;
type LnSide = MergeIter<ConcatIter<SstIter<FileBlockReader>>>;
// TwoMergeIter: L0 (A) wins over L1+ (B) on key overlap — L0 数据更新。
type SstSide = TwoMergeIter<L0Side, LnSide>;
type MergedIter = TwoMergeIter<MemSide, SstSide>;

/// Per-shard LSM data container. Slim: holds only `state` (memtable + levels),
/// a shared `DiskManager` (injected by [`LsmEngine`](crate::lsm_engine::LsmEngine)),
/// and the table's [`LsmTableOption`]. No flush thread of its own — flush is
/// scheduled cross-shard by LsmEngine. Implements [`TableStore`].
pub struct LsmStore {
    state: Arc<RwLock<Arc<LsmState>>>,
    table_option: LsmTableOption,
    disk_manager: Arc<DiskManager>,
    shard_id: ShardId,
    /// Shared per-RG WAL (injected by LsmEngine at create_shard time, sourced
    /// from LogService). `write` appends here before applying the memtable.
    wal: Arc<crate::wal::DynWalStore>,
    /// Serializes concurrent flush of this shard (write force vs scheduler).
    flush_lock: Mutex<()>,
}

impl LsmStore {
    /// `disk_manager`, `table_option` and `wal` are injected by LsmEngine at
    /// `create_shard` time.
    pub(crate) fn new(
        table_option: LsmTableOption,
        disk_manager: Arc<DiskManager>,
        shard_id: ShardId,
        wal: Arc<DynWalStore>,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(Arc::new(LsmState::new()))),
            table_option,
            disk_manager,
            shard_id,
            wal,
            flush_lock: Mutex::new(()),
        }
    }

    pub fn disk_manager(&self) -> &Arc<DiskManager> {
        &self.disk_manager
    }

    pub fn shard_id(&self) -> ShardId {
        self.shard_id
    }

    pub fn table_option(&self) -> &LsmTableOption {
        &self.table_option
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

        if old.active_mem.estimate_memory() < self.table_option.memtable_size_limit {
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

    /// Build one SST from an immutable memtable; return its meta.
    fn build_sst_from_memtable(&self, imm: &Arc<MemTable<Bytes, Bytes>>) -> StorageResult<SstMeta> {
        let writer = self.disk_manager.create_sst()?;
        let sst_id = writer.sst_id();
        let opt = SstOption::default().block_size(self.disk_manager.block_size());
        let mut builder = SstBuilder::new(DefaultSstWriter::new(writer, &opt), opt);

        let mut iter = imm.iter();
        ForwardIter::seek_to_first(&mut iter)?;
        while iter.valid() {
            let k = iter.key().unwrap().clone();
            match iter.value().unwrap() {
                Entry::Put(v) => builder.add(k, v.clone())?,
                Entry::Delete => builder.add_delete(k)?,
            }
            ForwardIter::next(&mut iter)?;
        }

        // key_range sourced from the footer (single source of truth), not from
        // a separate memtable front/back scan.
        let (footer, sst_writer) = builder.finish()?;
        let file_size = sst_writer.into_inner().file_size();
        Ok(SstMeta {
            id: sst_id,
            key_range: (footer.first_key, footer.last_key),
            file_size,
        })
    }

    /// Flush the oldest immutable memtable (FIFO) to an SST. Serialized by
    /// `flush_lock` so the scheduler and write-force cannot flush concurrently.
    pub fn flush_oldest_imm(&self) -> StorageResult<()> {
        let _flush_guard = self.flush_lock.lock();

        let oldest = {
            let state = self.snapshot();
            match state.imm_memtables.last() {
                Some(imm) => imm.clone(),
                None => return Ok(()),
            }
        };

        let sst_meta = self.build_sst_from_memtable(&oldest)?;

        let mut guard = self.state.write();
        let old = guard.clone();
        let mut new_imms = old.imm_memtables.clone();
        new_imms.pop(); // remove oldest (last == oldest, front == newest)
        let mut new_levels = old.levels.clone();
        if new_levels.is_empty() {
            new_levels.push(LevelMeta {
                level: 0,
                ssts: Vec::new(),
            });
        }
        new_levels[0].ssts.push(sst_meta);
        *guard = Arc::new(LsmState {
            active_mem: old.active_mem.clone(),
            imm_memtables: new_imms,
            levels: new_levels,
        });
        Ok(())
    }

    /// Compose the full merge iterator over memtable + L0 + Ln SSTs.
    fn build_merged_iter(&self, state: &LsmState) -> StorageResult<MergedIter> {
        let block_size = self.disk_manager().block_size();

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
                let opened = self.disk_manager().open(sst.id)?;
                let opt = SstOption::default().block_size(block_size);
                SstIter::new(Arc::new(opened.reader), opened.footer, opt)
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
                        let opened = self.disk_manager().open(sst.id)?;
                        let opt = SstOption::default().block_size(block_size);
                        SstIter::new(Arc::new(opened.reader), opened.footer, opt)
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

impl TableStore for LsmStore {
    fn write(&self, batch: WriteBatch) -> StorageResult<()> {
        self.wal.append(self.shard_id, &batch)?; // WAL first (crash safety)
        let need_switch = {
            let state = self.snapshot();
            let active = &state.active_mem;
            for entry in batch.entries {
                match entry {
                    WriteEntry::Put { key, value } => active.put(key, value),
                    WriteEntry::Delete { key } => active.delete(key),
                }
            }
            active.estimate_memory() >= self.table_option.memtable_size_limit
        };

        if need_switch {
            self.switch_memtable();
            // Passive flush: too many immutables piled up -> flush oldest
            // synchronously to reclaim slots (backpressure on the writer).
            if self.snapshot().imm_memtables.len() >= self.table_option.max_imm_memtables {
                self.flush_oldest_imm()?;
            }
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
