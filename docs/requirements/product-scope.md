# Product Scope

## Current Milestone: Single-Node Storage Engine with WAL

**Target**: Production-ready single-node LSM storage engine with durable WAL and PostgreSQL wire protocol.

### Completed Features

✅ LSM-tree storage engine
  - Memtable (crossbeam skiplist)
  - SST files with B+-tree indexing
  - Multi-level merge iterators
  - Flush worker (memtable → L0 SST)

✅ Write-Ahead Log
  - Per-replication-group append-only log
  - Segmented files with async sync
  - WAL recovery interface (basic)

✅ ReplicationGroup abstraction
  - Single-node (rf=1)
  - DEFAULT_RG auto-created
  - Prepared for Raft integration

✅ SQL Integration
  - DataFusion query engine
  - Custom TableProvider
  - PostgreSQL wire protocol server

✅ SST enhancements
  - Fixed block size with padding
  - Footer with first/last key metadata

### In-Scope for Current Milestone

- Single-node operation only (no distributed consensus)
- Basic CRUD operations (put, delete, scan)
- Range scans with iterator-based streaming
- Memory-bound durability (async WAL sync every 100ms)
- Single table operations (no joins)

### Explicitly Out of Scope

- Multi-replica replication (Raft)
- Multi-level compaction (L1+)
- MVCC / snapshot isolation
- Distributed transactions
- Cross-shard queries
- Full restart recovery (partial: WAL recovery ready, metadata persistence pending)

## Next Milestone: Compaction and Full Recovery

See `docs/backlog/README.md` for prioritized items.

Key features:
- L0 → L1 compaction (size-tiered or leveled)
- Metadata persistence (RG + shard info)
- Full restart recovery (load metadata + replay WAL)
- WAL truncation after checkpoint

## Future Milestones

### M3: Raft Replication
- Multi-replica consensus (rf=3)
- Leader election and log replication
- Dynamic membership changes

### M4: MVCC and Transactions
- Multi-version concurrency control
- Snapshot isolation
- 2PC or Percolator-style distributed transactions

### M5: Query Optimization
- Partition pruning
- Predicate pushdown to storage
- Secondary indexes
