use crate::{
    block::InMemoryBlockReader,
    iterators::{concat_iter::ConcatIter, iter::StorageIter, sst_iter::SstIter},
    row_key::RowKey,
};

use super::helpers::{make_key, make_sst_iter};

// An empty iterator list is immediately invalid.
#[test]
fn test_empty_list_is_invalid() {
    let mut iter: ConcatIter<SstIter<InMemoryBlockReader>> = ConcatIter::new(vec![]);
    iter.seek_to_first().unwrap();
    assert!(!iter.valid());
}

// A single SST: seek_to_first visits all keys in order.
#[test]
fn test_single_sst_seek_to_first() {
    let mut iter = ConcatIter::new(vec![make_sst_iter(0, 5)]);
    iter.seek_to_first().unwrap();
    for i in 0..5u64 {
        assert!(iter.valid(), "expected valid at i={}", i);
        assert_eq!(iter.key().unwrap(), (&make_key(i)).into());
        iter.next().unwrap();
    }
    assert!(!iter.valid());
}

// Multiple SSTs: seek_to_first traverses all keys across SST boundaries in order.
#[test]
fn test_multi_sst_seek_to_first_crosses_boundary() {
    let mut iter = ConcatIter::new(vec![
        make_sst_iter(0, 3),
        make_sst_iter(3, 6),
        make_sst_iter(6, 9),
    ]);
    iter.seek_to_first().unwrap();
    for i in 0..9u64 {
        assert!(iter.valid(), "expected valid at i={}", i);
        assert_eq!(iter.key().unwrap(), (&make_key(i)).into());
        iter.next().unwrap();
    }
    assert!(!iter.valid());
}

// next() at the last key of one SST advances to the first key of the next SST.
#[test]
fn test_next_crosses_sst_boundary() {
    let mut iter = ConcatIter::new(vec![make_sst_iter(0, 2), make_sst_iter(2, 4)]);
    iter.seek_to_first().unwrap();
    let mut keys: Vec<u64> = vec![];
    while iter.valid() {
        let k = iter.key().unwrap();
        keys.push(u64::from_be_bytes(k.as_bytes().try_into().unwrap()));
        iter.next().unwrap();
    }
    assert_eq!(keys, vec![0, 1, 2, 3]);
}

// seek to a key that exists inside one of the SSTs.
#[test]
fn test_seek_exact_match_in_second_sst() {
    let mut iter = ConcatIter::new(vec![make_sst_iter(0, 5), make_sst_iter(5, 10)]);
    iter.seek(&(&make_key(7)).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(7)).into());
}

// seek to a key in a gap between SSTs lands on the first key of the next SST.
#[test]
fn test_seek_between_ssts_lands_on_next_sst_first_key() {
    let mut iter = ConcatIter::new(vec![make_sst_iter(0, 3), make_sst_iter(10, 13)]);
    iter.seek(&(&make_key(5)).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(10)).into());
}

// seek past the last key in all SSTs results in invalid.
#[test]
fn test_seek_past_all_keys_is_invalid() {
    let mut iter = ConcatIter::new(vec![make_sst_iter(0, 5), make_sst_iter(5, 10)]);
    iter.seek(&(&make_key(u64::MAX)).into()).unwrap();
    assert!(!iter.valid());
}

// An empty SST in the middle is skipped transparently.
#[test]
fn test_empty_sst_in_middle_is_skipped() {
    let mut iter = ConcatIter::new(vec![
        make_sst_iter(0, 3),
        make_sst_iter(5, 5), // empty
        make_sst_iter(10, 13),
    ]);
    iter.seek_to_first().unwrap();
    let mut keys: Vec<u64> = vec![];
    while iter.valid() {
        let k = iter.key().unwrap();
        keys.push(u64::from_be_bytes(k.as_bytes().try_into().unwrap()));
        iter.next().unwrap();
    }
    assert_eq!(keys, vec![0, 1, 2, 10, 11, 12]);
}
