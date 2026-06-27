use std::io;

/// Crate-wide error type. See `docs/architecture/wal-design.md` §Error type.
#[derive(Debug, thiserror::Error)]
pub enum WalError {
    #[error("io: {0}")]
    Io(#[from] io::Error),

    #[error("frame crc mismatch at lsn {lsn} (offset {offset})")]
    CrcMismatch { lsn: u64, offset: u64 },

    #[error("segment {seg_id} header corrupt (both copies failed crc)")]
    HeaderCorrupt { seg_id: u32 },

    #[error("invalid config: {0}")]
    InvalidConfig(String),

    #[error("wal closed")]
    Closed,
}
