# WAL Design Tradeoffs

> Date: 2026-06-22
> Related: `docs/architecture/wal-design.md`, `docs/requirements/2026-06-22-wal-core.md`
> Status: settled conclusions live in `docs/architecture/wal-design.md`; this file captures the reasoning and rejected alternatives.

## Context

Design discussion for the `wal-demo` Write-Ahead Log, scoped as a high-performance WAL primitive intended as the bottom layer of a Raft log store. Each section records a decision, the alternatives considered, and why the winner was chosen.

## 1. LSN Shape — dense `u64` vs composite `(seg_id, offset)`

| Option | Verdict |
| --- | --- |
| `Lsn(u64)` dense monotonic | **Chosen** |
| `(seg_id, offset)` composite | Rejected |

Rationale: Raft requires a globally monotonic index. A dense `u64` makes `scan`/`truncate` pure numeric range comparisons. The composite form only pays off when segments roll frequently and the engine addresses by physical location; here segments are an internal storage detail and LSN is the logical address. Composite is a production multi-segment optimization that this demo does not need.

## 2. Term Ownership — payload vs frame field

| Option | Verdict |
| --- | --- |
| (a) `term` inside payload bytes | **Chosen** |
| (b) `term: u64` as a frame header field | Rejected |

Rationale: keep the WAL a general log primitive with no Raft coupling in the on-disk format. The caller serializes `(term, command)` into the payload. `prevLogTerm` lookup is an upper-layer concern (deserialize, or upper-layer LSN→term cache). A frame field would inflate every frame's header by 8 B and bake Raft semantics into the storage layer.

## 3. Frame Spanning Blocks — allow vs forbid

| Option | Verdict |
| --- | --- |
| Frame may span any number of blocks | **Chosen** |
| Frame must not cross a block boundary (payload ≤ ~4080 B) | Rejected |

Rationale (user correction): an earlier simplification proposed forbidding cross-block frames to keep append trivial. That breaks for Raft entries with large payloads (snapshot chunks, large command batches). Correct model: a block is purely an I/O alignment unit with no header; frames are a contiguous byte stream that may start mid-block and span any number of blocks. Reading a frame just preads all blocks covering `[start_offset, start_offset + total_len)` and slices out the bytes.

## 4. Truncate Tail Alignment — read-modify-write vs padding

| Option | Verdict |
| --- | --- |
| `set_len` to block boundary, keep the whole block, tail is padding (dead bytes gated by `logical_eof`/`max_live_lsn`) | **Chosen** |
| `set_len` to exact frame end (mid-block); next `append` does read-modify-write on the partial block | Rejected |

Rationale: `O_DIRECT` requires block-aligned writes. Truncating to a frame-exact boundary leaves the file tail mid-block; the next `append` would have to pread the partial block, modify, and pwrite it whole. Padding to the block boundary costs at most ~4 KiB of dead bytes per `truncate_after`, gated by `logical_eof` and `max_live_lsn` so they are never scanned or indexed. `truncate_after` is typically followed immediately by `append` (Raft leader change), so avoiding the read-modify-write on the hot path is worth the trivial dead space.

## 5. `truncate_before` Reclaim — logical delete vs physical rewrite

| Option | Verdict |
| --- | --- |
| Rewrite `.idx` (drop `< lsn` entries, set `min_live_lsn`); leave `.log` untouched; reclaim via later `compact()` | **Chosen** |
| Rewrite the segment file to drop the head physically | Rejected (for now) |

Rationale: head truncation is compaction (Raft post-snapshot). LSNs never revisit old positions, so the dead head bytes are zero-cost until the whole segment becomes reclaimable. Immediate physical rewrite would copy potentially large surviving data on every compaction. Manual `compact()` handles whole-segment reclaim lazily. Automatic threshold-based compaction is deferred.

## 6. Write Buffer — ring buffer vs buffer swap + flush thread

| Option | Verdict |
| --- | --- |
| **B**: buffer swap + flush thread (double buffering, atomic active pointer) | **Chosen** |
| **A**: fixed-size ring buffer with `write_pos`/`synced_pos`, append and flush on the same memory | Rejected |

Comparison:

| Dimension | A ring | B swap |
| --- | --- | --- |
| Memory | 1× `buffer_size` | 2× (active + flushing) |
| Concurrency | append/flush on same memory; needs atomic pointers + fences against tail-gating | physically separated; one atomic swap; flush owns its buffer |
| Append latency | hurt by flush tail-gating backpressure | near lock-free (one swap) |
| Flush | ring tail→head split write | whole-buffer sequential write |
| Scan of un-flushed data | competes for same memory | holds buffer reference |
| Lineage | SPSC queue style | production DB WAL style (RocksDB / InnoDB) |

Rationale: the project goal is high-performance WAL exploration and Raft backend. B's read/write separation gives a near-lock-free append path and a flush thread that never blocks append, at the cost of 2× memory (128 MiB → 256 MiB, acceptable). B is the mainstream production WAL architecture; A's savings are not worth the concurrency edge cases.

## 7. Write Buffer Allocation — `Vec<AlignedBuffer>` vs one contiguous aligned block

| Option | Verdict |
| --- | --- |
| Pre-allocate one `buffer_size` aligned block per `WalBuffer` | **Chosen** |
| `Vec<AlignedBuffer>` (one 4 KiB alloc per block) | Rejected |

Rationale: one allocation per buffer, cache-friendly contiguous memory, natural fit for swap. `Vec<AlignedBuffer>` does one allocation per 4 KiB block and fragments across the allocator.

## 8. Index Location — in-memory `BTreeMap` vs on-disk `.idx`

| Option | Verdict |
| --- | --- |
| On-disk per-segment `.idx` file; only the small segment route table in memory | **Chosen** |
| In-memory `BTreeMap<Lsn, RecordLoc>` for the whole WAL | Rejected |

Rationale: the WAL can be large; a record-level index must not be memory-resident. Each segment has a `wal-{seg_id}.idx` with dense `(lsn, start_offset, total_len)` entries packed into 4 KiB blocks. The in-memory segment route table (`seg_id → {min/max_live_lsn, file handles}`) is small (≈1 KiB per segment), built at startup by scanning `.idx` headers, and is routing metadata, not a record index.

## 9. Index Density — dense vs sparse

| Option | Verdict |
| --- | --- |
| Dense: one entry per record (20 B/entry) | **Chosen (default)** |
| Sparse: one entry per N records, intra-segment binary search + frame-header skip | Rejected (revisit later) |

Rationale: per-segment `.idx` size is bounded by `segment_size`, so dense does not explode. Dense makes `get`/`scan` pure index lookups without intra-segment scanning. Sparse is a later optimization if `.idx` footprint matters.

## 10. I/O — direct I/O vs OS buffered I/O / mmap

| Option | Verdict |
| --- | --- |
| `O_DIRECT` only; memory managed by the database | **Chosen (hard constraint)** |
| `std::fs` buffered I/O or `mmap` | Rejected |

Rationale: the WAL must own its memory (self-managed buffer pool), not rely on the OS page cache. `mmap` and buffered I/O are both forbidden. This forces block-aligned I/O (4 KiB), which is why frames pack into blocks and why truncate uses padding alignment.

## 11. Write Buffer Concurrency — lock-free `fetch_add` vs writer-thread channel

| Option | Verdict |
| --- | --- |
| **A**: multiple append threads write the active buffer directly via `fetch_add` + `ArcSwap` | **Chosen** |
| B: MPSC channel → single writer thread → flush channel (RocksDB WriterQueue style) | Rejected |

Rationale (owner-driven): `std::sync::mpsc` carries a Mutex on the append path; even lock-free channels add an enqueue → wake → dequeue → reply round-trip per append. Direct `fetch_add` keeps the append hot path to atomic ops + one memory copy. The WAL's bottleneck is disk flush, but the append path's per-call latency under high concurrency is materially lower without a single-writer serialization point. Accepted cost: `in_flight` barrier + `ArcSwap` coordination complexity (see §12–§15).

## 12. LSN Allocation — global counter vs slot-bound

| Option | Verdict |
| --- | --- |
| `lsn = buf.min_lsn + idx` where `idx = entries_allocated.fetch_add(1)` | **Chosen** |
| Global `next_lsn: AtomicU64` with `fetch_add` | Rejected |

Rationale: a global counter decouples lsn from the physical slot. With variable-length frames, two independent `fetch_add`s (pos and lsn) cannot be atomically bound; an `append` that fails the slot check after claiming an lsn leaves a **hole** (lsn allocated, no record) — fatal for Raft. Binding `lsn = min_lsn + idx` makes claiming the slot and claiming the lsn the same atomic op: no slot → no lsn → no hole. `min_lsn` is carried across swaps for cross-buffer continuity.

## 13. In-Buffer Index — entries array vs flush-scan rebuild

| Option | Verdict |
| --- | --- |
| `entries[lsn - min_lsn]` array, written per-append, walked in order at flush | **Chosen** |
| No in-buffer index; flush scans `data` and `sort_by_key(lsn)` to build `.idx` | Rejected |

Rationale: indexing `entries` by lsn slot makes it **naturally lsn-ordered** — flush writes `.idx` by walking `entries[0..count]` with no sort. The earlier "drop entries + flush sort" idea removed the sort by paying an O(N log N) sort at flush; keeping the array preserves ordering for free. The array has a strict upper bound `MAX_ENTRIES = BUFFER_SIZE / 16` (smallest frame is the 16 B header), and the slot-full check gates writes before they overflow, so there is no out-of-bounds panic. Overflow handling is backpressure (park until swap), not `continue` (which would waste an lsn — see §12).

## 14. Position Overflow — rollback vs swap-on-overflow

| Option | Verdict |
| --- | --- |
| Overflow → immediate `state = Full` → swap; dead byte range flushed away by `logical_eof` | **Chosen** |
| `fetch_sub` to roll back `write_pos`, keep buffer `Active`, let smaller frames fill the tail | Rejected |

Rationale: rolling back only pays off if the buffer stays `Active` to accept smaller frames in the reclaimed tail. That requires changing the swap trigger to "residual < min frame" and forces large-frame appends to spin/park until the tail drains — complexity plus a livelock risk (large-frame appends retried in a loop with nobody obligated to swap). Swap-on-overflow is the only model that is both self-consistent and livelock-free; the wasted tail is at most one max frame, negligible against `buffer_size`.

## 15. Swap Coordination — ArcSwap + Condvar vs pure spin vs channel handoff

| Option | Verdict |
| --- | --- |
| `ArcSwap` active pointer + `state: cmpxchg(Active, Full)` + `swap_version: Mutex<u64>` + `Condvar` (spin 64 then park) | **Chosen** |
| Pure spin until active changes | Rejected |
| Channel handoff (swap thread sends "switch" event to append threads) | Rejected |

Rationale: pure spin burns CPU when the swap stalls on an empty free pool (flush slower than append, ms-scale). A channel handoff re-introduces the very per-append coordination cost §11 rejected. `ArcSwap` gives lock-free active reads on the hot path; the `Mutex + Condvar` touches only the wait/notify path (taken only on overflow), never the append write path. `wait_timeout(1ms)` guards against a missed notify. Two-layer backpressure (`wait_active_change` for append threads, `wait_for_free_buffer` for the swap thread) propagates flush slowness all the way back to callers without any spin on disk I/O.

## 16. Segment Preallocation — fallocate at creation vs grow-on-demand

| Option | Verdict |
| --- | --- |
| `.log` `fallocate(segment_size)` at creation; re-`fallocate` after `truncate_after` | **Chosen** |
| Empty file, grow on append | Rejected |

Rationale: `O_DIRECT` writes on a growing file trigger per-block filesystem allocation + metadata updates on the hot path → jitter and fragmentation. Preallocating `segment_size` once makes every append hit already-allocated blocks. Cost: `segment_size × active_segments` physical space (bounded — compaction deletes old segments). `truncate_after`'s `set_len` releases tail blocks, so re-`fallocate` restores the preallocation. `.idx` is not preallocated (small, append-grown). ext4/xfs `fallocate` zeroes blocks by default, which recovery's zero-detection relies on.

## 17. Recovery Tail Detection — triple validation vs external eof marker

| Option | Verdict |
| --- | --- |
| Scan frames with triple validation: `len` sane + `lsn` contiguous + `crc` passes | **Chosen** |
| External `logical_eof` marker file, or treat `.idx` as authority | Rejected |

Rationale: an external eof marker or `.idx`-as-authority introduces a second source of truth that can itself be torn (crash mid-update). Frame self-description (len + crc) plus `lsn` contiguity — starting from the `.idx` tail's last `lsn` — locates the torn boundary without any external marker. `lsn` contiguity is the decisive guard against misreading preallocated/stale blocks as valid frames. The scan starts from the `.idx` tail (not block 0), so normally only the last un-flushed buffer's worth of frames is scanned, not the whole segment.

## 18. `.idx` vs `.log` — decoupled twin accumulators

| Option | Verdict |
| --- | --- |
| `.idx` is its own accumulator (`IdxTail`), flushed at idx-block (204 entries) granularity, decoupled from wal-buffer flush | **Chosen** |
| `.idx` written synchronously on every wal-buffer flush (per-buffer append) | Rejected |

Rationale: tying `.idx` writes to wal-buffer flush forces a sub-block `.idx` write on every flush (read-modify-write the tail block, or pad-and-overwrite) — complex and I/O-inefficient. Treating `IdxTail` as a second accumulator (same fill / flush-at-block-granularity / crash-loses-tail / recovery-rebuilds pattern as `WalBuffer`) keeps `.idx` writes at clean block-aligned `O_DIRECT` pwrite, no RMW, at the cost of `.idx` lagging `.log`. The lag is safe: `.log` is the source of truth, `.idx` is a rebuildable cache; recovery rebuilds the missing tail by scanning `.log` with the triple validation (§17). `sync` flushes `.log` only; `IdxTail` is flushed on segment rollover so rolled-out segments stay self-contained.

## 19. `scan`/`get` read only synced on-disk data — no buffer visibility

| Option | Verdict |
| --- | --- |
| `scan`/`get` read only synced (on-disk) records; un-synced buffer records invisible until `sync` | **Chosen** |
| `scan`/`get` transparently span buffer + disk (read un-flushed records from the active buffer) | Rejected |

Rationale: un-synced records may be lost on crash (the WAL durability contract is "durable only after `sync`"), so exposing them to readers violates read-after-sync and creates a buffer-recycling race (swap resets the buffer while a reader borrows it). Reading only on-disk data removes that race entirely — the flush thread recycles buffers immediately, no `Arc<WalBuffer>` reference counting or pending-recycle queue needed. Readers still hold `Arc<SegmentRoute>` for deferred-remove safety against concurrent `truncate` (see §20). Cost: a caller that wants the latest records must `sync` first — acceptable for Raft (replication replays committed/synced entries).

## 20. Concurrency coordination — reference counting + swap isolation + RwLock (no global lock)

| Option | Verdict |
| --- | --- |
| Segment `Arc` reference counting (deferred remove) + `truncate_after` swap isolation + route-table `RwLock` | **Chosen** |
| Global `Mutex` around `truncate`/`scan` (block append during reads/truncation) | Rejected |

Rationale: a global lock would block the lock-free append hot path during every `scan`/`truncate`, defeating the performance goal. The chosen split keeps `append` lock-free (it never touches the route table; only swap-rollover and `truncate_*` take the write lock, both rare), makes `scan`/`get` wait-free for readers (read-lock → clone `Arc` → drop lock → read), and isolates the one true write/write race (`truncate_after` on the active buffer) via the existing swap mechanism instead of a new lock. Complexity is bounded: one `RwLock`, one deferred-remove flag, reuse of the swap path.

## 21. `.idx` header crash-consistency — double write (A/B identical copies)

| Option | Verdict |
| --- | --- |
| Double write: two identical header copies (block 0 = A, block 1 = B) each with `header_crc`; write A → fsync → B → fsync; recovery uses whichever copy passes crc | **Chosen** |
| Single header copy, rely on 4 KiB block-aligned write atomicity | Rejected |
| Shadow paging: alternating A/B writes with a generation counter | Rejected |

Rationale: `min_live_lsn` cannot be rebuilt from `.log` (truncated-head dead space is indistinguishable from live frames), so the header must survive a torn write. `O_DIRECT` 4 KiB writes span 8× 512 B sectors — device-level atomicity is only per-sector, so a single 4 KiB header write can tear. Two identical copies written sequentially (A then B, never concurrent) guarantee at least one intact copy after any crash; `header_crc` selects the intact one without trusting block atomicity. This is the same principle as InnoDB's double write buffer. The alternating + generation variant (shadow paging) was rejected: A/B hold identical values here, so a generation counter to distinguish "newer" is meaningless — crc alone suffices, and writing both copies every time avoids losing the last truncate on crash. `max_live_lsn` needs no such protection — recovery rebuilds it from the `.log` tail (`.log` is the source of truth). Cost: 4 KiB extra per segment (one extra header block) + 2× header write on truncate (truncate is low-frequency, negligible).

## 22. Read path — `DiskManager` LRU + `Arc`-backed `RecordRef` (no copy, no `&self` borrow)

> **Reversed 2026-06-24 — see §22b.** The shipped design is a fixed-frame CLOCK buffer pool with frame pinning; `read_block` returns a borrowed `PinGuard`, not an `Arc`. The text below is the original (historical) reasoning, kept for the record.

| Option | Verdict |
| --- | --- |
| `DiskManager` serves `O_DIRECT` block reads via a simple LRU of `Arc<AlignedMem>`; `get`/`scan` return owned `RecordRef` holding an `Arc` clone + payload range | **Chosen** |
| Copy payload into an owned `Vec<u8>` on every `get`/`scan` yield | Rejected |
| Return `&Record` borrowing `&self` (Wal holds a shared read buffer) | Rejected |

Rationale: `O_DIRECT` requires 4 KiB-aligned buffers the WAL must own (no page cache); `get`/`scan` are concurrent, so `&self` cannot hold a single shared read buffer. Copying the payload on every yield wastes the direct-I/O aligned buffer and adds a memcpy per record. An `Arc<AlignedMem>` clone is cheap (refcount bump) and lets each `RecordRef` outlive LRU eviction — the payload's lifetime is owned by the `Arc`, not by `&self` or a borrowed iteration slot. The `DiskManager` LRU is a simple bounded cache; replacing it with an external block manager later touches only `DiskManager` internals (`read_block` signature stays). Cost: one `Arc` per record + one LRU lookup per block (amortized over records sharing a block).

## 22b. §22 Reversal (2026-06-24) — fixed-frame CLOCK buffer pool + frame pinning

The original §22 chose Arc-survives LRU (readers hold `Arc` clones; eviction never blocks). **Reversed** to a fixed-frame CLOCK buffer pool with per-frame pinning. Driver + reasoning:

- **Driver — hard memory bound.** The Arc-survives model has only a soft bound: under concurrent readers holding many blocks, resident memory grows past cache capacity because each reader's `Arc` keeps a block alive past eviction. A fixed-frame pool caps memory at N × `block_size`.
- **Consequence — borrowed `PinGuard`.** `read_block` returns a borrowed `PinGuard` (pin on acquire, unpin+notify on `Drop`), not an owned `Arc<AlignedMem>`. In-place frame reuse cannot coexist with long-lived owned references — a reader's `Arc` would be corrupted when the frame is evicted and overwritten. This reverses §22's "payload outlives eviction via `Arc`" rationale and its "`read_block` signature stays" note.
- **Replacer — CLOCK second-chance, not strict LRU.** The hot path sets one `ref_bit` per access (no list relink); the `hand` sweep + second-chance runs only on the (rarer) eviction path and naturally skips pinned frames. Trade: CLOCK is an LRU approximation.
- **Safety contract (load isolation).** A miss-load `pread` runs only on a frame whose old page-table mapping was removed under the lock **and** whose `pin_count` was 0 at eviction — so no concurrent hit can alias the frame being overwritten. On `pread` error the loader unpins + notifies — a failed load cannot leak a pin and exhaust the pool. No extra fences; the `Mutex` acq-rel edge carries loader-`pread` → install → reader-`Deref`.
- **Alternatives rejected.** (a) strict LRU + pin — heavier hot path (relink on every hit); (b) Arc-survives + CLOCK victim selector without pin/block — soft bound, no real reason to block, collapses to the old model; (c) safe checkout-by-value `PinGuard` — zero `unsafe` but serializes same-block readers; (d) safe Arc-share — soft bound.
- **Residual.** `get`/`scan`/`RecordRef` not yet implemented; `RecordRef` (shipped, dormant) still holds `Arc<AlignedMem>` — its contract will be revisited (hold a `PinGuard` or copy the payload) when read ops are built. Single `Mutex` for now (per-frame latch / lock-free is a measured-contention follow-up). Plan: `docs/plans/2026-06-24-read-cache-clock-buffer-pool-plan.md`.

## 23. Compaction — restart-time segment rewrite (no runtime `compact()`)

| Option | Verdict |
| --- | --- |
| Restart-time: on `open`, rewrite segments whose `.idx` first live entry has `start_offset > 0` (head dead-space from `truncate_before`); new-file + rename; no runtime `compact()` | **Chosen** |
| Runtime `compact()` (online, concurrent with append/scan/truncate) | Rejected |
| In-place `pwrite` forward-move + `set_len` | Rejected |

Rationale: `truncate_before` only logical-deletes (cheap — rewrite `.idx` `min_live_lsn`); the head dead-space is reclaimed physically on the next restart, when the dir is scanned anyway (headers read for the route table). This removes a runtime concurrency hazard (compact vs append/scan/truncate coordination) at the cost of dead-space living until next restart (bounded; for Raft, one segment after each snapshot). New-file + rename is mandatory: in-place forward-move crashes mid-move and loses data (source frames overwritten before destination is complete). After rewrite the segment is block-aligned (pad last block, `set_len(align_up(...))`) and **not** `fallocate`-d (the segment is no longer the append target — release reclaimed space to the FS). `.new` orphans from an interrupted rewrite are cleaned on every open (recovery Step 0). Online (no-restart) compaction remains a pre-production option (Deferred).

## Summary Of Settled Decisions

All chosen options above are固化 in `docs/architecture/wal-design.md`. Items still open (block size configurability, backpressure policy, `sync` blocking contract, automatic compaction threshold) are listed there as Open Questions.
