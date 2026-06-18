use std::cmp::Reverse;
use std::{cmp::Ordering, collections::BinaryHeap};

use crate::iterators::storage_iter::{ForwardIter, IterBase, IterRead, ReverseIter};

struct HeapWrap<I> {
    level: usize,
    iter: I,
}

impl<I: ForwardIter> PartialEq for HeapWrap<I> {
    fn eq(&self, other: &Self) -> bool {
        self.level == other.level
    }
}

impl<I: ForwardIter> Eq for HeapWrap<I> {}

impl<I: ForwardIter> PartialOrd for HeapWrap<I> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<I: ForwardIter> Ord for HeapWrap<I> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.iter.key(), other.iter.key()) {
            (Some(a), Some(b)) => a.cmp(&b).then(self.level.cmp(&other.level)),
            (None, None) => Ordering::Equal,
            // None (invalid iterator) sorts greater so it sinks in a min-heap
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
        }
    }
}

struct MaxHeapWrap<I>(HeapWrap<I>);

impl<I: ReverseIter> PartialEq for MaxHeapWrap<I> {
    fn eq(&self, other: &Self) -> bool {
        self.0.level == other.0.level
    }
}

impl<I: ReverseIter> Eq for MaxHeapWrap<I> {}

impl<I: ReverseIter> PartialOrd for MaxHeapWrap<I> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<I: ReverseIter> Ord for MaxHeapWrap<I> {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is already a max-heap — normal key ordering puts the
        // largest key on top. Reverse the level tiebreak so lower level
        // (newer data) wins on equal keys.
        match (self.0.iter.key(), other.0.iter.key()) {
            (Some(a), Some(b)) => a.cmp(&b).then(other.0.level.cmp(&self.0.level)),
            (None, None) => Ordering::Equal,
            // Invalid iters sink to the bottom of the max-heap.
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
        }
    }
}

/// Active merge heap, dispatched by iteration direction.
enum MergeHeap<I> {
    Forward(BinaryHeap<Reverse<HeapWrap<I>>>),
    Reverse(BinaryHeap<MaxHeapWrap<I>>),
}

pub struct MergeIter<I> {
    items: Vec<HeapWrap<I>>,
    heap: MergeHeap<I>,
}

impl<I> MergeIter<I> {
    pub fn new(iters: Vec<I>) -> Self {
        let items = iters
            .into_iter()
            .enumerate()
            .map(|(level, iter)| HeapWrap { level, iter })
            .collect();
        Self {
            items,
            heap: MergeHeap::Forward(BinaryHeap::new()),
        }
    }

    /// Move every wrapper from the active heap back into `items`.
    fn drain_to_items(&mut self) {
        match &mut self.heap {
            MergeHeap::Forward(h) => {
                for Reverse(w) in h.drain() {
                    self.items.push(w);
                }
            }
            MergeHeap::Reverse(h) => {
                for MaxHeapWrap(w) in h.drain() {
                    self.items.push(w);
                }
            }
        }
    }
}

impl<I: ForwardIter> MergeIter<I> {
    /// Rebuild the forward heap from `items`, pushing only valid iters and recycling invalid ones.
    fn build_forward_heap(&mut self) {
        let inactive = std::mem::take(&mut self.items);
        let mut heap = BinaryHeap::new();
        for w in inactive {
            if w.iter.valid() {
                heap.push(Reverse(w));
            } else {
                self.items.push(w);
            }
        }
        self.heap = MergeHeap::Forward(heap);
    }
}

impl<I: ReverseIter> MergeIter<I> {
    /// Rebuild the reverse heap from `items`, pushing only valid iters and recycling invalid ones.
    fn build_reverse_heap(&mut self) {
        let inactive = std::mem::take(&mut self.items);
        let mut heap = BinaryHeap::new();
        for w in inactive {
            if w.iter.valid() {
                heap.push(MaxHeapWrap(w));
            } else {
                self.items.push(w);
            }
        }
        self.heap = MergeHeap::Reverse(heap);
    }
}

impl<I: IterBase> IterBase for MergeIter<I> {
    type Key<'a> = I::Key<'a>;
    type Value<'a> = I::Value<'a>;
}

impl<I: IterRead> IterRead for MergeIter<I> {
    fn valid(&self) -> bool {
        match &self.heap {
            MergeHeap::Forward(h) => h.peek().is_some(),
            MergeHeap::Reverse(h) => h.peek().is_some(),
        }
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        match &self.heap {
            MergeHeap::Forward(h) => h.peek().and_then(|r| r.0.iter.key()),
            MergeHeap::Reverse(h) => h.peek().and_then(|m| m.0.iter.key()),
        }
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        match &self.heap {
            MergeHeap::Forward(h) => h.peek().and_then(|r| r.0.iter.value()),
            MergeHeap::Reverse(h) => h.peek().and_then(|m| m.0.iter.value()),
        }
    }
}

impl<I> ForwardIter for MergeIter<I>
where
    I: ForwardIter,
{
    fn seek_to_first(&mut self) -> crate::errors::StorageResult<()> {
        self.drain_to_items();
        for w in &mut self.items {
            w.iter.seek_to_first()?;
        }
        self.build_forward_heap();
        Ok(())
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> crate::errors::StorageResult<()> {
        self.drain_to_items();
        for w in &mut self.items {
            w.iter.seek(target)?;
        }
        self.build_forward_heap();
        Ok(())
    }

    fn next(&mut self) -> crate::errors::StorageResult<()> {
        if let MergeHeap::Forward(h) = &mut self.heap {
            if let Some(Reverse(mut w)) = h.pop() {
                w.iter.next()?;
                if w.iter.valid() {
                    h.push(Reverse(w));
                } else {
                    self.items.push(w);
                }
            }
        }
        Ok(())
    }
}

impl<I> ReverseIter for MergeIter<I>
where
    I: ReverseIter,
{
    fn seek_to_first(&mut self) -> crate::errors::StorageResult<()> {
        self.drain_to_items();
        for w in &mut self.items {
            w.iter.seek_to_first()?;
        }
        self.build_reverse_heap();
        Ok(())
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> crate::errors::StorageResult<()> {
        self.drain_to_items();
        for w in &mut self.items {
            w.iter.seek(target)?;
        }
        self.build_reverse_heap();
        Ok(())
    }

    fn next(&mut self) -> crate::errors::StorageResult<()> {
        if let MergeHeap::Reverse(h) = &mut self.heap {
            if let Some(MaxHeapWrap(mut w)) = h.pop() {
                w.iter.next()?;
                if w.iter.valid() {
                    h.push(MaxHeapWrap(w));
                } else {
                    self.items.push(w);
                }
            }
        }
        Ok(())
    }
}
