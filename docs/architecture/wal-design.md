# WAL Technical Design

## Purpose

Own the technical contract for the `wal-demo` Write-Ahead Log: on-disk format, index shape, I/O model, write-buffer architecture, operation paths, and crash recovery. App-layer behavior lives in `docs/design/app-overview.md`; milestone scope in `docs/requirements/2026-06-22-wal-core.md`.

This is a living architecture contract. Update it in the same change that changes any decision below.

## Positioning

- High-performance WAL primitive exploration in Rust.
- Intended as the **bottom layer for a Raft log store**: monotonic `u64` LSN, durable `sync`, frequent `truncate_after` on leader change, `truncate_before` for snapshot compaction, `scan` for replication replay, `get` for `prevLogTerm` lookup.
- Out of scope: Raft consensus, snapshot storage, state machine. The WAL only stores log entries; Raft snapshot is an upper-layer concern that calls `truncate_before` after snapshotting.

## Operation Surface

| Op | Signature (sketch) | Semantics |
| --- | --- | --- |
| `append` | `append(&self, payload) -> Result<Lsn, WalError>` | Encode frame into the active write buffer via lock-free `fetch_add`; assign the next LSN bound to the claimed entry slot. Does not touch disk. |
| `sync` | `sync(&self) -> Result<Lsn, WalError>` | Swap the active buffer out to the flush path; returns the durable LSN after flush. |
| `truncate_before` | `truncate_before(&self, lsn: Lsn) -> Result<(), WalError>` | Drop records with `lsn < arg`; keep `>= arg`. |
| `truncate_after` | `truncate_after(&self, lsn: Lsn) -> Result<(), WalError>` | Drop records with `lsn > arg`; keep `<= arg`. |
| `scan` | `scan(&self, range: Range<Lsn>) -> ScanIter` | Yield synced live records with `start <= lsn < end`, ascending; `Item = Result<RecordRef, WalError>`. |
| `get` | `get(&self, lsn: Lsn) -> Result<Option<RecordRef>, WalError>` | Random lookup by LSN (Raft `prevLogTerm` path); `None` if not live/unsynced. |
| `close` | `close(&self) -> Result<(), WalError>` | Graceful shutdown: force-sync, stop flush thread, close fds (see Close/Drop). |

> Note: `RecordRef` is dormant (no `get`/`scan` ships yet). After the read cache became a fixed-frame CLOCK pool with `PinGuard`, `RecordRef` can no longer simply hold an `Arc<AlignedMem>` that outlives eviction — its contract will be revisited (hold a `PinGuard`, or copy the payload) when `get`/`scan` are implemented. See Read path + `tradeoffs §22` reversal.

### Error type

```rust
#[derive(Debug, thiserror::Error)]
pub enum WalError {
    #[error("io: {0}")]                         Io(#[from] io::Error),
    #[error("frame crc mismatch at lsn {lsn} (offset {offset})")]
    CrcMismatch { lsn: u64, offset: u64 },
    #[error("segment {seg_id} header corrupt (both copies failed crc)")]
    HeaderCorrupt { seg_id: u32 },
    #[error("invalid config: {0}")]             InvalidConfig(String),
    #[error("wal closed")]                      Closed,
}
```

### Config

```rust
pub struct WalConfig {
    pub segment_size: usize,
    pub buffer_size: usize,        // < segment_size
    pub block_size: usize,         // 4096
    pub buffer_count: usize,       // free-pool capacity, >= 2
    pub read_cache_blocks: usize,  // DiskManager CLOCK frame-pool size (>= 1)
}
```

Validated on open (`buffer_size < segment_size`; all sizes multiples of `block_size`; `buffer_count >= 2`; `read_cache_blocks >= 1`) → `InvalidConfig` otherwise.

### Read path — `DiskManager` + `RecordRef`

`get`/`scan` read on-disk data via `O_DIRECT`, which requires 4 KiB-aligned buffers the WAL owns (no page cache). `DiskManager` serves single-block reads through a **fixed-frame CLOCK buffer pool**: N pre-allocated `AlignedMem` frames (hard memory bound = N × `block_size`; N = `read_cache_blocks`), a page table `BlockKey → frame index` where `BlockKey = { seg_id, block_idx }`, and a CLOCK second-chance replacer (a `hand` index + per-frame `ref_bit`). A miss picks a CLOCK victim (`pin_count == 0 && ref_bit == 0`; a `ref_bit == 1` frame is cleared and given a second chance), **removes the victim's old page-table mapping under the lock**, drops the lock to `pread` into the frame in place, then installs the new mapping; on `pread` error it unpins + notifies and returns `Err` (no pin leak). If a full sweep finds all frames `pin_count > 0`, `read_block` blocks on a `Condvar` until an unpin frees a frame.

`read_block(seg_id, fd, block_idx) -> Result<PinGuard<'_>, WalError>` returns a **borrowed `PinGuard`** (pins the frame on acquire; `Drop` unpins + notifies), not an owned `Arc<AlignedMem>`. In-place frame reuse cannot coexist with long-lived owned references to the same buffer — a reader's `Arc` would be corrupted when the frame is evicted and overwritten. `PinGuard` derefs to the frame bytes via a raw pointer (`AlignedMem` already exposes `as_mut_ptr` for the lock-free append path, disciplined by the `in_flight` barrier); soundness rests on: (i) the pool is a fixed-length `Box<[Frame]>` (no realloc → stable buffer pointers), (ii) `pin_count > 0` prevents eviction/overwrite while any guard lives, (iii) a miss-load `pread` runs only on a frame whose mapping was removed under the lock and whose `pin_count` was 0 at eviction. No extra fences — the `Mutex` acq-rel edge carries loader-`pread` → install → reader-`Deref`.

**Deadlock discipline**: a caller holding a `PinGuard` must not call a `read_block` that may block (`get` single-block is safe; `scan` drops the previous guard before fetching the next).

This reverses the earlier Arc-survives LRU choice: readers no longer hold `Arc` clones that outlive eviction, and `read_block`'s signature changed from `Arc<AlignedMem>` to `PinGuard`. Rationale + reversal in `tradeoffs §22`.

### Close / Drop

```
Wal::close():
  1. stop_flag.store(true, Release)
  2. force-sync: swap active → Full, send flush_tx   (flush writes .log; under stop_flag it does NOT return the buffer to free_pool — append cannot continue)
  3. drop flush_tx                                    (close the channel)
  4. swap_cv.notify_all() + free_pool_cv.notify_all() (wake parked append/sync threads → they see stop_flag → Err(Closed))
  5. flush_thread.join()                              (drain remaining buffers → write .log → final idx_tail padded flush to .idx → exit)
  6. close all segment fds
```

`Drop` calls `close` if not already called; `join` propagates a flush-thread panic (process-crash semantics — no in-process fault tolerance for flush panic). `append`/`sync` after `stop_flag` return `Err(WalError::Closed)`.

## LSN And Term

- `Lsn` is a dense monotonic `u64` newtype. Never decreases across `append`. Unique within a `Wal` instance.
- **LSN allocation is bound to the write buffer's entry slot**: `lsn = buf.min_lsn + idx`, where `idx` is claimed atomically via `entries_allocated.fetch_add(1)`. There is **no global `next_lsn` counter**. This binds lsn to a physical slot atomically — an `append` that cannot claim a slot consumes no lsn, so there are no holes and no wasted lsn. `buf.min_lsn` is carried across buffer swaps to keep lsn continuous across buffers.
- `term` is **inside the payload** (decision A): the WAL is term-agnostic. Callers serialize `(term, command)` into the payload bytes. Looking up `prevLogTerm` is an upper-layer deserialize, or the upper layer keeps its own LSN→term cache.
- Rationale: keep the WAL a general log primitive; no Raft coupling in the frame.

## Physical Format

### Segment

- Multiple rolling segment files. Naming: `wal-{seg_id:010}.log` + `wal-{seg_id:010}.idx`.
- `seg_id` is a `u32`, monotonic. New segment when the active segment's written bytes reach `segment_size`.
- `segment_size` is a creation-time config (e.g. 1 GiB).
- `buffer_size < segment_size` (e.g. 128 MiB buffer / 1 GiB segment), so a single write buffer never spans more than two segments on flush and never "owns" a full segment.
- **`.log` preallocation**: on segment creation, `fallocate(fd, 0, segment_size)` allocates all physical blocks once, so `O_DIRECT` writes never trigger per-block filesystem allocation or metadata jitter. `.idx` is **not** preallocated (small, append-grown). After `truncate_after` physically shrinks `.log` via `set_len`, re-`fallocate(fd, new_logical_eof, segment_size - new_logical_eof)` restores preallocation for the tail. ext4/xfs `fallocate` zeroes allocated blocks by default — unwritten regions read back as zero, which recovery relies on.

### Block

- `BLOCK_SIZE = 4096` bytes (default; must align to the device logical block for `O_DIRECT`). A block is **only an I/O alignment unit** — it carries no header.
- Every `.log` and `.idx` file is a sequence of 4 KiB blocks. All `O_DIRECT` reads/writes operate on whole blocks at block-aligned offsets.

### Frame

```
frame = [ len: u32 LE | crc32: u32 LE | lsn: u64 LE | payload: len bytes ]
         \_________ 16 B header _________/   \____ variable ____
```

- `len` = payload length only (header is fixed 16 B). Total frame length = `16 + len`.
- `crc32` covers `lsn || payload` (the crc32fast of those bytes). Detects torn writes.
- `lsn` is embedded so a recovery scan can rebuild the index without any external map.
- A frame **may span any number of blocks**. Large payloads are fully supported.
- Frames are laid out contiguously; when a block cannot hold the next frame's remaining bytes, the frame simply continues into the next block. There is no per-block framing.

### Padding

- Memory write buffer does **not** zero-fill tail padding during `append`.
- On flush, the last non-full block of a buffer is zero-padded to 4 KiB before `O_DIRECT pwrite`. This padding is never indexed (gated by `logical_eof` and the index entries).

### `logical_eof`

- Logical byte end of a segment = end offset of the last valid frame, may land mid-block.
- Maintained on `append` (advance) and `truncate` (retreat).
- Physical file size is always 4 KiB aligned; `logical_eof` records where valid data ends. `scan` and recovery scanning stop at `logical_eof`; padding bytes past it are never read.

## Index

### Disk index `wal-{seg_id}.idx`

Record-level index lives on disk (the WAL can be large; the record index is not held in memory).

**Entry layout — 20 B, packed, little-endian (hand-serialized; no `#[repr(C)]` to avoid alignment padding):**

| offset | size | field | notes |
| --- | --- | --- | --- |
| 0 | 8 | `lsn` | u64 LE |
| 8 | 8 | `start_offset` | u64 LE, byte offset into `.log` (not block-aligned) |
| 16 | 4 | `total_len` | u32 LE, full frame length incl. 16 B header; `0` = padding sentinel |

**Header layout — 36 B, stored in two identical copies: block 0 (copy A) and block 1 (copy B); entries start at block 2:**

| offset | size | field | notes |
| --- | --- | --- | --- |
| 0 | 4 | `magic` | `b"WIDX"` |
| 4 | 4 | `version` | u32 LE |
| 8 | 4 | `seg_id` | u32 LE |
| 12 | 8 | `min_live_lsn` | u64 LE, truncate_before lower bound; double-written (crash-consistency, see below) |
| 20 | 8 | `max_live_lsn` | u64 LE, truncate_after upper bound; recovery overwrites from `.log` tail (not trusted) |
| 28 | 4 | `entry_count` | u32 LE, may lag at crash; recovery scans, does not trust it |
| 32 | 4 | `header_crc` | u32 LE, crc32 over bytes [0,32); validates this copy |
| 36..4096 | — | reserved zero | block remainder |

**Double-write (header crash-consistency):** `min_live_lsn` cannot be rebuilt from `.log` (truncated-head dead space is indistinguishable from live frames), so the header must survive a torn write. On any header change (`truncate_*`, segment create, recovery finalize): write copy A (full 4 KiB block, with fresh `header_crc`) → `fdatasync` → write copy B (identical) → `fdatasync`. Writing A then B (never concurrent) guarantees at least one intact copy after any crash. Recovery reads both, verifies `header_crc`, uses whichever passes (either if both pass — they are identical; full-`.log` rescan fallback if both fail). See `tradeoffs §21`.

**Block arrangement:** blocks 0–1 hold the two header copies (A/B); entries start at block 2. `ENTRIES_PER_BLOCK = floor(4096 / 20) = 204` entries (4080 B) + 16 B zero padding per entry block. The 205th slot reads `total_len == 0` → recovery stops there.

- Dense: one entry per record; bounded by `segment_size` per file.
- `start_offset` is a byte offset into `.log`, **not** block-aligned (a frame may start mid-block).
- `total_len` lets `scan` advance by index alone without re-reading every frame header.

### `.idx` is a second decoupled accumulator (`IdxTail`)

`.idx` is **not** written synchronously on each wal-buffer flush. It is its own accumulator, structurally identical to the wal buffer but for index entries — same fill / flush-at-block-granularity / crash-loses-tail / recovery-rebuilds pattern:

| | `WalBuffer` (data) | `IdxTail` (index) |
| --- | --- | --- |
| Holds | frame bytes | `IdxEntry` 20 B records |
| Fill | `fetch_add` on `write_pos` / `entries_allocated` | wal-buffer flush appends entries |
| Flush trigger | buffer full / `sync` → swap | `idx_tail` reaches 204 entries |
| Flush unit | buffer → multi-block `.log` pwrite | one idx block → 4 KiB `.idx` pwrite |
| Crash loses | last un-swapped buffer (never in `.log`) | last sub-block entries (data is in `.log`, only index missing) |

`IdxTail` is single-writer (flush thread owns it) — a simple `Vec<IdxEntry>` capped at 204, no lock-free machinery. See Write Buffer → Flush thread for the pseudocode that drives both `.log` and `.idx` from one loop.

**Flush timing:**

- `sync` flushes `.log` only (the data-durability promise); it does **not** force-flush `IdxTail`. Recovery rebuilds the missing tail from `.log`.
- **Segment rollover flushes `IdxTail`** (zero-padded to a block) so the rolled-out segment is self-contained — recovery need not scan its `.log`, only the active segment's tail.

### In-memory segment route table

Small routing metadata (not a record index) held in memory:

```rust
struct SegmentRoute {
    seg_id: u32,
    min_live_lsn: Lsn,
    max_live_lsn: Lsn,
    log: File,   // opened .log handle
    idx: File,   // opened .idx handle
}
```

- Built at startup by scanning each segment's `.idx` header (cheap; user accepted this startup cost).
- Lets `get`/`scan`/`truncate` locate the target segment in O(log segments) without touching disk.
- Size is bounded by segment count: 1 TiB WAL / 1 GiB segment ≈ 1 k routes × ~64 B ≈ 64 KiB.

## I/O Model

- **Direct I/O (`O_DIRECT`) only.** No `mmap`, no OS buffered I/O (`std::fs` read/write that goes through the page cache is forbidden).
- Rationale: memory is managed by the database itself (self-managed buffer pool), not the OS. This is a hard constraint from the project owner.
- Rust implementation: `libc` (or `nix`) `open(.., O_RDWR | O_DIRECT)`; aligned buffers via `std::alloc::Layout::from_size_align(size, 4096)`; `pread`/`pwrite` at block-aligned offsets with block-aligned lengths.
- `sync` calls `fdatasync` after the flush thread writes.
- The read path ships a fixed-frame CLOCK buffer pool (`DiskManager`); the write path uses buffer swap + a free pool (see Write Buffer) rather than a slot-based buffer pool.

## Write Buffer Architecture (Decision: Lock-Free Multi-Writer + Buffer Swap)

Multiple append threads write the active buffer directly via `fetch_add` — no writer thread, no channel on the append hot path. The active buffer sits behind an atomic swap pointer; when full, one thread swaps it out to the flush thread and installs a fresh buffer.

### Shape

```rust
const MAX_ENTRIES: usize = BUFFER_SIZE / 16;   // strict upper bound: min frame is the 16 B header

struct WalBuffer {
    data: AlignedMem,                          // one buffer_size allocation, 4 KiB aligned
    write_pos: AtomicUsize,                    // next byte offset (fetch_add)
    entries_allocated: AtomicUsize,            // next entry slot (fetch_add) — the lsn source
    entries: Box<[AtomicU64; MAX_ENTRIES]>,    // packed (pos, frame_len), indexed by lsn - min_lsn
    in_flight: AtomicU32,                      // slots claimed but frame not yet encoded
    state: AtomicU8,                           // Active | Full | Flushing
    min_lsn: Lsn,                              // lsn of entries[0]; carried across swaps
    count: AtomicUsize,                        // finalized entry count (set at swap)
    seg_id: u32,
}

struct Wal {
    active: ArcSwap<WalBuffer>,                // atomic active pointer (lock-free read)
    free_pool: FreePool,                       // bounded pool of reusable buffers
    flush_tx: Sender<Arc<WalBuffer>>,          // full buffers → flush thread
    durable_lsn: AtomicU64,                    // advanced by the flush thread
    swap_lock: Mutex<()>,                      // pairs notify with wait_active_change's park (ptr_eq is the real test; no version counter)
    swap_cv: Condvar,
    routes: RwLock<Vec<SegmentRoute>>,
}
```

- Pre-allocate one contiguous aligned block per `WalBuffer`; reuse via the free pool. Do **not** use `Vec<AlignedBuffer>`.
- `entries` is indexed by `lsn - min_lsn`, so it is **naturally lsn-ordered** — flush writes `.idx` by walking `entries[0..count]` with **no sort**.
- `MAX_ENTRIES = BUFFER_SIZE / 16` is a strict upper bound; byte capacity triggers swap before the entry count reaches it. The entry-count check is a safety net.

### `append` path (lock-free)

```rust
fn append(&self, payload: &[u8]) -> Lsn {
    let frame_len = 16 + payload.len();
    loop {
        let buf = self.active.load_full();                      // lock-free read
        let pos = buf.write_pos.fetch_add(frame_len, AcqRel);   // claim byte range
        if pos + frame_len > BUFFER_SIZE {                      // byte full → swap
            self.try_swap_full(&buf);
            self.wait_active_change(&buf);
            continue;
        }
        let idx = buf.entries_allocated.fetch_add(1, AcqRel);   // claim slot = claim lsn
        if idx >= MAX_ENTRIES {                                 // slot full → swap
            self.try_swap_full(&buf);
            self.wait_active_change(&buf);
            continue;                                           // new buffer, idx from 0
        }
        let lsn = buf.min_lsn + idx as u64;                     // bound, no hole
        buf.in_flight.fetch_add(1, AcqRel);
        buf.entries[idx].store(pack(pos, frame_len), Release);
        encode_frame(&mut buf.data[pos..], lsn, payload);       // len|crc|lsn|payload
        buf.in_flight.fetch_sub(1, Release);
        return lsn;
    }
}
```

Invariants:

- **lsn = `min_lsn + idx`**: claiming `idx` via `fetch_add` is claiming the lsn, atomically bound. A failed slot claim consumes no lsn → no holes. `min_lsn` is carried across swaps.
- **No pos rollback on overflow**: an overflowed `fetch_add` leaves a dead byte range, but the buffer is immediately swapped to `Full` and flushed up to `logical_eof` (last valid frame end). Rollback is pointless (the buffer is closed) and risks livelock. See tradeoffs §14.
- **`in_flight` barrier**: incremented before encode, decremented after. The swap thread spins until `in_flight == 0` before sending the buffer to flush, so flush never reads a half-written frame.

### Swap (only one thread performs it)

```rust
fn try_swap_full(&self, old: &WalBuffer) {
    if old.state.compare_exchange(Active, Full, AcqRel, Acquire).is_err() {
        return;                              // another thread is swapping; caller goes to wait
    }
    while old.in_flight.load(Acquire) != 0 { core::hint::spin_loop(); }  // barrier
    // `entries_allocated` is bounded by `claim_slot`'s `assert!(slot < max_entries)`
    // (append claims a byte range first → byte-overflow trips swap before any slot
    // overflows), so no `.min(MAX_ENTRIES)` truncation is needed.
    let count = old.entries_allocated.load(Acquire);
    old.count.store(count, Release);
    let next_min_lsn = old.min_lsn + count as u64;            // carry lsn continuity
    self.flush_tx.send(old.clone());                          // → flush thread
    let new = self.free_pool.pop()
        .unwrap_or_else(|| self.wait_for_free_buffer());      // pool empty → park (backpressure)
    new.reset(next_min_lsn);
    self.active.store(new);                                   // ArcSwap atomic swap
    { let _g = self.swap_lock.lock().unwrap(); self.swap_cv.notify_all(); }  // pair with wait_active_change's park
}
```

### `wait_active_change` (spin fast path + Condvar slow path)

```rust
fn wait_active_change(&self, old: &WalBuffer) {
    for _ in 0..64 {                          // swap usually completes in μs
        if !ptr::eq(self.active.load().as_ref(), old) { return; }
        core::hint::spin_loop();
    }
    // ptr_eq at the loop top is the real "did active change?" test; `wait_timeout`
    // bounds the park so a lost wake just costs a re-check (no version counter).
    let mut g = self.swap_lock.lock().unwrap();
    loop {
        if !ptr::eq(self.active.load().as_ref(), old) { return; }
        let (g2, _) = self.swap_cv.wait_timeout(g, Duration::from_millis(1)).unwrap();
        g = g2;
    }
}
```

The lock is only on the wait/notify path, never on the `append` write hot path.

### Flush thread

```rust
fn flush_loop(&self) {
    let mut seg_written: u64 = 0;   // bytes already written to the current segment's .log (reset on rollover)
    for buf in &self.flush_rx {
        let count = buf.count.load(Acquire);
        let logical_end = compute_logical_end(&buf, count);
        let padded = align_up(logical_end, BLOCK_SIZE);
        self.ensure_segment(seg_written + padded as u64);   // rollover if it won't fit; resets seg_written
        buf.flush_odirect(seg_written, padded);             // .log: O_DIRECT pwrite at seg_written + pad + fdatasync
        // .idx is decoupled: entries go into IdxTail (mem), pwrite only at idx-block granularity.
        // start_offset is the ABSOLUTE offset in .log = seg_written + buffer-local `pos`,
        // because one segment receives many buffer flushes (segment_size >> buffer_size);
        // storing bare `pos` would collide across buffers.
        for i in 0..count {
            let (pos, frame_len) = unpack(buf.entries[i].load(Acquire));
            self.idx_tail.push(IdxEntry {
                lsn: buf.min_lsn + i as u64,
                start_offset: seg_written + pos as u64,
                total_len: frame_len as u32,
            });
            if self.idx_tail.len() == ENTRIES_PER_BLOCK {    // 204
                self.pwrite_idx_block(&mut self.idx_tail);   // .idx: O_DIRECT + fdatasync
                self.idx_tail.clear();
            }
        }
        seg_written += padded as u64;
        self.durable_lsn.store(buf.min_lsn + count as u64 - 1, Release);
        buf.reset_for_reuse();
        self.free_pool.push(buf);                 // wake wait_for_free_buffer
    }
}
```

### Two-layer backpressure

1. **`wait_active_change`**: append threads park when the buffer is full and another thread is swapping. Woken by `swap_cv.notify_all()` after `active.store`.
2. **`wait_for_free_buffer`**: the swap thread parks when the free pool is empty (flush has not returned a buffer). Woken by `free_pool.push` in the flush thread.

When flush is slower than append, the chain backpressures all the way to the callers: pool empty → swap thread parks → no new active → append threads park on `wait_active_change` → append calls stall until flush catches up. No spin on disk I/O.

### Why lock-free over a writer-thread channel

See `docs/analysis/2026-06-22-1500-wal-design-tradeoffs.md` §11. Summary: `std::sync::mpsc` carries a Mutex; even lock-free channels add an enqueue → wake → dequeue → reply round-trip on every append. Direct `fetch_add` + `ArcSwap` keeps the append hot path to atomic ops plus a memory copy, with no per-append coordination cost.

## Operation Paths

### `append`

See Write Buffer Architecture for the full lock-free path. Summary: `fetch_add` byte range → `fetch_add` entry slot → `lsn = min_lsn + idx` → encode frame → return lsn. Overflow (byte or slot) triggers swap + `wait_active_change`. No disk I/O.

### `sync`

1. Mark the current active `Full` (runs the swap path: `in_flight` barrier → send to flush → install fresh active with carried `min_lsn`).
2. Wait until `durable_lsn >= old_active.min_lsn + old_active.count - 1`.
3. Return `durable_lsn`. (Blocking contract is the default; async/pending-marker is an open question.)

### `get(lsn)`

1. Binary-search the route table → `seg_id`.
2. In that segment's `.idx`, binary-search `lsn` → `(start_offset, total_len)`.
3. Compute block range `block_a..=block_b` covering `[start_offset, start_offset + total_len)`.
4. `O_DIRECT pread` those whole blocks into an aligned buffer.
5. Slice out the frame bytes at `start_offset % BLOCK_SIZE`, verify `crc32`, return `&payload`.

### `scan(a..b)`

1. Route table → list of segments whose `[min_live_lsn, max_live_lsn]` intersects `[a, b)`.
2. For each segment, read its `.idx` entries in `[a, b)` → ordered `(offset, len)` list.
3. For each entry, `O_DIRECT pread` the covering blocks (can batch sequential entries in one read) → decode frame → yield `&Record`.
4. Ascending LSN order, no duplicates, no gaps among live records.
5. **Un-flushed records in the active buffer are NOT returned** — `scan`/`get` read only synced on-disk data. The caller must `sync` first to make buffer records visible. Rationale: un-synced records may be lost on crash; read-after-sync is the WAL durability contract.

### `truncate_before(lsn)` (logical delete on the head)

1. Locate segment `S` containing `lsn`.
2. Segments with `seg_id < S` → `remove_file` both `.log` and `.idx` (physical delete).
3. Segment `S` → rewrite `wal-{S}.idx`: set `min_live_lsn = lsn`, drop entries with `lsn' < lsn`, recompute `header_crc`, write via **double-write** (copy A → `fdatasync` → copy B → `fdatasync`). **Do not touch `wal-{S}.log`** — the head bytes become unreachable dead space, gated by `min_live_lsn` in the index.
4. Dead space is reclaimed later by manual `compact()` (whole-segment rewrite or delete).

### `truncate_after(lsn)` (physical truncate on the tail)

1. Locate segment `S` containing `lsn`.
2. Segments with `seg_id > S` → `remove_file` both files.
3. Segment `S` → `set_len` to the block boundary at or after the end of `lsn`'s frame (keep the whole block containing `lsn`; tail bytes past the frame become padding, gated by `max_live_lsn`). Rewrite `wal-{S}.idx`: set `max_live_lsn = lsn`, drop entries with `lsn' > lsn`, recompute `header_crc`, write via **double-write** (copy A → `fdatasync` → copy B → `fdatasync`).
4. If `lsn`'s frame is still in the active buffer (not yet flushed): just truncate the buffer — retreat `entries_allocated`/`write_pos` past `lsn`'s slot and clear trailing entries; disk untouched. The next `append` reuses the freed slot.

Asymmetry rationale: `truncate_after` is usually followed immediately by `append` (Raft leader change), so a clean physical tail is preferred. `truncate_before` is compaction; dead head space is zero-cost since LSNs never revisit old positions.

## Crash Recovery

Required by Raft durability. The tail (last valid frame) is located by **scanning frames with triple validation** — not by trusting an external eof marker or treating `.idx` as authority (both can be torn). `.idx` is appended incrementally on flush (see Index), so it normally already covers almost all flushed data; the scan only walks the small un-flushed tail.

### Step 0 — clean `.new` orphans

Scan the directory for any `wal-*.log.new` / `wal-*.idx.new` left by an interrupted compaction (see Step 5) and delete them. These are crash leftovers; the original `.log`/`.idx` are intact (or `.log` was renamed but `.idx` not — handled by Step 3 rebuild). Safe to delete unconditionally.

### Step 1 — discover segments

Scan the directory for `wal-*.log` / `wal-*.idx` pairs.

### Step 2 — load `.idx` header (double-write) + entries to valid tail

1. Read both header copies (block 0 = A, block 1 = B), verify each `header_crc`. Use whichever passes: either if both pass (identical); if only one passes, use it; if both fail, treat the segment as un-indexed (full `.log` rescan, below).
2. From the surviving header → route table entry (`seg_id`, `min_live_lsn`, `max_live_lsn`). **`max_live_lsn` is not trusted** — Step 3 overwrites it from the `.log` tail.
3. Scan `.idx` entries from block 2 onward; stop at the first entry whose `lsn` is not `prev_lsn + 1` or whose `total_len == 0` → that is the `.idx`'s own valid tail (`.idx` may itself be torn if crash hit mid-append).
4. Record the last valid entry's `(lsn, start_offset, total_len)` → the `.log` offset where indexed data ends, plus `prev_lsn` for the `.log` scan.

If both header copies fail crc, treat the segment as un-indexed and start the `.log` scan from block 0 with `prev_lsn = min_live_lsn - 1` (min from any surviving hint, else `.log` first frame).

### Step 3 — scan `.log` tail with triple validation → locate `logical_eof`

From the `.idx` tail offset, decode frames sequentially:

```
loop:
  read frame header (len, crc, lsn) at offset
  check 1 — len sane:        len > 0  AND  offset + 16 + len <= segment_size
  check 2 — lsn contiguous:  lsn == prev_lsn + 1
  check 3 — crc passes:      crc32(lsn || payload) == header.crc
  all three pass → append (lsn, offset, 16+len) to rebuilt entries; offset += 16+len; prev_lsn = lsn
  any fail     → stop; this offset is the torn / unwritten region
```

`lsn` contiguity is the decisive guard: preallocated-but-unwritten blocks read back as zero (ext4/xfs `fallocate` default) → `len == 0` fails check 1; even if stale non-zero bytes existed, `lsn != prev+1` fails check 2. crc is the final backstop. The scan stops at the first incomplete frame; the stopping offset is the segment's `logical_eof`.

### Step 4 — truncate torn tail + finalize `.idx`

1. `set_len(.log, align_up(logical_eof, BLOCK_SIZE))` — physically drop the torn tail.
2. Append the rebuilt entries (step 3) to `.idx` (from block 2); update header `entry_count` and `max_live_lsn = prev_lsn` (from `.log` tail — source of truth, not the old header).
3. Rewrite the header via **double-write**: copy A (with fresh `header_crc`) → `fdatasync` → copy B → `fdatasync`.
4. Re-`fallocate(.log, logical_eof, segment_size - logical_eof)` to restore preallocation for the remaining tail (see Segment).

### Step 5 — compaction (rewrite segments with head dead-space)

For each segment whose `.idx` first live entry has `start_offset > 0` (head dead-space from `truncate_before`), physically reclaim it via new-file + rename (never in-place `pwrite` — a crash mid-move would corrupt the segment):

1. Create `wal-{seg_id}.log.new`; write surviving frames (`min_live_lsn`..`=max_live_lsn`) read from the original `.log`, starting at offset 0.
2. Pad the last block to 4 KiB (same rule as flush); `logical_eof` = end of surviving frames.
3. Create `wal-{seg_id}.idx.new`: header (same `min_live_lsn`/`max_live_lsn`, fresh `header_crc`, double-write copy A/B) + entries with `start_offset` shifted down by the dead-space offset; pad to block.
4. `fdatasync` both `.new` files.
5. `rename(.log.new → .log)` then `rename(.idx.new → .idx)` — a crash between the two leaves `.log` new + `.idx` old, recovered by Step 3 (`scan .log` rebuilds `.idx`).
6. `set_len(align_up(logical_eof, BLOCK))` (block-aligned physical size). **Do NOT `fallocate`** — the segment is no longer the append target; the reclaimed dead-space is released to the FS.

`start_offset == 0` → skip (no cost). Single-threaded (recovery owns the dir; the active buffer is not yet created until Step 6). Rationale in `tradeoffs §23`.

### Step 6 — resume

Install the route table; start a fresh active `WalBuffer` with `min_lsn = global_max_lsn + 1`.

A torn tail never corrupts earlier valid records: every frame is self-checksummed and lsn-contiguous, so recovery stops cleanly at the first incomplete frame.

## Concurrency

- **`append`**: lock-free multi-writer. Multiple external threads call `append` concurrently; each claims a byte range and an entry slot via `fetch_add` and writes its own frame. No mutex on the append hot path.
- **Active buffer swap**: `ArcSwap` atomic pointer + `state: cmpxchg(Active, Full)` ensures exactly one thread performs each swap.
- **Swap→flush coordination**: `swap_version: Mutex<u64>` + `Condvar` wake parked append threads after `active.store`. The lock is only on the wait/notify path, never on the `append` write path.
- **Flush**: a single dedicated flush thread owns each full buffer outright (physically separated from appends → no data race), performs `O_DIRECT pwrite` + `fdatasync`, advances `durable_lsn` (`AtomicU64`), and returns the buffer to the free pool.
- **`scan` / `get`** read only synced on-disk data (never the active buffer), so they hold **only `Arc<SegmentRoute>`** — no `Arc<WalBuffer>`. This removes the buffer-recycling race entirely: the flush thread can reset + return a buffer to the free pool immediately after `fdatasync`, with no pending-recycle queue.
- **Read cache (`DiskManager`)**: one `Mutex` + `Condvar` (off the append hot path). Readers pin frames via `PinGuard`; `read_block` blocks on the `Condvar` only when all frames are pinned, woken by an unpin. Pin discipline: a caller must not hold a `PinGuard` across a `read_block` that may block.
- **Segment handle reference counting (deferred remove)**: `routes: RwLock<Vec<Arc<SegmentRoute>>>`. `scan`/`get` take the read lock, clone the `Arc<SegmentRoute>` they need, then drop the lock and read `.idx`/`.log` through it. `truncate_*` / segment rollover take the write lock to mutate the vec. If `truncate` removes a segment while a reader still holds its `Arc` (`strong_count > 1`), the physical `remove_file` is deferred until the last reference drops (mark `pending_remove`). Rationale in `tradeoffs §20`.
- **Route table is not on the append hot path**: `append` writes frames into the active buffer via `fetch_add` and never touches `routes`. Only the swap path (segment rollover, rare) and `truncate_*` take the write lock.
- **`truncate_after` vs `append` on the same active buffer (swap isolation)**: if the target `lsn` is in the active buffer, `truncate_after` sets `state = Full` to trigger the swap path → appends are redirected to a fresh active buffer → wait `in_flight == 0` on the old buffer → retreat `entries_allocated = idx + 1` on the now-exclusive old buffer → send it to flush with the truncated `logical_end`. Append is never blocked, only redirected.
- **`truncate_before` does not touch the active buffer**: it is a head-side logical delete with no overlap with the tail-side active buffer — only drops `seg_id < S` (deferred remove) and rewrites segment `S`'s `.idx` (`min_live_lsn`).
- **Staging (resolved)**: the shipped form is the **background flush thread** + two-layer backpressure (per `tradeoffs §15`); the "synchronous flush inside `sync`" staging variant was considered and **not taken** (adjudicated in plan Phase 4a Decision).

## Module Structure

```
src/
├── main.rs              // demo binary
├── lib.rs               // pub mod wal; pub use wal::Wal;
└── wal/
    ├── mod.rs           // Wal facade: append/sync/truncate_*/scan/get + swap coordination
    ├── lsn.rs           // Lsn(u64) newtype + Range helpers
    ├── record.rs        // Record / RecordRef
    ├── frame.rs         // frame encode/decode + crc32
    ├── error.rs         // WalError (thiserror)
    ├── config.rs        // WalConfig + validation
    ├── disk.rs          // DiskManager (O_DIRECT read + CLOCK buffer pool)
    ├── buffer.rs        // WalBuffer + fetch_add append path + swap + wait_active_change
    ├── segment.rs       // disk Segment: O_DIRECT open/read/write/set_len/fsync
    ├── index.rs         // disk .idx read/write, binary search, route table
    └── recovery.rs      // startup scan + torn-tail truncation + index rebuild
```

Dependency direction: `main.rs` → `Wal` → (`buffer`, `segment`, `index`, `recovery`) → (`frame`, `record`, `lsn`). Lower layers never depend on `Wal`.

## External Dependencies

- `crc32fast` — frame checksum (zero-dep pure Rust, fast).
- `arc-swap` — lock-free atomic swap pointer for the active `WalBuffer` (reader side is a single atomic load).
- `nix` (or `libc` directly) — `O_DIRECT` open, `pread`/`pwrite`, aligned I/O.
- `crossbeam` (optional) — `SegQueue` or bounded channel for the free pool / flush queue if a lock-free pool is preferred over `Mutex + Condvar`.
- Optional later: `io-uring` crate for async direct I/O; not in the first slice.

## Open Questions (to resolve in the plan)

- `BLOCK_SIZE` configurable or fixed at 4096? Default 4096, align to device logical block.
- `MAX_ENTRIES` policy: strict `BUFFER_SIZE / 16` upper bound (byte capacity triggers swap first), or a smaller estimate by expected average frame size (slot count triggers swap first, trading tail byte waste for a smaller `entries` array)? Default: `BUFFER_SIZE / 16`.
- Free pool implementation: `Mutex<Vec<Arc<WalBuffer>>> + Condvar` (simple) or `crossbeam` lock-free queue? Default: `Mutex + Condvar` (the lock is off the append hot path).
- `sync` return contract: block until `durable_lsn` catches up, or return a pending marker the caller polls? Default: block.
- Spin threshold for `wait_active_change` (currently 64) — tune after benchmarking.
- `compact()` automatic threshold (dead-space ratio) or manual only? Default: manual only for the first slice.
- `.idx` dense vs sparse: dense chosen; revisit if `.idx` size matters.
