# Plan: Implement WAL Index Read Path

**Date**: 2026-06-27
**Type**: Architecture change (Storage engine internals)
**Status**: Refreshed post-GAT; revised after audit (B1–B3, M1–M4 addressed); ready for implementation
**Autonomy**: plan-first (protected area: WAL internals)
**Reviewer**: subagent

## Goal

Implement the WAL index read path (`get(lsn)`, `scan(lsn_range)`) so the WAL v2 can serve random lookup and range replay — completing the primitive a Raft log store needs.

## Foundation Already In Place (Plan 1 — committed)

The GAT block-I/O unification is done and committed:

- `BlockReader` is a GAT: `type Guard<'a>: Deref<Target=[u8]>`.
- `NormalBlockIter<B: Deref<Target=[u8]>>` is generic over storage — LSM uses `Bytes`, WAL uses `PinGuard`.
- WAL `PinGuard` is `'static` (holds `Arc<DiskManagerInner>`, not `&'a DiskManager`) → no self-reference → a stateful iterator can store a cached frame zero-copy.

Consequence: `SstIter<PinGuard>` over a WAL-`DiskManager`-backed reader is now well-formed with no materialization and no iterator-stack rewrite. This plan plugs the index SST into that pipeline.

> **2026-06-27 correction (Phase 1.2 verification):** that composition only held
> for shallow trees. The end-to-end round-trip test (`block_size=64` → height 6)
> exposed two latent SST bugs that corrupt any tree of height ≥ 4 (root orphaning
> in `SstBuilder::finish`; subtree-skip in `IndexTreeIter::inner_next`). Both are
> now fixed; see `docs/bugs/2026-06-27-sst-deep-tree-traversal.md`. Phase 1.2
> (`WalIndexReader`) is done and verified.

## Settled Decisions

1. **Index `.idx` SST uses the WAL `DiskManager` (O_DIRECT + CLOCK cache)** — not LSM's buffered `DiskManager`. The whole point of making `PinGuard` `'static` was to enable this; reads are zero-copy cached.
2. **One index SST per segment** — a single `MemTable` per active segment, sealed once at segment close. Simple file structure (`seg_NNNNN.{meta,idx,log}`), bounded by `segment_size`.
3. **Index fed at buffer-flush time** — the flush thread already captures `(lsn, offset, len)` per frame at `facade.rs:380` (currently feeding `IdxTail`). Retarget that loop to a `MemTable`; replace `IdxTail` / `write_idx_block` with `MemTable` + `SstBuilder` seal.

## Design (owner: `docs/architecture/wal-index-design.md`)

- **File layout**: `seg_NNNNN.meta` (mutable header, 8 KB double-write) + `seg_NNNNN.idx` (immutable SST, LSN→(offset,len)) + `seg_NNNNN.log` (frames, unchanged).
- **Key**: LSN as 8-byte **big-endian** `Bytes` (lexicographic = numeric). **Value**: `(offset:u64, len:u32)` **little-endian**, 12 B.
- **Truncate**: rewrite `.meta` only (`min_live_lsn` / `max_live_lsn`); dead segment (`min > max`) → delete all three files.
- **Index I/O**: O_DIRECT via WAL `DiskManager`; SST writes need an aligned `BlockWriter`.

## Scope

### In Scope

**Phase 1 — Core infrastructure**
- 1.1 Aligned O_DIRECT `BlockWriter` (`ODirectBlockWriter`) for `SstBuilder`.
- 1.2 `BlockReader` wrapper over WAL `DiskManager` (`WalIndexReader`, returns `'static PinGuard`).
- 1.3 Index SST file lifecycle: id allocation, open fd, path layout; a **separate `DiskManager` instance** for the index (avoids cache-key/pool collision with `.log` blocks).
- 1.4 `.meta` I/O: rename `IdxHeader` read/write to `.meta` (keep double-write + `select_valid_header`).
- 1.5 Encoding helpers (`lsn_to_key`, `encode_offset_len`, …) + path helpers.
- 1.6 `SegmentIndex` (header + SST reader) struct.

**Phase 2 — Write path (decision 3)**
- 2.1 Retarget `facade.rs:380` capture loop: `idx_tail.push(IdxEntry{..})` → `index_mem.put(lsn_be, encode(offset,len))`. One `MemTable` per active segment in `FlushState`.
- 2.2 `finalize_segment`: seal `MemTable` → `.idx` SST via `SstBuilder` (aligned writer); write `.meta` header with `[min_live_lsn, max_live_lsn, entry_count]`.
- 2.3 Remove `IdxTail`, `write_idx_block`, and the old 4 KB-entry `.idx` block format.

**Phase 3 — Read path**
- 3.1 `SegmentIndex::get(lsn)`: range-filter via `.meta`, then `SstIter` point seek.
- 3.2 `SegmentIndex::scan(range)`: intersect with live range, `SstIter` range scan.
- 3.3 `Wal::get` / `Wal::scan`: locate segment(s), query index, read record bytes from `.log` via the WAL `DiskManager`.

**Phase 4 — Truncate**
- 4.1 `truncate_before` / `truncate_after`: update `.meta` only (8 KB double-write).
- 4.2 Dead-segment detection + file deletion.

**Phase 5 — Recovery**
- 5.1 Discover `.meta` files on `open`; load each `SegmentIndex` (`.meta` + `.idx` SST).
- 5.2 Rebuild the active (unsealed) segment's in-memory `MemTable` by replaying the unflushed `.log` tail (existing tail-recovery path).

### Out of Scope

- Bloom filters, index compression, cross-segment global index.
- `.log` format changes.
- Migrating LSM data files to O_DIRECT (separate, project-wide effort).

## Key Design Point — O_DIRECT SST I/O

`DefaultSstWriter` already asserts data/index blocks are `block_size`-aligned, so they pwrite cleanly via O_DIRECT. Only the **footer** is variable-length.

**Write side** (`ODirectBlockWriter::finish`): pad the footer up to a `block_size` boundary and pwrite it as one final aligned block. Do NOT buffer the whole SST — `SstBuilder` is streaming; only the footer tail needs padding. (The padding bytes after the footer body are never read back as data.)

**Read side** (footer decode on `open`): the existing LSM `DiskManager::open` reads the last 4 bytes (body_len trailer) via buffered `read_exact_at` — **this fails under O_DIRECT** (unaligned). The index reader must read the footer block-aligned: `pread` the last `block_size` (or last 2 blocks if the footer body can straddle a boundary — footer is small, so 1 block suffices once block_size is known) at offset `floor(file_size / block_size) * block_size`, then parse the trailer + body from that buffer.

## Implementation Strategy (Skill: none for all phases)

### Phase 1 — Core infrastructure

**1.1 `ODirectBlockWriter`** (new, `wal/` or `block.rs`): opens fd with `O_DIRECT|O_RDWR|O_CREAT`, `pwrite`s block-sized aligned slices, tracks `next_offset`. Implements `BlockWriter`. Footer handling per the design point above.

**1.2 `WalIndexReader`** (new): the reader side.
```rust
pub struct WalIndexReader {
    dm: Arc<DiskManager>,   // separate instance for the index
    file_id: u32,           // cache-key namespace (sst file id)
    fd: RawFd,
}
impl BlockReader for WalIndexReader {
    type Guard<'a> = PinGuard;   // 'static
    fn read_block(&self, pos: &Position) -> StorageResult<PinGuard> {
        let block_idx = pos.offset / (self.dm.block_size() as u64);
        self.dm.read_block(self.file_id, self.fd, block_idx)
            .map_err(WalError::into)
    }
    fn block_size(&self) -> usize { self.dm.block_size() }
}
```
`DiskManager::read_block(seg_id, fd, block_idx)` already takes a cache-key id + fd; reuse as-is (the `seg_id` param is just the namespace — index passes its `file_id`).

**fd ownership**: `Segment` already owns `idx_fd` (opened O_DIRECT) and closes it on `Drop`. `WalIndexReader` must **borrow** that fd (hold `&RawFd` / dup on demand), not open a second one — avoid double ownership and double-close. Concretely: the reader is constructed from a `SegmentRoute`/`Segment` reference and does not outlive it; reads borrow the route's `idx_fd`. **fd-exhaustion policy**: rather than a long-lived fd per segment for every sealed segment, keep fds open only for segments in the read working set; idle segments close their `idx_fd` and reopen on next access (the `.idx` path is deterministic). Document the ulimit implication; default ulimit 1024 bounds concurrent open segments.

**1.3 Index SST file lifecycle**: id allocator (`next_index_file_id: AtomicU32`), `create_index_file(seg_id) -> (file_id, fd)`, `open_index_file(file_id) -> fd`, path `seg_{seg_id:05}.idx`. The index `DiskManager` is a separate instance from the `.log` block cache.

**1.4 `.meta` I/O**: `IdxHeader` read/write retargeted to `.meta`; double-write + `select_valid_header` unchanged. (`IdxHeader`/`IdxEntry`/crc helpers stay; `IdxTail` goes.)

**1.5 Encoding + path helpers**: `lsn_to_key` (be), `key_to_lsn`, `encode_offset_len` (le), `decode_offset_len`; `meta_path/idx_path/log_path(seg_id, dir)`.

**1.6 `SegmentIndex`**:
```rust
pub struct SegmentIndex {
    header: IdxHeader,
    meta_path: PathBuf,
    reader: WalIndexReader,   // or hold footer + open lazily
    footer: SstFooter,
}
```

### Phase 2 — Write path (decision 3)

**2.1** In `FlushState`, replace `idx_tail: IdxTail` with `index_mem: MemTable<Bytes, Bytes>` (one per active segment; reset in `ensure_segment`). The capture loop at `facade.rs:380` becomes:
```rust
for i in 0..count {
    let (pos, flen) = unpack(buf.entries[i].load(Ordering::Acquire));
    index_mem.put(lsn_to_key(min_lsn + i as u64),
                  encode_offset_len(base + pos as u64, flen));
}
```

**2.2** `finalize_segment`: build `.idx` SST via `SstBuilder` over `ODirectBlockWriter` (drain `index_mem` in key order — `MemTable` iterates sorted), write `.meta` header.

**2.3** Field changes in `FlushState`: **remove** `idx_tail: IdxTail` and `idx_blocks: usize` (no longer needed — the SST builder tracks block count). `seg_entry_count` becomes `index_mem.len()` at seal time. Delete `IdxTail`, `write_idx_block`, and the 4 KB-entry `.idx` block format (`IdxEntry` block serialization; keep `IdxEntry` only if recovery still references it — likely removable).

### Phase 3 — Read path

**3.1 `get`**: range-filter (`min_live_lsn..=max_live_lsn`), then `SstIter` seek to `lsn_to_key(lsn)`, decode value. Read record bytes from `.log` via the `.log` `DiskManager` (or buffered, since `.log` reads may stay buffered — TBD, see Open Questions).

**3.2 `scan`**: intersect requested range with live range; `SstIter` range scan; decode each `(lsn, offset, len)`.

**3.3 `Wal::get/scan`**: binary-search `routes` by LSN; delegate to that segment's `SegmentIndex`; for `get`, read the record bytes from `.log`.

### Phase 4 — Truncate

`truncate_before(lsn)`: for each segment, `header.min_live_lsn = max(min_live_lsn, lsn)`, rewrite `.meta`. `truncate_after`: mirror with `max_live_lsn`. Then drop segments where `min_live_lsn > max_live_lsn` (remove `.meta`/`.idx`/`.log`). SST untouched.

### Phase 5 — Recovery (greenfield)

**Reality check**: `Wal::open` today explicitly does NOT recover — it opens a fresh WAL and loads no existing files. So Phase 5 is **new development**, not a retarget. It must: discover segments, distinguish sealed from active, load sealed indices, rebuild the active index from the `.log` tail.

**Commit invariant** (defines the discriminator): seal writes the `.idx` SST first (durable), **then** finalizes `.meta` with the real `entry_count`/`max_live_lsn`. A `.meta` written at segment creation has `entry_count == 0`. Therefore on recovery:

- `.meta` with `entry_count > 0` → **sealed**: open `.idx` SST via `WalIndexReader`, load `SegmentIndex`.
- `.meta` with `entry_count == 0` → **active** (at most one, the highest seg_id): the index MemTable was in-memory only and was lost; rebuild it by **replaying the `.log` tail** — decode frames from `.log` start, validate each (crc + len, reuse existing frame decode), `put(lsn_be, encode(offset,len))` into a fresh MemTable until the first invalid/trailing frame (crash tear). Reconstruct `FlushState` (`seg_written`, `seg_max_lsn`, `seg_entry_count`) from the replayed tail.

**Steps**:
1. Discover `seg_*.meta` → seg_ids (sorted).
2. For each: read `.meta` (double-write recover via `select_valid_header`); branch on `entry_count`.
3. Sealed → `SegmentIndex::open` (`.meta` + `.idx` SST footer read block-aligned per the O_DIRECT design point).
4. Active → replay `.log` tail into MemTable; install as the live `FlushState`'s current segment.
5. Edge case: `.idx` present but `.meta` still `entry_count == 0` (crash mid-seal) → treat as active (replay `.log`), the partial `.idx` is discarded/overwritten on next seal.

**Tests** (extend the scenario list): crash after seal → recovers sealed index; crash mid-flush → active `.log` tail replayed; crash mid-seal → treated as active.

## File Changes

| File | Change |
|------|--------|
| `wal/disk.rs` | (already `'static PinGuard`) — no change expected; reuse `read_block` |
| `wal/odirect_writer.rs` (new) | `ODirectBlockWriter` |
| `wal/index.rs` | `.meta` I/O; encoding helpers; `SegmentIndex`; drop `IdxTail`/block format |
| `wal/segment.rs` | index SST file lifecycle helpers |
| `wal/facade.rs` | retarget `flush_buffer` capture → `MemTable`; `finalize_segment` seals SST; wire `get/scan/truncate` |
| `wal/mod.rs` | exports |
| `tests/wal_v2/wal_index_ops.rs` (new) | index build/get/scan/truncate/recovery tests |

**Estimated impact**: ~6 files, ~900–1200 lines (incl. ~400 tests).

## Risks

1. **O_DIRECT footer alignment** — variable-length footer can't be written raw via O_DIRECT. Mitigation: design point (a) staging buffer; test on ext4 + xfs.
2. **Cache-key / pool collision** between `.log` and `.idx` blocks. Mitigation: separate `DiskManager` instance for the index.
3. **PinGuard deadlock discipline in `scan`** — holding a guard across a blocking `read_block` can pin all frames. Mitigation: `SstIter`/`NormalBlockIter` drop the previous guard before fetching the next (already the case — guards are consumed per `from_block`); size the index pool > max concurrent scan blocks.
4. **Key endianness** — wrong order breaks seek. Mitigation: unit test `lsn1 < lsn2 ⇒ key1 < key2`.
5. **`.meta` both-copies corrupt**. Mitigation: `select_valid_header` already returns `HeaderCorrupt`; tested.
6. **Truncate vs get/scan race**. Mitigation: reads hold `Arc<SegmentRoute>` (routes are already `Arc`'d); truncate removes the route from `routes` but **defers file deletion** until the last reader's `Arc` drops. An in-flight read keeps its fd valid; the dead segment's files are removed only when the last reader finishes. (`Arc` refcount is the synchronization — no `RwLock` on the bytes path.)
7. **MemTable memory** is NOT `entries × 20 B` — count the skip-list node overhead (~40–80 B/entry) + `Bytes` allocation headers. Worst case (tiny frames, large `segment_size`) can be several hundred MB. Bound by **entry count** (derive from `segment_size` / min-frame-size), not just bytes; tune `segment_size` down if the index MemTable footprint is too high.

## Verification

```bash
cargo test -p storage wal_index      # new index tests
cargo test -p storage wal_v2         # all WAL tests
cargo build --workspace
```

**Test scenarios**:
1. Key encoding preserves order.
2. `flush_buffer` feeds `MemTable` correctly (replace `IdxTail` test).
3. `finalize_segment` produces a valid `.idx` SST + `.meta`.
4. `get` returns correct `(offset, len)`; respects truncate range.
5. `scan` yields ascending entries in live range; respects truncate.
6. `truncate_before`/`truncate_after` update `.meta` only (`.idx` byte-identical).
7. Dead segment deleted on truncate.
8. Recovery loads `.meta` + `.idx`; active-segment tail replay rebuilds `MemTable`.
9. Double-write recovery: one corrupt `.meta` copy survives.
10. Index `DiskManager` cache isolated from `.log` cache (no key collision).
11. `.idx` cache hit: two `get`s of the same LSN return `PinGuard`s with equal `as_ptr()` (no re-pread).
12. Recovery: crash after seal → sealed index loads; crash mid-flush → active `.log` tail replayed into MemTable; crash mid-seal (`.idx` present, `.meta` `entry_count==0`) → treated as active.

**Smoke checklist**:
- [ ] Each sealed segment has exactly `seg_NNNNN.{meta,idx,log}`.
- [ ] `.meta` is 8192 bytes.
- [ ] `.idx` opens as a valid SST — footer decoded from a **block-aligned** tail read (not buffered `read_exact_at`).
- [ ] `.idx` reads served by the CLOCK cache: two `get`s of the same LSN return `PinGuard`s with **equal `as_ptr()`** (cache hit, no re-pread) — observable via `PinGuard::as_ptr()`.
- [ ] Active (unsealed) segment has `.meta` with `entry_count == 0` and no `.idx` SST.

## Closure Criteria

1. All phases compile; `cargo build --workspace` clean.
2. All unit + integration tests pass (existing WAL tests + new index tests).
3. `get`/`scan`/`truncate_before`/`truncate_after` work end-to-end.
4. Recovery reconstructs sealed + active segments.
5. `.idx` reads go through the WAL `DiskManager` cache (zero-copy `PinGuard`) — verified by a test asserting two `get`s of the same LSN return guards with equal `PinGuard::as_ptr()` (cache hit, no re-pread).
6. `IdxTail`/old `.idx` block format fully removed.
7. Docs updated (`wal-index-design.md`, daily log).

## Dependencies

**Requires** (done): GAT block-I/O unification (Plan 1, committed) — `PinGuard` `'static`, `NormalBlockIter<B>`, `BlockReader` GAT.

**Blocks**: WAL v2 ↔ `LsmStore` integration (backlog Priority 1).

## Open Questions

- `.log` record reads in `get`: stay buffered, or also route through a `DiskManager` cache? (Index is O_DIRECT; `.log` reads are one-per-get, low frequency — buffered is likely fine for now.)
- Index `DiskManager` pool sizing default (tune after measuring scan working set).
