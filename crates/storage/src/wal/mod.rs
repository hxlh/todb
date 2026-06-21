//! WAL (write-ahead log) for replication groups.
//!
//! Submodules:
//! - [`store`] — `WalStore` trait, `WalEntry`/`WalPayload` ADT, `NoopWal`.
//! - [`file_wal_store`] — on-disk `FileWalStore` (segments + buffer + sync thread).
//! - [`serialize`] — `WriteBatch` encode/decode for WAL payloads.

pub mod file_wal_store;
pub mod serialize;
pub mod store;

pub use store::{Lsn, NoopWal, WalEntry, WalPayload, WalStore};
