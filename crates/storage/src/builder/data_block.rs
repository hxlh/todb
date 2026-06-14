use bytes::{Bytes, BytesMut};

use crate::builder::SstOption;

// Fixed header for an empty block:
// - entry_count: u32
// - key_offsets sentinel: u32
// - value_offsets sentinel: u32
// Per-entry offset slots are accounted for in `estimated_size`; this constant
// covers only the invariant 12-byte prefix so `would_exceed()` matches the
// serialized block length.
const BLOCK_FIXED_HEADER_SIZE: usize = 12;

const DATA_ENTRY_VERSION: u8 = 1;
// Data entries are serialized as `[format_version][entry_kind][payload]`.
const DATA_ENTRY_HEADER_LEN: usize = 2;
// Each entry contributes one key offset and one value offset (u32 + u32).
const PER_ENTRY_OFFSET_TABLE_BYTES: usize = 8;

// Entry kind byte values — must match [`crate::iterators::entry_decode_iter`].
const KIND_PUT: u8 = 0;
const KIND_DELETE: u8 = 1;

/// A data block entry: a live value or a delete tombstone.
pub(crate) enum BlockEntry {
    Put(Bytes, Bytes),
    Delete(Bytes),
}

impl BlockEntry {
    fn key(&self) -> &Bytes {
        match self {
            BlockEntry::Put(k, _) => k,
            BlockEntry::Delete(k) => k,
        }
    }

    fn value_len(&self) -> usize {
        match self {
            BlockEntry::Put(_, v) => v.len(),
            BlockEntry::Delete(_) => 0,
        }
    }

    fn kind_byte(&self) -> u8 {
        match self {
            BlockEntry::Put(_, _) => KIND_PUT,
            BlockEntry::Delete(_) => KIND_DELETE,
        }
    }

    fn value(&self) -> &[u8] {
        match self {
            BlockEntry::Put(_, v) => v,
            BlockEntry::Delete(_) => &[],
        }
    }
}

/// Builds a data block from sorted (key, value) entries.
pub struct DataBlockBuilder {
    option: SstOption,
    entries: Vec<BlockEntry>,
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
        let entry_size = key.len() + value.len() + PER_ENTRY_OFFSET_TABLE_BYTES + DATA_ENTRY_HEADER_LEN;
        assert!(BLOCK_FIXED_HEADER_SIZE + entry_size < self.option.block_size);
        // `estimated_size` tracks only per-entry bytes:
        // - key payload
        // - value payload
        // - one key offset + one value offset (8 bytes total)
        // - the 2-byte entry envelope header
        // The invariant 12-byte block prefix is accounted for separately by
        // `BLOCK_FIXED_HEADER_SIZE` in `would_exceed()` and the assertion above.
        self.estimated_size += entry_size;
        self.entries.push(BlockEntry::Put(key, value));
    }

    /// Add a delete tombstone. `value_len` is 0; only the key is stored.
    pub fn add_delete(&mut self, key: Bytes) {
        let entry_size = key.len() + PER_ENTRY_OFFSET_TABLE_BYTES + DATA_ENTRY_HEADER_LEN;
        assert!(BLOCK_FIXED_HEADER_SIZE + entry_size < self.option.block_size);
        self.estimated_size += entry_size;
        self.entries.push(BlockEntry::Delete(key));
    }

    pub fn estimated_size(&self) -> usize {
        self.estimated_size
    }

    pub fn would_exceed(&self, key: &Bytes, value: &Bytes) -> bool {
        let next_entry_size =
            key.len() + value.len() + PER_ENTRY_OFFSET_TABLE_BYTES + DATA_ENTRY_HEADER_LEN;
        BLOCK_FIXED_HEADER_SIZE + self.estimated_size + next_entry_size > self.option.block_size
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn last_key(&self) -> Option<&Bytes> {
        self.entries.last().map(|e| e.key())
    }

    /// Encode entries into a data block.
    ///
    /// Layout:
    /// ```text
    /// [entry_count: u32 BE]
    /// [key_offset_0: u32 BE] ... [key_offset_{n-1}: u32 BE] [key_end_sentinel: u32 BE]
    /// [val_offset_0: u32 BE] ... [val_offset_{n-1}: u32 BE] [val_end_sentinel: u32 BE]
    /// [[format_version: u8] [entry_kind: u8] [payload bytes]] ...
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

        for entry in &self.entries {
            key_offsets.push(data_offset as u32);
            data_offset += entry.key().len();
        }
        key_offsets.push(data_offset as u32); // key sentinel = start of value area

        for entry in &self.entries {
            val_offsets.push(data_offset as u32);
            data_offset += DATA_ENTRY_HEADER_LEN + entry.value_len();
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
        for entry in &self.entries {
            buf.extend_from_slice(entry.key());
        }

        // Write entry-encoded value bytes: [version][kind][payload]
        for entry in &self.entries {
            buf.extend_from_slice(&[DATA_ENTRY_VERSION, entry.kind_byte()]);
            buf.extend_from_slice(entry.value());
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

    // Empty block encodes as count=0 plus two sentinel offsets (key and value).
    #[test]
    fn test_empty_block_layout() {
        let mut b = DataBlockBuilder::new(&option());
        let buf = b.finish();
        // header = count(4) + key_sentinel(4) + val_sentinel(4) = 12
        assert_eq!(buf.len(), option().block_size);
        assert_eq!(&buf[..4], &0u32.to_be_bytes());
    }

    // Single entry: verify versioned entry payload layout.
    #[test]
    fn test_single_entry_layout() {
        let mut b = DataBlockBuilder::new(&option());
        b.add(Bytes::from("k"), Bytes::from("v"));
        let buf = b.finish();

        // count = 1
        assert_eq!(u32::from_be_bytes(buf[0..4].try_into().unwrap()), 1);
        // header_size = 4 + (1+1)*8 = 20
        // key_offset[0]=20, key_sentinel=21, val_offset[0]=21, val_sentinel=24
        assert_eq!(u32::from_be_bytes(buf[4..8].try_into().unwrap()), 20);
        assert_eq!(u32::from_be_bytes(buf[8..12].try_into().unwrap()), 21);
        assert_eq!(u32::from_be_bytes(buf[12..16].try_into().unwrap()), 21);
        assert_eq!(u32::from_be_bytes(buf[16..20].try_into().unwrap()), 24);
        assert_eq!(&buf[20..21], b"k");
        assert_eq!(&buf[21..24], &[1, 0, b'v']);
        assert_eq!(buf.len(), option().block_size);
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

        assert_eq!(key0, 28);
        assert_eq!(key1, 30);
        assert_eq!(key_sent, 31);
        assert_eq!(val0, 31);
        assert_eq!(val1, 35); // first value = 2-byte header + 2-byte payload
        assert_eq!(val_sent, 38); // second value = 2-byte header + 1-byte payload

        assert_eq!(&buf[key0..key1], b"ab");
        assert_eq!(&buf[key1..key_sent], b"c");
        assert_eq!(&buf[val0..val1], &[1, 0, b'x', b'y']);
        assert_eq!(&buf[val1..val_sent], &[1, 0, b'z']);
    }

    // Delete tombstone encodes as kind=1 with empty payload.
    #[test]
    fn test_delete_entry_layout() {
        let mut b = DataBlockBuilder::new(&option());
        b.add_delete(Bytes::from("k"));
        let buf = b.finish();

        // count = 1, header_size = 20
        assert_eq!(u32::from_be_bytes(buf[0..4].try_into().unwrap()), 1);
        // key at offset 20, value at offset 21
        assert_eq!(&buf[20..21], b"k");
        // value = [version=1][kind=1] (delete, no payload)
        assert_eq!(&buf[21..23], &[1, 1]);
    }

    #[test]
    fn test_would_exceed_accounts_for_fixed_header() {
        let option = SstOption::default().block_size(23);
        let b = DataBlockBuilder::new(&option);
        assert!(b.would_exceed(&Bytes::from("k"), &Bytes::from("v")));
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
