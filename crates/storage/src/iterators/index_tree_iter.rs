use std::sync::Arc;

use crate::{
    block::{BlockReader, Position},
    builder::{SstFooter, SstOption},
    errors::{StorageError, StorageResult},
    iterators::{
        block_iter::NormalBlockIter,
        index_entry_decode_iter::IndexEntryDecodeIter,
        iter::{AsArray, IndexBlockIter, StorageIter},
    },
    row_key::RowKey,
};

use tracing::{debug, span};

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
{
    pub fn new(reader: Arc<R>, footer: &SstFooter, option: &SstOption) -> StorageResult<Self> {
        let s = Self {
            reader,
            option: option.clone(),
            tree_height: footer.tree_height as usize,
            root_position: footer.root_position,
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

    fn inner_seek<'a>(&mut self, target: Option<&I::Key<'a>>) -> StorageResult<()> {
        if self.tree_height <= 1 {
            // no index block
            return Ok(());
        }
        self.reset();

        let root_block = self.reader.read_block(&self.root_position)?;
        let mut root_iter = IndexEntryDecodeIter::new(I::from_block(root_block)?);
        if let Some(target) = target {
            root_iter.seek(target)?;
        } else {
            root_iter.seek_to_first()?;
        }
        self.index_iters.push(root_iter);

        while self.curr_iter_idx < self.last_index_level() {
            let layer_span = span!(
                tracing::Level::DEBUG,
                "inner_next",
                level = self.curr_iter_idx,
                stack_size = self.index_iters.len(),
                is_last = self.curr_iter_idx == self.last_index_level()
            );
            let _enter = layer_span.enter();

            let curr_iter = &mut self.index_iters[self.curr_iter_idx];
            if !curr_iter.valid() {
                self.index_iters.remove(self.curr_iter_idx);
                if self.curr_iter_idx == 0 {
                    break;
                }
                self.curr_iter_idx -= 1;
                continue;
            }

            let next_level_position: Position = curr_iter
                .value()
                .ok_or_else(|| StorageError::InvalidValue("iter values not exists".into()))?
                .into();

            // create child iter
            let block = self.reader.read_block(&next_level_position)?;
            let mut child_iter = IndexEntryDecodeIter::new(I::from_block(block)?);
            if let Some(target) = target {
                child_iter.seek(target)?;
            } else {
                child_iter.seek_to_first()?;
            }
            debug!(
                "create level {} curr_iters_size: {}",
                self.curr_iter_idx + 1,
                self.index_iters.len() + 1
            );
            self.index_iters.push(child_iter);

            // scan next level
            self.curr_iter_idx += 1;
        }

        Ok(())
    }

    fn inner_next(&mut self) -> StorageResult<()> {
        let last_index_level = self.last_index_level();

        while self.curr_iter_idx <= last_index_level {
            let curr_iter = &mut self.index_iters[self.curr_iter_idx];
            curr_iter.next()?;
            if !curr_iter.valid() {
                // return to parent level
                self.index_iters.remove(self.curr_iter_idx);
                // if curr is root,no next item
                if self.curr_iter_idx == 0 {
                    break;
                }
                self.curr_iter_idx -= 1;
                continue;
            }

            // 如果是最后一层，直接成功
            if self.curr_iter_idx == last_index_level {
                break;
            }

            // if not leaf, move to next child iter
            let next_level_position: Position = curr_iter
                .value()
                .ok_or_else(|| StorageError::InvalidValue("iter values not exists".into()))?
                .into();

            // create child iter
            let block = self.reader.read_block(&next_level_position)?;
            let mut child_iter = IndexEntryDecodeIter::new(I::from_block(block)?);
            child_iter.seek_to_first()?;
            debug!(
                "create level {} curr_iters_size: {}",
                self.curr_iter_idx + 1,
                self.index_iters.len() + 1
            );
            self.index_iters.push(child_iter);

            self.curr_iter_idx += 1;

            // 直接退出，防止最后一层再次出发next
            if self.curr_iter_idx == last_index_level {
                break;
            }
        }
        Ok(())
    }
}

impl<R, I> StorageIter for IndexTreeIter<R, I>
where
    R: BlockReader,
    I: IndexBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    type Key<'a> = I::Key<'a>;
    type Value<'a>
        = Position
    where
        Self: 'a;

    fn valid(&self) -> bool {
        if !self.index_iters.is_empty()
            && self.curr_iter_idx < self.index_iters.len()
            && self.index_iters[self.curr_iter_idx].valid()
        {
            return true;
        }
        false
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner_seek(None)
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        self.inner_seek(Some(target))
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner_next()
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        if self.valid() {
            return self.index_iters[self.curr_iter_idx].key();
        }
        None
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        if self.valid() {
            return self.index_iters[self.curr_iter_idx]
                .value()
                .map(|v| v.into());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use std::sync::Arc;

    use super::IndexTreeIter;
    use crate::iterators::block_iter::NormalBlockIter;
    use crate::testing::init_tracing;
    use crate::{
        block::{BlockReader, InMemoryBlockReader, InMemoryBlockWriter},
        builder::{DefaultSstWriter, SstBuilder, SstFooter, SstOption},
        iterators::iter::StorageIter,
    };

    fn make_key(i: u64) -> Bytes {
        Bytes::copy_from_slice(&i.to_be_bytes())
    }

    fn make_value(i: u64) -> Bytes {
        Bytes::from(format!("v{}", i))
    }
    fn build_sst(n: u64, block_size: usize) -> (Vec<u8>, SstFooter, SstOption) {
        let option = SstOption::default().block_size(block_size);
        let mut builder = SstBuilder::new(DefaultSstWriter::new(InMemoryBlockWriter::new(), &option), option.clone());
        for i in 0..n {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, sst_writer) = builder.finish().unwrap();
        (sst_writer.into_inner().into_inner(), footer, option)
    }

    fn make_iter(n: u64, block_size: usize) -> IndexTreeIter<InMemoryBlockReader> {
        let (bytes, footer, option) = build_sst(n, block_size);
        let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(bytes), block_size));
        IndexTreeIter::<_, NormalBlockIter>::new(reader, &footer, &option).unwrap()
    }

    // Empty SST has no index blocks; iter must be invalid after seek_to_first.
    #[test]
    fn test_empty_sst_is_invalid() {
        let (bytes, footer, option) = build_sst(0, 256);
        let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(bytes), 256));
        let mut iter = IndexTreeIter::<_, NormalBlockIter>::new(reader, &footer, &option).unwrap();
        iter.seek_to_first().unwrap();
        assert!(!iter.valid());
    }

    // seek_to_first positions at the first data block (offset 0).
    #[test]
    fn test_seek_to_first_points_to_first_data_block() {
        let mut iter = make_iter(200, 256);
        iter.seek_to_first().unwrap();
        assert!(iter.valid());
        // Data blocks are written first, so the first one is always at offset 0.
        assert_eq!(iter.value().unwrap().offset, 0);
    }

    // next() must visit every data block in strictly increasing offset order.
    #[test]
    fn test_next_visits_all_data_blocks_in_order() {
        let mut iter = make_iter(200, 256);
        iter.seek_to_first().unwrap();
        let mut prev = iter.value().unwrap().offset;
        let mut count = 1usize;
        loop {
            iter.next().unwrap();
            if !iter.valid() {
                break;
            }
            let off = iter.value().unwrap().offset;
            assert!(off > prev, "offsets must be strictly increasing");
            prev = off;
            count += 1;
        }
        assert!(count > 1, "expected multiple data blocks");
    }

    // seek() to a key must land on the data block that contains it.
    #[test]
    fn test_seek_lands_on_block_containing_key() {
        init_tracing();
        let (bytes, footer, option) = build_sst(200, 256);
        let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(bytes), 256));
        let mut iter =
            IndexTreeIter::<_, NormalBlockIter>::new(reader.clone(), &footer, &option).unwrap();

        let target = make_key(100);
        iter.seek(&(&target).into()).unwrap();
        assert!(iter.valid());

        // The returned data block must actually contain key 100.
        use crate::iterators::block_iter::NormalBlockIter;
        let block = reader.read_block(&iter.value().unwrap()).unwrap();
        let mut bi = NormalBlockIter::new(block).unwrap();
        bi.seek(&(&target).into()).unwrap();
        assert!(bi.valid());
        assert_eq!(bi.key().unwrap(), (&target).into());
    }

    // seek() past the last key must leave the iter invalid.
    #[test]
    fn test_seek_past_last_key_is_invalid() {
        let mut iter = make_iter(200, 256);
        let beyond = make_key(u64::MAX);
        iter.seek(&(&beyond).into()).unwrap();
        assert!(!iter.valid());
    }

    #[test]
    fn invalid_index_entry_version_returns_error() {
        let (bytes, footer, option) = build_sst(200, 256);
        let mut raw = bytes;

        let root_offset = footer.root_position.offset as usize;
        let root_block_len = 256.min(raw.len() - root_offset);
        let root_block = &raw[root_offset..root_offset + root_block_len];
        let count = u32::from_be_bytes(root_block[0..4].try_into().unwrap()) as usize;
        assert!(count > 0);
        let value_offset_pos = 4 + (count + 1) * 4;
        let first_value_offset = u32::from_be_bytes(
            root_block[value_offset_pos..value_offset_pos + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        raw[root_offset + first_value_offset] = 2;

        let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(raw), 256));
        let mut iter = IndexTreeIter::<_, NormalBlockIter>::new(reader, &footer, &option).unwrap();

        assert!(iter.seek_to_first().is_err());
    }
}
