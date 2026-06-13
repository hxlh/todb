use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::iterators::storage_iter::StorageIter;

struct HeapWrap<I> {
    level: usize,
    iter: I,
}

impl<I: StorageIter> PartialEq for HeapWrap<I> {
    fn eq(&self, other: &Self) -> bool {
        self.level == other.level
    }
}

impl<I: StorageIter> Eq for HeapWrap<I> {}

impl<I: StorageIter> PartialOrd for HeapWrap<I> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<I: StorageIter> Ord for HeapWrap<I> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.iter.key(), other.iter.key()) {
            (Some(a), Some(b)) => a.cmp(&b).then(self.level.cmp(&other.level)),
            (None, None) => std::cmp::Ordering::Equal,
            // None (invalid iterator) sorts greater so it sinks in a min-heap
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
        }
    }
}

pub struct MergeIter<I> {
    inactive: Vec<HeapWrap<I>>,
    heap: BinaryHeap<Reverse<HeapWrap<I>>>,
}

impl<I> MergeIter<I>
where
    I: StorageIter,
{
    pub fn new(iters: Vec<I>) -> Self {
        let inactive = iters
            .into_iter()
            .enumerate()
            .map(|(level, iter)| HeapWrap { level, iter })
            .collect();
        Self {
            inactive,
            heap: BinaryHeap::new(),
        }
    }
}

impl<I> StorageIter for MergeIter<I>
where
    I: StorageIter,
{
    type Key<'a> = I::Key<'a>;

    type Value<'a>
        = I::Value<'a>
    where
        Self: 'a;

    fn valid(&self) -> bool {
        self.heap.peek().is_some()
    }

    fn seek_to_first(&mut self) -> crate::errors::StorageResult<()> {
        while let Some(Reverse(w)) = self.heap.pop() {
            self.inactive.push(w);
        }

        for w in &mut self.inactive {
            w.iter.seek_to_first()?;
        }

        let inactive = std::mem::take(&mut self.inactive);
        for w in inactive {
            if w.iter.valid() {
                self.heap.push(Reverse(w));
            } else {
                self.inactive.push(w);
            }
        }
        Ok(())
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> crate::errors::StorageResult<()> {
        while let Some(Reverse(w)) = self.heap.pop() {
            self.inactive.push(w);
        }

        for w in &mut self.inactive {
            w.iter.seek(target)?;
        }

        let inactive = std::mem::take(&mut self.inactive);
        for w in inactive {
            if w.iter.valid() {
                self.heap.push(Reverse(w));
            } else {
                self.inactive.push(w);
            }
        }
        Ok(())
    }

    fn next(&mut self) -> crate::errors::StorageResult<()> {
        if let Some(Reverse(mut w)) = self.heap.pop() {
            w.iter.next()?;
            if w.iter.valid() {
                self.heap.push(Reverse(w));
            } else {
                self.inactive.push(w);
            }
        }
        Ok(())
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        self.heap.peek()?.0.iter.key()
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        self.heap.peek()?.0.iter.value()
    }
}

