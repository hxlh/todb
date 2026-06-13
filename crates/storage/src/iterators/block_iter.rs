use core::fmt;
use std::fmt::Write;

use bytes::Bytes;
use tracing::debug;

use crate::{
    errors::StorageResult,
    iterators::storage_iter::{AsArray, DataBlockIter, IndexBlockIter, StorageIter},
    row_key::{BinaryKey, RowKey},
};

pub struct RawEntry<'a> {
    buf: &'a [u8],
}

impl<'a> From<&'a [u8]> for RawEntry<'a> {
    fn from(buf: &'a [u8]) -> Self {
        Self { buf }
    }
}

impl<'a> AsArray<'a> for RawEntry<'a> {
    fn as_array(&self) -> &'a [u8] {
        self.buf
    }
}

#[allow(dead_code)]
pub struct NormalBlockIter {
    block: Bytes,
    key_offsets: Vec<usize>,
    values_offsets: Vec<usize>,
    count: usize,
    curr_idx: usize,
}

impl NormalBlockIter {
    pub fn new(block: Bytes) -> StorageResult<Self> {
        let mut s = Self {
            block,
            key_offsets: vec![],
            values_offsets: vec![],
            curr_idx: 0,
            count: 0,
        };

        s.parse_header();
        Ok(s)
    }

    fn parse_header(&mut self) {
        self.reset();

        let mut start = 0;
        // parse count
        let buf = &self.block[start..start + size_of::<u32>()];
        let count = u32::from_be_bytes(buf.try_into().unwrap()) as usize;
        start += size_of::<u32>();

        // parse key offsets (count + 1, last is sentinel)
        for _ in 0..=count {
            let buf = &self.block[start..start + size_of::<u32>()];
            let offset = u32::from_be_bytes(buf.try_into().unwrap()) as usize;
            self.key_offsets.push(offset);
            start += size_of::<u32>();
        }

        // parse values offsets (count + 1, last is sentinel)
        for _ in 0..=count {
            let buf = &self.block[start..start + size_of::<u32>()];
            let offset = u32::from_be_bytes(buf.try_into().unwrap()) as usize;
            self.values_offsets.push(offset);
            start += size_of::<u32>();
        }

        self.count = count;
        debug!("read block: count= {}", &self.count);
    }

    fn reset(&mut self) {
        // clean
        self.curr_idx = 0;
        self.key_offsets.clear();
        self.values_offsets.clear();
    }
}

impl StorageIter for NormalBlockIter {
    type Key<'a> = RowKey<'a>;

    type Value<'a> = RawEntry<'a>;

    fn valid(&self) -> bool {
        self.count > 0 && self.curr_idx < self.count
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        Ok(())
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        // Binary search for the first key >= target (lower_bound).
        // Upper bound excludes the sentinel slot.
        let mut lo = 0usize;
        let mut hi = self.count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            self.curr_idx = mid;
            match self.key() {
                Some(k) if k < *target => lo = mid + 1,
                _ => hi = mid,
            }
        }
        self.curr_idx = lo;
        Ok(())
    }

    fn next(&mut self) -> StorageResult<()> {
        self.curr_idx += 1;
        Ok(())
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        if self.valid() {
            let start = self.key_offsets[self.curr_idx];
            let end = self.key_offsets[self.curr_idx + 1]; // sentinel always present
            return Some(BinaryKey::from(&self.block[start..end]));
        }
        None
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        if self.valid() {
            let start = self.values_offsets[self.curr_idx];
            let end = self.values_offsets[self.curr_idx + 1]; // sentinel always present
            return Some(RawEntry::from(&self.block[start..end]));
        }
        None
    }
}

impl fmt::Display for NormalBlockIter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = f.write_fmt(format_args!(
            r#"
BlockIter
    count: {},
    key_offsets: {:?},
    values_offsets: {:?},
    curr_idx: {}
"#,
            self.key_offsets.len(),
            self.key_offsets,
            self.values_offsets,
            self.curr_idx
        ));
        Ok(())
    }
}

// BlockIter serves as both the default index block format and data block format.
// Future formats implement these traits independently.
impl IndexBlockIter for NormalBlockIter {
    fn from_block(block: bytes::Bytes) -> StorageResult<Self> {
        NormalBlockIter::new(block)
    }
}

impl DataBlockIter for NormalBlockIter {
    fn from_block(block: bytes::Bytes) -> StorageResult<Self> {
        NormalBlockIter::new(block)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use crate::{
        iterators::storage_iter::{AsArray, StorageIter},
        row_key::BinaryKey,
    };

    use super::{NormalBlockIter, RawEntry};

    fn key(bytes: &'static [u8]) -> BinaryKey<'static> {
        BinaryKey::from(bytes)
    }

    fn raw_entry_bytes<'a>(entry: RawEntry<'a>) -> &'a [u8] {
        entry.as_array()
    }

    // Build a raw block from a list of (key, value) pairs.
    // Layout: count(u32) | key_offsets+sentinel([u32]) | value_offsets+sentinel([u32]) | keys | values
    fn build_block(entries: &[(&[u8], &[u8])]) -> Bytes {
        let count = entries.len();
        let header_size = 4 + (count + 1) * 4 + (count + 1) * 4;

        let mut key_off = header_size;
        let mut key_offsets: Vec<usize> = entries
            .iter()
            .map(|(k, _)| {
                let o = key_off;
                key_off += k.len();
                o
            })
            .collect();
        key_offsets.push(key_off); // key sentinel

        let mut val_off = key_off;
        let mut val_offsets: Vec<usize> = entries
            .iter()
            .map(|(_, v)| {
                let o = val_off;
                val_off += v.len();
                o
            })
            .collect();
        val_offsets.push(val_off); // value sentinel

        let mut buf = Vec::new();
        buf.extend_from_slice(&(count as u32).to_be_bytes());
        for &o in &key_offsets {
            buf.extend_from_slice(&(o as u32).to_be_bytes());
        }
        for &o in &val_offsets {
            buf.extend_from_slice(&(o as u32).to_be_bytes());
        }
        for (k, _) in entries {
            buf.extend_from_slice(k);
        }
        for (_, v) in entries {
            buf.extend_from_slice(v);
        }
        Bytes::from(buf)
    }

    fn make_iter() -> NormalBlockIter {
        let block = build_block(&[(b"apple", b"v1"), (b"banana", b"v2"), (b"cherry", b"v3")]);
        let mut iter = NormalBlockIter::new(block).unwrap();
        iter.seek_to_first().unwrap();
        iter
    }

    #[test]
    fn test_seek_to_first() {
        let iter = make_iter();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), key(b"apple"));
        assert_eq!(raw_entry_bytes(iter.value().unwrap()), b"v1");
    }

    #[test]
    fn test_next_iterates_all() {
        let mut iter = make_iter();
        assert_eq!(iter.key().unwrap(), key(b"apple"));
        assert_eq!(raw_entry_bytes(iter.value().unwrap()), b"v1");
        iter.next().unwrap();
        assert_eq!(iter.key().unwrap(), key(b"banana"));
        assert_eq!(raw_entry_bytes(iter.value().unwrap()), b"v2");
        iter.next().unwrap();
        assert_eq!(iter.key().unwrap(), key(b"cherry"));
        assert_eq!(raw_entry_bytes(iter.value().unwrap()), b"v3");
        iter.next().unwrap();
        assert!(!iter.valid());
    }

    #[test]
    fn test_seek_exact_match() {
        let mut iter = make_iter();
        iter.seek(&key(b"banana")).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), key(b"banana"));
        assert_eq!(raw_entry_bytes(iter.value().unwrap()), b"v2");
    }

    #[test]
    fn test_seek_between_keys() {
        // "b" sorts between "apple" and "banana"
        let mut iter = make_iter();
        iter.seek(&key(b"b")).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), key(b"banana"));
    }

    #[test]
    fn test_seek_before_first() {
        let mut iter = make_iter();
        iter.seek(&key(b"aaa")).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), key(b"apple"));
    }

    #[test]
    fn test_seek_after_last() {
        let mut iter = make_iter();
        iter.seek(&key(b"zzz")).unwrap();
        assert!(!iter.valid());
    }
}
