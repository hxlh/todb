use crate::{errors::StorageResult, iterators::iter::StorageIter};

// Concatenates multiple ordered, non-overlapping SST iterators into a single
// sequential iterator. SSTs must be provided in ascending key order.
pub struct ConcatIter<I: StorageIter> {
    iter: Vec<I>,
    curr: usize,
}

impl<I: StorageIter> ConcatIter<I> {
    pub fn new(iter: Vec<I>) -> Self {
        Self { iter, curr: 0 }
    }
}

impl<I: StorageIter> StorageIter for ConcatIter<I> {
    type Key<'a> = I::Key<'a>;

    type Value<'a>
        = I::Value<'a>
    where
        Self: 'a;

    fn valid(&self) -> bool {
        if !self.iter.is_empty() && self.curr < self.iter.len() {
            return self.iter[self.curr].valid();
        }
        false
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.curr = 0;
        // Lazily initialize: only seek the first SST now.
        // Subsequent SSTs are initialized in next() when we switch to them.
        while self.curr < self.iter.len() {
            self.iter[self.curr].seek_to_first()?;
            if self.iter[self.curr].valid() {
                break;
            }
            self.curr += 1; // skip empty SST
        }
        Ok(())
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        self.curr = 0;
        // Find the first SST that has a key >= target, stop there.
        // Subsequent SSTs are ordered after this one, so seek_to_first
        // in next() is sufficient — all their keys are already >= target.
        while self.curr < self.iter.len() {
            self.iter[self.curr].seek(target)?;
            if self.iter[self.curr].valid() {
                break;
            }
            self.curr += 1;
        }
        Ok(())
    }

    fn next(&mut self) -> StorageResult<()> {
        if !self.valid() {
            return Ok(());
        }
        self.iter[self.curr].next()?;
        if self.iter[self.curr].valid() {
            return Ok(());
        }
        // Current SST exhausted — lazily initialize the next SSTs one by one.
        self.curr += 1;
        while self.curr < self.iter.len() {
            self.iter[self.curr].seek_to_first()?;
            if self.iter[self.curr].valid() {
                break;
            }
            self.curr += 1; // skip empty SST
        }
        Ok(())
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        if self.valid() {
            return self.iter[self.curr].key();
        }
        None
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        if self.valid() {
            return self.iter[self.curr].value();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;

    use crate::{
        block::{InMemoryBlockReader, InMemoryBlockWriter},
        builder::{SstBuilder, SstOption},
        iterators::{iter::StorageIter, sst_iter::SstIter},
        row_key::RowKey,
    };

    use super::ConcatIter;

    fn make_key(i: u64) -> Bytes {
        Bytes::copy_from_slice(&i.to_be_bytes())
    }

    fn make_value(i: u64) -> Bytes {
        Bytes::from(format!("value_{:04}", i))
    }

    // Build an SST containing keys [start, end) and return a ready-to-use SstIter.
    fn make_sst_iter(start: u64, end: u64) -> SstIter<InMemoryBlockReader> {
        let option = SstOption::default().block_size(256);
        let mut builder = SstBuilder::new(InMemoryBlockWriter::new(), option.clone());
        for i in start..end {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, writer) = builder.finish().unwrap();
        let bytes = Bytes::from(writer.into_inner());
        let reader = Arc::new(InMemoryBlockReader::new(bytes, 256));
        SstIter::new(reader, footer, option).unwrap()
    }

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
            assert_eq!(iter.key().unwrap(), RowKey::from_slice(&make_key(i)));
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
            assert_eq!(iter.key().unwrap(), RowKey::from_slice(&make_key(i)));
            iter.next().unwrap();
        }
        assert!(!iter.valid());
    }

    // next() at the last key of one SST automatically advances to the first key
    // of the next SST without skipping any entry.
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
        let target = make_key(7);
        iter.seek(&RowKey::from_slice(&target)).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), RowKey::from_slice(&make_key(7)));
    }

    // seek to a key between two SSTs lands on the first key of the next SST.
    #[test]
    fn test_seek_between_ssts_lands_on_next_sst_first_key() {
        // SST1: [0,3), SST2: [10,13) — gap between 3 and 10
        let mut iter = ConcatIter::new(vec![make_sst_iter(0, 3), make_sst_iter(10, 13)]);
        let target = make_key(5);
        iter.seek(&RowKey::from_slice(&target)).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), RowKey::from_slice(&make_key(10)));
    }

    // seek past the last key in all SSTs results in invalid.
    #[test]
    fn test_seek_past_all_keys_is_invalid() {
        let mut iter = ConcatIter::new(vec![make_sst_iter(0, 5), make_sst_iter(5, 10)]);
        let target = make_key(u64::MAX);
        iter.seek(&RowKey::from_slice(&target)).unwrap();
        assert!(!iter.valid());
    }

    // An empty SST in the middle is skipped; iteration continues with the next SST.
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
}
