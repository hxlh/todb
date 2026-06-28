use std::{
    ops::Bound,
    sync::{
        Arc,
        atomic::{self, AtomicUsize},
    },
};

use bytes::Bytes;
use crossbeam_skiplist::SkipMap;
use ouroboros::self_referencing;

use crate::{
    errors::StorageResult,
    iterators::{
        data_entry_decode_iter::EntryValue,
        map_iter::MappedStorageIter,
        storage_iter::{ForwardIter, IterBase, IterRead, ReverseIter},
    },
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

    /// Number of entries currently in the table.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the table holds no entries.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
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

impl<K, V> IterBase for OwnedMemTableIter<K, V>
where
    K: Ord + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    type Key<'a> = &'a K;
    type Value<'a> = &'a Entry<V>;
}

impl<K, V> IterRead for OwnedMemTableIter<K, V>
where
    K: Ord + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    fn valid(&self) -> bool {
        self.borrow_item().is_some()
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        self.borrow_item().as_ref().map(|(k, _)| k)
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        self.borrow_item().as_ref().map(|(_, v)| v)
    }
}

impl<K, V> ForwardIter for OwnedMemTableIter<K, V>
where
    K: Ord + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
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

    /// Generic lower-bound seek.
    ///
    /// Note: the production memtable path does NOT call this — `OwnedMemTableIter`
    /// is always wrapped in `MapIter`, whose `seek` forwards to `seek_mapped`
    /// (the zero-copy `Bytes` specialization that feeds `&[u8]` straight to
    /// `lower_bound`). This is kept correct for the `ForwardIter` contract and
    /// for generic `K` (non-`Bytes` users bypass `MapIter`, which only targets
    /// `Bytes`). The `(*target).clone()` below is an `Arc` bump for `Bytes`,
    /// not a `copy_from_slice`.
    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
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
}

impl<K, V> ReverseIter for OwnedMemTableIter<K, V>
where
    K: Ord + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    fn seek_to_first(&mut self) -> StorageResult<()> {
        let map = self.with_map(|m| m.clone());
        let mut new = OwnedMemTableIterBuilder {
            map,
            current_builder: |map| map.back(),
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

    /// Generic upper-bound seek (last key <= target).
    ///
    /// Note: the production memtable path does NOT call this — `MapIter`'s
    /// `ReverseIter::seek` forwards to `seek_mapped_for_prev` (the zero-copy
    /// `Bytes` specialization). Kept correct for the `ReverseIter` contract
    /// and for generic `K`. The `(*target).clone()` is an `Arc` bump for
    /// `Bytes`, not a `copy_from_slice`.
    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        let map = self.with_map(|m| m.clone());
        let target = (*target).clone();
        let mut new = OwnedMemTableIterBuilder {
            map,
            current_builder: |map| map.upper_bound(Bound::Included(&target)),
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
            *current = current.as_ref().and_then(|e| e.prev());
            current
                .as_ref()
                .map(|e| (e.key().clone(), e.value().clone()))
        });
        self.with_mut(|fields| *fields.item = kv);
        Ok(())
    }
}

impl MappedStorageIter for OwnedMemTableIter<Bytes, Bytes> {
    type MappedKey<'a> = RowKey<'a>;
    type MappedValue<'a> = EntryValue<'a>;

    fn map_key<'a>(key: Self::Key<'a>) -> RowKey<'a> {
        (key).into()
    }

    fn map_value<'a>(value: Self::Value<'a>) -> EntryValue<'a> {
        match value {
            Entry::Put(v) => EntryValue::Put(v.as_ref()),
            Entry::Delete => EntryValue::Delete,
        }
    }

    fn seek_mapped(&mut self, target: &RowKey<'_>) -> StorageResult<()> {
        let map = self.with_map(|m| m.clone());
        let mut new = OwnedMemTableIterBuilder {
            map,
            current_builder: |map| map.lower_bound(Bound::Included(target.as_bytes())),
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

    fn seek_mapped_for_prev(&mut self, target: &RowKey<'_>) -> StorageResult<()> {
        let map = self.with_map(|m| m.clone());
        let mut new = OwnedMemTableIterBuilder {
            map,
            current_builder: |map| map.upper_bound(Bound::Included(target.as_bytes())),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iterators::storage_iter::ForwardIter;
    use bytes::Bytes;

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
        ForwardIter::seek(&mut iter, &&target).unwrap();
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
        ForwardIter::seek(&mut iter, &&target).unwrap();
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
        ForwardIter::seek(&mut iter, &&target).unwrap();
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
        ForwardIter::seek(&mut iter, &&target).unwrap();
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
        ForwardIter::seek_to_first(&mut iter).unwrap();
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
        ForwardIter::seek_to_first(&mut iter).unwrap();

        assert_eq!(*iter.key().unwrap(), k("a"));
        assert_eq!(iter.value(), Some(&Entry::Put(v("1"))));
        ForwardIter::next(&mut iter).unwrap();

        assert_eq!(*iter.key().unwrap(), k("b"));
        assert_eq!(iter.value(), Some(&Entry::Delete)); // tombstone
        ForwardIter::next(&mut iter).unwrap();

        assert_eq!(*iter.key().unwrap(), k("c"));
        assert_eq!(iter.value(), Some(&Entry::Put(v("3"))));
        ForwardIter::next(&mut iter).unwrap();

        assert!(!iter.valid());
    }

    // Empty memtable — iterator is invalid after seek_to_first.
    #[test]
    fn test_empty_memtable_is_invalid() {
        let mem: MemTable<Bytes, Bytes> = MemTable::new();
        let mut iter = mem.iter();
        ForwardIter::seek_to_first(&mut iter).unwrap();
        assert!(!iter.valid());
    }
}
