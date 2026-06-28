//! Segment index on-disk structures and helpers.
//!
//! # Layout (per segment)
//!
//! - `seg_{seg_id:05}.meta` — mutable [`IdxHeader`] (36 B), double-written to two
//!   `block_size` copies for crash consistency. Carries `min/max_live_lsn` +
//!   `entry_count`. Rewrite-only on truncate; `entry_count == 0` marks the segment
//!   active/unsealed (the discriminator recovery will use).
//! - `seg_{seg_id:05}.idx` — immutable SST mapping LSN→(offset,len), sealed once
//!   at segment close via `SstBuilder`. Read back zero-copy through `WalIndexReader`.
//! - `seg_{seg_id:05}.log` — frames (unchanged).
//!
//! # Encoding
//!
//! The header is fixed little-endian (matches SQLite/PostgreSQL/InnoDB, native on
//! the target LE CPUs). The SST key/value encoding is:
//! - Key: LSN as 8-byte **big-endian** (lexicographic byte order == numeric order).
//! - Value: `(offset: u64, len: u32)` little-endian, 12 B.

use std::path::{Path, PathBuf};

use bytes::{Bytes, BytesMut};

use crate::wal::WalError;

// ---- encoding helpers ----

/// Encode an LSN as an 8-byte big-endian key (lexicographic == numeric order).
pub fn lsn_to_key(lsn: u64) -> Bytes {
    Bytes::copy_from_slice(&lsn.to_be_bytes())
}

/// Decode an 8-byte big-endian key back to an LSN.
pub fn key_to_lsn(key: &[u8]) -> u64 {
    u64::from_be_bytes(key[..8].try_into().expect("lsn key is 8 bytes"))
}

/// Encode `(offset, len)` as a 12-byte little-endian value: `offset:u64 ++ len:u32`.
pub fn encode_offset_len(offset: u64, len: u32) -> Bytes {
    let mut b=BytesMut::with_capacity(12);
    b.extend_from_slice(&offset.to_le_bytes());
    b.extend_from_slice(&len.to_le_bytes());
    b.freeze()
}

/// Decode a 12-byte little-endian value into `(offset, len)`.
pub fn decode_offset_len(b: &[u8]) -> (u64, u32) {
    (
        u64::from_le_bytes(b[0..8].try_into().expect("offset is 8 bytes")),
        u32::from_le_bytes(b[8..12].try_into().expect("len is 4 bytes")),
    )
}

// ---- path helpers ----

/// Segment file paths: `seg_{seg_id:05}.{log,meta,idx}`. Zero-padded so a
/// directory listing is already in seg_id order (recovery scans + sorts the dir).
pub fn log_path(dir: &Path, seg_id: u32) -> PathBuf {
    dir.join(format!("seg_{seg_id:05}.log"))
}
pub fn meta_path(dir: &Path, seg_id: u32) -> PathBuf {
    dir.join(format!("seg_{seg_id:05}.meta"))
}
pub fn idx_path(dir: &Path, seg_id: u32) -> PathBuf {
    dir.join(format!("seg_{seg_id:05}.idx"))
}

pub const MAGIC: [u8; 4] = *b"WIDX";
pub const IDX_HEADER_LEN: usize = 36;

/// `.idx` header (36 B). Stored in two identical copies (block 0 = A, block 1 = B);
/// `header_crc` covers bytes [0,32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdxHeader {
    pub magic: [u8; 4],
    pub version: u32,
    pub seg_id: u32,
    pub min_live_lsn: u64,
    pub max_live_lsn: u64,
    pub entry_count: u32,
    pub header_crc: u32,
}

impl IdxHeader {
    pub fn new(seg_id: u32, min_live_lsn: u64, max_live_lsn: u64, entry_count: u32) -> Self {
        let mut h = Self {
            magic: MAGIC,
            version: 1,
            seg_id,
            min_live_lsn,
            max_live_lsn,
            entry_count,
            header_crc: 0,
        };
        h.header_crc = crc_of_header_fields(&h.serialize_fields());
        h
    }

    fn serialize_fields(&self) -> [u8; 32] {
        let mut buf = [0u8; 32];
        buf[0..4].copy_from_slice(&self.magic);
        buf[4..8].copy_from_slice(&self.version.to_le_bytes());
        buf[8..12].copy_from_slice(&self.seg_id.to_le_bytes());
        buf[12..20].copy_from_slice(&self.min_live_lsn.to_le_bytes());
        buf[20..28].copy_from_slice(&self.max_live_lsn.to_le_bytes());
        buf[28..32].copy_from_slice(&self.entry_count.to_le_bytes());
        buf
    }

    /// Serialize to 36 B (32 fields + 4 crc).
    pub fn serialize(&self) -> [u8; IDX_HEADER_LEN] {
        let fields = self.serialize_fields();
        let mut buf = [0u8; IDX_HEADER_LEN];
        buf[0..32].copy_from_slice(&fields);
        buf[32..36].copy_from_slice(&self.header_crc.to_le_bytes());
        buf
    }

    /// Deserialize + verify crc. Returns `None` on short buf or crc mismatch.
    ///
    /// Returns `Option` (not `Result`) because the sole caller, `select_valid_header`,
    /// treats a single corrupt copy as "this copy invalid — try the other": the
    /// double-write design tolerates one bad copy. Both copies failing is reported
    /// by `select_valid_header` as `Err(HeaderCorrupt)`.
    pub fn deserialize(buf: &[u8]) -> Option<Self> {
        if buf.len() < IDX_HEADER_LEN {
            return None;
        }
        let stored_crc = u32::from_le_bytes(buf[32..36].try_into().unwrap());
        if crc_of_header_fields(buf[0..32].try_into().unwrap()) != stored_crc {
            return None;
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&buf[0..4]);
        Some(Self {
            magic,
            version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            seg_id: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            min_live_lsn: u64::from_le_bytes(buf[12..20].try_into().unwrap()),
            max_live_lsn: u64::from_le_bytes(buf[20..28].try_into().unwrap()),
            entry_count: u32::from_le_bytes(buf[28..32].try_into().unwrap()),
            header_crc: stored_crc,
        })
    }
}

fn crc_of_header_fields(fields: &[u8; 32]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(fields);
    h.finalize()
}

/// Select the valid header copy from two block buffers (double-write recovery).
/// Returns `Err(HeaderCorrupt)` only if BOTH copies fail crc.
pub fn select_valid_header(copy_a: &[u8], copy_b: &[u8]) -> Result<IdxHeader, WalError> {
    match (
        IdxHeader::deserialize(copy_a),
        IdxHeader::deserialize(copy_b),
    ) {
        (Some(h), _) => Ok(h),
        (_, Some(h)) => Ok(h),
        (None, None) => Err(WalError::HeaderCorrupt {
            seg_id: u32::from_le_bytes(copy_a[8..12].try_into().unwrap_or([0; 4])),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsn_key_ordering_preserved() {
        // big-endian ⇒ lexicographic byte order == numeric order
        assert!(lsn_to_key(1) < lsn_to_key(2));
        assert!(lsn_to_key(255) < lsn_to_key(256));
        assert!(lsn_to_key(u64::MAX - 1) < lsn_to_key(u64::MAX));
        assert_eq!(key_to_lsn(&lsn_to_key(0x0f0e_0d0c_0b0a_0908)), 0x0f0e_0d0c_0b0a_0908);
    }

    #[test]
    fn offset_len_roundtrip() {
        for (off, len) in [(0u64, 0u32), (0x1234, 88), (u64::MAX, u32::MAX), (1 << 40, 4096)] {
            let v = encode_offset_len(off, len);
            assert_eq!(v.len(), 12);
            assert_eq!(decode_offset_len(&v), (off, len));
        }
    }

    #[test]
    fn paths_are_zero_padded_and_ordered() {
        let dir = Path::new("/tmp");
        assert_eq!(log_path(dir, 7), Path::new("/tmp/seg_00007.log"));
        assert_eq!(meta_path(dir, 42), Path::new("/tmp/seg_00042.meta"));
        assert_eq!(idx_path(dir, 42), Path::new("/tmp/seg_00042.idx"));
        // listing order == numeric seg_id order (no lexicographic surprise)
        assert!(log_path(dir, 9).to_str().unwrap() < log_path(dir, 10).to_str().unwrap());
    }

    #[test]
    fn header_serialize_deserialize_roundtrip() {
        let h = IdxHeader::new(7, 100, 200, 50);
        let bytes = h.serialize();
        assert_eq!(bytes.len(), 36);
        let d = IdxHeader::deserialize(&bytes).unwrap();
        assert_eq!(d, h);
    }

    #[test]
    fn header_crc_tamper_rejected() {
        let h = IdxHeader::new(1, 0, 10, 3);
        let mut bytes = h.serialize();
        bytes[12] ^= 0xff; // 篡改 min_live_lsn
        assert!(IdxHeader::deserialize(&bytes).is_none());
    }

    #[test]
    fn select_valid_header_prefers_intact_copy() {
        let h = IdxHeader::new(5, 10, 90, 8);
        let mut a = vec![0u8; 4096];
        let mut b = vec![0u8; 4096];
        a[..36].copy_from_slice(&h.serialize());
        b[..36].copy_from_slice(&h.serialize());
        b[12] ^= 0xff; // 损坏 B
        let selected = select_valid_header(&a, &b).unwrap();
        assert_eq!(selected, h);
    }

    #[test]
    fn select_valid_header_both_corrupt_errors() {
        let h = IdxHeader::new(9, 0, 5, 1);
        let mut a = vec![0u8; 4096];
        let mut b = vec![0u8; 4096];
        a[..36].copy_from_slice(&h.serialize());
        b[..36].copy_from_slice(&h.serialize());
        a[12] ^= 0xff;
        b[12] ^= 0xff;
        assert!(select_valid_header(&a, &b).is_err());
    }
}
