use bytes::{Bytes, BytesMut};

use crate::builder::SstOption;

/// Builds a data block from sorted (key, value) entries.
pub struct DataBlockBuilder {
    option: SstOption,
    entries: Vec<(Bytes, Bytes)>,
    estimated_size: usize,
}

impl DataBlockBuilder {
    pub fn new(option: &SstOption) -> Self {
        Self {
            option: option.clone(),
            entries: Vec::new(),
            estimated_size: 0,
        }
    }

    pub fn add(&mut self, key: Bytes, value: Bytes) {
        assert!(key.len() + value.len() + 8 < self.option.block_size);
        // Overhead: 8 bytes per entry for key offset + value offset
        self.estimated_size += key.len() + value.len() + 8;
        self.entries.push((key, value));
    }

    pub fn estimated_size(&self) -> usize {
        self.estimated_size
    }

    pub fn would_exceed(&self, key: &Bytes, value: &Bytes) -> bool {
        self.estimated_size + key.len() + value.len() + 8 > self.option.block_size
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn last_key(&self) -> Option<&Bytes> {
        self.entries.last().map(|(k, _)| k)
    }

    /// Encode entries into a data block.
    ///
    /// Layout:
    /// ```text
    /// [entry_count: u32 BE]
    /// [key_offset_0: u32 BE] [key_offset_1: u32 BE] ...
    /// [val_offset_0: u32 BE] [val_offset_1: u32 BE] ...
    /// [key_0 bytes] [key_1 bytes] ...
    /// [val_0 bytes] [val_1 bytes] ...
    /// ```
    pub fn finish(&mut self) -> Bytes {
        let count = self.entries.len() as u32;
        let mut buf = BytesMut::new();

        // Header: entry count
        buf.extend_from_slice(&count.to_be_bytes());

        // Header: count(4) + key_offsets(4*(n+1)) + val_offsets(4*(n+1))
        // The extra +1 slot is a sentinel enabling O(1) size lookup for any entry.
        let header_size = 4 + (self.entries.len() + 1) * 8;
        let mut data_offset = header_size;

        let mut key_offsets = Vec::with_capacity(self.entries.len() + 1);
        let mut val_offsets = Vec::with_capacity(self.entries.len() + 1);

        for (key, _value) in &self.entries {
            key_offsets.push(data_offset as u32);
            data_offset += key.len();
        }
        key_offsets.push(data_offset as u32); // key sentinel = start of value area

        for (_key, value) in &self.entries {
            val_offsets.push(data_offset as u32);
            data_offset += value.len();
        }
        val_offsets.push(data_offset as u32); // value sentinel = end of block

        // Write offset tables
        for off in &key_offsets {
            buf.extend_from_slice(&off.to_be_bytes());
        }
        for off in &val_offsets {
            buf.extend_from_slice(&off.to_be_bytes());
        }

        // Write key bytes
        for (key, _value) in &self.entries {
            buf.extend_from_slice(key);
        }

        // Write value bytes
        for (_key, value) in &self.entries {
            buf.extend_from_slice(value);
        }

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

    // Empty block encodes as count=0 plus two sentinel offsets (key and value).
    #[test]
    fn test_empty_block_layout() {
        let mut b = DataBlockBuilder::new(&option());
        let buf = b.finish();
        // header = count(4) + key_sentinel(4) + val_sentinel(4) = 12
        assert_eq!(buf.len(), 12);
        assert_eq!(&buf[..4], &0u32.to_be_bytes());
    }

    // Single entry: verify every field position in the encoded bytes.
    #[test]
    fn test_single_entry_layout() {
        let mut b = DataBlockBuilder::new(&option());
        b.add(Bytes::from("k"), Bytes::from("v"));
        let buf = b.finish();

        // count = 1
        assert_eq!(u32::from_be_bytes(buf[0..4].try_into().unwrap()), 1);
        // header_size = 4 + (1+1)*8 = 20
        // key_offset[0]=20, key_sentinel=21, val_offset[0]=21, val_sentinel=22
        assert_eq!(u32::from_be_bytes(buf[4..8].try_into().unwrap()), 20); // key_offset[0]
        assert_eq!(u32::from_be_bytes(buf[8..12].try_into().unwrap()), 21); // key sentinel
        assert_eq!(u32::from_be_bytes(buf[12..16].try_into().unwrap()), 21); // val_offset[0]
        assert_eq!(u32::from_be_bytes(buf[16..20].try_into().unwrap()), 22); // val sentinel
        assert_eq!(&buf[20..21], b"k");
        assert_eq!(&buf[21..22], b"v");
        assert_eq!(buf.len(), 22);
    }

    // Two entries: offsets must be contiguous and non-overlapping.
    #[test]
    fn test_two_entry_layout() {
        let mut b = DataBlockBuilder::new(&option());
        b.add(Bytes::from("ab"), Bytes::from("xy"));
        b.add(Bytes::from("c"), Bytes::from("z"));
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
        assert_eq!(val1, 33); // "xy" = 2 bytes
        assert_eq!(val_sent, 34); // "z"  = 1 byte

        assert_eq!(&buf[key0..key1], b"ab");
        assert_eq!(&buf[key1..key_sent], b"c");
        assert_eq!(&buf[val0..val1], b"xy");
        assert_eq!(&buf[val1..val_sent], b"z");
    }

    // finish() resets the builder so it can be reused.
    #[test]
    fn test_finish_resets_builder() {
        let mut b = DataBlockBuilder::new(&option());
        b.add(Bytes::from("k"), Bytes::from("v"));
        let _ = b.finish();
        assert!(b.is_empty());
        assert_eq!(b.estimated_size(), 0);
    }
}
