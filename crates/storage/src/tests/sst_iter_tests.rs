use std::sync::Arc;

use bytes::Bytes;

use crate::{
    block::{InMemoryBlockReader, InMemoryBlockWriter},
    builder::{SstBuilder, SstOption},
    iterators::{
        block_iter::NormalBlockIter,
        entry_decode_iter::EntryDecodeIter,
        storage_iter::{AsArray, StorageIter},
        sst_iter::SstIter,
    },
    row_key::RowKey,
    testing::init_tracing,
};

use super::helpers::{build_sst, make_key, make_value};

fn make_iter(
    n: u64,
    block_size: usize,
) -> SstIter<InMemoryBlockReader, NormalBlockIter, EntryDecodeIter<NormalBlockIter>> {
    let (bytes, footer, option) = build_sst(n, block_size);
    let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(bytes), block_size));
    SstIter::<_, NormalBlockIter, EntryDecodeIter<NormalBlockIter>>::new(reader, footer, option)
        .unwrap()
}

// Empty SST: seek_to_first results in invalid.
#[test]
fn test_empty_sst_is_invalid() {
    let mut iter = make_iter(0, 256);
    iter.seek_to_first().unwrap();
    assert!(!iter.valid());
}

// seek_to_first positions at the first key with correct key and value.
#[test]
fn test_seek_to_first_returns_first_key() {
    let mut iter = make_iter(200, 256);
    iter.seek_to_first().unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(0)).into());
    assert_eq!(iter.value().unwrap().as_array(), make_value(0).as_ref());
}

// next() traverses all keys in ascending order across block boundaries.
#[test]
fn test_next_iterates_all_keys_in_order() {
    init_tracing();
    let n = 200u64;
    let mut iter = make_iter(n, 256);
    iter.seek_to_first().unwrap();
    for i in 0..n {
        assert!(iter.valid(), "expected valid at i={}", i);
        assert_eq!(iter.key().unwrap(), (&make_key(i)).into());
        assert_eq!(iter.value().unwrap().as_array(), make_value(i).as_ref());
        iter.next().unwrap();
    }
    assert!(!iter.valid(), "expected invalid after last entry");
}

// seek with an exact key match lands on that key.
#[test]
fn test_seek_exact_match() {
    let mut iter = make_iter(200, 256);
    let k = make_key(100);
    iter.seek(&(&k).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&k).into());
    assert_eq!(iter.value().unwrap().as_array(), make_value(100).as_ref());
}

// seek between two keys lands on the next key (lower-bound semantics).
#[test]
fn test_seek_between_keys() {
    let mut iter = make_iter(200, 256);
    let mut between = make_key(50).to_vec();
    *between.last_mut().unwrap() += 1;
    iter.seek(&(&between).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(51)).into());
}

// seek with a key smaller than all keys lands on the first key.
#[test]
fn test_seek_before_first_key() {
    let mut iter = make_iter(200, 256);
    let before = vec![0u8; 7];
    iter.seek(&(&before).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(0)).into());
}

// seek past the last key results in invalid.
#[test]
fn test_seek_after_last_key() {
    let mut iter = make_iter(200, 256);
    iter.seek(&(&make_key(u64::MAX)).into()).unwrap();
    assert!(!iter.valid());
}

#[test]
fn test_seek_to_first_still_reads_first_key_with_versioned_index_entries() {
    let mut iter = make_iter(200, 256);
    iter.seek_to_first().unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(0)).into());
    assert_eq!(iter.value().unwrap().as_array(), make_value(0).as_ref());
}
