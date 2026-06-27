# System Baseline

## Technology Stack

### Core Stack
- Language: Rust (edition 2024)
- Async runtime: Tokio 1.x (full features)
- Build system: Cargo workspace

### Storage Layer
- Storage engine: Custom LSM-tree implementation
- WAL: Append-only write-ahead log with segment rotation
- Memtable: Crossbeam skiplist-based in-memory structure
- SST format: Custom B+-tree indexed block storage with fixed-size slots
- Block I/O: Direct file I/O with fsync guarantees

### SQL Layer
- Query engine: DataFusion 44
- Wire protocol: PostgreSQL (pgwire 0.38.3)
- Table provider: Custom integration between storage and DataFusion

### Data Structures
- Key-value encoding: Length-prefixed binary format
- Iteration: Multi-level merge iterators (memtable + SST layers)
- Replication: ReplicationGroup abstraction (currently single-node rf=1)

### Memory Management
- Allocator: jemalloc with stats tracking
- Memory context: Hierarchical memory tracking tree

### Observability
- Tracing: OpenTelemetry + tracing crate
- Metrics: Prometheus exporter
- Logging: tracing-subscriber with JSON formatter

## Crate Structure

```
crates/
├── common/        # Shared error types and utilities
├── runtime/       # Async runtime configuration
├── observability/ # Tracing and metrics
├── memory/        # Memory tracking and management
├── rpc/           # RPC protocol definitions
├── sql/           # SQL layer and DataFusion integration
└── storage/       # Storage engine (LSM, WAL, iterators)
server/            # PostgreSQL wire protocol server
```

## External Dependencies

### Critical Path
- `tokio`: Async runtime
- `datafusion`: SQL query engine
- `pgwire`: PostgreSQL wire protocol
- `crossbeam-skiplist`: Memtable implementation
- `bytes`: Zero-copy buffer management

### Serialization
- `serde` + `serde_json`: Configuration and metadata
- `prost` + `tonic`: gRPC protocol buffers (future)

### Storage
- File I/O: Standard library with direct fsync control
- No external storage engine dependency (custom implementation)

## Current Limitations

- Single-node only (replication group rf=1)
- No Raft integration yet (prepared but not implemented)
- No compaction (L0 only)
- No MVCC (single-version writes)
- No distributed transaction coordination
- Recovery partially implemented (WAL recovery ready, metadata persistence pending)

## Future Architecture Direction

See `old_docs/superpowers/specs/` for detailed design docs on:
- WAL + ReplicationGroup (implemented, single-node)
- Storage layering (MetaManager, StorageLayer, LsmEngine)
- Memtable flush and disk manager
- SST footer with key range metadata
