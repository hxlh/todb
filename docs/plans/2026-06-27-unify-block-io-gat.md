# Plan: Unify Block I/O with GAT

**Date**: 2026-06-27  
**Type**: Architecture change (Storage engine internals)  
**Status**: Phase 1/3/4 complete; Phase 2 (WAL adapter) deferred per user request  
**Autonomy**: plan-first (protected area: storage internals)  
**Reviewer**: subagent (audit completed, all findings addressed)

## Implementation Outcome (2026-06-27)

**Implemented**: Phase 1 (trait GAT), Phase 3 (iterators, generic-over-storage), Phase 4 (LSM integration).
**Deferred**: Phase 2 (WAL `SegmentBlockReader` adapter) — user instructed not to change WAL yet.

### Actual approach (approach A — generic over storage type)

The plan's original "cache parsed entries" idea was abandoned, as was a stopgap `AsRef`/`Into<Bytes>` version (the latter reintroduced a per-block copy on the LSM hot path — rejected). The settled design makes the block-format iterator **generic over its storage type**, so it stores the guard directly with no copy:

- `BlockReader::Guard<'a>: Deref<Target = [u8]>` (no `AsRef`, no `Into<Bytes>`).
- `NormalBlockIter<B: Deref<Target = [u8]> = Bytes>` — `block: B`, stores the source directly.
- `IndexBlockIter`/`DataBlockIter` gain an associated `type Block: Deref<Target=[u8]>`; `from_block(block: Self::Block)` takes ownership with no conversion.
- `IndexTreeIter`/`SstIter` carry the bound `for<'a> R::Guard<'a>: Into<I::Block>` (resp. `D::Block`) and `.into()` the guard at the call site. For the LSM path `R::Guard<'a> = Bytes` and `I::Block = Bytes`, so `Bytes: Into<Bytes>` is the blanket **identity move — zero copy**.

**Why this fixes the regression**: the earlier `AsRef` version always did `Bytes::copy_from_slice`. Now `NormalBlockIter<Bytes>::from_block(Bytes)` stores the `Bytes` directly (a move) — same cost as before the GAT work.

**Borrowed-guard (WAL `PinGuard`) status**: the type-level plumbing (`NormalBlockIter<B>` with non-Bytes `B`) is proven by `test_generic_storage_vec` (iterates a `Vec<u8>`-backed block). But holding a real `PinGuard<'r>` in the iterator is still blocked by the self-referential lifetime (`read_block(&self) -> Guard<'_>` ties the guard to `&self`; storing it back is self-reference). That lifetime-on-struct redesign is deferred to WAL Phase 2.

### Verification

- `cargo build --workspace` — passes.
- `cargo test --workspace` — all pass (156 storage lib tests incl. `test_generic_storage_vec`, plus all integration tests).
- `block::tests::test_arc_block_reader` verifies `Arc<R>` implements `BlockReader` under GAT.
- `block_iter::tests::test_generic_storage_vec` verifies `NormalBlockIter<B>` works with non-Bytes storage (de-risks the future borrowed-guard path).
- `cargo clippy -p storage` — no new warnings from the GAT/generic changes (pre-existing dead-code warnings in disabled modules only).

### Files changed

| File | Change |
|------|--------|
| `crates/storage/src/block.rs` | `BlockReader` GAT `Guard<'a>: Deref<Target=[u8]>`; `Arc<R>` forwards; readers return `Bytes`; `test_arc_block_reader` |
| `crates/storage/src/iterators/storage_iter.rs` | `IndexBlockIter`/`DataBlockIter` gain associated `type Block`; `from_block(block: Self::Block)` |
| `crates/storage/src/iterators/block_iter.rs` | `NormalBlockIter<B: Deref<Target=[u8]> = Bytes>` generic; `new(block: B)` stores directly; `test_generic_storage_vec` |
| `crates/storage/src/iterators/data_entry_decode_iter.rs` | `from_block(block: I::Block)`, `type Block = I::Block` |
| `crates/storage/src/iterators/index_tree_iter.rs` | bound `for<'a> R::Guard<'a>: Into<I::Block>`; `.into()` at `from_block` call sites |
| `crates/storage/src/iterators/sst_iter.rs` | bounds `Into<I::Block>` and `Into<D::Block>`; `.into()` |
| `crates/storage/src/lsm_iter.rs` | Removed unused imports |




## Goal

Refactor `BlockReader` trait to use Generic Associated Types (GAT), enabling both zero-copy pinned reads (WAL with cache) and owned reads (LSM with page cache) through a unified abstraction.

## Background

**Current situation**:
- `BlockReader` returns `Bytes` (owned data)
- WAL's `DiskManager` returns `PinGuard` (borrowed, pinned cache frame)
- Two incompatible interfaces, cannot share code

**Root cause**: The original `BlockReader` trait was designed before cache support.

**Goal**: Unified abstraction where upper layers only care about `Deref<Target = [u8]>`, not whether data is pinned or owned.

## Scope

### In Scope

**Phase 1: Refactor BlockReader trait**
- Change `BlockReader::read_block()` to return GAT `Guard<'_>`
- Update `FileBlockReader` to return `Bytes` as `Guard`
- Update `InMemoryBlockReader` to return `Bytes` as `Guard`

**Phase 2: Adapt WAL DiskManager**
- Implement `BlockReader` for WAL `DiskManager`
- Return `PinGuard` as `Guard`
- Add position-based read (convert `Position` → `(seg_id, fd, block_idx)`)

**Phase 3: Update SST readers/iterators**
- Update `SstReader` to use GAT guard
- Update `SstIter` to not store guards (only borrow temporarily)
- Update all iterator implementations

**Phase 4: Update LSM usage**
- Update `LsmStore::build_merged_iter()` to use new API
- Update any other LSM code using `BlockReader`

### Out of Scope

- **Not changing SST file format** (only I/O abstraction)
- **Not implementing LSM O_DIRECT yet** (WAL uses it, LSM still uses page cache)
- **Not changing write path** (only read path)

## Design

### New BlockReader Trait (with GAT)

```rust
pub trait BlockReader {
    /// Guard type that derefs to block bytes.
    /// - For cached reads: `PinGuard<'a>` (zero-copy, must unpin on drop)
    /// - For uncached reads: `Bytes` (owned, no lifetime constraint)
    type Guard<'a>: Deref<Target = [u8]> where Self: 'a;
    
    fn read_block(&self, position: &Position) -> StorageResult<Self::Guard<'_>>;
    fn block_size(&self) -> usize;
}
```

### Implementation: FileBlockReader (LSM, no cache)

```rust
impl BlockReader for FileBlockReader {
    type Guard<'a> = Bytes;  // Owned
    
    fn read_block(&self, position: &Position) -> StorageResult<Bytes> {
        use std::os::unix::fs::FileExt;
        let mut buf = vec![0u8; self.block_size];
        self.file.read_exact_at(&mut buf, position.offset)?;
        Ok(Bytes::from(buf))
    }
    
    fn block_size(&self) -> usize {
        self.block_size
    }
}
```

### Implementation: WAL DiskManager (with CLOCK cache)

```rust
// In crates/storage/src/wal/disk.rs

impl crate::block::BlockReader for DiskManager {
    type Guard<'a> = PinGuard<'a>;  // Borrowed, pinned
    
    fn read_block(&self, position: &crate::block::Position) 
        -> Result<PinGuard<'_>, crate::errors::StorageError> 
    {
        // Need to map Position to (seg_id, fd, block_idx)
        // This requires context - see "Context Problem" below
        todo!("needs segment context")
    }
    
    fn block_size(&self) -> usize {
        self.block_size
    }
}
```

### Context Problem: Position → (seg_id, fd)

**Problem**: `BlockReader::read_block(position)` only has byte offset, but WAL `DiskManager::read_block(seg_id, fd, block_idx)` needs segment context.

**Solution**: Wrapper struct that carries segment context

```rust
// In crates/storage/src/wal/index.rs or disk.rs

/// BlockReader adapter for WAL DiskManager with segment context.
pub struct SegmentBlockReader<'a> {
    dm: &'a DiskManager,
    seg_id: u32,
    fd: RawFd,
}

impl<'a> SegmentBlockReader<'a> {
    pub fn new(dm: &'a DiskManager, seg_id: u32, fd: RawFd) -> Self {
        Self { dm, seg_id, fd }
    }
}

impl BlockReader for SegmentBlockReader<'_> {
    type Guard<'a> = PinGuard<'a> where Self: 'a;
    
    fn read_block(&self, position: &Position) -> StorageResult<PinGuard<'_>> {
        let block_idx = position.offset / (self.dm.block_size() as u64);
        self.dm.read_block(self.seg_id, self.fd, block_idx)
            .map_err(|e| StorageError::Io(e.into()))
    }
    
    fn block_size(&self) -> usize {
        self.dm.block_size()
    }
}
```

### SST Iterator Changes

**Key insight**: Iterator does **not** store the guard, only borrows it temporarily in `next()`.

```rust
// Before
pub struct SstIter<R: BlockReader> {
    reader: R,
    current_block: Option<Bytes>,  // stored
    // ...
}

// After
pub struct SstIter<R: BlockReader> {
    reader: R,
    current_block_pos: Option<Position>,  // only store position
    // ...
}

impl<R: BlockReader> ForwardIter for SstIter<R> {
    fn next(&mut self) -> StorageResult<()> {
        // Temporarily borrow guard
        if let Some(pos) = self.current_block_pos {
            let guard = self.reader.read_block(&pos)?;  // PinGuard or Bytes
            // Parse entries from &*guard
            // Guard dropped here, unpin happens automatically
        }
        Ok(())
    }
    
    fn key(&self) -> Option<Self::Key<'_>> {
        // Problem: need to access current block data
        // Solution: re-read or cache parsed entries
    }
}
```

**Challenge**: `key()` and `value()` need to return references, but we can't store the guard.

**Solution 1**: Re-read block on every `key()`/`value()` call
- ❌ Too expensive

**Solution 2**: Cache parsed entries, not raw block
```rust
pub struct SstIter<R: BlockReader> {
    reader: R,
    current_entries: Vec<(Bytes, Bytes)>,  // parsed k-v pairs
    current_idx: usize,
    // ...
}
```
- ✅ Practical, entries are small
- ✅ No lifetime issues

## Implementation Strategy

### Phase 1: Refactor BlockReader trait

**1.1 Update trait definition in `block.rs`**

```rust
pub trait BlockReader {
    type Guard<'a>: Deref<Target = [u8]> where Self: 'a;
    
    fn read_block(&self, position: &Position) -> StorageResult<Self::Guard<'_>>;
    fn block_size(&self) -> usize;
}
```

**1.2 Update `FileBlockReader`**

```rust
impl BlockReader for FileBlockReader {
    type Guard<'a> = Bytes;
    
    fn read_block(&self, position: &Position) -> StorageResult<Bytes> {
        // existing implementation, return owned Bytes
    }
    
    fn block_size(&self) -> usize {
        self.block_size
    }
}
```

**1.3 Update `InMemoryBlockWriter`**

```rust
impl BlockReader for InMemoryBlockWriter {
    type Guard<'a> = Bytes;
    
    fn read_block(&self, position: &Position) -> StorageResult<Bytes> {
        // slice and clone to Bytes
    }
    
    fn block_size(&self) -> usize {
        self.block_size
    }
}
```

### Phase 2: Adapt WAL DiskManager

**2.1 Add `SegmentBlockReader` in `wal/disk.rs`**

```rust
pub struct SegmentBlockReader<'a> {
    dm: &'a DiskManager,
    seg_id: u32,
    fd: RawFd,
}

impl<'a> BlockReader for SegmentBlockReader<'a> {
    type Guard<'b> = PinGuard<'b> where Self: 'b;
    
    fn read_block(&self, position: &Position) -> StorageResult<PinGuard<'_>> {
        let block_idx = position.offset / (self.dm.block_size() as u64);
        self.dm.read_block(self.seg_id, self.fd, block_idx)
            .map_err(|e| StorageError::Io(e.into()))
    }
    
    fn block_size(&self) -> usize {
        self.dm.block_size()
    }
}
```

**2.2 Export from `wal/mod.rs`**

```rust
pub use disk::{DiskManager, PinGuard, SegmentBlockReader};
```

### Phase 3: Update SST readers/iterators

**3.0 Verify Arc<R> wrapper compiles**

```rust
// In tests
#[test]
fn test_arc_block_reader() {
    let reader = Arc::new(FileBlockReader::open(...));
    let guard = reader.read_block(&Position { offset: 0 }).unwrap();
    assert_eq!(guard.len(), 4096);
}
```

**3.2 Update `SstIter` to cache parsed entries**

```rust
pub struct SstIter<R: BlockReader> {
    reader: R,
    // Before: current_block: Option<Bytes>
    // After: cache parsed entries
    current_entries: Vec<(Bytes, Bytes)>,
    current_idx: usize,
    current_block_pos: Option<Position>,
    // ...
}

impl<R: BlockReader> SstIter<R> {
    fn load_block(&mut self, pos: Position) -> StorageResult<()> {
        let guard = self.reader.read_block(&pos)?;  // Borrow temporarily
        // Parse all entries from &*guard into self.current_entries
        self.current_entries = parse_entries(&*guard)?;
        self.current_idx = 0;
        self.current_block_pos = Some(pos);
        Ok(())
        // guard dropped here, PinGuard unpins automatically
    }
}

impl<R: BlockReader> ForwardIter for SstIter<R> {
    fn key(&self) -> Option<Self::Key<'_>> {
        self.current_entries.get(self.current_idx).map(|(k, _)| k)
    }
    
    fn value(&self) -> Option<Self::Value<'_>> {
        self.current_entries.get(self.current_idx).map(|(_, v)| v)
    }
    
    fn next(&mut self) -> StorageResult<()> {
        self.current_idx += 1;
        if self.current_idx >= self.current_entries.len() {
            // Load next block
        }
        Ok(())
    }
}
```

**3.3 Update `IndexTreeIter` to cache parsed entries**

```rust
pub struct IndexTreeIter<R: BlockReader> {
    reader: R,
    // Cache parsed index entries (separator keys + child positions)
    current_entries: Vec<(Bytes, Position)>,
    current_idx: usize,
    // ...
}

impl<R: BlockReader> IndexTreeIter<R> {
    fn load_index_block(&mut self, pos: Position) -> StorageResult<()> {
        let guard = self.reader.read_block(&pos)?;
        self.current_entries = parse_index_entries(&*guard)?;
        self.current_idx = 0;
        Ok(())
        // guard dropped, unpin happens
    }
}
```

**3.4 Update `SstReader` (if needed)**

Most likely no changes needed, as it uses `SstIter` internally.

### Phase 4: Update LSM usage

**4.1 Update `LsmStore::build_merged_iter()`**

No changes needed if `SstIter::new()` signature unchanged.

**4.2 Verify merged iterators compile**

```rust
// In lsm_store.rs - verify TwoMergeIter, MergeIter work with new SstIter
let merged = self.build_merged_iter(&state)?;  // Should compile
```

**4.3 Update `build_sst_from_memtable()`**

No changes needed (write path unchanged).

## File Changes

| File | Change Type | Description |
|------|-------------|-------------|
| `crates/storage/src/block.rs` | Modify | Add GAT to `BlockReader` trait |
| `crates/storage/src/wal/disk.rs` | Modify | Add `SegmentBlockReader` |
| `crates/storage/src/wal/mod.rs` | Modify | Export `SegmentBlockReader` |
| `crates/storage/src/iterators/sst_iter.rs` | Modify | Cache parsed entries, use GAT |
| `crates/storage/src/iterators/index_tree_iter.rs` | Modify | Cache parsed index entries, use GAT |
| `crates/storage/src/lsm_store.rs` | Verify | Check merged iterator compilation |
| `crates/storage/src/builder/sst.rs` | Verify | Check if changes needed |
| `crates/storage/src/disk_manager.rs` | Verify | Check if changes needed |

**Estimated impact**: ~8 files modified/verified, ~300-400 lines changed

## Risks

1. **GAT lifetime complexity**: Compiler errors may be hard to debug
   - **Mitigation**: Start with simple cases, add complexity incrementally

2. **Performance regression**: Caching parsed entries vs raw block
   - **Mitigation**: Benchmark before/after, entries are small (~20-50 bytes each)

3. **Iterator self-reference**: If iterator needs to store guard
   - **Mitigation**: Cache parsed entries, not raw guard (solution 2 above)

4. **Breaking existing tests**: Many tests use `BlockReader`
   - **Mitigation**: Fix incrementally, start with unit tests

## Verification

### Unit Tests

```bash
cargo test -p storage block::tests
cargo test -p storage iterators::tests
cargo test -p storage wal::tests
```

**New tests**:
- `test_gat_file_block_reader` - FileBlockReader returns Bytes
- `test_gat_wal_block_reader` - SegmentBlockReader returns PinGuard
- `test_sst_iter_with_gat` - SstIter works with both reader types
- `test_index_tree_iter_with_gat` - IndexTreeIter works with GAT
- `test_arc_block_reader` - Arc<R> implements BlockReader

### Integration Tests

```bash
cargo test -p storage lsm_store
cargo test -p storage wal_v2
```

**Scenarios**:
- Merged iterators (`TwoMergeIter`, `MergeIter`) work with new `SstIter`
- Reverse iteration works with cached entries
- LSM scan performance (see benchmark below)

### Performance Benchmark

**Before refactor** (baseline):
```bash
cargo bench --bench lsm_scan
# Record: throughput (MB/s), latency (μs)
```

**After refactor** (verification):
```bash
cargo bench --bench lsm_scan
# Compare: accept ≤10% regression (upfront parsing overhead)
```

### Smoke Test

```bash
cargo run --bin server
# Insert data, query, verify correct results
```

## Closure Criteria

1. ✅ All existing tests pass after refactor
2. ✅ New GAT tests pass (5 new tests)
3. ✅ `BlockReader` trait uses GAT
4. ✅ `SegmentBlockReader` implements `BlockReader` for WAL
5. ✅ SST Iterator works with both `Bytes` and `PinGuard`
6. ✅ `IndexTreeIter` works with GAT
7. ✅ `Arc<R>` wrapper compiles with GAT
8. ✅ Merged iterators compile and pass tests
9. ✅ Performance benchmark shows ≤10% regression (upfront parsing overhead)

## Dependencies

**Requires**:
- Rust 1.65+ (GAT stabilized)

**Blocks**:
- WAL index implementation (needs this refactor first)

## Notes

- This is a **prerequisite** for WAL index read path
- GAT allows zero-copy reads with cache while maintaining type safety
- After this, both WAL and LSM can use the same SST infrastructure
- Future: LSM can also adopt O_DIRECT + CLOCK cache with minimal changes

## Open Questions

None at plan time. GAT approach confirmed during discussion.
