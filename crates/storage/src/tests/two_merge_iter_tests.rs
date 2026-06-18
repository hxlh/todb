use bytes::Bytes;

use crate::{
    iterators::{
        data_entry_decode_iter::EntryValue,
        storage_iter::{ForwardIter, IterRead},
        map_iter::MapIter,
        merge_iter::MergeIter,
        two_merge_iter::TwoMergeIter,
    },
    memtable::{MemTable, OwnedMemTableIter},
    tests::helpers::{make_key, make_sst_iter},
};

/// Memtable side maps native entries to (RowKey<'a>, EntryValue<'a>) so it matches
/// SST iteration directly; only memtable needs MapIter.
type MemIter = MapIter<OwnedMemTableIter<Bytes, Bytes>>;

fn mem_iter(mem: &MemTable<Bytes, Bytes>) -> MemIter {
    MapIter::new(mem.iter())
}

#[test]
fn test_memtable_wins_on_overlap() {
    let mem = MemTable::new();
    for i in 0..5u64 {
        mem.put(make_key(i), Bytes::from(format!("mem_{:04}", i)));
    }
    let sst = make_sst_iter(3, 8);

    let mem_merge = MergeIter::new(vec![mem_iter(&mem)]);
    let sst_merge = MergeIter::new(vec![sst]);
    let mut iter = TwoMergeIter::new(mem_merge, sst_merge).unwrap();
    let mut keys = vec![];
    let mut values: Vec<Option<Vec<u8>>> = vec![];
    while iter.valid() {
        let k = iter.key().unwrap();
        let v = match iter.value().unwrap() {
            EntryValue::Put(buf) => Some(buf.to_vec()),
            EntryValue::Delete => None,
        };
        keys.push(u64::from_be_bytes(k.as_bytes().try_into().unwrap()));
        values.push(v);
        iter.next().unwrap();
    }

    assert_eq!(keys, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    for i in 0..5 {
        assert_eq!(values[i].as_deref(), Some(format!("mem_{:04}", i).as_bytes()));
    }
    for i in 5..8usize {
        assert_eq!(values[i].as_deref(), Some(format!("value_{:04}", i).as_bytes()));
    }
}

#[test]
fn test_multiple_memtables_and_ssts() {
    let mem1 = MemTable::new();
    for i in (0..10u64).step_by(2) {
        mem1.put(make_key(i), Bytes::from(format!("m1_{:04}", i)));
    }
    let mem2 = MemTable::new();
    for i in (1..10u64).step_by(2) {
        mem2.put(make_key(i), Bytes::from(format!("m2_{:04}", i)));
    }

    let sst1 = make_sst_iter(0, 5);
    let sst2 = make_sst_iter(5, 10);

    let mem_merge = MergeIter::new(vec![mem_iter(&mem1), mem_iter(&mem2)]);
    let sst_merge = MergeIter::new(vec![sst1, sst2]);
    let mut iter = TwoMergeIter::new(mem_merge, sst_merge).unwrap();

    let mut keys = vec![];
    while iter.valid() {
        let k = iter.key().unwrap();
        keys.push(u64::from_be_bytes(k.as_bytes().try_into().unwrap()));
        iter.next().unwrap();
    }
    assert_eq!(keys, (0..10).collect::<Vec<_>>());
}

#[test]
fn test_seek_across_merge() {
    let mem = MemTable::new();
    for i in [1u64, 3, 5, 7] {
        mem.put(make_key(i), Bytes::from(format!("mem_{:04}", i)));
    }
    let sst = make_sst_iter(0, 10);

    let mem_merge = MergeIter::new(vec![mem_iter(&mem)]);
    let sst_merge = MergeIter::new(vec![sst]);
    let mut iter = TwoMergeIter::new(mem_merge, sst_merge).unwrap();

    let target_bytes = 5u64.to_be_bytes();
    let target = (&target_bytes).into();
    iter.seek(&target).unwrap();

    let k = iter.key().unwrap();
    assert_eq!(u64::from_be_bytes(k.as_bytes().try_into().unwrap()), 5);
    assert_eq!(iter.value().unwrap(), EntryValue::Put(b"mem_0005"));
}

#[test]
fn test_tombstone_shadows_sst() {
    let mem = MemTable::new();
    mem.delete(make_key(3));
    let sst = make_sst_iter(0, 5);

    let mem_merge = MergeIter::new(vec![mem_iter(&mem)]);
    let sst_merge = MergeIter::new(vec![sst]);
    let mut iter = TwoMergeIter::new(mem_merge, sst_merge).unwrap();

    let mut found: Vec<(u64, Option<Vec<u8>>)> = vec![];
    while iter.valid() {
        let k = u64::from_be_bytes(iter.key().unwrap().as_bytes().try_into().unwrap());
        let v = match iter.value().unwrap() {
            EntryValue::Put(buf) => Some(buf.to_vec()),
            EntryValue::Delete => None,
        };
        found.push((k, v));
        iter.next().unwrap();
    }

    let entry3 = found.iter().find(|(k, _)| *k == 3).unwrap();
    assert_eq!(entry3.1, None);
    assert_eq!(found.len(), 5);
}
