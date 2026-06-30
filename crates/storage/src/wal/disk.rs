//! `O_DIRECT` block read cache. Serves aligned block reads via a fixed-frame
//! CLOCK buffer pool keyed by `BlockKey` (segment id + block index): N pre-
//! allocated frames, second-chance eviction, per-frame pinning. `read_block`
//! returns an owned (`'static`) [`PinGuard`] holding an `Arc` to the pool, and
//! blocks on a `Condvar` when every frame is pinned. See `wal-design.md`
//! §Read path.

use std::collections::HashMap;
use std::ops::Deref;
use std::os::unix::io::RawFd;
use std::sync::{Arc, Condvar, Mutex};

use crate::wal::segment::pread_all;
use crate::wal::{AlignedMem, WalError};

/// Identity of one cached block: a block index within a segment.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BlockKey {
    seg_id: u32,
    block_idx: u64,
}

impl BlockKey {
    fn new(seg_id: u32, block_idx: u64) -> Self {
        Self { seg_id, block_idx }
    }
}

/// One slot in the CLOCK buffer pool: an aligned block buffer plus its resident
/// block identity, reference bit (second-chance), and pin count.
struct Frame {
    buf: AlignedMem,
    key: Option<BlockKey>,
    ref_bit: bool,
    pin_count: u32,
}

/// Fixed-frame CLOCK buffer pool (second-chance eviction). Hard memory bound =
/// `frames.len() × block_size`. Victim selection via [`ClockCache::find_victim`];
/// pinning and page-table mutation are driven by `DiskManager::read_block`.
struct ClockCache {
    frames: Box<[Frame]>,
    table: HashMap<BlockKey, usize>,
    hand: usize,
}

impl ClockCache {
    fn new(capacity: usize, block_size: usize) -> Result<Self, WalError> {
        let mut frames = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            frames.push(Frame {
                buf: AlignedMem::zeroed(block_size, block_size)?,
                key: None,
                ref_bit: false,
                pin_count: 0,
            });
        }
        Ok(Self {
            frames: frames.into_boxed_slice(),
            table: HashMap::new(),
            hand: 0,
        })
    }

    /// CLOCK second-chance victim selection: advance `hand` past pinned frames
    /// (`pin_count > 0`), clear a set `ref_bit` (second chance), and return the
    /// first `pin_count == 0 && !ref_bit` frame (hand ends just past it). Returns
    /// `None` when every frame is pinned (caller blocks on its `Condvar`).
    fn find_victim(&mut self) -> Option<usize> {
        let n = self.frames.len();
        if n == 0 {
            return None;
        }
        // At most two full sweeps: pass 1 clears ref bits (second chance); pass 2
        // evicts. If every frame is pinned, both passes only skip → None.
        for _ in 0..(2 * n) {
            let i = self.hand;
            if self.frames[i].pin_count == 0 {
                if !self.frames[i].ref_bit {
                    self.hand = (i + 1) % n;
                    return Some(i);
                }
                self.frames[i].ref_bit = false;
            }
            self.hand = (i + 1) % n;
        }
        None
    }

    /// Hit path: block resident → pin++ and mark recently used. Returns frame idx.
    fn pin_hit(&mut self, key: BlockKey) -> Option<usize> {
        if let Some(&idx) = self.table.get(&key) {
            self.frames[idx].pin_count += 1;
            self.frames[idx].ref_bit = true;
            Some(idx)
        } else {
            None
        }
    }

    /// Prepare a miss-load on `idx`: tear down the old page-table mapping (load
    /// isolation — no concurrent hit can alias this frame during the `pread`),
    /// pin the frame for the loader, mark it loading. Returns the buffer's raw
    /// pointer + len for the caller's `pread` outside the lock.
    fn prepare_load(&mut self, idx: usize) -> (*mut u8, usize) {
        let f = &mut self.frames[idx];
        if let Some(old) = f.key.take() {
            self.table.remove(&old);
        }
        f.pin_count = 1;
        f.ref_bit = false;
        (f.buf.as_mut_ptr(), f.buf.len())
    }

    /// Commit a miss-load: publish the new mapping + mark recently used.
    fn commit_load(&mut self, idx: usize, key: BlockKey) {
        let f = &mut self.frames[idx];
        f.key = Some(key);
        f.ref_bit = true;
        self.table.insert(key, idx);
    }

    /// Release one pin (reader drop or load abort). Returns `true` if the frame
    /// became evictable (pin reached 0) so the caller can notify blocked readers.
    fn unpin(&mut self, idx: usize) -> bool {
        self.frames[idx].pin_count -= 1;
        self.frames[idx].pin_count == 0
    }

    /// Snapshot a frame's buffer pointer for a [`PinGuard`] (called under the lock).
    fn buf_ptr(&self, idx: usize) -> (*const u8, usize) {
        (
            self.frames[idx].buf.as_bytes().as_ptr(),
            self.frames[idx].buf.len(),
        )
    }
}

/// Shared pool state, refcounted so each [`PinGuard`] keeps it alive via an
/// `Arc<Inner>`. This is what makes `PinGuard` `'static` (no borrow of the
/// manager): its `Drop` reaches the pool through the `Arc`, so the pool cannot
/// be freed while any guard is live. See `wal-design.md` §Read path.
struct DiskManagerInner {
    block_size: usize,
    cache: Mutex<ClockCache>,
    cv: Condvar,
}

/// Fixed-frame CLOCK buffer pool under one `Mutex` + `Condvar`. Readers pin frames
/// via [`PinGuard`]; `read_block` blocks only when all frames are pinned. Rationale
/// + the reversal from the earlier Arc-survives LRU: `tradeoffs §22`.
///
/// The shared state lives behind `Arc<DiskManagerInner>` so [`PinGuard`] can be
/// `'static` (it clones the `Arc`) instead of borrowing `&self` — borrowing
/// `&self` would make a guard self-referential once stored in a stateful iterator.
pub struct DiskManager {
    inner: Arc<DiskManagerInner>,
}

impl DiskManager {
    pub fn new(block_size: usize, capacity_blocks: usize) -> Result<Self, WalError> {
        Ok(Self {
            inner: Arc::new(DiskManagerInner {
                block_size,
                cache: Mutex::new(ClockCache::new(capacity_blocks, block_size)?),
                cv: Condvar::new(),
            }),
        })
    }

    /// Read one block. A hit pins the resident frame and returns a [`PinGuard`]; a
    /// miss evicts a CLOCK victim, `pread`s the block into it in place, and returns
    /// a guard. When every frame is pinned, this blocks on the `Condvar` until an
    /// unpin frees one. The `pread` runs without the lock held; a concurrent miss on
    /// the same block may `pread` twice (last install wins) — harmless redundancy.
    ///
    /// The returned guard is `'static`: it holds an `Arc` clone of the pool, not a
    /// borrow of `self`, so it can be stored in a stateful iterator (e.g.
    /// `NormalBlockIter<PinGuard>`) without self-reference.
    ///
    /// Deadlock discipline: do **not** hold a `PinGuard` on this manager across a
    /// `read_block` that may block (`get` single-block is safe; `scan` must drop the
    /// previous guard before fetching the next), or N readers can each pin one frame
    /// and block forever.
    pub fn read_block(
        &self,
        seg_id: u32,
        fd: RawFd,
        block_idx: u64,
    ) -> Result<PinGuard, WalError> {
        let key = BlockKey::new(seg_id, block_idx);
        let off = (block_idx as i64) * (self.inner.block_size as i64);
        let mut cache = self.inner.cache.lock().unwrap();
        loop {
            if let Some(idx) = cache.pin_hit(key) {
                let (ptr, len) = cache.buf_ptr(idx);
                drop(cache);
                return Ok(PinGuard {
                    inner: Arc::clone(&self.inner),
                    idx,
                    ptr,
                    len,
                });
            }
            match cache.find_victim() {
                Some(idx) => {
                    let (buf_ptr, buf_len) = cache.prepare_load(idx);
                    drop(cache);
                    // SAFETY: frame `idx` is pinned by this loader (`pin_count == 1`,
                    // set in `prepare_load`) and its page-table mapping was removed
                    // under the lock, so no other thread can reach this buffer while
                    // we write it. Exclusive `&mut` is valid for the `pread` window.
                    let res = pread_all(
                        fd,
                        unsafe { std::slice::from_raw_parts_mut(buf_ptr, buf_len) },
                        off,
                    );
                    let mut cache = self.inner.cache.lock().unwrap();
                    match res {
                        Ok(()) => {
                            cache.commit_load(idx, key);
                            let (ptr, len) = cache.buf_ptr(idx);
                            drop(cache);
                            return Ok(PinGuard {
                                inner: Arc::clone(&self.inner),
                                idx,
                                ptr,
                                len,
                            });
                        }
                        Err(e) => {
                            // B2: a failed load must release the loader's pin so the
                            // frame is not leaked (else N failures exhaust the pool).
                            let became_zero = cache.unpin(idx);
                            drop(cache);
                            if became_zero {
                                self.inner.cv.notify_all();
                            }
                            return Err(e);
                        }
                    }
                }
                None => {
                    // All frames pinned — release the lock and wait for an unpin.
                    cache = self.inner.cv.wait(cache).unwrap();
                }
            }
        }
    }

    pub fn block_size(&self) -> usize {
        self.inner.block_size
    }

    /// Read arbitrary bytes from an fd at an offset, bypassing the block cache.
    /// Allocates aligned memory for O_DIRECT compatibility. Used for reading
    /// SST footers and other metadata.
    pub fn raw_read(&self, fd: RawFd, offset: u64, len: usize) -> Result<Vec<u8>, WalError> {
        let block_size = self.inner.block_size;

        // Align offset down to block boundary
        let aligned_offset = (offset / block_size as u64) * block_size as u64;
        let offset_in_block = (offset - aligned_offset) as usize;

        // Align length up to block boundary
        let total_len = offset_in_block + len;
        let aligned_len = ((total_len + block_size - 1) / block_size) * block_size;

        // Allocate aligned buffer
        let mut aligned_buf = AlignedMem::zeroed(aligned_len, block_size)?;

        // Read into aligned buffer
        pread_all(fd, aligned_buf.as_bytes_mut(), aligned_offset as i64)?;

        // Extract the requested slice
        Ok(aligned_buf.as_bytes()[offset_in_block..offset_in_block + len].to_vec())
    }
}

/// Owned (`'static`), pinned handle to a cached block's bytes. `Drop` unpins the
/// frame (notifying blocked readers if it becomes evictable). The bytes are safe
/// to read for the guard's lifetime: the frame is pinned (`pin_count > 0`), so it
/// cannot be evicted or overwritten while this guard exists, and the pool (`Arc`
/// clone) is kept alive until the guard drops.
///
/// `'static` (holds an `Arc` to the pool, not a borrow of `&self`) means a guard
/// can be stored in a stateful iterator without self-reference — see
/// `NormalBlockIter<PinGuard>`.
pub struct PinGuard {
    inner: Arc<DiskManagerInner>,
    idx: usize,
    ptr: *const u8,
    len: usize,
}

impl PinGuard {
    /// Frame buffer pointer — lets tests assert two reads hit the same frame.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }
}

impl Deref for PinGuard {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY (see `wal-design.md` §Read path + `tradeoffs §22b`): `ptr`/`len`
        // come from a `Frame`'s `AlignedMem` in a fixed-length `Box<[Frame]>` (no
        // realloc → the allocation is stable for the pool's life). The pool is kept
        // alive by the `Arc` in this guard, and the frame is pinned (`pin_count >
        // 0`), so no eviction or overwrite can touch these bytes while the guard
        // lives. The only writer is a miss-load `pread`, which runs on a frame whose
        // mapping was removed under the lock and whose pin was 0 at eviction — so it
        // never overlaps a live guard. Multiple guards to the same frame share
        // `&[u8]` (read-only), which is sound.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for PinGuard {
    fn drop(&mut self) {
        // Safe: `inner` is a live `Arc` to the pool; unpin + notify is plain pool
        // bookkeeping. (The `unsafe` is confined to `Deref`'s raw-pointer read.)
        let mut cache = self.inner.cache.lock().unwrap();
        let became_zero = cache.unpin(self.idx);
        drop(cache);
        if became_zero {
            self.inner.cv.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(n: usize) -> ClockCache {
        ClockCache::new(n, 4096).unwrap()
    }

    #[test]
    fn second_chance_survives_one_sweep() {
        let mut c = cache(2);
        c.frames[0].ref_bit = true; // recently used
        c.frames[1].ref_bit = false;
        c.hand = 0;
        // hand=0: ref set → cleared (second chance); hand=1: ref clear → victim 1
        assert_eq!(c.find_victim(), Some(1));
        assert!(!c.frames[0].ref_bit, "frame 0 given a second chance");
    }

    #[test]
    fn pinned_frame_is_skipped() {
        let mut c = cache(2);
        c.frames[0].pin_count = 1; // pinned
        c.frames[1].pin_count = 0;
        c.hand = 0;
        assert_eq!(c.find_victim(), Some(1), "pinned frame 0 must be skipped");
    }

    #[test]
    fn all_pinned_yields_no_victim() {
        let mut c = cache(2);
        c.frames[0].pin_count = 1;
        c.frames[1].pin_count = 1;
        c.hand = 0;
        assert!(
            c.find_victim().is_none(),
            "all pinned → no victim (caller blocks)"
        );
    }

    #[test]
    fn all_ref_set_evicts_on_second_sweep() {
        let mut c = cache(3);
        for f in c.frames.iter_mut() {
            f.ref_bit = true;
        }
        c.hand = 0;
        // pass 1 clears all ref bits; pass 2 evicts frame 0 (hand returns to 0)
        assert_eq!(c.find_victim(), Some(0));
        assert!(c.frames.iter().all(|f| !f.ref_bit));
    }

    #[test]
    fn capacity_one_cycles() {
        let mut c = cache(1);
        c.frames[0].ref_bit = true;
        c.hand = 0;
        assert_eq!(c.find_victim(), Some(0));
        c.frames[0].ref_bit = true; // re-arm
        assert_eq!(c.find_victim(), Some(0));
    }

    // Compile-time proof: PinGuard is `'static` (holds an `Arc`, no lifetime
    // param) and derefs to `[u8]`, so it is a valid zero-copy storage type for
    // the generic block iterator — no self-reference, no materialization.
    #[test]
    fn pin_guard_is_static_and_derefs() {
        fn needs_static<T: 'static>() {}
        fn needs_deref<B: std::ops::Deref<Target = [u8]>>() {}

        needs_static::<PinGuard>();
        needs_deref::<PinGuard>();
        // NormalBlockIter<PinGuard> is a well-formed type.
        fn _accepts<B: std::ops::Deref<Target = [u8]>>(_: &crate::iterators::block_iter::NormalBlockIter<B>) {}
        let _f: fn(&crate::iterators::block_iter::NormalBlockIter<PinGuard>) = _accepts;
    }
}
