use core::fmt;
use std::ops::Deref;

use bytes::Bytes;
use tracing::debug;

use crate::{
    errors::StorageResult,
    iterators::storage_iter::{
        AsArray, DataBlockIter, ForwardIter, IndexBlockIter, IterBase, IterRead, ReverseIter,
    },
    row_key::RowKey,
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

/// Block-format iterator, generic over the block's storage type `B`.
///
/// - `B = Bytes`: owned, `'static` — the LSM path. `from_block(Bytes)` stores it
///   directly (a move, zero-copy).
/// - `B = PinGuard`: a pinned cache frame (`'static` via `Arc` to the pool),
///   zero-copy — held directly with no materialization.
#[allow(dead_code)]
pub struct NormalBlockIter<B: Deref<Target = [u8]> = Bytes> {
    block: B,
    key_offsets: Vec<usize>,
    values_offsets: Vec<usize>,
    count: usize,
    /// `None` means the iterator is invalid (exhausted or not positioned).
    curr: Option<usize>,
}

impl<B: Deref<Target = [u8]>> NormalBlockIter<B> {
    pub fn new(block: B) -> StorageResult<Self> {
        let mut s = Self {
            block,
            key_offsets: vec![],
            values_offsets: vec![],
            curr: None,
            count: 0,
        };

        s.parse_header();
        Ok(s)
    }

    fn parse_header(&mut self) {
        self.reset();

        let block = self.block.deref();
        let mut start = 0;
        // parse count
        let buf = &block[start..start + size_of::<u32>()];
        let count = u32::from_be_bytes(buf.try_into().unwrap()) as usize;
        start += size_of::<u32>();

        // parse key offsets (count + 1, last is sentinel)
        for _ in 0..=count {
            let buf = &block[start..start + size_of::<u32>()];
            let offset = u32::from_be_bytes(buf.try_into().unwrap()) as usize;
            self.key_offsets.push(offset);
            start += size_of::<u32>();
        }

        // parse values offsets (count + 1, last is sentinel)
        for _ in 0..=count {
            let buf = &block[start..start + size_of::<u32>()];
            let offset = u32::from_be_bytes(buf.try_into().unwrap()) as usize;
            self.values_offsets.push(offset);
            start += size_of::<u32>();
        }

        self.count = count;
        debug!("read block: count= {}", &self.count);
    }

    fn reset(&mut self) {
        // clean
        self.curr = None;
        self.key_offsets.clear();
        self.values_offsets.clear();
    }
}
impl<B: Deref<Target = [u8]>> IterBase for NormalBlockIter<B> {
    type Key<'a> = RowKey<'a>;
    type Value<'a> = RawEntry<'a>;
}

impl<B: Deref<Target = [u8]>> IterRead for NormalBlockIter<B> {
    fn valid(&self) -> bool {
        self.curr.is_some()
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        let i = self.curr?;
        let start = self.key_offsets[i];
        let end = self.key_offsets[i + 1]; // sentinel always present
        Some(RowKey::from(&self.block[start..end]))
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        let i = self.curr?;
        let start = self.values_offsets[i];
        let end = self.values_offsets[i + 1]; // sentinel always present
        Some(RawEntry::from(&self.block[start..end]))
    }
}

impl<B: Deref<Target = [u8]>> ForwardIter for NormalBlockIter<B> {
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.curr = (self.count > 0).then_some(0);
        Ok(())
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        // Binary search for the first key >= target (lower_bound).
        let mut lo = 0usize;
        let mut hi = self.count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            self.curr = Some(mid);
            match self.key() {
                Some(k) if k < *target => lo = mid + 1,
                _ => hi = mid,
            }
        }
        self.curr = (lo < self.count).then_some(lo);
        Ok(())
    }

    fn next(&mut self) -> StorageResult<()> {
        if let Some(i) = self.curr {
            self.curr = (i + 1 < self.count).then_some(i + 1);
        }
        Ok(())
    }
}

impl<B: Deref<Target = [u8]>> ReverseIter for NormalBlockIter<B> {
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.curr = (self.count > 0).then_some(self.count - 1);
        Ok(())
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        // Binary search for upper_bound (first key > target), then subtract 1.
        let mut lo = 0usize;
        let mut hi = self.count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            self.curr = Some(mid);
            match self.key() {
                Some(k) if k <= *target => lo = mid + 1,
                _ => hi = mid,
            }
        }
        // lo = first index where key > target (or count if all keys <= target).
        // Position at lo - 1 (the last key <= target), or None if lo == 0.
        self.curr = lo.checked_sub(1);
        Ok(())
    }

    fn next(&mut self) -> StorageResult<()> {
        self.curr = self.curr.and_then(|i| i.checked_sub(1));
        Ok(())
    }
}

impl<B: Deref<Target = [u8]>> fmt::Display for NormalBlockIter<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = f.write_fmt(format_args!(
            r#"
BlockIter
    count: {},
    key_offsets: {:?},
    values_offsets: {:?},
    curr: {:?}
"#,
            self.key_offsets.len(),
            self.key_offsets,
            self.values_offsets,
            self.curr
        ));
        Ok(())
    }
}

// BlockIter serves as both the default index block format and data block format.
// Future formats implement these traits independently.
impl<B: Deref<Target = [u8]>> IndexBlockIter for NormalBlockIter<B> {
    type Block = B;

    fn from_block(block: B) -> StorageResult<Self> {
        NormalBlockIter::new(block)
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        // Reuse the ForwardIter lower-bound implementation; NormalBlockIter
        // serves as both index and data block format, and index-block seek
        // is the same lower_bound semantics.
        ForwardIter::seek(self, target)
    }
}

impl<B: Deref<Target = [u8]>> DataBlockIter for NormalBlockIter<B> {
    type Block = B;

    fn from_block(block: B) -> StorageResult<Self> {
        NormalBlockIter::new(block)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use crate::{
        iterators::storage_iter::{AsArray, ForwardIter, IterRead},
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

    // Generic storage: NormalBlockIter<B> is not pinned to Bytes. A Vec-backed
    // block (the shape a borrowed WAL PinGuard guard will take) parses and
    // iterates identically. De-risks the future zero-copy borrowed-guard path.
    #[test]
    fn test_generic_storage_vec() {
        let block = build_block(&[(b"apple", b"v1"), (b"banana", b"v2")]).to_vec();
        let mut iter: NormalBlockIter<Vec<u8>> = NormalBlockIter::new(block).unwrap();
        iter.seek_to_first().unwrap();
        assert_eq!(iter.key().unwrap(), key(b"apple"));
        assert_eq!(raw_entry_bytes(iter.value().unwrap()), b"v1");
        iter.next().unwrap();
        assert_eq!(iter.key().unwrap(), key(b"banana"));
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
