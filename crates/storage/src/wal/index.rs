//! `.idx` on-disk structures: entry, header (double-write), and the in-memory
//! `IdxTail` accumulator flushed at 204-entry block granularity.
//!
//! # On-disk layout (fixed little-endian — independent of host CPU byte order)
//!
//! LE is chosen to match mainstream storage formats (SQLite / PostgreSQL / InnoDB)
//! and to be native on the target LE CPUs (x86-64 / AArch64) — zero `bswap` on the
//! hot path. The format is fixed and must not depend on the host byte order.
//!
//! - `IdxEntry` (20 B): `lsn: u64 | start_offset: u64 | total_len: u32`.
//!   `total_len == 0` is the padding sentinel (recovery stops there).
//! - `IdxHeader` (36 B): `magic[4] | version: u32 | seg_id: u32 | min_live_lsn: u64
//!   | max_live_lsn: u64 | entry_count: u32 | header_crc: u32` (crc32 over [0,32)).
//!   Stored in two identical copies (block 0 = A, block 1 = B); entries begin at block 2.

use crate::wal::WalError;

/// One index entry: `(lsn, start_offset, total_len)` — 20 B, little-endian.
/// `total_len == 0` is the padding sentinel (recovery stops there).
pub struct IdxEntry {
    pub lsn: u64,
    pub start_offset: u64,
    pub total_len: u32,
}

impl IdxEntry {
    pub const SERIALIZED_LEN: usize = 20;

    pub fn serialize(&self) -> [u8; Self::SERIALIZED_LEN] {
        let mut buf = [0u8; Self::SERIALIZED_LEN];
        buf[0..8].copy_from_slice(&self.lsn.to_le_bytes());
        buf[8..16].copy_from_slice(&self.start_offset.to_le_bytes());
        buf[16..20].copy_from_slice(&self.total_len.to_le_bytes());
        buf
    }

    /// Deserialize from a 20 B slice. Returns `None` if `buf` is too short.
    pub fn deserialize(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SERIALIZED_LEN {
            return None;
        }
        Some(Self {
            lsn: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            start_offset: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            total_len: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
        })
    }
}

pub const MAGIC: [u8; 4] = *b"WIDX";
pub const IDX_HEADER_LEN: usize = 36;
/// Entries per 4 KiB block: `floor(4096 / 20) = 204`.
pub const ENTRIES_PER_BLOCK: usize = 204;
pub const BLOCK_SIZE: usize = 4096;

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

/// In-memory accumulator for `.idx` entries, flushed at `ENTRIES_PER_BLOCK` granularity.
/// Single-writer (the flush thread owns it).
pub struct IdxTail {
    entries: Vec<IdxEntry>,
}

impl IdxTail {
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(ENTRIES_PER_BLOCK),
        }
    }

    pub fn push(&mut self, entry: IdxEntry) -> bool {
        if self.entries.len() >= ENTRIES_PER_BLOCK {
            return false;
        }
        self.entries.push(entry);
        true
    }

    pub fn is_full(&self) -> bool {
        self.entries.len() >= ENTRIES_PER_BLOCK
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Serialize the accumulated entries into one 4 KiB block (entries + zero padding),
    /// clearing the accumulator. Padding slots read back as `total_len == 0` (sentinel).
    pub fn drain_into_block(&mut self) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_SIZE];
        for (i, e) in self.entries.drain(..).enumerate() {
            let off = i * IdxEntry::SERIALIZED_LEN;
            block[off..off + IdxEntry::SERIALIZED_LEN].copy_from_slice(&e.serialize());
        }
        block
    }
}

impl Default for IdxTail {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_serialize_roundtrip() {
        let e = IdxEntry {
            lsn: 42,
            start_offset: 0x1234,
            total_len: 88,
        };
        let bytes = e.serialize();
        assert_eq!(bytes.len(), 20);
        let d = IdxEntry::deserialize(&bytes).unwrap();
        assert_eq!(d.lsn, 42);
        assert_eq!(d.start_offset, 0x1234);
        assert_eq!(d.total_len, 88);
    }

    #[test]
    fn padding_sentinel_is_zero_total_len() {
        let zero = IdxEntry {
            lsn: 0,
            start_offset: 0,
            total_len: 0,
        };
        let bytes = zero.serialize();
        // 全零 → recovery 视为 padding
        assert!(bytes.iter().all(|&b| b == 0));
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

    #[test]
    fn idx_tail_fills_to_204_then_rejects() {
        let mut tail = IdxTail::new();
        for i in 0..ENTRIES_PER_BLOCK {
            assert!(tail.push(IdxEntry {
                lsn: i as u64,
                start_offset: 0,
                total_len: 16
            }));
        }
        assert!(tail.is_full());
        assert!(!tail.push(IdxEntry {
            lsn: 999,
            start_offset: 0,
            total_len: 16
        }));
    }

    #[test]
    fn idx_tail_drain_into_block_pads_to_4k() {
        let mut tail = IdxTail::new();
        for i in 0..10 {
            tail.push(IdxEntry {
                lsn: i,
                start_offset: i * 16,
                total_len: 16,
            });
        }
        let block = tail.drain_into_block();
        assert_eq!(block.len(), BLOCK_SIZE);
        // 前 10 个 entries 正确
        for i in 0..10u64 {
            let off = (i as usize) * 20;
            let e = IdxEntry::deserialize(&block[off..off + 20]).unwrap();
            assert_eq!(e.lsn, i);
        }
        // 第 11 槽（offset 200）是 padding（全零）
        let pad = &block[200..220];
        assert!(pad.iter().all(|&b| b == 0));
        assert!(tail.is_empty());
    }
}
