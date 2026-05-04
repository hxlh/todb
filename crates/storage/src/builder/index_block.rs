use bytes::{Bytes, BytesMut};

use crate::block::{BlockHandle, BLOCK_SIZE};

/// One entry in an index block: end_key -> child block location.
#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub end_key: Bytes,
    pub child: BlockHandle,
}

/// Builds an index block from sorted index entries.
pub struct IndexBlockBuilder {
    entries: Vec<IndexEntry>,
    estimated_size: usize,
}

impl IndexBlockBuilder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            estimated_size: 0,
        }
    }

    pub fn add(&mut self, end_key: Bytes, child: BlockHandle) {
        assert!(end_key.len() + 16 < BLOCK_SIZE);
        // Overhead: 4 bytes key offset + 8 bytes child offset + 4 bytes child size
        self.estimated_size += end_key.len() + 16;
        self.entries.push(IndexEntry { end_key, child });
    }

    pub fn estimated_size(&self) -> usize {
        self.estimated_size
    }

    pub fn would_exceed(&self, end_key: &Bytes, _child: &BlockHandle) -> bool {
        self.estimated_size + end_key.len() + 16 > BLOCK_SIZE
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
    /// Layout:
    /// ```text
    /// [entry_count: u32 BE]
    /// [key_offset_0: u32 BE] [key_offset_1: u32 BE] ...
    /// [child_offset_0: u64 BE] [child_size_0: u32 BE]
    /// [child_offset_1: u64 BE] [child_size_1: u32 BE] ...
    /// [end_key_0 bytes] [end_key_1 bytes] ...
    /// ```
    pub fn finish(&mut self) -> Bytes {
        let count = self.entries.len() as u32;
        let mut buf = BytesMut::new();

        buf.extend_from_slice(&count.to_be_bytes());

        let header_size = 4 + self.entries.len() * 4 + self.entries.len() * 12;
        let mut data_offset = header_size;

        let mut key_offsets = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            key_offsets.push(data_offset as u32);
            data_offset += entry.end_key.len();
        }

        // Write key offsets
        for off in &key_offsets {
            buf.extend_from_slice(&off.to_be_bytes());
        }

        // Write child handles
        for entry in &self.entries {
            buf.extend_from_slice(&entry.child.offset.to_be_bytes());
            buf.extend_from_slice(&entry.child.size.to_be_bytes());
        }

        // Write end keys
        for entry in &self.entries {
            buf.extend_from_slice(&entry.end_key);
        }

        self.reset();
        buf.freeze()
    }

    pub fn reset(&mut self) {
        self.entries.clear();
        self.estimated_size = 0;
    }
}
