# Storage Engine Design

This document describes the current LSM-based storage engine design for todb.

For detailed original design, see `old_docs/storage_design.md`.

## Architecture Overview

```
StorageLayer (global singleton)
  ├─ MetaManager (table → shard routing)
  ├─ LogService (per-RG WAL management)
  └─ engines: Map<Engine, Arc<dyn StorageEngine>>
       └─ LsmEngine
            └─ shards: Map<shard_id, LsmStore>
```

## Key Components

### StorageLayer
- Global entry point for storage operations
- Routes table-level operations to appropriate shards
- Manages ResourceContext (SST ID allocation, memory tracking)

### MetaManager
- Maintains table metadata (schema, shard mapping)
- Maps tables to shards, shards to replication groups
- Currently: one shard per table (DEFAULT_SHARD)

### LogService
- Per-replication-group WAL management
- Creates and retrieves WalStore instances
- Configured via RgOption (buffer size, sync interval, segment size)

### LsmEngine
- Storage engine implementation (implements StorageEngine trait)
- Manages multiple LsmStore instances (one per shard)
- Coordinates flush scheduling and disk management

### LsmStore
- Per-shard LSM-tree instance
- State: active memtable + immutable memtables + SST levels
- Write path: WAL append → memtable insert
- Scan path: merge iterator over memtables + SSTs

## Write Path

```
write(batch)
  → wal.append(shard_id, &batch)  // Returns LSN
  → memtable.apply(batch)
  → if memtable size ≥ limit:
       switch_memtable()
       → signal flush worker
```

## Scan Path

```
scan(range)
  → clone Arc<LsmState>  // Snapshot
  → TwoMergeIter:
       mem: MergeIter([active, imm[0], imm[1], ...])
       sst: MergeIter([sst_iter_0, sst_iter_1, ...])
  → mem takes precedence on key conflicts
```

## Flush Worker

Background task per shard:
1. Wait for flush signal
2. Take oldest immutable memtable
3. Build SST (DataBlocks + IndexBlocks)
4. Write to disk
5. Update state: remove from imm, add to L0

## SST Format

See `old_docs/storage_design.md` § "SST File Format" for detailed layout.

Key points:
- Fixed block_size slots (with padding)
- B+-tree indexed blocks
- 12-byte footer: [root_offset: u64 BE][tree_height: u32 BE]

## WAL Format

See `old_docs/superpowers/specs/2026-06-20-wal-replication-group-design.md` for detailed design.

Key points:
- Per-replication-group append-only log
- Segmented files: `wal/{rg_id}/{segment_id}.wal`
- Entry framing: `| len:u32 | shard_id:u64 | lsn:u64 | payload_type:u8 | payload |`
- Async sync: background thread flushes every 100ms or on buffer full

## Replication Groups

- RgId = u64, DEFAULT_RG = 0
- ReplicaGroup { rg_id, rf, members }
- Currently: rf=1 (single-node), no Raft integration
- One RG → one WalStore (shared across shards in that RG)

## Future Work

- Multi-level compaction (L0 → L1, L1 → L2, ...)
- MVCC: key encoding with commit_ts, snapshot isolation
- Raft integration: multi-replica consensus
- Metadata persistence: recover ReplicationGroup and shard info on restart
- Truncate/compact WAL: reclaim space after checkpoint
