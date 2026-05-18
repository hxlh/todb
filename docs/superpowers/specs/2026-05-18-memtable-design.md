# MemTable Design

Date: 2026-05-18

## Goal

Implement an in-memory write buffer backed by `crossbeam-skiplist`, shared via
`Arc<MemTable<K, V>>`. Supports concurrent lock-free reads and writes, tracks
approximate memory usage, and exposes a `StorageIter`-compatible iterator for
flushing to SST.

## Data Structures

```
crates/storage/src/memtable.rs
├── pub trait MemSize
│     └── fn mem_size(&self) -> usize
├── pub enum Entry<V>
│     ├── Put(V)
│     └── Delete
├── pub struct MemTable<K, V>   (K: Ord + MemSize, V: MemSize)
│     ├── map: SkipMap<K, Entry<V>>
│     └── size: AtomicUsize
└── pub struct MemTableIter<'a, K, V>
      └── current: Option<crossbeam_skiplist::map::Entry<'a, K, Entry<V>>>
```

## Public API

```rust
impl<K, V> MemTable<K, V>
where
    K: Ord + Send + MemSize + 'static,
    V: Send + MemSize + 'static,
{
    pub fn new() -> Self
    pub fn put(&self, key: K, value: V)
    pub fn delete(&self, key: K)
    pub fn get(&self, key: &K) -> Option<Entry<&V>>
    pub fn estimate_memory(&self) -> usize
}
```

Sharing: callers wrap in `Arc<MemTable<K, V>>`. Flush tasks clone the Arc.
No `inner()` method needed.

## Iteration

`MemTable<K, V>` implements `IntoIterator` for `&MemTable<K, V>`, returning
`MemTableIter<'_, K, V>`. This is more idiomatic than an explicit `iter()`
method.

`MemTableIter` implements `StorageIter`:
- `Key<'a> = &'a K`
- `Value<'a> = Option<&'a V>` — `None` means Delete tombstone, `Some` means Put

`seek` uses lower_bound semantics (first key >= target), consistent with
`StorageIter`.

## Memory Estimation

`estimate_memory` uses an `AtomicUsize` with `Relaxed` ordering:
- `put(key, value)`: adds `key.mem_size() + value.mem_size()`
- `delete(key)`: adds `key.mem_size()` only

Duplicate writes accumulate size without correction. This is intentional:
`estimate_memory` is a flush trigger hint, not an exact accounting.

`MemSize` implementations:
- `Bytes`: `self.len()`
- `Vec<u8>`: `self.len()`
- `String`: `self.len()`

## Concurrency

`crossbeam_skiplist::SkipMap` is lock-free and concurrent-safe. `MemTable`
adds no additional locks. `AtomicUsize` with `Relaxed` ordering is sufficient
for the approximate size counter.

## Test Plan

### Basic reads/writes
1. `put` then `get` returns correct value
2. `delete` then `get` returns `Entry::Delete`
3. Repeated `put` on same key — `get` returns latest value
4. `get` on missing key returns `None`

### Memory estimation
5. Empty MemTable: `estimate_memory() == 0`
6. After `put`: size accumulates `key.mem_size() + value.mem_size()`
7. After `delete`: size accumulates `key.mem_size()` only

### Iterator
8. `seek_to_first` positions at the smallest key
9. `next` traverses all entries in ascending key order (Put and Delete mixed)
10. `seek` exact match — positions at that key
11. `seek` between two keys — positions at the next key (lower_bound)
12. `seek` past the largest key — `valid() == false`
