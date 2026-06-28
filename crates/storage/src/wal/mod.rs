//! WAL core — high-performance write-ahead log primitives.
//!
//! Layered: pure data (`lsn` / `record` / `frame` / `error`) → in-memory (`buffer` /
//! `index` / `aligned`) → disk (`segment`) → facade ([`Wal`]). See
//! `docs/architecture/wal-design.md`.

pub mod aligned;
pub mod buffer;
pub mod config;
pub mod disk;
pub mod r#impl;
pub mod frame;
pub mod index;
pub mod index_reader;
pub mod lsn;
pub mod odirect_writer;
pub mod record;
pub mod segment;
mod error;

pub use aligned::AlignedMem;
pub use buffer::{STATE_ACTIVE, STATE_FULL, WalBuffer, pack, unpack};
pub use config::WalConfig;
pub use disk::{DiskManager, PinGuard};
pub use error::WalError;
pub use r#impl::Wal;
pub use frame::{DecodedFrame, HEADER_LEN, encode};
pub use index::{
    IDX_HEADER_LEN, IdxHeader, decode_offset_len, encode_offset_len, idx_path, key_to_lsn,
    log_path, lsn_to_key, meta_path, select_valid_header,
};
pub use index_reader::WalIndexReader;
pub use lsn::{Lsn, LsnRange};
pub use odirect_writer::{SegmentIndexBlockWriter, ODirectSstWriter};
pub use record::{Record, RecordRef};
pub use segment::Segment;
