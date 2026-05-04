use bytes::{Bytes, BytesMut};

use crate::block::BLOCK_SIZE;

/// Builds a data block from sorted (key, value) entries.
pub struct DataBlockBuilder {
    entries: Vec<(Bytes, Bytes)>,
    estimated_size: usize,
}

impl DataBlockBuilder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            estimated_size: 0,
        }
    }

    pub fn add(&mut self, key: Bytes, value: Bytes) {
        assert!(key.len() + value.len() + 8 < BLOCK_SIZE);
        // Overhead: 8 bytes per entry for key offset + value offset
        self.estimated_size += key.len() + value.len() + 8;
        self.entries.push((key, value));
    }

    pub fn estimated_size(&self) -> usize {
        self.estimated_size
    }

    pub fn would_exceed(&self, key: &Bytes, value: &Bytes) -> bool {
        self.estimated_size + key.len() + value.len() + 8 > BLOCK_SIZE
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

        // Compute offsets
        let header_size = 4 + self.entries.len() * 8;
        let mut data_offset = header_size;

        let mut key_offsets = Vec::with_capacity(self.entries.len());
        let mut val_offsets = Vec::with_capacity(self.entries.len());

        for (key, _value) in &self.entries {
            key_offsets.push(data_offset as u32);
            data_offset += key.len();
        }
        for (_key, value) in &self.entries {
            val_offsets.push(data_offset as u32);
            data_offset += value.len();
        }

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
