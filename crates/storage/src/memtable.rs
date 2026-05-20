use std::{
    ops::Bound,
    sync::atomic::{self, AtomicUsize, Ordering},
};

use crossbeam_skiplist::SkipMap;

use crate::{errors::StorageResult, iterators::iter::StorageIter};

/// Trait for types that can report their approximate heap size.
pub trait MemSize {
    fn mem_size(&self) -> usize;
}

impl MemSize for bytes::Bytes {
    fn mem_size(&self) -> usize {
        self.len()
    }
}

impl MemSize for Vec<u8> {
    fn mem_size(&self) -> usize {
        self.len()
    }
}

impl MemSize for String {
    fn mem_size(&self) -> usize {
        self.len()
    }
}

/// A value stored in the MemTable: either a live value or a delete tombstone.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry<V> {
    Put(V),
    Delete,
}

/// In-memory write buffer backed by a lock-free skip list.
///
/// Share via `Arc<MemTable<K, V>>`. All operations are concurrent-safe.
pub struct MemTable<K, V>
where
    K: Ord + Send + MemSize + 'static,
    V: Send + MemSize + 'static,
{
    map: SkipMap<K, Entry<V>>,
    memory_size: AtomicUsize,
}

impl<K, V> MemTable<K, V>
where
    K: Ord + Send + MemSize + 'static,
    V: Send + MemSize + 'static,
{
    pub fn new() -> Self {
        Self {
            map: SkipMap::new(),
            memory_size: AtomicUsize::new(0),
        }
    }

    /// Insert or overwrite a key-value pair. Accumulates key + value size.
    pub fn put(&self, key: K, value: V) {
        self.memory_size
            .fetch_add(key.mem_size() + value.mem_size(), atomic::Ordering::Relaxed);
        self.map.insert(key, Entry::Put(value));
    }

    /// Write a delete tombstone. Accumulates key size only.
    pub fn delete(&self, key: K) {
        self.memory_size
            .fetch_add(key.mem_size(), atomic::Ordering::Relaxed);
        self.map.insert(key, Entry::Delete);
    }

    /// Approximate memory usage in bytes.
    pub fn estimate_memory(&self) -> usize {
        self.memory_size.load(atomic::Ordering::Relaxed)
    }

    /// Return an iterator positioned before the first entry.
    pub fn iter(&self) -> MemTableIter<'_, K, Entry<V>> {
        MemTableIter::new(&self.map)
    }
}

/// Iterator over a [`MemTable`]. Implements [`StorageIter`].
///
/// `Key<'a> = &'a K` — borrows directly from the skip list node.
/// `Value<'a> = Option<&'a V>` — `None` means Delete tombstone.
pub struct MemTableIter<'m, K, V>
where
    K: Ord + Send + 'static,
    V: Send + 'static,
{
    map: &'m SkipMap<K, V>,
    current: Option<crossbeam_skiplist::map::Entry<'m, K, V>>,
}

impl<'m, K, V> MemTableIter<'m, K, V>
where
    K: Ord + Send + 'static,
    V: Send + 'static,
{
    pub fn new(map: &'m SkipMap<K, V>) -> Self {
        Self { map, current: None }
    }
}

impl<'m, K, V> StorageIter for MemTableIter<'m, K, V>
where
    K: Ord + Send + 'static,
    V: Send + 'static,
{
    type Key<'k> = &'k K;
    type Value<'v>
        = &'v V
    where
        Self: 'v;

    fn valid(&self) -> bool {
        self.current.is_some()
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.current = self.map.iter().next();
        Ok(())
    }

    fn seek<'k>(&mut self, target: &Self::Key<'k>) -> StorageResult<()> {
        self.current = self.map.lower_bound(Bound::Included(target));
        Ok(())
    }

    fn next(&mut self) -> StorageResult<()> {
        if let Some(v) = &self.current {
            self.current = v.next();
        }
        Ok(())
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        if let Some(v) = &self.current {
            return Some(v.key());
        }
        None
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        if let Some(v) = &self.current {
            return Some(v.value());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::iterators::iter::StorageIter;

    fn k(s: &str) -> Bytes {
        Bytes::from(s.to_string())
    }
    fn v(s: &str) -> Bytes {
        Bytes::from(s.to_string())
    }

    // --- Basic reads/writes ---

    // put a key then seek to it — iterator should land on that key with the correct value.
    #[test]
    fn test_put_then_seek() {
        let mem = MemTable::new();
        mem.put(k("hello"), v("world"));
        let mut iter = mem.iter();
        let target = k("hello");
        iter.seek(&&target).unwrap();
        assert!(iter.valid());
        assert_eq!(*iter.key().unwrap(), k("hello"));
        assert_eq!(iter.value(), Some(&Entry::Put(v("world"))));
    }

    // delete a previously written key — seek should land on it and value should be None
    // (tombstone), not the old value.
    #[test]
    fn test_delete_then_seek() {
        let mem = MemTable::new();
        mem.put(k("hello"), v("world"));
        mem.delete(k("hello"));
        let mut iter = mem.iter();
        let target = k("hello");
        iter.seek(&&target).unwrap();
        assert!(iter.valid());
        assert_eq!(*iter.key().unwrap(), k("hello"));
        assert_eq!(iter.value(), Some(&Entry::Delete)); // tombstone
    }

    // writing the same key twice — seek should return the latest value, not the first.
    #[test]
    fn test_overwrite_returns_latest() {
        let mem = MemTable::new();
        mem.put(k("key"), v("v1"));
        mem.put(k("key"), v("v2"));
        let mut iter = mem.iter();
        let target = k("key");
        iter.seek(&&target).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.value(), Some(&Entry::Put(v("v2"))));
    }

    // seek to a key that was never written — iterator lands on the next key or is invalid.
    #[test]
    fn test_seek_missing_key_lands_on_next() {
        let mem = MemTable::new();
        mem.put(k("a"), v("1"));
        mem.put(k("c"), v("3"));
        let mut iter = mem.iter();
        let target = k("b"); // not present
        iter.seek(&&target).unwrap();
        assert!(iter.valid());
        assert_eq!(*iter.key().unwrap(), k("c")); // lands on next key
    }

    // --- Memory estimation ---

    // a freshly created MemTable has zero memory usage.
    #[test]
    fn test_empty_estimate_memory() {
        let mem: MemTable<Bytes, Bytes> = MemTable::new();
        assert_eq!(mem.estimate_memory(), 0);
    }

    // put accumulates key.mem_size() + value.mem_size() into estimate_memory.
    #[test]
    fn test_put_accumulates_size() {
        let mem = MemTable::new();
        let key = k("abc");
        let val = v("xyz");
        let expected = key.mem_size() + val.mem_size();
        mem.put(key, val);
        assert_eq!(mem.estimate_memory(), expected);
    }

    // delete only charges key.mem_size() — tombstone carries no value bytes.
    #[test]
    fn test_delete_accumulates_key_size_only() {
        let mem: MemTable<Bytes, Bytes> = MemTable::new();
        let key = k("abc");
        let expected = key.mem_size();
        mem.delete(key);
        assert_eq!(mem.estimate_memory(), expected);
    }

    // --- Iterator ---

    // seek_to_first positions at the smallest key regardless of insertion order.
    #[test]
    fn test_seek_to_first() {
        let mem = MemTable::new();
        mem.put(k("b"), v("2"));
        mem.put(k("a"), v("1"));
        mem.put(k("c"), v("3"));
        let mut iter = mem.iter();
        iter.seek_to_first().unwrap();
        assert!(iter.valid());
        assert_eq!(*iter.key().unwrap(), k("a"));
    }

    // next advances in ascending key order; Delete entries appear as None values,
    // Put entries appear as Some. After the last entry valid() returns false.
    #[test]
    fn test_next_traverses_in_order() {
        let mem = MemTable::new();
        mem.put(k("a"), v("1"));
        mem.delete(k("b"));
        mem.put(k("c"), v("3"));
        let mut iter = mem.iter();
        iter.seek_to_first().unwrap();

        assert_eq!(*iter.key().unwrap(), k("a"));
        assert_eq!(iter.value(), Some(&Entry::Put(v("1"))));
        iter.next().unwrap();

        assert_eq!(*iter.key().unwrap(), k("b"));
        assert_eq!(iter.value(), Some(&Entry::Delete)); // tombstone
        iter.next().unwrap();

        assert_eq!(*iter.key().unwrap(), k("c"));
        assert_eq!(iter.value(), Some(&Entry::Put(v("3"))));
        iter.next().unwrap();

        assert!(!iter.valid());
    }

    // seek with an exact key match — iterator lands on that key and returns its value.
    #[test]
    fn test_seek_exact_match() {
        let mem = MemTable::new();
        mem.put(k("a"), v("1"));
        mem.put(k("b"), v("2"));
        mem.put(k("c"), v("3"));
        let mut iter = mem.iter();
        let target = k("b");
        // Key<'k> = &'k Bytes, so seek takes &&Bytes
        iter.seek(&&target).unwrap();
        assert!(iter.valid());
        assert_eq!(*iter.key().unwrap(), k("b"));
        assert_eq!(iter.value(), Some(&Entry::Put(v("2"))));
    }

    // seek with a key that falls between two existing keys — iterator lands on the
    // first key >= target (lower_bound semantics).
    #[test]
    fn test_seek_lower_bound() {
        let mem = MemTable::new();
        mem.put(k("a"), v("1"));
        mem.put(k("c"), v("3"));
        let mut iter = mem.iter();
        let target = k("b"); // between "a" and "c"
        iter.seek(&&target).unwrap();
        assert!(iter.valid());
        assert_eq!(*iter.key().unwrap(), k("c"));
    }

    // seek past the largest key — iterator should be invalid immediately.
    #[test]
    fn test_seek_past_last_key() {
        let mem = MemTable::new();
        mem.put(k("a"), v("1"));
        mem.put(k("b"), v("2"));
        let mut iter = mem.iter();
        let target = k("z");
        iter.seek(&&target).unwrap();
        assert!(!iter.valid());
    }
}
