# WAL Index Technical Design

## Purpose

Defines the WAL index implementation that maps LSN to log record location (`offset`, `len`). This design completes the read path (`get`, `scan`) for the WAL v2 implementation.

Related documents:
- Parent design: `docs/architecture/wal-design.md` (overall WAL architecture)
- Requirements: `docs/requirements/wal-core-v2.md` (acceptance criteria)

This is a living architecture contract. Update it in the same change that changes any decision below.

## Problem Statement

The current WAL v2 implementation (migrated from wal-demo) has a complete write path but no read path:

**Implemented (write path)**:
- Lock-free append via `fetch_add`
- Write buffer → `.log` file flush
- `IdxTail` accumulator (204 entries → 4KB block)
- Double-write header protection

**Missing (read path)**:
- ✗ `get(lsn) → Option<(offset, len)>` - point lookup
- ✗ `scan(lsn_range) → Iterator` - range scan
- ✗ Recovery index scan during startup
- ✗ Index caching/buffer management

## Design Decision

Use **LSM SST infrastructure** for the index data, with **separate `.meta` files** for mutable metadata.

### File Structure

```
seg_00001.meta  - Metadata (8KB = 2×4KB double-write), mutable via truncate
seg_00001.idx   - Index data (SST format), immutable after seal
seg_00001.log   - Log data (frame stream)
```

### Why SST for Index Data?

1. **Zero-cost reuse**: `MemTable`, `SstBuilder`, `SstIter` are production-ready
2. **Mature infrastructure**: block cache, iterator merge, compression
3. **Natural fit**: LSN is monotonic → SST's sorted key-value model
4. **Future extensibility**: bloom filters, compression, cache integration

### Why Separate `.meta` Files?

**The core insight**: Truncate needs mutable metadata, but SST wants immutable data.

- **Truncate operations** (`truncate_before`, `truncate_after`) only update `[min_live_lsn, max_live_lsn]` range
- Rewriting entire SST on every truncate is prohibitively expensive
- Solution: metadata in mutable `.meta`, data in immutable `.idx` SST

## On-Disk Format

### `.meta` File (8 KB)

Stores the `IdxHeader` structure (36 bytes) in a double-write pattern:

```
Block 0 (4KB): Header copy A + padding
Block 1 (4KB): Header copy B + padding
```

**Header structure** (already exists in `index.rs`):
```rust
pub struct IdxHeader {
    pub magic: [u8; 4],           // *b"WIDX"
    pub version: u32,             // 1
    pub seg_id: u32,              // segment ID
    pub min_live_lsn: u64,        // 👈 mutable via truncate_before
    pub max_live_lsn: u64,        // 👈 mutable via truncate_after
    pub entry_count: u32,         // total entries in .idx
    pub header_crc: u32,          // crc32 over first 32 bytes
}
```

**Truncate cost**: 8 KB write (two 4KB blocks), fully crash-safe via double-write.

### `.idx` File (SST Format)

Standard LSM SST file containing the index entries:

- **Key**: LSN encoded as 8-byte big-endian `Bytes` (ensures lexicographic order = numeric order)
- **Value**: 12-byte `(offset: u64, len: u32)` encoded as little-endian
- **Total**: 20 bytes per entry (same as current `IdxEntry` serialized size)

**SST overhead**: ~10% for block headers, footer, restart points (acceptable trade-off for mature infrastructure)

### Comparison with Current `.idx` Format

| Aspect | Current (unused) | New Design |
|--------|------------------|------------|
| Entry storage | `.idx` blocks | `.idx` SST |
| Metadata | `.idx` header | `.meta` separate |
| Entry size | 20 B | 20 B (8 + 12) |
| Overhead | ~2% | ~10% (SST) |
| Read infra | TODO | MemTable+SST |
| Truncate cost | 8KB header | 8KB header |
| Mutability | Single file | Split meta/data |

## In-Memory Representation

### Per-Segment Index

```rust
pub struct SegmentIndex {
    /// Mutable metadata (from .meta file)
    header: IdxHeader,
    header_fd: File,
    
    /// Immutable index data (SST)
    sst: Arc<SstReader>,
}
```

### Segment Index Builder (During Flush)

```rust
pub struct SegmentIndexBuilder {
    /// Accumulate entries in memory before seal
    mem: Arc<MemTable<Bytes, Bytes>>,
    seg_id: u32,
    min_lsn: u64,
    max_lsn: u64,
}
```

### Global WAL Index

```rust
pub struct WalIndex {
    /// All segments, ordered by seg_id
    segments: Vec<SegmentIndex>,
}
```

## Operation Paths

### 1. Append Flow (Write Path)

```
append(payload) → lsn
  ├─ lock-free fetch_add into write buffer
  └─ record (lsn, offset_in_buffer, len) to builder
     └─ builder.mem.put(lsn_bytes, encode(offset, len))
```

### 2. Flush Flow (Seal Segment)

```
flush_buffer() when segment reaches segment_size
  ├─ flush buffer → .log file (already implemented)
  └─ seal index:
      ├─ build .idx SST from builder.mem via SstBuilder
      ├─ write .meta header with [min_live_lsn, max_live_lsn]
      └─ open SegmentIndex { header, sst }
```

### 3. Point Lookup (Read Path)

```
get(lsn) → Option<(offset, len)>
  ├─ binary search segments by lsn range
  ├─ found segment:
  │   ├─ range filter: if lsn ∉ [min_live_lsn, max_live_lsn] → None
  │   └─ sst.get(lsn_bytes) → decode(offset, len)
  └─ not found → None
```

**Cost**: O(log N) segment search + O(log B) SST block seek + 1 block read

### 4. Range Scan (Read Path)

```
scan(start_lsn..end_lsn) → Iterator
  ├─ collect overlapping segments
  ├─ for each segment:
  │   ├─ range filter: intersect [start, end) with [min_live_lsn, max_live_lsn]
  │   └─ sst.scan(start_bytes..end_bytes)
  └─ merge iterators (segments already non-overlapping, sorted)
```

**Cost**: O(K) segments touched + O(M) blocks read (M = entries in range / 204)

### 5. Truncate Before

```
truncate_before(lsn)
  ├─ for each segment:
  │   └─ update header.min_live_lsn = max(header.min_live_lsn, lsn)
  │       └─ rewrite .meta (8KB double-write)
  └─ delete dead segments where min_live_lsn > max_live_lsn
      └─ remove .meta + .idx + .log
```

**Cost per segment**: 8 KB write
**Deletion**: When entire segment is dead

### 6. Truncate After

```
truncate_after(lsn)
  ├─ for each segment:
  │   └─ update header.max_live_lsn = min(header.max_live_lsn, lsn)
  │       └─ rewrite .meta (8KB double-write)
  └─ delete dead segments where min_live_lsn > max_live_lsn
      └─ remove .meta + .idx + .log
```

**Cost per segment**: 8 KB write

### 7. Recovery Flow

```
open_wal(dir)
  ├─ discover all .meta files
  ├─ for each seg_id:
  │   ├─ read .meta with double-write recovery
  │   ├─ open .idx SST
  │   └─ register SegmentIndex
  └─ scan last segment .log for unflushed tail (existing recovery logic)
```

## Key Encoding

**Critical**: Key must preserve numeric order in lexicographic order.

```rust
fn lsn_to_key(lsn: u64) -> Bytes {
    Bytes::from(lsn.to_be_bytes().to_vec())  // 👈 big-endian
}

fn key_to_lsn(key: &[u8]) -> u64 {
    u64::from_be_bytes(key[0..8].try_into().unwrap())
}
```

**Why big-endian**: Ensures `lsn1 < lsn2 ⇒ key1 < key2` lexicographically.

## Value Encoding

```rust
fn encode_offset_len(offset: u64, len: u32) -> Bytes {
    let mut buf = Vec::with_capacity(12);
    buf.extend_from_slice(&offset.to_le_bytes());  // little-endian (host order)
    buf.extend_from_slice(&len.to_le_bytes());
    Bytes::from(buf)
}

fn decode_offset_len(bytes: &[u8]) -> (u64, u32) {
    let offset = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    (offset, len)
}
```

## Segment Lifecycle

```
1. Create segment
   ├─ allocate seg_id
   ├─ create .log file (preallocated)
   └─ create SegmentIndexBuilder with empty MemTable

2. Active append
   ├─ append() writes to buffer
   └─ builder.mem.put(lsn, (offset, len))

3. Seal segment (when .log reaches segment_size)
   ├─ flush remaining buffer → .log
   ├─ build .idx SST from builder.mem
   ├─ write .meta header
   └─ SegmentIndexBuilder → SegmentIndex

4. Query (get/scan)
   ├─ read .meta for range filter
   └─ query .idx SST

5. Truncate
   └─ update .meta only (8KB write)

6. Delete segment (when dead)
   └─ remove .meta + .idx + .log
```

## Error Handling

### Crash Recovery

1. **`.meta` corruption**: Double-write provides redundancy
   - Try copy A, if CRC fails try copy B
   - Both fail → `Err(HeaderCorrupt)`

2. **`.idx` corruption**: SST has built-in CRC per block
   - Block read CRC mismatch → `Err(WalError::CrcMismatch)`

3. **Partial write during seal**: `.meta` not present → segment not sealed yet
   - Recovery treats as incomplete, rebuilds from .log

### Truncate Atomicity

- **`.meta` double-write**: Atomic update via fsync after both copies written
- **Segment deletion**: OS-level file removal is atomic per-file
- **Multi-segment truncate**: Non-atomic across segments, but idempotent

## Performance Characteristics

### Space

- **Index entry**: 20 bytes (8 key + 12 value)
- **SST overhead**: ~10% (block headers, footer, restart points)
- **Metadata**: 8 KB per segment (negligible)

### Time

| Operation | Complexity | Notes |
|-----------|-----------|-------|
| `append` | O(1) | MemTable insert |
| `seal` | O(N log N) | MemTable → SST (N = entries) |
| `get` | O(log S + log B) | S = segments, B = blocks |
| `scan` | O(K + M) | K = segments, M = blocks in range |
| `truncate_before` | O(S) × 8KB | S = segments |
| `truncate_after` | O(S) × 8KB | S = segments |

### Memory

- **Build phase**: MemTable holds all unflushed entries (~204 × 20 B = 4KB per block)
- **Query phase**: SST block cache (configured, shared with LSM)

## Integration Points

### With Existing WAL Components

- `segment.rs`: Add index builder, seal logic
- `index.rs`: Extend with SST read/write, keep `IdxHeader` structure
- `facade.rs`: Wire `get`/`scan` to index queries

### With LSM Components

- Reuse: `MemTable`, `SstBuilder`, `SstIter`
- **Block I/O unification**: Extend `BlockReader` trait with GAT to support both:
  - WAL: O_DIRECT with `PinGuard` (zero-copy, pinned cache frame)
  - LSM: Standard I/O with `Bytes` (owned, page cache)
- **Cache integration**: WAL index uses WAL's `DiskManager` with CLOCK cache
- **Important**: WAL index and LSM data share the same `BlockReader` abstraction but use different implementations

## Non-Goals

- **Not replacing `.log` format**: Index only, log format unchanged
- **Not merging with LSM data**: Separate storage, different lifecycle
- **Not implementing compaction**: Index segments map 1:1 to log segments, deleted together
- **Not implementing bloom filters yet**: Can add later if point lookups dominate

## Open Questions

None at design time. All confirmed during discussion:

1. ✅ Use SST for index data (not custom format)
2. ✅ Separate `.meta` for mutable metadata
3. ✅ Truncate only updates metadata (no SST rewrite)
4. ✅ Big-endian LSN keys for correct ordering

## Future Extensions

1. **Bloom filters**: If `get(lsn)` misses dominate (unlikely — LSN usually in range)
2. **Compression**: Index is highly compressible (monotonic LSNs, similar offsets)
3. **Global index**: Cross-segment index for faster lookups (adds complexity)
4. **Index compaction**: Merge multiple segment indices (not needed — segments already coarse)

## References

- `docs/architecture/wal-design.md` - Overall WAL architecture
- `docs/requirements/wal-core-v2.md` - Functional requirements
- `crates/storage/src/wal/index.rs` - Current index structures
- `crates/storage/src/builder.rs` - SST builder implementation
- `crates/storage/src/memtable.rs` - MemTable implementation
