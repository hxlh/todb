use bytes::{Bytes, BytesMut};

use crate::{block::Position, builder::SstOption};

const INDEX_VALUE_VERSION: u8 = 1;
const INDEX_VALUE_LEN: usize = 1 + size_of::<u64>();

// Fixed header for an empty block:
// - entry_count: u32
// - key_offsets sentinel: u32
// - value_offsets sentinel: u32
// Per-entry offset slots are accounted for in `estimated_size`; this constant
// covers only the invariant 12-byte prefix so `would_exceed()` matches the
// serialized block length.
const BLOCK_FIXED_HEADER_SIZE: usize = 12;

/// One entry in an index block: end_key -> child block location.
#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub end_key: Bytes,
    pub child: Position,
}

/// Builds an index block from sorted index entries.
pub struct IndexBlockBuilder {
    option: SstOption,
    entries: Vec<IndexEntry>,
    estimated_size: usize,
}

impl IndexBlockBuilder {
    pub fn new(option: &SstOption) -> Self {
        Self {
            option: option.clone(),
            entries: Vec::new(),
            estimated_size: 0,
        }
    }

    pub fn add(&mut self, end_key: Bytes, child: Position) {
        assert!(BLOCK_FIXED_HEADER_SIZE + end_key.len() + 17 < self.option.block_size);
        self.estimated_size += end_key.len() + 17;
        self.entries.push(IndexEntry { end_key, child });
    }

    pub fn estimated_size(&self) -> usize {
        self.estimated_size
    }

    pub fn would_exceed(&self, end_key: &Bytes, _child: &Position) -> bool {
        BLOCK_FIXED_HEADER_SIZE + self.estimated_size + end_key.len() + 17 > self.option.block_size
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn last_entry(&self) -> Option<&IndexEntry> {
        self.entries.last()
    }

    /// Encode entries into an index block.
    ///
    /// Layout mirrors DataBlock so BlockIter can parse it directly:
    /// ```text
    /// [entry_count: u32 BE]
    /// [key_offset_0: u32 BE] ... [key_offset_{n-1}: u32 BE] [key_end_sentinel: u32 BE]
    /// [val_offset_0: u32 BE] ... [val_offset_{n-1}: u32 BE] [val_end_sentinel: u32 BE]
    /// [end_key_0 bytes] [end_key_1 bytes] ... [end_key_{n-1} bytes]
    /// [[format_version: u8] [child_offset: u64 BE]] ...
    /// ```
    pub fn finish(&mut self) -> Bytes {
        let count = self.entries.len() as u32;
        let mut buf = BytesMut::new();

        buf.extend_from_slice(&count.to_be_bytes());

        // Header: count(4) + key_offsets(4*(n+1)) + val_offsets(4*(n+1))
        // The extra +1 slot is a sentinel enabling O(1) size lookup for any entry.
        let header_size = 4 + (self.entries.len() + 1) * 8;
        let mut data_offset = header_size;

        let mut key_offsets = Vec::with_capacity(self.entries.len() + 1);
        let mut val_offsets = Vec::with_capacity(self.entries.len() + 1);

        for entry in &self.entries {
            key_offsets.push(data_offset as u32);
            data_offset += entry.end_key.len();
        }
        key_offsets.push(data_offset as u32); // key sentinel = start of value area

        // Each value is a fixed 1-byte version + 8-byte u64 child offset
        for _ in &self.entries {
            val_offsets.push(data_offset as u32);
            data_offset += INDEX_VALUE_LEN;
        }
        val_offsets.push(data_offset as u32); // value sentinel = end of block

        for off in &key_offsets {
            buf.extend_from_slice(&off.to_be_bytes());
        }
        for off in &val_offsets {
            buf.extend_from_slice(&off.to_be_bytes());
        }
        for entry in &self.entries {
            buf.extend_from_slice(&entry.end_key);
        }
        for entry in &self.entries {
            buf.extend_from_slice(&[INDEX_VALUE_VERSION]);
            buf.extend_from_slice(&entry.child.offset.to_be_bytes());
        }

        buf.resize(self.option.block_size, 0);
        self.reset();
        buf.freeze()
    }

    pub fn reset(&mut self) {
        self.entries.clear();
        self.estimated_size = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option() -> SstOption {
        SstOption::default()
    }

    // Empty index block encodes as count=0 plus two sentinel offsets.
    #[test]
    fn test_empty_block_layout() {
        let mut b = IndexBlockBuilder::new(&option());
        let buf = b.finish();
        // header = count(4) + key_sentinel(4) + val_sentinel(4) = 12
        assert_eq!(buf.len(), option().block_size);
        assert_eq!(&buf[..4], &0u32.to_be_bytes());
    }

    // Single entry: verify every field position.
    #[test]
    fn test_single_entry_layout() {
        let mut b = IndexBlockBuilder::new(&option());
        b.add(Bytes::from("k"), Position { offset: 0xAB });
        let buf = b.finish();

        // count = 1
        assert_eq!(u32::from_be_bytes(buf[0..4].try_into().unwrap()), 1);
        // header_size = 4 + (1+1)*8 = 20
        // key_offset[0]=20, key_sentinel=21, val_offset[0]=21, val_sentinel=30
        assert_eq!(u32::from_be_bytes(buf[4..8].try_into().unwrap()), 20); // key_offset[0]
        assert_eq!(u32::from_be_bytes(buf[8..12].try_into().unwrap()), 21); // key sentinel
        assert_eq!(u32::from_be_bytes(buf[12..16].try_into().unwrap()), 21); // val_offset[0]
        assert_eq!(u32::from_be_bytes(buf[16..20].try_into().unwrap()), 30); // val sentinel
        assert_eq!(&buf[20..21], b"k");
        assert_eq!(buf[21], 1);
        assert_eq!(u64::from_be_bytes(buf[22..30].try_into().unwrap()), 0xAB);
        assert_eq!(buf.len(), option().block_size);
    }

    // Two entries: offsets, keys, and child handles must be correctly positioned.
    #[test]
    fn test_two_entry_layout() {
        let mut b = IndexBlockBuilder::new(&option());
        b.add(Bytes::from("ab"), Position { offset: 0x10 });
        b.add(Bytes::from("c"), Position { offset: 0x20 });
        let buf = b.finish();

        // count = 2
        assert_eq!(u32::from_be_bytes(buf[0..4].try_into().unwrap()), 2);
        // header_size = 4 + (2+1)*8 = 28
        let key0 = u32::from_be_bytes(buf[4..8].try_into().unwrap()) as usize;
        let key1 = u32::from_be_bytes(buf[8..12].try_into().unwrap()) as usize;
        let key_sent = u32::from_be_bytes(buf[12..16].try_into().unwrap()) as usize;
        let val0 = u32::from_be_bytes(buf[16..20].try_into().unwrap()) as usize;
        let val1 = u32::from_be_bytes(buf[20..24].try_into().unwrap()) as usize;
        let val_sent = u32::from_be_bytes(buf[24..28].try_into().unwrap()) as usize;

        assert_eq!(key0, 28); // header ends at 28
        assert_eq!(key1, 30); // "ab" = 2 bytes
        assert_eq!(key_sent, 31); // "c"  = 1 byte
        assert_eq!(val0, 31); // values start where keys end
        assert_eq!(val1, 40); // first value is 9 bytes
        assert_eq!(val_sent, 49); // second value is 9 bytes

        assert_eq!(&buf[key0..key1], b"ab");
        assert_eq!(&buf[key1..key_sent], b"c");
        assert_eq!(buf[val0], 1);
        assert_eq!(
            u64::from_be_bytes(buf[val0 + 1..val1].try_into().unwrap()),
            0x10
        );
        assert_eq!(buf[val1], 1);
        assert_eq!(
            u64::from_be_bytes(buf[val1 + 1..val_sent].try_into().unwrap()),
            0x20
        );
    }

    #[test]
    fn test_size_estimation_accounts_for_versioned_index_value() {
        let mut b = IndexBlockBuilder::new(&option());
        b.add(Bytes::from("ab"), Position { offset: 1 });
        assert_eq!(b.estimated_size(), 19);
        assert!(!b.would_exceed(&Bytes::from("c"), &Position { offset: 2 }));
    }

    #[test]
    fn test_would_exceed_accounts_for_fixed_header() {
        let option = SstOption::default().block_size(29);
        let b = IndexBlockBuilder::new(&option);
        assert!(b.would_exceed(&Bytes::from("k"), &Position { offset: 1 }));
    }

    // finish() resets the builder so it can be reused.
    #[test]
    fn test_finish_resets_builder() {
        let mut b = IndexBlockBuilder::new(&option());
        b.add(Bytes::from("k"), Position { offset: 1 });
        let _ = b.finish();
        assert!(b.is_empty());
        assert_eq!(b.estimated_size(), 0);
    }
}
