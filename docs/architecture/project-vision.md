# Project Vision

## What is todb?

todb is a distributed database built from first principles in Rust, focusing on:
- LSM-based storage engine with custom implementation
- PostgreSQL wire protocol compatibility
- DataFusion SQL query engine integration
- Raft-based replication (planned)
- Strong durability guarantees through WAL

## Long-term Goals

### Storage Engine
- Multi-level LSM compaction (currently L0 only)
- MVCC snapshot isolation
- Efficient range scans and point lookups
- Configurable storage policies per table/shard

### Distribution
- Raft consensus for replication groups
- Multi-replica fault tolerance
- Dynamic membership changes
- Load-balanced read replicas

### Transaction Support
- Distributed ACID transactions
- 2PC or Percolator-style transaction protocol
- Global timestamp ordering (TSO)

### Query Optimization
- Push-down filters and projections to storage layer
- Partition pruning based on key ranges
- Custom physical plan operators for storage integration

## Non-Goals

- Full PostgreSQL compatibility (wire protocol only, not all features)
- Distributed SQL joins across shards (initial focus: single-table operations)
- Embedded mode (server-only architecture)
- GUI or web console (CLI and programmatic access only)

## Design Philosophy

1. **Simplicity first**: Start with minimal working implementation, add complexity only when justified
2. **Rust safety**: Leverage Rust's type system and ownership for correctness
3. **Explicit over implicit**: Clear separation of concerns between layers
4. **Testability**: Design for unit and integration testing from the start
5. **Reference existing systems**: Learn from OceanBase, TiKV, and CockroachDB designs

## Current Status

**Milestone achieved**: Single-node storage engine with WAL
- LSM storage with memtable + SST
- B+-tree indexed SST format
- Append-only WAL with segment rotation
- ReplicationGroup abstraction (rf=1)
- DataFusion SQL integration
- PostgreSQL wire protocol server

**Next milestone**: Multi-level compaction and recovery
- L0 → L1 compaction
- Full restart recovery from WAL + metadata persistence
- Improved SST key range filtering

**Future milestones**: Raft replication, MVCC, distributed transactions
