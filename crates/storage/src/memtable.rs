use std::{
    ops::Bound,
    sync::{
        Arc,
        atomic::{self, AtomicUsize, Ordering},
    },
};

use bytes::Bytes;
use crossbeam_skiplist::SkipMap;
use ouroboros::self_referencing;

use crate::{
    errors::StorageResult,
    iterators::{entry_decode_iter::EntryValue, storage_iter::StorageIter, map_iter::MappedStorageIter},
    row_key::RowKey,
};

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
    K: Ord + Send + Clone + MemSize + 'static,
    V: Send + Clone + MemSize + 'static,
{
    map: Arc<SkipMap<K, Entry<V>>>,
    memory_size: AtomicUsize,
}

impl<K, V> MemTable<K, V>
where
    K: Ord + Send + Clone + MemSize + 'static,
    V: Send + Clone + MemSize + 'static,
{
    pub fn new() -> Self {
        Self {
            map: Arc::new(SkipMap::new()),
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

    /// Return a clone of the internal Arc<SkipMap> for use in read-path iterators.
    pub fn map_arc(&self) -> Arc<SkipMap<K, Entry<V>>> {
        self.map.clone()
    }

    /// Return an owned iterator backed by `Arc<SkipMap>`. The iterator is
    /// `'static` and can be used in `TwoMergeIter` without lifetime issues.
    pub fn iter(&self) -> OwnedMemTableIter<K, V> {
        OwnedMemTableIter::new(self.map.clone(), |_map| None, None)
    }
}

/// Iterator over a [`MemTable`]. Borrows the skip list — use for unit tests
/// and non-`TwoMergeIter` contexts where `'static` is not required.
pub struct MemTableIter<'m, K, V>
where
    K: Ord + Send + 'static,
    V: Send + 'static,
{
    map: &'m SkipMap<K, Entry<V>>,
    current: Option<crossbeam_skiplist::map::Entry<'m, K, Entry<V>>>,
}

impl<'m, K, V> MemTableIter<'m, K, V>
where
    K: Ord + Send + 'static,
    V: Send + 'static,
{
    pub fn new(map: &'m SkipMap<K, Entry<V>>) -> Self {
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
        = &'v Entry<V>
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
        self.current.as_ref().map(|e| e.key())
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        self.current.as_ref().map(|e| e.value())
    }
}

/// Owned iterator over a [`MemTable`]. Holds `Arc<SkipMap>` so it is `'static`.
/// Uses an ouroboros self-referential struct to keep a live `Entry` for
/// O(1) `next()` via `entry.next()` (hybrid: `lower_bound` to seek,
/// `Entry::next()` to advance).
#[self_referencing]
pub struct OwnedMemTableIter<K, V>
where
    K: Ord + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    map: Arc<SkipMap<K, Entry<V>>>,
    #[borrows(map)]
    #[not_covariant]
    current: Option<crossbeam_skiplist::map::Entry<'this, K, Entry<V>>>,
    item: Option<(K, Entry<V>)>,
}

impl<K, V> StorageIter for OwnedMemTableIter<K, V>
where
    K: Ord + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    type Key<'a> = &'a K;
    type Value<'a>
        = &'a Entry<V>
    where
        Self: 'a;

    fn valid(&self) -> bool {
        self.borrow_item().is_some()
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        let map = self.with_map(|m| m.clone());
        let mut new = OwnedMemTableIterBuilder {
            map,
            current_builder: |map| map.iter().next(),
            item: None,
        }
        .build();
        let kv = new.with_current(|current| {
            current
                .as_ref()
                .map(|e| (e.key().clone(), e.value().clone()))
        });
        new.with_mut(|fields| *fields.item = kv);
        *self = new;
        Ok(())
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        let map = self.with_map(|m| m.clone());
        let target = (*target).clone();
        let mut new = OwnedMemTableIterBuilder {
            map,
            current_builder: |map| map.lower_bound(Bound::Included(&target)),
            item: None,
        }
        .build();
        let kv = new.with_current(|current| {
            current
                .as_ref()
                .map(|e| (e.key().clone(), e.value().clone()))
        });
        new.with_mut(|fields| *fields.item = kv);
        *self = new;
        Ok(())
    }

    fn next(&mut self) -> StorageResult<()> {
        let kv = self.with_current_mut(|current| {
            *current = current.as_ref().and_then(|e| e.next());
            current
                .as_ref()
                .map(|e| (e.key().clone(), e.value().clone()))
        });
        self.with_mut(|fields| *fields.item = kv);
        Ok(())
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        self.borrow_item().as_ref().map(|(k, _)| k)
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        self.borrow_item().as_ref().map(|(_, v)| v)
    }
}

impl OwnedMemTableIter<Bytes, Bytes> {
    /// Seek using a borrowed byte slice. Allocates a temporary `Bytes`
    /// for the search; the allocation is short-lived (only for the
    /// `seek` call) because `lower_bound` only reads the key.
    pub fn seek_by_bytes(&mut self, target: &[u8]) {
        let key = Bytes::copy_from_slice(target);
        let _ = self.seek(&&key);
    }
}

impl MappedStorageIter for OwnedMemTableIter<Bytes, Bytes> {
    type MappedKey<'a> = RowKey<'a>;
    type MappedValue<'a> = EntryValue<'a>;

    fn map_key<'a>(key: Self::Key<'a>) -> RowKey<'a> {
        (key).into()
    }

    fn map_value<'a>(
        value: Self::Value<'a>,
    ) -> EntryValue<'a> {
        match value {
            Entry::Put(v) => EntryValue::Put(v.as_ref()),
            Entry::Delete => EntryValue::Delete,
        }
    }

    fn seek_mapped(&mut self, target: &RowKey<'_>) -> StorageResult<()> {
        self.seek_by_bytes(target.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::iterators::storage_iter::StorageIter;

    fn k(s: &str) -> Bytes {
        Bytes::from(s.to_string())
    }
    fn v(s: &str) -> Bytes {
        Bytes::from(s.to_string())
    }

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

    // next advances in ascending key order.
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

    // Empty memtable — iterator is invalid after seek_to_first.
    #[test]
    fn test_empty_memtable_is_invalid() {
        let mem: MemTable<Bytes, Bytes> = MemTable::new();
        let mut iter = mem.iter();
        iter.seek_to_first().unwrap();
        assert!(!iter.valid());
    }
}
