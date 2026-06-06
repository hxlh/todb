use crate::{
    block::InMemoryBlockReader,
    iterators::{iter::StorageIter, merge_iter::MergeIter, sst_iter::SstIter},
    row_key::RowKey,
};

use super::helpers::{collect_keys, make_key, make_sst_iter};

// Empty iterator list is immediately invalid.
#[test]
fn test_empty_is_invalid() {
    let mut iter: MergeIter<SstIter<InMemoryBlockReader>> = MergeIter::new(vec![]);
    iter.seek_to_first().unwrap();
    assert!(!iter.valid());
}

// Single SST: seek_to_first visits all keys in order.
#[test]
fn test_single_sst_seek_to_first() {
    let mut iter = MergeIter::new(vec![make_sst_iter(0, 5)]);
    iter.seek_to_first().unwrap();
    assert_eq!(collect_keys(&mut iter), vec![0, 1, 2, 3, 4]);
}

// Non-overlapping SSTs provided in any order are merged into a sorted sequence.
#[test]
fn test_non_overlapping_ssts_merged_in_order() {
    let mut iter = MergeIter::new(vec![
        make_sst_iter(5, 8),
        make_sst_iter(0, 3),
        make_sst_iter(3, 5),
    ]);
    iter.seek_to_first().unwrap();
    assert_eq!(collect_keys(&mut iter), vec![0, 1, 2, 3, 4, 5, 6, 7]);
}

// Overlapping SSTs: lower level (level=0) entry comes before level=1 for the same key.
#[test]
fn test_overlapping_ssts_lower_level_comes_first() {
    let mut iter = MergeIter::new(vec![
        make_sst_iter(0, 3), // level 0
        make_sst_iter(1, 4), // level 1
    ]);
    iter.seek_to_first().unwrap();
    let keys = collect_keys(&mut iter);
    let pos_first = keys.iter().position(|&k| k == 1).unwrap();
    let pos_last = keys.iter().rposition(|&k| k == 1).unwrap();
    assert!(pos_first < pos_last, "level=0 entry for key=1 must precede level=1");
}

// seek lands on the first key >= target across all SSTs.
#[test]
fn test_seek_across_ssts() {
    let mut iter = MergeIter::new(vec![make_sst_iter(0, 5), make_sst_iter(5, 10)]);
    iter.seek(&(&make_key(7)).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(7)).into());
}

// seek past all keys results in invalid.
#[test]
fn test_seek_past_all_keys_is_invalid() {
    let mut iter = MergeIter::new(vec![make_sst_iter(0, 5)]);
    iter.seek(&(&make_key(u64::MAX)).into()).unwrap();
    assert!(!iter.valid());
}

// seek_to_first resets the iterator correctly after exhaustion.
#[test]
fn test_seek_to_first_is_repeatable() {
    let mut iter = MergeIter::new(vec![make_sst_iter(0, 3), make_sst_iter(3, 6)]);
    iter.seek_to_first().unwrap();
    collect_keys(&mut iter);
    iter.seek_to_first().unwrap();
    assert_eq!(collect_keys(&mut iter), vec![0, 1, 2, 3, 4, 5]);
}

// An empty SST mixed with non-empty SSTs is skipped transparently.
#[test]
fn test_empty_sst_is_skipped() {
    let mut iter = MergeIter::new(vec![
        make_sst_iter(0, 3),
        make_sst_iter(5, 5), // empty
        make_sst_iter(5, 8),
    ]);
    iter.seek_to_first().unwrap();
    assert_eq!(collect_keys(&mut iter), vec![0, 1, 2, 5, 6, 7]);
}
