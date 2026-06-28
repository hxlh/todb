use std::sync::Arc;

use crate::{
    block::{BlockReader, Position},
    builder::{SstFooter, SstOption},
    errors::{StorageError, StorageResult},
    iterators::{
        block_iter::NormalBlockIter,
        index_entry_decode_iter::IndexEntryDecodeIter,
        storage_iter::{AsArray, ForwardIter, IndexBlockIter, IterBase, IterRead, ReverseIter},
    },
};

#[cfg(test)]
use crate::block::{InMemoryBlockReader, InMemoryBlockWriter};

use tracing::debug;

#[allow(dead_code)]
pub struct IndexTreeIter<R, I: IndexBlockIter = NormalBlockIter> {
    reader: Arc<R>,
    #[allow(dead_code)]
    option: SstOption,
    tree_height: usize,
    root_position: Position,
    /// iters[0] = root, iters[tree_height-2] = leaf index level.
    index_iters: Vec<IndexEntryDecodeIter<I>>,
    curr_iter_idx: usize,
}

impl<R, I> IndexTreeIter<R, I>
where
    R: BlockReader,
    I: IndexBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<I::Block>,
{
    pub fn new(reader: Arc<R>, footer: &SstFooter, option: &SstOption) -> StorageResult<Self> {
        let s = Self {
            reader,
            option: option.clone(),
            tree_height: footer.tree_height as usize,
            root_position: footer.root_index_block_position,
            index_iters: Vec::new(),
            curr_iter_idx: 0,
        };

        debug!("index tree heigh: {}", footer.tree_height);
        Ok(s)
    }

    fn reset(&mut self) {
        self.index_iters.clear();
    }

    fn last_index_level(&self) -> usize {
        self.tree_height - 2
    }
}


impl<R, I> IndexTreeIter<R, I>
where
    R: BlockReader,
    I: IndexBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<I::Block>,
{
    /// Descend root→leaf, positioning each level at the first key >= target
    /// (lower bound). Direction-agnostic block location; both forward and
    /// reverse seek call this to find the block containing `target`, then
    /// traverse within it via ForwardIter/ReverseIter.
    fn locate(&mut self, target: &I::Key<'_>) -> StorageResult<()> {
        if self.tree_height <= 1 {
            return Ok(());
        }
        self.reset();

        let root_block = self.reader.read_block(&self.root_position)?;
        let mut root_iter = IndexEntryDecodeIter::new(I::from_block(root_block.into())?);
        root_iter.seek_lower_bound(target)?;
        self.index_iters.push(root_iter);

        while self.curr_iter_idx < self.last_index_level() {
            let curr_iter = &mut self.index_iters[self.curr_iter_idx];
            if !curr_iter.valid() {
                if self.curr_iter_idx == 0 {
                    // Root has no end_key >= target → target exceeds every
                    // key. Valid "not found": leave iter invalid.
                    break;
                }
                // A mid-level child came up empty under a positioned parent
                // (parent end_key >= target guarantees a key >= target in the
                // child). That is a corrupt index — surface the error rather
                // than masquerade as "not found".
                return Err(StorageError::InvalidValue(format!(
                    "corrupt index: empty child block at level {} under a positioned parent",
                    self.curr_iter_idx
                )));
            }

            let next_pos: Position = curr_iter
                .value()
                .ok_or_else(|| StorageError::InvalidValue("iter values not exists".into()))?
                .into();

            let block = self.reader.read_block(&next_pos)?;
            let mut child_iter = IndexEntryDecodeIter::new(I::from_block(block.into())?);
            child_iter.seek_lower_bound(target)?;
            self.index_iters.push(child_iter);
            self.curr_iter_idx += 1;
        }

        Ok(())
    }
}

// ── Forward navigation ──
impl<R, I> IndexTreeIter<R, I>
where
    R: BlockReader,
    I: IndexBlockIter + ForwardIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<I::Block>,
{
    /// Descend root→leaf for forward `seek_to_first`, positioning each level
    /// at its first entry. Unlike [`locate`](Self::locate) there is no target
    /// key, so each level uses `seek_to_first` rather than a lower bound.
    fn inner_seek_to_first(&mut self) -> StorageResult<()> {
        if self.tree_height <= 1 {
            return Ok(());
        }
        self.reset();

        let root_block = self.reader.read_block(&self.root_position)?;
        let mut root_iter = IndexEntryDecodeIter::new(I::from_block(root_block.into())?);
        root_iter.seek_to_first()?;
        self.index_iters.push(root_iter);

        while self.curr_iter_idx < self.last_index_level() {
            let curr_iter = &mut self.index_iters[self.curr_iter_idx];
            if !curr_iter.valid() {
                // seek_to_first on a non-empty block always positions valid;
                // an empty child here means a corrupt index — surface it.
                return Err(StorageError::InvalidValue(
                    "corrupt index: empty block during seek_to_first/last descent".into(),
                ));
            }

            let next_pos: Position = curr_iter
                .value()
                .ok_or_else(|| StorageError::InvalidValue("iter values not exists".into()))?
                .into();

            let block = self.reader.read_block(&next_pos)?;
            let mut child_iter = IndexEntryDecodeIter::new(I::from_block(block.into())?);
            child_iter.seek_to_first()?;
            self.index_iters.push(child_iter);
            self.curr_iter_idx += 1;
        }

        Ok(())
    }
    fn inner_next(&mut self) -> StorageResult<()> {
        let last_index_level = self.last_index_level();
        if self.index_iters.is_empty() {
            return Ok(());
        }

        // Phase 1 — advance the current level by one. If it is exhausted, pop it
        // and advance its parent; repeat up the tree until some level has a next
        // entry (or the root is exhausted, ending iteration).
        loop {
            let curr = self.curr_iter_idx;
            let curr_iter = &mut self.index_iters[curr];
            curr_iter.next()?;
            if curr_iter.valid() {
                break;
            }
            self.index_iters.remove(curr);
            if curr == 0 {
                return Ok(());
            }
            self.curr_iter_idx = curr - 1;
        }

        // Phase 2 — the level now holding a next entry may be above the leaf.
        // Re-descend to the leaf through FIRST entries (seek_to_first), mirroring
        // inner_seek_to_first. Each freshly-read child is already at its first
        // entry; advancing it here would skip that entry (and its whole subtree),
        // so never call next() on a just-pushed level.
        while self.curr_iter_idx < last_index_level {
            let next_pos: Position = self.index_iters[self.curr_iter_idx]
                .value()
                .ok_or_else(|| StorageError::InvalidValue("iter values not exists".into()))?
                .into();
            let block = self.reader.read_block(&next_pos)?;
            let mut child_iter = IndexEntryDecodeIter::new(I::from_block(block.into())?);
            child_iter.seek_to_first()?;
            if !child_iter.valid() {
                return Err(StorageError::InvalidValue(
                    "corrupt index: empty block during next descent".into(),
                ));
            }
            self.index_iters.push(child_iter);
            self.curr_iter_idx += 1;
        }
        Ok(())
    }
}

// ── Reverse navigation ──

impl<R, I> IndexTreeIter<R, I>
where
    R: BlockReader,
    I: IndexBlockIter + ReverseIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<I::Block>,
{
    /// Descend root→leaf for reverse `seek_to_first` (which positions at the
    /// largest key, i.e. seek_to_last). Each level uses `ReverseIter::seek_to_first`
    /// to land on its largest entry, so the descent reaches the leaf's last key.
    fn inner_seek_to_last(&mut self) -> StorageResult<()> {
        if self.tree_height <= 1 {
            return Ok(());
        }
        self.reset();

        let root_block = self.reader.read_block(&self.root_position)?;
        let mut root_iter = IndexEntryDecodeIter::new(I::from_block(root_block.into())?);
        root_iter.seek_to_first()?;
        self.index_iters.push(root_iter);

        while self.curr_iter_idx < self.last_index_level() {
            let curr_iter = &mut self.index_iters[self.curr_iter_idx];
            if !curr_iter.valid() {
                // seek_to_first on a non-empty block always positions valid;
                // an empty child here means a corrupt index — surface it.
                return Err(StorageError::InvalidValue(
                    "corrupt index: empty block during seek_to_first/last descent".into(),
                ));
            }

            let next_pos: Position = curr_iter
                .value()
                .ok_or_else(|| StorageError::InvalidValue("iter values not exists".into()))?
                .into();

            let block = self.reader.read_block(&next_pos)?;
            let mut child_iter = IndexEntryDecodeIter::new(I::from_block(block.into())?);
            child_iter.seek_to_first()?;
            self.index_iters.push(child_iter);
            self.curr_iter_idx += 1;
        }

        Ok(())
    }

    fn inner_prev(&mut self) -> StorageResult<()> {
        let last_index_level = self.last_index_level();
        if self.index_iters.is_empty() {
            return Ok(());
        }

        // Phase 1 — step the current level backward (`ReverseIter::next`). If it
        // is exhausted (at the block's first/smallest entry), pop it and step its
        // parent backward; repeat up the tree until some level has a previous
        // entry (or the root is exhausted, ending iteration).
        loop {
            let curr = self.curr_iter_idx;
            let curr_iter = &mut self.index_iters[curr];
            curr_iter.next()?;
            if curr_iter.valid() {
                break;
            }
            self.index_iters.remove(curr);
            if curr == 0 {
                return Ok(());
            }
            self.curr_iter_idx = curr - 1;
        }

        // Phase 2 — re-descend to the leaf through LAST entries
        // (`ReverseIter::seek_to_first`), mirroring inner_seek_to_last. Each
        // freshly-read child is already at its last entry; stepping it here
        // would skip that entry (and its whole subtree), so never call next()
        // on a just-pushed level. (Symmetric to inner_next's forward descent.)
        while self.curr_iter_idx < last_index_level {
            let next_pos: Position = self.index_iters[self.curr_iter_idx]
                .value()
                .ok_or_else(|| StorageError::InvalidValue("iter values not exists".into()))?
                .into();
            let block = self.reader.read_block(&next_pos)?;
            let mut child_iter = IndexEntryDecodeIter::new(I::from_block(block.into())?);
            child_iter.seek_to_first()?;
            if !child_iter.valid() {
                return Err(StorageError::InvalidValue(
                    "corrupt index: empty block during prev descent".into(),
                ));
            }
            self.index_iters.push(child_iter);
            self.curr_iter_idx += 1;
        }
        Ok(())
    }
}

// ── Trait impls ──

impl<R, I> IterBase for IndexTreeIter<R, I>
where
    I: IndexBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    type Key<'a> = I::Key<'a>;
    type Value<'a> = Position;
}

impl<R, I> IterRead for IndexTreeIter<R, I>
where
    I: IndexBlockIter + IterRead,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    fn valid(&self) -> bool {
        !self.index_iters.is_empty()
            && self.curr_iter_idx < self.index_iters.len()
            && self.index_iters[self.curr_iter_idx].valid()
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        if self.valid() {
            self.index_iters[self.curr_iter_idx].key()
        } else {
            None
        }
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        if self.valid() {
            self.index_iters[self.curr_iter_idx].value().map(Into::into)
        } else {
            None
        }
    }
}

impl<R, I> ForwardIter for IndexTreeIter<R, I>
where
    R: BlockReader,
    I: IndexBlockIter + ForwardIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<I::Block>,
{
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner_seek_to_first()
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        self.locate(target)
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner_next()
    }
}

impl<R, I> ReverseIter for IndexTreeIter<R, I>
where
    R: BlockReader,
    I: IndexBlockIter + ReverseIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<I::Block>,
{
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner_seek_to_last()
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        // End-key convention: locate the block containing target via lower
        // bound (first end_key >= target) — direction-agnostic. If target
        // exceeds all end_keys, fall back to the last entry.
        self.locate(target)?;
        if !self.valid() {
            self.inner_seek_to_last()?;
        }
        Ok(())
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner_prev()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::{DefaultSstWriter, SstBuilder};
    use bytes::Bytes;
    use std::sync::Arc;

    fn init_tracing() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    fn make_key(i: u64) -> Bytes {
        Bytes::copy_from_slice(&i.to_be_bytes())
    }

    fn make_value(i: u64) -> Bytes {
        Bytes::from(format!("v{}", i))
    }

    fn build_sst(n: u64, block_size: usize) -> (Vec<u8>, SstFooter, SstOption) {
        let option = SstOption::default().block_size(block_size);
        let mut builder = SstBuilder::new(
            DefaultSstWriter::new(InMemoryBlockWriter::new(), &option),
            option.clone(),
        );
        for i in 0..n {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, sst_writer) = builder.finish().unwrap();
        (sst_writer.into_inner().into_inner(), footer, option)
    }

    #[test]
    fn test_seek_lands_on_block_containing_key() {
        init_tracing();
        let (data, footer, option) = build_sst(100, 64);
        let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(data), 64));
        let mut iter = IndexTreeIter::<_, NormalBlockIter>::new(reader, &footer, &option).unwrap();
        ForwardIter::seek(&mut iter, &(&make_key(50)).into()).unwrap();

        assert!(iter.valid());
        let pos = iter.value().unwrap();
        assert!(pos.offset > 0);
    }

    #[test]
    fn test_seek_past_last_key_is_invalid() {
        init_tracing();
        let (data, footer, option) = build_sst(10, 64);
        let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(data), 64));
        let mut iter = IndexTreeIter::<_, NormalBlockIter>::new(reader, &footer, &option).unwrap();
        ForwardIter::seek(&mut iter, &(&make_key(1000)).into()).unwrap();

        assert!(!iter.valid());
    }
}
