use crate::{errors::StorageResult, iterators::storage_iter::{ForwardIter, IterBase, IterRead, ReverseIter}};

// Concatenates multiple ordered, non-overlapping SST iterators into a single
// sequential iterator. SSTs must be provided in ascending key order.
pub struct ConcatIter<I> {
    iter: Vec<I>,
    curr: usize,
}

impl<I> ConcatIter<I> {
    pub fn new(iter: Vec<I>) -> Self {
        Self { iter, curr: 0 }
    }
}

impl<I: IterBase> IterBase for ConcatIter<I> {
    type Key<'a> = I::Key<'a>;
    type Value<'a> = I::Value<'a>;
}

impl<I: IterRead> IterRead for ConcatIter<I> {
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

impl<I: ForwardIter> ForwardIter for ConcatIter<I> {
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.curr = 0;
        while self.curr < self.iter.len() {
            self.iter[self.curr].seek_to_first()?;
            if self.iter[self.curr].valid() {
                break;
            }
            self.curr += 1;
        }
        Ok(())
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        self.curr = 0;
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
        self.curr += 1;
        while self.curr < self.iter.len() {
            self.iter[self.curr].seek_to_first()?;
            if self.iter[self.curr].valid() {
                break;
            }
            self.curr += 1;
        }
        Ok(())
    }
}

impl<I: ReverseIter> ReverseIter for ConcatIter<I> {
    fn seek_to_first(&mut self) -> StorageResult<()> {
        if self.iter.is_empty() {
            return Ok(());
        }
        self.curr = self.iter.len() - 1;
        loop {
            self.iter[self.curr].seek_to_first()?;
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

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        for i in (0..self.iter.len()).rev() {
            self.iter[i].seek(target)?;
            if self.iter[i].valid() {
                self.curr = i;
                return Ok(());
            }
        }
        self.curr = 0;
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
        if self.curr > 0 {
            self.curr -= 1;
            loop {
                self.iter[self.curr].seek_to_first()?;
                if self.iter[self.curr].valid() || self.curr == 0 {
                    break;
                }
                self.curr -= 1;
            }
        }
        Ok(())
    }
}


