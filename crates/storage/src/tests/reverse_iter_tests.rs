use std::sync::Arc;

use crate::{
    block::{InMemoryBlockReader, InMemoryBlockWriter},
    builder::{DefaultSstWriter, SstBuilder, SstOption},
    iterators::{
        concat_iter::ConcatIter,
        merge_iter::MergeIter,
        storage_iter::{ForwardIter, IterRead, ReverseIter},
        sst_iter::SstIter,
    },
    row_key::RowKey,
};

use super::helpers::{make_key, make_sst_iter, make_value};

/// Collect keys by walking backward via `seek_to_first` + repeated `next`.
fn collect_keys_reverse<I: ReverseIter>(iter: &mut I) -> Vec<u64>
where
    for<'a> I::Key<'a>: AsRef<[u8]>,
{
    let mut keys = vec![];
    iter.seek_to_first().unwrap();
    while iter.valid() {
        let bytes: Vec<u8> = iter.key().unwrap().as_ref().to_vec();
        keys.push(u64::from_be_bytes(bytes.try_into().unwrap()));
        iter.next().unwrap();
    }
    keys
}

// ---------------------------------------------------------------------------
// SstIter reverse
// ---------------------------------------------------------------------------

#[test]
fn test_sst_seek_to_last_descending() {
    let mut iter = make_sst_iter(0, 5);
    assert_eq!(collect_keys_reverse(&mut iter), vec![4, 3, 2, 1, 0]);
}

#[test]
fn test_sst_seek_for_prev_exact() {
    let mut iter = make_sst_iter(0, 10);
    ReverseIter::seek(&mut iter, &(&make_key(5)).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(5)).into());
}

#[test]
fn test_sst_seek_for_prev_between() {
    let mut iter = make_sst_iter(0, 10); // keys 0..9 (even only implied by make_key)
    ReverseIter::seek(&mut iter, &(&make_key(5)).into()).unwrap();
    // seek_for_prev positions at last key <= target → key 5
    assert_eq!(iter.key().unwrap(), (&make_key(5)).into());
    // prev walks backward
    ReverseIter::next(&mut iter).unwrap();
    assert_eq!(iter.key().unwrap(), (&make_key(4)).into());
}

#[test]
fn test_sst_seek_for_prev_before_all_keys() {
    // No key <= target → invalid
    let mut iter = make_sst_iter(5, 10);
    // Use a target smaller than all keys (key 5 is the first).
    // Key bytes are u64 big-endian, so key 5 = 0x0000...0005.
    let zero = make_key(0);
    let small_target: RowKey<'_> = (&zero).into();
    ReverseIter::seek(&mut iter, &small_target).unwrap();
    assert!(!iter.valid());
}

#[test]
fn test_sst_seek_for_prev_after_all_keys() {
    let mut iter = make_sst_iter(0, 5); // keys 0..4
    ReverseIter::seek(&mut iter, &(&make_key(u64::MAX)).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(4)).into());
}

#[test]
fn test_sst_prev_exhaustion() {
    let mut iter = make_sst_iter(0, 3);
    ReverseIter::seek_to_first(&mut iter).unwrap();
    assert!(iter.valid());
    ReverseIter::next(&mut iter).unwrap(); // key 1
    assert!(iter.valid());
    ReverseIter::next(&mut iter).unwrap(); // key 0
    assert!(iter.valid());
    ReverseIter::next(&mut iter).unwrap(); // past first → invalid
    assert!(!iter.valid());
}

// ---------------------------------------------------------------------------
// MergeIter reverse
// ---------------------------------------------------------------------------

#[test]
fn test_merge_seek_to_last_descending() {
    let mut iter = MergeIter::new(vec![make_sst_iter(0, 5)]);
    assert_eq!(collect_keys_reverse(&mut iter), vec![4, 3, 2, 1, 0]);
}

#[test]
fn test_merge_reverse_non_overlapping() {
    let mut iter = MergeIter::new(vec![
        make_sst_iter(0, 3),
        make_sst_iter(3, 5),
        make_sst_iter(5, 8),
    ]);
    assert_eq!(
        collect_keys_reverse(&mut iter),
        vec![7, 6, 5, 4, 3, 2, 1, 0]
    );
}

#[test]
fn test_merge_reverse_overlapping_no_dedup() {
    // MergeIter does NOT dedup equal keys — both levels output their entry.
    // Lower level (newer) comes first in the same direction of travel.
    // Descending: 3, 2(l1), 2(l0), 1(l1), 1(l0), 0
    let mut iter = MergeIter::new(vec![
        make_sst_iter(0, 3), // level 0: keys 0,1,2
        make_sst_iter(1, 4), // level 1: keys 1,2,3
    ]);
    assert_eq!(collect_keys_reverse(&mut iter), vec![3, 2, 2, 1, 1, 0]);
}

#[test]
fn test_merge_seek_for_prev() {
    let mut iter = MergeIter::new(vec![make_sst_iter(0, 5), make_sst_iter(5, 10)]);
    ReverseIter::seek(&mut iter, &(&make_key(6)).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(6)).into());
    ReverseIter::next(&mut iter).unwrap();
    assert_eq!(iter.key().unwrap(), (&make_key(5)).into());
}

#[test]
fn test_merge_seek_for_prev_before_all() {
    let mut iter = MergeIter::new(vec![make_sst_iter(5, 10)]);
    ReverseIter::seek(&mut iter, &(&make_key(0)).into()).unwrap();
    assert!(!iter.valid());
}

// ---------------------------------------------------------------------------
// ConcatIter reverse
// ---------------------------------------------------------------------------

#[test]
fn test_concat_reverse_descending() {
    let mut iter = ConcatIter::new(vec![make_sst_iter(0, 3), make_sst_iter(3, 6)]);
    assert_eq!(collect_keys_reverse(&mut iter), vec![5, 4, 3, 2, 1, 0]);
}

#[test]
fn test_concat_seek_for_prev() {
    let mut iter = ConcatIter::new(vec![make_sst_iter(0, 3), make_sst_iter(3, 6)]);
    ReverseIter::seek(&mut iter, &(&make_key(4)).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(4)).into());
}

// ---------------------------------------------------------------------------
// Forward→reverse→forward re-seek
// ---------------------------------------------------------------------------

#[test]
fn test_forward_then_reverse_reseek() {
    let mut iter = MergeIter::new(vec![make_sst_iter(0, 10)]);
    // Forward scan first
    ForwardIter::seek_to_first(&mut iter).unwrap();
    assert_eq!(iter.key().unwrap(), (&make_key(0)).into());
    ForwardIter::next(&mut iter).unwrap();
    assert_eq!(iter.key().unwrap(), (&make_key(1)).into());

    // Switch to reverse — must rebuild heap
    ReverseIter::seek_to_first(&mut iter).unwrap();
    assert_eq!(iter.key().unwrap(), (&make_key(9)).into());
    ReverseIter::next(&mut iter).unwrap();
    assert_eq!(iter.key().unwrap(), (&make_key(8)).into());
}

// ---------------------------------------------------------------------------
// seek_for_prev in key gaps (W1 regression test)
// ---------------------------------------------------------------------------

/// Build an SST with specific keys and block size.
fn make_sst_with_keys(keys: &[u64], block_size: usize) -> SstIter<InMemoryBlockReader> {
    let option = SstOption::default().block_size(block_size);
    let mut builder = SstBuilder::new(
        DefaultSstWriter::new(InMemoryBlockWriter::new(), &option),
        option.clone(),
    );
    for &k in keys {
        builder.add(make_key(k), make_value(k)).unwrap();
    }
    let (footer, sst_writer) = builder.finish().unwrap();
    let bytes = bytes::Bytes::from(sst_writer.into_inner().into_inner());
    let reader = Arc::new(InMemoryBlockReader::new(bytes, block_size));
    SstIter::new(reader, footer, option).unwrap()
}

#[test]
fn test_sst_seek_for_prev_gap_between_blocks() {
    // 8 even keys, 4 per data block (block_size=128):
    //   Block 0: [0, 2, 4, 6]  end_key=6
    //   Block 1: [8, 10, 12, 14]  end_key=14
    // seek_for_prev(7): target between the two blocks.
    //   index.seek(7) → Block 1 (end_key=14 >= 7)
    //   Block 1.seek_for_prev(7) → invalid (all keys > 7)
    //   Without fallback: returns invalid (BUG W1)
    //   With fallback: prev to Block 0, → key 6
    let mut iter = make_sst_with_keys(&[0, 2, 4, 6, 8, 10, 12, 14], 128);
    let seven = make_key(7);
    ReverseIter::seek(&mut iter, &(&seven).into()).unwrap();
    assert!(iter.valid());
    assert_eq!(iter.key().unwrap(), (&make_key(6)).into());
}

#[test]
fn test_sst_seek_for_prev_gap_before_first_block() {
    // All keys > target → no fallback possible → invalid
    let mut iter = make_sst_with_keys(&[10, 20, 30], 128);
    let five = make_key(5);
    ReverseIter::seek(&mut iter, &(&five).into()).unwrap();
    assert!(!iter.valid());
}
