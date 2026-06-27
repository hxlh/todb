# Plan: Migrate WAL Implementation from wal-demo

**Date**: 2026-06-27  
**Type**: Architecture change  
**Status**: Complete  
**Autonomy**: plan-first (protected area: storage engine internals)  
**Reviewer**: subagent

## Goal

Replace the current todb WAL implementation with the more advanced implementation from wal-demo. The wal-demo implementation is incomplete but provides a better foundation with O_DIRECT I/O, lock-free append, CLOCK buffer pool, and comprehensive design documentation.

## Background

**Current todb WAL** (`crates/storage/src/wal/`, ~1046 lines):
- Segment-based file storage (`wal/{rg_id}/{segment_id}.wal`)
- Async sync with background thread (100ms interval)
- Basic append/sync/recover interface
- Integrated with LsmStore and ReplicationGroup
- Simple serialization format

**wal-demo WAL** (`/home/hxlh/data/project/demo/wal-demo/src/wal/`, ~2190 lines):
- O_DIRECT I/O (no page cache, self-managed buffers)
- Lock-free multi-writer append via `fetch_add`
- Fixed-frame CLOCK buffer pool for reads
- Separate `.log` + `.idx` files per segment
- Frame format: `len|crc32|lsn|payload`
- Comprehensive crash recovery with triple validation
- `truncate_before` / `truncate_after` operations
- Dense LSN index on disk (not memory-resident)

**Key differences**:
1. I/O model: O_DIRECT vs buffered I/O
2. Concurrency: lock-free vs mutex-based
3. Index: disk-based .idx files vs in-memory only
4. Operations: 6 ops (append/sync/truncate_before/truncate_after/scan/get) vs 3 (append/sync/recover)
5. Design maturity: extensive tradeoffs doc vs minimal docs

## Scope

### In Scope
- Copy all WAL source files from wal-demo to todb
- Copy WAL design docs and tradeoffs analysis
- Copy WAL tests
- Preserve both implementations temporarily (new in separate module)
- Update documentation to reference new implementation
- **Do NOT** integrate with LsmStore/ReplicationGroup yet (follow-up task)

### Out of Scope
- Removing old WAL implementation (keep for reference)
- Integrating new WAL with LsmStore (requires separate integration plan)
- Modifying ReplicationGroup to use new WAL
- Porting old WAL's ReplicationGroup-specific logic
- Running integrated storage tests (will break until integration)

## Migration Strategy

**Phase 1: Copy source files**
- Copy `wal-demo/src/wal/` → `todb/crates/storage/src/wal_v2/`
- Keep old `src/wal/` untouched (rename to `wal_legacy/` for clarity)
- Update `Cargo.toml` to add new dependencies (`arc-swap`, `crc32fast`, `nix`)

**Phase 2: Copy documentation**
- Copy `wal-demo/docs/architecture/wal-design.md` → `todb/docs/architecture/`
- Copy `wal-demo/docs/analysis/2026-06-22-1500-wal-design-tradeoffs.md` → `todb/docs/analysis/`
- Copy `wal-demo/docs/requirements/2026-06-22-wal-core.md` → `todb/docs/requirements/`
- Update `todb/docs/design/storage-engine-design.md` to note two WAL versions

**Phase 3: Copy tests**
- Copy `wal-demo/tests/` → `todb/crates/storage/tests/wal_v2_tests.rs`
- Tests will compile and run independently of storage layer

**Phase 4: Update project docs**
- Add entry to backlog for WAL integration task
- Document the migration status in logs
- Update design docs to note transitional state

## File Operations

### Source Files to Copy
From `wal-demo/src/wal/`:
- `mod.rs` → `crates/storage/src/wal_v2/mod.rs`
- `lsn.rs` → `crates/storage/src/wal_v2/lsn.rs`
- `record.rs` → `crates/storage/src/wal_v2/record.rs`
- `frame.rs` → `crates/storage/src/wal_v2/frame.rs`
- `error.rs` → `crates/storage/src/wal_v2/error.rs`
- `config.rs` → `crates/storage/src/wal_v2/config.rs`
- `disk.rs` → `crates/storage/src/wal_v2/disk.rs`
- `buffer.rs` → `crates/storage/src/wal_v2/buffer.rs`
- `segment.rs` → `crates/storage/src/wal_v2/segment.rs`
- `index.rs` → `crates/storage/src/wal_v2/index.rs`
- `facade.rs` → `crates/storage/src/wal_v2/facade.rs`
- `aligned.rs` → `crates/storage/src/wal_v2/aligned.rs`

### Documentation to Copy
From `wal-demo/docs/`:
- `architecture/wal-design.md` → `docs/architecture/wal-design.md`
- `analysis/2026-06-22-1500-wal-design-tradeoffs.md` → `docs/analysis/wal-design-tradeoffs.md`
- `requirements/2026-06-22-wal-core.md` → `docs/requirements/wal-core-v2.md`

### Tests to Copy
From `wal-demo/tests/`:
- All test files → `crates/storage/tests/wal_v2/`

### Cargo Dependencies to Add
```toml
arc-swap = "1"
crc32fast = "1"
nix = { version = "0.29", features = ["fs"] }
```

## Risks

1. **Integration complexity**: New WAL has different interface than old one
   - Mitigation: Keep both implementations, integrate separately
   
2. **Dependencies conflicts**: New deps may conflict with existing
   - Mitigation: Check versions before adding

3. **Tests may fail**: wal-demo tests may assume different environment
   - Mitigation: Copy as-is, fix compilation errors, document failures

4. **O_DIRECT may not work**: Platform-specific (Linux/macOS differences)
   - Mitigation: Document platform requirements, conditional compilation if needed

5. **Incomplete implementation**: wal-demo has dormant get/scan
   - Mitigation: Document what's implemented, what's TODO

## Verification

After migration:
- [ ] `cargo check --all-targets` - Compiles with wal_v2 module
- [ ] `cargo test -p storage wal_v2` - New WAL tests pass (or documented failures)
- [ ] Old WAL tests still pass: `cargo test -p storage wal_legacy`
- [ ] Documentation copied and referenced correctly
- [ ] No accidental deletion of old implementation

## Dependencies

None (this is foundation work for future integration)

## Blocked By

None

## Blocks

- Integration of wal_v2 with LsmStore (future plan)
- Removal of old WAL implementation (future cleanup)

## Notes

- This is a **copy-only** migration - no integration yet
- Both WAL implementations will coexist during transition
- Old implementation renamed `wal_legacy` for clarity
- New implementation in `wal_v2` module
- Storage tests will temporarily have two WAL test suites
- Integration plan will be separate (complex, needs design decisions)

## Open Questions

1. Should we conditionally compile wal_v2 on Linux only? (O_DIRECT)
   - Decision: Start with Linux-only, document macOS TODO
   
2. How to handle the different error types? (old vs new WalError)
   - Decision: Keep separate for now, unify during integration

3. Should we update lib.rs to expose wal_v2 publicly?
   - Decision: Yes, expose as `pub mod wal_v2` for testing
