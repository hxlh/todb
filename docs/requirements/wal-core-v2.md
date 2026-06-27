# Feature: High-Performance WAL Core

## Goal

Build a minimal but realistic Write-Ahead Log (WAL) in Rust to explore high-performance logging primitives, intended as the **bottom layer of a Raft log store**. The core delivers six operations — `append`, `sync`, `truncate_before`, `truncate_after`, `scan`, `get` — over a streaming container format backed by a self-managed write buffer and direct-I/O on-disk segments, with an on-disk LSN index for fast positioning.

Full technical contract: `docs/architecture/wal-design.md`. Design tradeoffs: `docs/analysis/2026-06-22-1500-wal-design-tradeoffs.md`.

## In Scope

- `append(record) -> lsn`: encode a log record into the active write buffer; assign a monotonic LSN. Does not touch disk.
- `sync() -> sync_lsn`: flush buffered records to disk; durability boundary.
- `truncate_before(lsn)`: discard records with LSN strictly less than `lsn` (keep `>= lsn`). Retention/compaction path.
- `truncate_after(lsn)`: discard records with LSN strictly greater than `lsn` (keep `<= lsn`). Raft leader-change rollback path.
- `scan(range) -> ScanIter`: iterate **synced** live records whose LSN falls in a half-open `[start, end)` range, in ascending LSN order; yields owned `RecordRef` items (`Item = Result<RecordRef, WalError>`). Un-synced records still in the active buffer are not visible until `sync`.
- `get(lsn) -> Result<Option<RecordRef>, WalError>`: random lookup by LSN (Raft `prevLogTerm` path); returns an owned `RecordRef` (payload backed by the read buffer's `Arc`, no copy).
- Streaming container format: framed records (`len | crc32 | lsn | payload`), self-describing, torn-tail detectable, frames may span multiple 4 KiB blocks.
- On-disk LSN index `wal-{seg_id}.idx` mapping LSN to `(start_offset, total_len)` for O(log n) positioning and range scan.

## Out Of Scope

- Raft consensus, state machine, snapshot storage (snapshot is an upper-layer concern that calls `truncate_before` after snapshotting).
- Crash-recovery replay of arbitrary pre-existing on-disk logs beyond index rebuild (recovery rebuilds the index and truncates torn tails; full replay semantics are a follow-up).
- Concurrent multi-writer transactions (single-writer model first; staged concurrency in the plan).
- Compression / encryption of log payload.
- Network API; this slice is a library + local binary surface only.

## Architecture Constraints (hard)

These are non-negotiable project constraints; full rationale in `docs/architecture/wal-design.md`.

- **Direct I/O only (`O_DIRECT`)**: no `mmap`, no OS buffered I/O. Memory is managed by the database itself.
- **Block-aligned I/O**: 4 KiB block unit; reads/writes are whole-block at block-aligned offsets.
- **Self-managed write buffer**: pre-allocated aligned memory, buffer-swap + flush-thread architecture (double buffering), `buffer_size < segment_size`, a buffer never crosses a segment boundary.
- **On-disk record index**: record-level LSN index is not memory-resident; per-segment `.idx` files. Only the small segment route table is in memory.
- **LSN is a dense monotonic `u64`**: required by Raft.
- **`term` lives in the payload**: the WAL is term-agnostic.

## Main User Flows

1. Caller `append`s N records; each returns its LSN.
2. Caller `sync()`s to persist up to the latest appended LSN.
3. Caller `truncate_before(lsn)` to reclaim old log space (e.g. after a snapshot).
4. Caller `truncate_after(lsn)` to discard an uncommitted tail after a Raft leader change.
5. Caller `scan(start..end)` to iterate live records for replication replay.
6. Caller `get(lsn)` to look up a specific entry (e.g. `prevLogTerm`).

## Business Rules

- LSNs are monotonically increasing and unique within a log instance.
- `truncate_before(lsn)` removes records with `record.lsn < lsn`; the record at exactly `lsn` survives.
- `truncate_after(lsn)` removes records with `record.lsn > lsn`; the record at exactly `lsn` survives.
- Truncating an already-truncated range is idempotent.
- Records not yet synced may be truncated from the buffer without touching disk.
- Records already synced and then truncated require index rewrite (and, for `truncate_after`, physical `set_len`).
- `scan` yields records in ascending LSN order, half-open `[start, end)`, no duplicates, no gaps among live records.
- `scan` / `get` read only **synced** (on-disk) records. Records still in the active buffer (not yet `sync`ed) are not visible — the caller must `sync` first to read them. (Un-synced records may still be truncated from the buffer; see truncation rules.)

## Roles / Permissions

- Not applicable. Single-process library; no auth surface in this slice.

## Edge Cases

- `append` when the write-buffer pool is full (backpressure; policy in `wal-design.md` Open Questions).
- `sync()` with nothing buffered (no-op, returns current durable LSN).
- `truncate_before` / `truncate_after` with an LSN outside the live range (clamp or error? open).
- `truncate_before(lsn)` where `lsn` is still only in the buffer (disk untouched).
- `truncate_after(lsn)` below the last synced LSN (physical `set_len` on the segment).
- Crash between `append` and `sync` (record lost by design — not durable yet).
- Torn (partial) tail write on disk after a crash mid-`sync` (must be detectable via crc, never silently replayed).
- `scan` over a range fully outside the live `[min_live_lsn, max_live_lsn]` window (empty iter, no error).
- `scan` / `get` over a range where part is still un-synced in the buffer (the un-synced part is not returned; caller must `sync` first).

## Open Questions

(Also tracked in `docs/architecture/wal-design.md`.)

- `BLOCK_SIZE` configurable or fixed at 4096?
- Backpressure policy when the buffer pool is full: block-wait for flush, or return `WouldBlock`?
- `sync` return contract: block until `durable_lsn` catches up, or return a pending marker?
- `compact()` automatic threshold (dead-space ratio) or manual only for the first slice?
- Out-of-range truncate LSN: clamp to live range or return an error?

None of these change the six user-visible operations; they shape the plan.

## Acceptance Criteria

- [ ] `append` returns a strictly increasing LSN for each call and does not touch disk.
- [ ] `sync` durably persists buffered records to disk (observable via a reopened read path or `fsync` evidence).
- [ ] `truncate_before(lsn)` leaves exactly records with `lsn >= lsn` and removes `lsn < lsn`.
- [ ] `truncate_after(lsn)` leaves exactly records with `lsn <= lsn` and removes `lsn > lsn`.
- [ ] Truncation is idempotent across repeated calls with the same LSN.
- [ ] `get(lsn)` returns the record at exactly that LSN, or `None` if not live.
- [ ] `scan(a..b)` yields exactly the live records with `a <= lsn < b`, in ascending order.
- [ ] `scan` / `get` read synced on-disk records without duplicates or gaps; un-synced buffer records are excluded until `sync`.
- [ ] LSN index resolves an LSN to its location in O(log n) or better.
- [ ] A torn (partial) tail record is detectable and does not corrupt earlier valid records.
- [ ] Recovery rebuilds the index on open and truncates a torn tail to the last valid frame.
- [ ] Unit/integration tests cover all six operations, truncation idempotency, `scan` range semantics, `get` hit/miss, and the empty/out-of-range edge cases.
