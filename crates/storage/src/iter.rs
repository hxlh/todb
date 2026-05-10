use bytes::Bytes;

use crate::{errors::StorageResult, key::InternalKey};

pub trait StorageIter {
    /// Key type returned by this iterator. Must support ordering for seek/binary search.
    type RowKey: Ord;

    /// Value type returned by this iterator.
    type RowValue;

    /// True if positioned at a valid entry.
    fn valid(&self) -> bool;

    /// Move to the first entry.
    fn seek_to_first(&mut self);

    /// Move to the first entry with key >= target.
    fn seek(&mut self, target: &Self::RowKey);

    /// Advance to the next entry.
    fn next(&mut self)-> StorageResult<()>;

    /// Current key. Only call when `valid()` is true.
    fn key(&self) -> &Self::RowKey;

    /// Current value. Only call when `valid()` is true.
    fn value(&self) -> Self::RowValue;
}


pub struct BlockIter {
    block: Bytes,
    entry_count: usize,
    key_offsets: Vec<u32>,
    val_offsets: Vec<u32>,
    pos: usize,
    current_key: Option<InternalKey>,
}

impl BlockIter {
    pub fn new(block: Bytes) -> Self {
        let mut iter = Self {
            block,
            entry_count: 0,
            key_offsets: Vec::new(),
            val_offsets: Vec::new(),
            pos: 0,
            current_key: None,
        };
        iter.parse_header();
        iter.seek_to_first();
        iter
    }

    fn parse_header(&mut self) {
        if self.block.len() < 4 {
            return;
        }
        let count = u32::from_be_bytes([
            self.block[0], self.block[1], self.block[2], self.block[3],
        ]) as usize;
        self.entry_count = count;

        let header_size = 4 + count * 8;
        if self.block.len() < header_size {
            self.entry_count = 0;
            return;
        }

        self.key_offsets.reserve(count);
        self.val_offsets.reserve(count);

        for i in 0..count {
            let off = 4 + i * 4;
            let key_off = u32::from_be_bytes([
                self.block[off],
                self.block[off + 1],
                self.block[off + 2],
                self.block[off + 3],
            ]);
            self.key_offsets.push(key_off);
        }

        for i in 0..count {
            let off = 4 + count * 4 + i * 4;
            let val_off = u32::from_be_bytes([
                self.block[off],
                self.block[off + 1],
                self.block[off + 2],
                self.block[off + 3],
            ]);
            self.val_offsets.push(val_off);
        }
    }

    /// Return (offset, len) for the key at `idx`.
    fn key_range(&self, idx: usize) -> (usize, usize) {
        let start = self.key_offsets[idx] as usize;
        let end = if idx + 1 < self.entry_count {
            self.key_offsets[idx + 1] as usize
        } else {
            self.val_offsets[0] as usize
        };
        (start, end - start)
    }

    /// Return (offset, len) for the value at `idx`.
    fn val_range(&self, idx: usize) -> (usize, usize) {
        let start = self.val_offsets[idx] as usize;
        let end = if idx + 1 < self.entry_count {
            self.val_offsets[idx + 1] as usize
        } else {
            self.block.len()
        };
        (start, end - start)
    }

    /// Build an InternalKey at the given index (used for binary search).
    fn key_at(&self, idx: usize) -> InternalKey {
        let (start, len) = self.key_range(idx);
        let key_bytes = self.block.slice(start..start + len);
        InternalKey::new(key_bytes)
    }

    fn update_cache(&mut self) {
        if self.pos < self.entry_count {
            let (start, len) = self.key_range(self.pos);
            let key_bytes = self.block.slice(start..start + len);
            self.current_key = Some(InternalKey::new(key_bytes));
        } else {
            self.current_key = None;
        }
    }
}

impl StorageIter for BlockIter {
    type RowKey = InternalKey;
    type RowValue = Bytes;

    fn valid(&self) -> bool {
        self.pos < self.entry_count
    }

    fn seek_to_first(&mut self) {
        self.pos = 0;
        self.update_cache();
    }

    fn seek(&mut self, target: &InternalKey) {
        let mut left = 0;
        let mut right = self.entry_count;
        while left < right {
            let mid = (left + right) / 2;
            let mid_key = self.key_at(mid);
            if mid_key < *target {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        self.pos = left;
        self.update_cache();
    }

    fn next(&mut self) -> StorageResult<()> {
        self.pos += 1;
        self.update_cache();
        Ok(())
    }

    fn key(&self) -> &InternalKey {
        self.current_key.as_ref().expect("invalid iterator")
    }

    fn value(&self) -> Bytes {
        let (start, len) = self.val_range(self.pos);
        self.block.slice(start..start + len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockReader, BlockWriter, InMemoryBlockReader, InMemoryBlockWriter};
    use crate::builder::DataBlockBuilder;
    use crate::key::{InternalKey, OpType};

    fn make_ik(user_key: &str, seq: u64) -> InternalKey {
        InternalKey::from_user_key(Bytes::from(user_key.to_string()), seq, OpType::Put)
    }

    /// Helper: build a data block, write it via BlockWriter, read it back via BlockReader.
    fn build_and_read_block(mut builder: DataBlockBuilder) -> Bytes {
        let block = builder.finish();
        let mut writer = InMemoryBlockWriter::new();
        let handle = writer.write_block(block).unwrap();
        let buf = Bytes::from(writer.into_inner());
        let reader = InMemoryBlockReader::new(buf);
        reader.read_block(handle).unwrap()
    }

    #[test]
    fn test_block_iter_empty() {
        let builder = DataBlockBuilder::new();
        let block = build_and_read_block(builder);
        let iter = BlockIter::new(block);
        assert!(!iter.valid());
    }

    #[test]
    fn test_block_iter_forward() {
        let mut builder = DataBlockBuilder::new();
        let k1 = make_ik("a", 10);
        let k2 = make_ik("b", 9);
        builder.add(k1.as_bytes().clone(), Bytes::from("v1"));
        builder.add(k2.as_bytes().clone(), Bytes::from("v2"));
        let block = build_and_read_block(builder);

        let mut iter = BlockIter::new(block);
        assert!(iter.valid());
        assert_eq!(iter.key().raw_key(), Bytes::from("a"));
        assert_eq!(iter.value(), Bytes::from_static(b"v1"));

        iter.next().unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().raw_key(), Bytes::from("b"));
        assert_eq!(iter.value(), Bytes::from_static(b"v2"));

        iter.next().unwrap();
        assert!(!iter.valid());
    }

    #[test]
    fn test_block_iter_seek() {
        let mut builder = DataBlockBuilder::new();
        let k1 = make_ik("a", 10);
        let k2 = make_ik("c", 8);
        let k3 = make_ik("e", 5);
        builder.add(k1.as_bytes().clone(), Bytes::from("v1"));
        builder.add(k2.as_bytes().clone(), Bytes::from("v2"));
        builder.add(k3.as_bytes().clone(), Bytes::from("v3"));
        let block = build_and_read_block(builder);

        let mut iter = BlockIter::new(block);
        let target = make_ik("c", 100); // higher seq, but same user key ordering
        iter.seek(&target);
        assert!(iter.valid());
        assert_eq!(iter.key().raw_key(), Bytes::from("c"));
        assert_eq!(iter.value(), Bytes::from_static(b"v2"));
    }

    #[test]
    fn test_block_iter_sequence_ordering() {
        // Same user key, different sequences. In storage order: higher seq first.
        let mut builder = DataBlockBuilder::new();
        let k_new = make_ik("x", 20);
        let k_old = make_ik("x", 10);
        builder.add(k_new.as_bytes().clone(), Bytes::from("new"));
        builder.add(k_old.as_bytes().clone(), Bytes::from("old"));
        let block = build_and_read_block(builder);

        let mut iter = BlockIter::new(block);
        assert_eq!(iter.key().sequence(), 20);
        assert_eq!(iter.value(), Bytes::from_static(b"new"));

        iter.next().unwrap();
        assert_eq!(iter.key().sequence(), 10);
        assert_eq!(iter.value(), Bytes::from_static(b"old"));
    }
}
