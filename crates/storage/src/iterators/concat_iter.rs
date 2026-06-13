use crate::{errors::StorageResult, iterators::storage_iter::StorageIter};

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

