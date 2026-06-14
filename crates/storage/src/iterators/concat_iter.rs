use crate::{errors::StorageResult, iterators::storage_iter::{ForwardIter, ReverseIter, StorageIter}};

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

impl<I: StorageIter> ForwardIter for ConcatIter<I> {
    type Key<'a> = I::Key<'a>;

    type Value<'a>
        = I::Value<'a>
    where
        Self: 'a;

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
}

impl<I: StorageIter> ReverseIter for ConcatIter<I> {
    fn seek_to_last(&mut self) -> StorageResult<()> {
        if self.iter.is_empty() {
            return Ok(());
        }
        self.curr = self.iter.len() - 1;
        loop {
            self.iter[self.curr].seek_to_last()?;
            if self.iter[self.curr].valid() {
                break;
            }
            if self.curr == 0 {
                break;
            }
            self.curr -= 1;
        }
        Ok(())
    }

    fn seek_for_prev<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        for i in (0..self.iter.len()).rev() {
            self.iter[i].seek_for_prev(target)?;
            if self.iter[i].valid() {
                self.curr = i;
                return Ok(());
            }
        }
        self.curr = 0;
        Ok(())
    }

    fn prev(&mut self) -> StorageResult<()> {
        if !self.valid() {
            return Ok(());
        }
        self.iter[self.curr].prev()?;
        if self.iter[self.curr].valid() {
            return Ok(());
        }
        // Current SST exhausted in reverse — move to previous one.
        if self.curr > 0 {
            self.curr -= 1;
            loop {
                self.iter[self.curr].seek_to_last()?;
                if self.iter[self.curr].valid() || self.curr == 0 {
                    break;
                }
                self.curr -= 1;
            }
        }
        Ok(())
    }
}

impl<I: StorageIter> StorageIter for ConcatIter<I> {
    fn valid(&self) -> bool {
        if !self.iter.is_empty() && self.curr < self.iter.len() {
            return self.iter[self.curr].valid();
        }
        false
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

