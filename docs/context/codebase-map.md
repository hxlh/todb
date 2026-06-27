# Codebase Map

## Purpose

This file gives AI agents a compact map of the live repository so they do not rediscover the structure by repeatedly searching imports and directories.

Keep it current enough to route common work. Do not turn it into a full architecture document.

## Entry Points

Replace placeholders after copying the template.

| Area         | Path     | Notes     | Last Verified  | Confidence |
| ------------ | -------- | --------- | -------------- | ---------- |
| Server entry | `server/src/main.rs` | PostgreSQL wire protocol server | 2026-06-27 | high |
| Storage engine | `crates/storage/src/` | LSM engine, WAL, memtable, SST | 2026-06-27 | high |
| SQL layer | `crates/sql/src/` | DataFusion integration | 2026-06-27 | high |
| Storage tests | `crates/storage/tests/` | WAL and engine integration tests | 2026-06-27 | high |
| Common utilities | `crates/common/src/` | Shared error types and utilities | 2026-06-27 | high |

## Common Change Routes

| Task Type           | Start Here | Then Check | Verification | Last Verified  | Confidence |
| ------------------- | ---------- | ---------- | ------------ | -------------- | ---------- |
| Add storage feature | `crates/storage/src/lsm_store.rs` | `crates/storage/src/engine.rs` | `cargo test -p storage` | 2026-06-27 | high |
| Modify WAL | `crates/storage/src/wal/` | `crates/storage/src/lsm_store.rs` | `cargo test wal_tests` | 2026-06-27 | high |
| Change SQL behavior | `crates/sql/src/` | `server/src/provider.rs` | `cargo test -p sql` | 2026-06-27 | high |
| Add iterator | `crates/storage/src/iterators/` | builder/reader code | `cargo test -p storage` | 2026-06-27 | high |
| Modify SST format | `crates/storage/src/builder/` | `crates/storage/src/block.rs` | `cargo test -p storage` | 2026-06-27 | high |

## Large Or Fragile Files

List files that agents should treat carefully because they are large, central, generated, or easy to edit incorrectly.

| Path     | Risk     | Preferred Approach |
| -------- | -------- | ------------------ |
| `crates/storage/src/lsm_store.rs` | Central write/scan path, complex state management | Read existing code carefully, test thoroughly |
| `crates/storage/src/wal/file_wal_store.rs` | Durability guarantees, low-level I/O | Verify fsync semantics, add recovery tests |
| `crates/storage/src/iterators/` | Complex merge logic, performance-critical | Preserve existing invariants, benchmark changes |

## Project-Specific Search Hints

- Use file patterns: `crates/storage/src/**/*.rs` for storage layer changes
- Use content anchors: `StorageEngine`, `LsmStore`, `WalStore`, `SstBuilder`, `MergeIter`
- Avoid editing generated files: none (no generated code currently)

## Update Rule

Update this file when a change creates a new major entry point, moves common code, adds a new test location, or repeatedly causes agents to rediscover the same path.

If a listed path is missing, placeholders remain, or live imports contradict this map, do not treat the map as authority. Verify with the live repo, then update the map or mark the row low confidence before implementation.

If `Last Verified` is old for the project's pace, predates major structural changes, or the task touches a listed route's boundary, verify the live repo before relying on the row. Low-confidence rows do not block low-risk work after live verification, but protected-area, migration, or cross-module work should update the row before implementation.
