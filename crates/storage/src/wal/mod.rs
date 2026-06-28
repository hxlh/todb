//! WAL core — high-performance write-ahead log primitives.
//!
//! Layered: pure data (`lsn` / `record` / `frame` / `error`) → in-memory (`buffer` /
//! `index` / `aligned`) → disk (`segment`) → facade ([`Wal`]). See
//! `docs/architecture/wal-design.md`.

pub mod aligned;
pub mod buffer;
pub mod config;
pub mod disk;
pub mod facade;
pub mod frame;
pub mod index;
pub mod index_reader;
pub mod lsn;
pub mod record;
pub mod segment;
mod error;

pub use aligned::AlignedMem;
pub use buffer::{STATE_ACTIVE, STATE_FULL, WalBuffer, pack, unpack};
pub use config::WalConfig;
pub use disk::{DiskManager, PinGuard};
pub use error::WalError;
pub use facade::Wal;
pub use frame::{DecodedFrame, HEADER_LEN, encode};
pub use index::{
    ENTRIES_PER_BLOCK, IDX_HEADER_LEN, IdxEntry, IdxHeader, IdxTail, select_valid_header,
};
pub use index_reader::WalIndexReader;
pub use lsn::{Lsn, LsnRange};
pub use record::{Record, RecordRef};
pub use segment::Segment;
