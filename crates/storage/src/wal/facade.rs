//! WAL facade — Phase 4a durable write path.
//!
//! Lock-free multi-writer `append` onto an `ArcSwap`-swapped active buffer; a single
//! flush thread drains full buffers to `.log` (`O_DIRECT` pwrite + pad + `fdatasync`)
//! and accumulates `.idx` entries via `IdxTail` (block-granularity pwrite). Segment
//! rollover fires when the active segment can no longer hold the next flushed buffer.
//! See `docs/architecture/wal-design.md` Write Buffer Architecture + Operation Paths.

use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::wal::segment::{fdatasync_fd, pwrite_all};
use crate::wal::{
    AlignedMem, ENTRIES_PER_BLOCK, HEADER_LEN, IdxEntry, IdxHeader, IdxTail, Lsn, STATE_ACTIVE,
    STATE_FULL, Segment, WalBuffer, WalConfig, WalError, encode, pack, unpack,
};

const SPIN_THRESHOLD: u32 = 64;
const SWAP_WAIT_TIMEOUT: Duration = Duration::from_millis(1);
const SYNC_POLL_INTERVAL: Duration = Duration::from_micros(200);
const SYNC_DEADLINE: Duration = Duration::from_secs(30);

/// In-memory routing entry for one on-disk segment. Phase 4a pushes one on every
/// rollover; Phase 4b reads locate a segment by lsn via the route table.
struct SegmentRoute {
    seg_id: u32,
    /// Lowest live lsn in this segment. Read by the Phase 4b route lookup; written
    /// here on rollover so the route is authoritative without re-reading `.idx`.
    #[allow(dead_code)]
    min_live_lsn: u64,
    max_live_lsn: AtomicU64,
    segment: Arc<Segment>,
}

struct FreePool {
    buffers: Mutex<Vec<Arc<WalBuffer>>>,
    cv: Condvar,
}

struct Inner {
    active: ArcSwap<WalBuffer>,
    free_pool: FreePool,
    flush_tx: Mutex<Option<Sender<Arc<WalBuffer>>>>,
    durable_lsn: AtomicU64,
    swap_lock: Mutex<()>,
    swap_cv: Condvar,
    routes: RwLock<Vec<Arc<SegmentRoute>>>,
    stop_flag: AtomicBool,
    next_seg_id: AtomicU32,
    dir: PathBuf,
    config: WalConfig,
}

pub struct Wal {
    inner: Arc<Inner>,
    flush_handle: Mutex<Option<JoinHandle<()>>>,
}

impl Wal {
    /// Open (or create) a WAL at `dir`. Phase 4a opens a fresh WAL only (crash
    /// recovery is Phase 5); existing `wal-*.log`/`.idx` files are not loaded.
    pub fn open(dir: impl AsRef<Path>, config: WalConfig) -> Result<Self, WalError> {
        config.validate()?;
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let block = config.block_size;
        let active = ArcSwap::from_pointee(WalBuffer::new(config.buffer_size, block, 0, 0)?);
        let free: Vec<Arc<WalBuffer>> = (0..config.buffer_count)
            .map(|_| WalBuffer::new(config.buffer_size, block, 0, 0).map(Arc::new))
            .collect::<Result<_, _>>()?;
        let (tx, rx) = mpsc::channel::<Arc<WalBuffer>>();
        let inner = Arc::new(Inner {
            active,
            free_pool: FreePool {
                buffers: Mutex::new(free),
                cv: Condvar::new(),
            },
            flush_tx: Mutex::new(Some(tx)),
            durable_lsn: AtomicU64::new(0),
            swap_lock: Mutex::new(()),
            swap_cv: Condvar::new(),
            routes: RwLock::new(Vec::new()),
            stop_flag: AtomicBool::new(false),
            next_seg_id: AtomicU32::new(0),
            dir,
            config,
        });
        let inner_for_thread = Arc::clone(&inner);
        let handle = thread::Builder::new()
            .name("wal-flush".into())
            .spawn(move || FlushState::run(inner_for_thread, rx))?;
        Ok(Wal {
            inner,
            flush_handle: Mutex::new(Some(handle)),
        })
    }

    /// Highest lsn whose frame is durable on disk (advanced by the flush thread).
    pub fn durable_lsn(&self) -> Lsn {
        Lsn(self.inner.durable_lsn.load(Ordering::Acquire))
    }

    /// Append `payload` and return its lsn. Lock-free hot path: `fetch_add` byte range
    /// → `fetch_add` entry slot → `lsn = min_lsn + idx` → encode frame → return. On
    /// buffer overflow the current thread triggers swap + waits for a fresh active.
    pub fn append(&self, payload: &[u8]) -> Result<Lsn, WalError> {
        let frame_len = HEADER_LEN + payload.len();
        loop {
            if self.inner.stop_flag.load(Ordering::Acquire) {
                return Err(WalError::Closed);
            }
            let buf = self.inner.active.load_full();
            let pos = buf.write_pos.fetch_add(frame_len, Ordering::AcqRel);
            if pos
                .checked_add(frame_len)
                .is_none_or(|end| end > buf.capacity)
            {
                self.try_swap_full(&buf);
                self.wait_active_change(&buf);
                continue;
            }
            let idx = buf.claim_slot();
            let min_lsn = buf.min_lsn.load(Ordering::Acquire);
            let lsn = min_lsn + idx as u64;
            buf.in_flight.fetch_add(1, Ordering::AcqRel);
            let frame = encode(Lsn(lsn), payload);
            // SAFETY: disjoint byte range [pos, pos+frame_len) claimed via fetch_add;
            // no other writer or reader touches this range — flush reads only after the
            // `in_flight` barrier drains all in-progress encoders.
            unsafe {
                let ptr = buf.data.as_mut_ptr().add(pos);
                std::ptr::copy_nonoverlapping(frame.as_ptr(), ptr, frame_len);
            }
            buf.entries[idx].store(pack(pos, frame_len as u32), Ordering::Release);
            buf.in_flight.fetch_sub(1, Ordering::Release);
            return Ok(Lsn(lsn));
        }
    }

    /// Block until every lsn claimed so far is durable on disk. Forces the active
    /// buffer through the swap→flush path, then polls `durable_lsn` up to the target.
    /// Does **not** force-flush `IdxTail` (recovery rebuilds the index tail).
    pub fn sync(&self) -> Result<Lsn, WalError> {
        if self.inner.stop_flag.load(Ordering::Acquire) {
            return Err(WalError::Closed);
        }
        let buf = self.inner.active.load_full();
        let min_lsn = buf.min_lsn.load(Ordering::Acquire);
        let allocated = buf.entries_allocated.load(Ordering::Acquire);
        let target_lsn = if allocated > 0 {
            min_lsn + allocated as u64 - 1
        } else {
            min_lsn.saturating_sub(1)
        };
        self.try_swap_full(&buf);
        let deadline = Instant::now() + SYNC_DEADLINE;
        while self.inner.durable_lsn.load(Ordering::Acquire) < target_lsn {
            if self.inner.stop_flag.load(Ordering::Acquire) {
                return Err(WalError::Closed);
            }
            if Instant::now() >= deadline {
                return Err(WalError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "sync timed out waiting for durable_lsn",
                )));
            }
            thread::sleep(SYNC_POLL_INTERVAL);
        }
        Ok(Lsn(self.inner.durable_lsn.load(Ordering::Acquire)))
    }

    /// Shutdown: stop appenders, force-flush the active buffer, drain the flush queue,
    /// finalize the active segment's `.idx`, and join the flush thread. Idempotent.
    pub fn close(&self) -> Result<(), WalError> {
        let mut handle_guard = self.flush_handle.lock().unwrap();
        if handle_guard.is_none() {
            return Ok(());
        }
        self.inner.stop_flag.store(true, Ordering::Release);
        let buf = self.inner.active.load_full();
        self.try_swap_full(&buf);
        {
            let _ = self.inner.flush_tx.lock().unwrap().take();
        }
        self.inner.swap_cv.notify_all();
        self.inner.free_pool.cv.notify_all();
        if let Some(handle) = handle_guard.take()
            && let Err(payload) = handle.join()
        {
            // Flush thread panicked → propagate (process-crash semantics, per
            // wal-design.md §Close/Drop: no in-process fault tolerance for a flush
            // panic; on-disk state may be inconsistent.
            std::panic::resume_unwind(payload);
        }
        Ok(())
    }

    /// Transition `old` Active→Full (single swapper wins via cmpxchg), drain the
    /// `in_flight` barrier, finalize `count`, send to flush, install a fresh active
    /// with carried `min_lsn`, and bump `swap_version` to wake parkers.
    fn try_swap_full(&self, old: &Arc<WalBuffer>) {
        if old
            .state
            .compare_exchange(
                STATE_ACTIVE,
                STATE_FULL,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return; // another thread is swapping; caller proceeds to wait
        }
        while old.in_flight.load(Ordering::Acquire) != 0 {
            std::hint::spin_loop();
        }
        // `entries_allocated` is bounded by `claim_slot`'s `assert!(slot < max_entries)`
        // (append claims a byte range first, so byte-overflow trips swap before any slot
        // overflows) — no `.min(max_entries)` truncation is needed here.
        let count = old.entries_allocated.load(Ordering::Acquire);
        old.count.store(count, Ordering::Release);
        let next_min_lsn = old.min_lsn.load(Ordering::Acquire) + count as u64;
        if let Some(tx) = self.inner.flush_tx.lock().unwrap().as_ref() {
            let _ = tx.send(Arc::clone(old));
        }
        let new = match self.pop_free_buffer() {
            Some(b) => {
                b.reset(next_min_lsn);
                b
            }
            None => {
                // `pop_free_buffer` returns None only when the pool is empty AND
                // stop_flag is set (close). So stop_flag is necessarily true here,
                // which means the append loop's next `wait_active_change` unwinds via
                // its stop_flag check — no throwaway buffer is needed. `old` is already
                // sent to flush; leave `active` as-is.
                // Invariant: None ⟹ stop_flag ⟹ wait_active_change returns.
                return;
            }
        };
        self.inner.active.store(new);
        // Pair the notify with the `swap_lock` held by `wait_active_change`'s slow path
        // so the wake cannot be lost between a parker's ptr_eq check and its wait.
        {
            let _guard = self.inner.swap_lock.lock().unwrap();
            self.inner.swap_cv.notify_all();
        }
    }

    /// Spin briefly, then park on `swap_cv`, until `active` is no longer `old`.
    fn wait_active_change(&self, old: &Arc<WalBuffer>) {
        for _ in 0..SPIN_THRESHOLD {
            let cur = self.inner.active.load_full();
            if !Arc::ptr_eq(&cur, old) || self.inner.stop_flag.load(Ordering::Acquire) {
                return;
            }
            std::hint::spin_loop();
        }
        // Park on `swap_cv`; the real "did active change?" test is `Arc::ptr_eq` at the
        // loop top on every wake. `wait_timeout` bounds the park, so a lost wake just
        // costs a re-check — no version counter is needed.
        let mut g = self.inner.swap_lock.lock().unwrap();
        loop {
            let cur = self.inner.active.load_full();
            if !Arc::ptr_eq(&cur, old) || self.inner.stop_flag.load(Ordering::Acquire) {
                return;
            }
            let (g2, _) = self
                .inner
                .swap_cv
                .wait_timeout(g, SWAP_WAIT_TIMEOUT)
                .unwrap();
            g = g2;
        }
    }

    fn pop_free_buffer(&self) -> Option<Arc<WalBuffer>> {
        let mut guard = self.inner.free_pool.buffers.lock().unwrap();
        loop {
            if let Some(b) = guard.pop() {
                return Some(b);
            }
            if self.inner.stop_flag.load(Ordering::Acquire) {
                return None;
            }
            guard = self.inner.free_pool.cv.wait(guard).unwrap();
        }
    }
}

impl Drop for Wal {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

// ---- flush thread (single writer for `.log` append + `IdxTail` + segment state) ----

struct FlushState {
    cur_seg: Option<Arc<SegmentRoute>>,
    seg_written: u64,
    idx_tail: IdxTail,
    idx_blocks: u32,
    seg_min_lsn: u64,
    seg_max_lsn: u64,
    seg_entry_count: u32,
}

impl FlushState {
    fn new() -> Self {
        Self {
            cur_seg: None,
            seg_written: 0,
            idx_tail: IdxTail::new(),
            idx_blocks: 0,
            seg_min_lsn: 0,
            seg_max_lsn: 0,
            seg_entry_count: 0,
        }
    }

    /// Flush-thread entry: drain `rx` into `.log`/`.idx`, then finalize the last segment.
    fn run(inner: Arc<Inner>, rx: Receiver<Arc<WalBuffer>>) {
        let mut st = FlushState::new();
        while let Ok(buf) = rx.recv() {
            st.flush_buffer(&inner, &buf);
        }
        st.finalize_segment(&inner);
    }

    /// Flush one buffer to `.log` and accumulate its entries into `idx_tail`.
    fn flush_buffer(&mut self, inner: &Inner, buf: &Arc<WalBuffer>) {
        let count = buf.count.load(Ordering::Acquire);
        let min_lsn = buf.min_lsn.load(Ordering::Acquire);
        if count == 0 {
            inner.recycle_buffer(buf);
            return;
        }
        // logical_end = end offset of the last valid frame (frames may be physically
        // unordered within the buffer; the index records each frame's absolute offset).
        let mut logical_end: usize = 0;
        for i in 0..count {
            let (pos, flen) = unpack(buf.entries[i].load(Ordering::Acquire));
            logical_end = logical_end.max(pos + flen as usize);
        }

        let padded = align_up(logical_end, inner.config.block_size);

        self.ensure_segment(inner, min_lsn, padded);
        let seg = self
            .cur_seg
            .as_ref()
            .expect("segment ensured before flush")
            .clone();

        if padded > logical_end {
            // SAFETY: zero the tail padding within [logical_end, padded); disjoint from any
            // encoded frame (all end <= logical_end) and no reader is active post-barrier.
            unsafe {
                let ptr = buf.data.as_mut_ptr().add(logical_end);
                std::ptr::write_bytes(ptr, 0, padded - logical_end);
            }
        }
        pwrite_all(
            seg.segment.log_fd(),
            &buf.data.as_bytes()[..padded],
            self.seg_written as i64,
        )
        .expect("pwrite .log");
        fdatasync_fd(seg.segment.log_fd()).expect("fdatasync .log");

        let base = self.seg_written;
        for i in 0..count {
            let (pos, flen) = unpack(buf.entries[i].load(Ordering::Acquire));
            let pushed = self.idx_tail.push(IdxEntry {
                lsn: min_lsn + i as u64,
                start_offset: base + pos as u64,
                total_len: flen,
            });
            debug_assert!(pushed, "idx_tail push within a block must succeed");
            if self.idx_tail.len() == ENTRIES_PER_BLOCK {
                self.write_idx_block(inner, seg.segment.idx_fd());
            }
        }
        self.seg_written = base + padded as u64;
        self.seg_max_lsn = min_lsn + count as u64 - 1;
        seg.max_live_lsn.store(self.seg_max_lsn, Ordering::Release);
        inner.durable_lsn.store(self.seg_max_lsn, Ordering::Release);
        inner.recycle_buffer(buf);
    }

    /// Create a new segment if none exists, or roll over when the current segment cannot
    /// hold `padded` more bytes. On rollover the prior segment's `IdxTail` tail is flushed
    /// (padded) and its header finalized so the rolled-out segment is self-contained.
    fn ensure_segment(&mut self, inner: &Inner, min_lsn: u64, padded: usize) {
        let need_new = match &self.cur_seg {
            None => true,
            Some(_) => self.seg_written + padded as u64 > inner.config.segment_size as u64,
        };
        if !need_new {
            return;
        }
        self.finalize_segment(inner);
        let seg_id = inner.next_seg_id.fetch_add(1, Ordering::AcqRel);
        let seg = Segment::create(
            &inner.dir,
            seg_id,
            inner.config.segment_size,
            inner.config.block_size,
            inner.config.o_direct,
        )
        .expect("create segment");
        let header = IdxHeader::new(seg_id, min_lsn, 0, 0);
        seg.write_idx_header_double(&header)
            .expect("init idx header");
        let route = Arc::new(SegmentRoute {
            seg_id,
            min_live_lsn: min_lsn,
            max_live_lsn: AtomicU64::new(min_lsn),
            segment: Arc::new(seg),
        });
        inner.routes.write().unwrap().push(route.clone());
        self.cur_seg = Some(route);
        self.seg_written = 0;
        self.idx_blocks = 0;
        self.idx_tail = IdxTail::new();
        self.seg_min_lsn = min_lsn;
        self.seg_max_lsn = min_lsn;
        self.seg_entry_count = 0;
    }

    /// Flush any partial `IdxTail` block (padded) and rewrite the header with finalized
    /// `max_live_lsn` / `entry_count`. No-op when no segment is active.
    fn finalize_segment(&mut self, inner: &Inner) {
        let Some(seg) = self.cur_seg.take() else {
            return;
        };
        if !self.idx_tail.is_empty() {
            self.write_idx_block(inner, seg.segment.idx_fd());
        }
        let header = IdxHeader::new(
            seg.seg_id,
            self.seg_min_lsn,
            self.seg_max_lsn,
            self.seg_entry_count,
        );
        seg.segment
            .write_idx_header_double(&header)
            .expect("finalize idx header");
    }

    /// Drain `IdxTail` into one zero-padded 4 KiB block and pwrite it to `.idx` at the
    /// next entry-block slot (blocks 0–1 are header copies; entries start at block 2).
    fn write_idx_block(&mut self, inner: &Inner, idx_fd: RawFd) {
        let n = self.idx_tail.len();
        let block_bytes = self.idx_tail.drain_into_block();
        let block_size = inner.config.block_size;

        let mut aligned = AlignedMem::zeroed(block_size, block_size).expect("alloc idx block");
        aligned.as_bytes_mut()[..block_size].copy_from_slice(&block_bytes);
        let off = (2 + self.idx_blocks) as i64 * block_size as i64;
        pwrite_all(idx_fd, aligned.as_bytes(), off).expect("pwrite .idx block");
        fdatasync_fd(idx_fd).expect("fdatasync .idx block");
        self.idx_blocks += 1;
        self.seg_entry_count += n as u32;
    }
}

impl Inner {
    /// Return `buf` to the free pool for reuse — unless we are stopping, in which case
    /// buffers are held back so further appends cannot proceed.
    fn recycle_buffer(&self, buf: &Arc<WalBuffer>) {
        if self.stop_flag.load(Ordering::Acquire) {
            return;
        }
        buf.reset(0);
        let mut g = self.free_pool.buffers.lock().unwrap();
        g.push(Arc::clone(buf));
        drop(g);
        self.free_pool.cv.notify_one();
    }
}

fn align_up(v: usize, align: usize) -> usize {
    (v + align - 1) & !(align - 1)
}
