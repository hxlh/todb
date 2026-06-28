use std::sync::Arc;

use crate::{
    block::{BlockReader, Position},
    builder::{SstFooter, SstOption},
    errors::{StorageError, StorageResult},
    iterators::{
        block_iter::NormalBlockIter,
        data_entry_decode_iter::DataEntryDecodeIter,
        index_tree_iter::IndexTreeIter,
        storage_iter::{
            AsArray, DataBlockIter, ForwardIter, IndexBlockIter, IterBase, IterRead, ReverseIter,
        },
    },
};

pub struct SstIter<R, I = NormalBlockIter, D = DataEntryDecodeIter<NormalBlockIter>>
where
    R: BlockReader,
    I: IndexBlockIter,
    D: DataBlockIter,
{
    reader: Arc<R>,
    #[allow(dead_code)]
    option: SstOption,
    index_iter: IndexTreeIter<R, I>,
    data_iter: Option<D>,
}

impl<R, I, D> SstIter<R, I, D>
where
    R: BlockReader,
    I: IndexBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<I::Block>,
    D: DataBlockIter,
{
    pub fn new(reader: Arc<R>, footer: SstFooter, option: SstOption) -> StorageResult<Self> {
        Ok(Self {
            index_iter: IndexTreeIter::<R, I>::new(reader.clone(), &footer, &option)?,
            option,
            reader,
            data_iter: None,
        })
    }
}

impl<R, I, D> SstIter<R, I, D>
where
    R: BlockReader,
    I: IndexBlockIter,
    D: DataBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<D::Block>,
{
    /// Load and decode the data block at the given block position.
    /// Callers obtain the Position from `value()` on the index iterator.
    fn load_data_block_from_index(&mut self) -> StorageResult<D> {
        let pos: Position = self.index_iter.value().ok_or_else(|| {
            StorageError::InvalidValue("index iter valid but value is none".into())
        })?;
        let block = self.reader.read_block(&pos)?;
        D::from_block(block.into())
    }
}

impl<R, I, D> IterBase for SstIter<R, I, D>
where
    R: BlockReader,
    I: IndexBlockIter,
    D: DataBlockIter,
{
    type Key<'a> = D::Key<'a>;
    type Value<'a> = D::Value<'a>;
}

impl<R, I, D> IterRead for SstIter<R, I, D>
where
    R: BlockReader,
    I: IndexBlockIter,
    D: DataBlockIter + IterRead,
{
    fn valid(&self) -> bool {
        self.data_iter.as_ref().map_or(false, |d| d.valid())
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        self.data_iter.as_ref()?.key()
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        self.data_iter.as_ref()?.value()
    }
}

impl<R, I, D> ForwardIter for SstIter<R, I, D>
where
    R: BlockReader,
    I: IndexBlockIter + for<'a> ForwardIter<Key<'a> = D::Key<'a>>,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<I::Block>,
    D: DataBlockIter + ForwardIter,
    for<'a> D::Value<'a>: AsArray<'a>,
    for<'a> R::Guard<'a>: Into<D::Block>,
{
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.index_iter.seek_to_first()?;
        if self.index_iter.valid() {
            let mut d = self.load_data_block_from_index()?;
            d.seek_to_first()?;
            self.data_iter = Some(d);
        }
        Ok(())
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        self.index_iter.seek(target)?;
        if self.index_iter.valid() {
            let mut d = self.load_data_block_from_index()?;
            d.seek(target)?;
            self.data_iter = Some(d);
        } else {
            self.data_iter = None;
        }
        Ok(())
    }

    fn next(&mut self) -> StorageResult<()> {
        if let Some(d) = &mut self.data_iter {
            d.next()?;
            if !d.valid() {
                self.index_iter.next()?;
                if !self.index_iter.valid() {
                    return Ok(());
                }
                let mut d = self.load_data_block_from_index()?;
                d.seek_to_first()?;
                self.data_iter = Some(d);
            }
        }
        Ok(())
    }
}

impl<R, I, D> ReverseIter for SstIter<R, I, D>
where
    R: BlockReader,
    D: DataBlockIter + ReverseIter,
    I: IndexBlockIter  + ReverseIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    for<'a> I: IterBase<Key<'a> = D::Key<'a>>,
    for<'a> R::Guard<'a>: Into<I::Block>,
    for<'a> R::Guard<'a>: Into<D::Block>,
{
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.index_iter.seek_to_first()?;
        if self.index_iter.valid() {
            let mut d = self.load_data_block_from_index()?;
            d.seek_to_first()?;
            self.data_iter = Some(d);
        }
        Ok(())
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        self.index_iter.seek(target)?;
        loop {
            if !self.index_iter.valid() {
                self.data_iter = None;
                return Ok(());
            }
            let mut d = self.load_data_block_from_index()?;
            d.seek(target)?;
            if d.valid() {
                self.data_iter = Some(d);
                return Ok(());
            }
            // Gap: all keys in this block exceed target — try previous block.
            self.index_iter.next()?;
        }
    }

    fn next(&mut self) -> StorageResult<()> {
        if let Some(d) = &mut self.data_iter {
            d.next()?;
            if !d.valid() {
                self.index_iter.next()?;
                if !self.index_iter.valid() {
                    return Ok(());
                }
                let mut d = self.load_data_block_from_index()?;
                d.seek_to_first()?;
                self.data_iter = Some(d);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;

    use crate::{
        block::{InMemoryBlockReader, InMemoryBlockWriter},
        builder::{DefaultSstWriter, SstBuilder, SstFooter, SstOption},
        iterators::{
            block_iter::NormalBlockIter,
            data_entry_decode_iter::DataEntryDecodeIter,
            storage_iter::{AsArray, ForwardIter, IterBase, IterRead},
        },
        testing::init_tracing,
    };

    use super::SstIter;

    fn make_key(i: u64) -> Bytes {
        Bytes::copy_from_slice(&i.to_be_bytes())
    }

    fn make_value(i: u64) -> Bytes {
        Bytes::from(format!("value_{:04}", i))
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
    fn make_iter(
        n: u64,
        block_size: usize,
    ) -> SstIter<InMemoryBlockReader, NormalBlockIter, DataEntryDecodeIter<NormalBlockIter>> {
        let (bytes, footer, option) = build_sst(n, block_size);
        let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(bytes), block_size));
        SstIter::<_, NormalBlockIter, DataEntryDecodeIter<NormalBlockIter>>::new(
            reader, footer, option,
        )
        .unwrap()
    }

    // 空 SST seek_to_first 后 valid() 应为 false
    #[test]
    fn test_empty_sst_is_invalid() {
        let mut iter = make_iter(0, 256);
        iter.seek_to_first().unwrap();
        assert!(!iter.valid());
    }

    // seek_to_first 定位到第一个 key
    #[test]
    fn test_seek_to_first_returns_first_key() {
        let mut iter = make_iter(200, 256);
        iter.seek_to_first().unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), (&make_key(0)).into());
        assert_eq!(iter.value().unwrap().as_array(), make_value(0).as_ref());
    }

    // next() 按顺序遍历所有 key，数量和顺序都正确
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

    // Regression: a deep index tree (height >= 4) must be fully reachable.
    // Two latent bugs once hid here, both uncovered only by tall trees:
    //  - SstBuilder::finish left the topmost index level unflushed when it held
    //    >1 entry, so `root_position` reached only the last top-level block and
    //    seek_to_first landed near the END of the key space (keys 32..39 of 40)
    //    instead of key 0.
    //  - IndexTreeIter::inner_next, when a forward step exhausted the leaf AND
    //    its parent in one call, re-descended by calling next() on a freshly
    //    seek_to_first'd child — skipping that child's first entry (and its
    //    whole subtree), dropping entries mid-scan.
    // block_size=64 + 40 entries forces height=6 here. A clean full scan of all
    // keys in order proves both the root reaches key 0 and no subtree is skipped.
    #[test]
    fn test_deep_tree_full_scan_reaches_all_keys() {
        init_tracing();
        let n = 40u64;
        let mut iter = make_iter(n, 64);
        // height sanity: this shape must actually be a tall tree, else the test
        // stops exercising the regressions it guards.
        let (bytes, footer, _option) = build_sst(n, 64);
        assert!(
            footer.tree_height >= 4,
            "expected deep tree, got height={}",
            footer.tree_height
        );
        let _ = bytes;

        iter.seek_to_first().unwrap();
        assert_eq!(iter.key().unwrap(), (&make_key(0)).into(), "seek_to_first must land on key 0");
        for i in 0..n {
            assert!(iter.valid(), "expected valid at i={}", i);
            assert_eq!(iter.key().unwrap(), (&make_key(i)).into(), "key mismatch at i={}", i);
            assert_eq!(iter.value().unwrap().as_array(), make_value(i).as_ref());
            iter.next().unwrap();
        }
        assert!(!iter.valid(), "expected invalid after last entry");
    }

    // seek 精确命中
    // Regression (reverse): mirror of test_deep_tree_full_scan_reaches_all_keys
    // for the backward path. IndexTreeIter::inner_prev had the same multi-level-
    // pop skip bug as inner_next — a reverse scan over a deep tree dropped large
    // key ranges (e.g. [39,38,37,36,33,32,1,0] instead of all 40 descending).
    #[test]
    fn test_reverse_deep_tree_full_scan_reaches_all_keys() {
        use crate::iterators::storage_iter::ReverseIter;
        init_tracing();
        let n = 40u64;
        let (bytes, footer, _option) = build_sst(n, 64);
        assert!(footer.tree_height >= 4, "expected deep tree");
        let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(bytes), 64));
        let mut iter = SstIter::<_, NormalBlockIter, DataEntryDecodeIter<NormalBlockIter>>::new(
            reader, footer, SstOption::default().block_size(64),
        )
        .unwrap();
        ReverseIter::seek_to_first(&mut iter).unwrap();
        assert_eq!(iter.key().unwrap(), (&make_key(n - 1)).into(), "reverse seek must land on last key");
        let mut got: Vec<u64> = Vec::new();
        while iter.valid() {
            let k = iter.key().unwrap();
            got.push(u64::from_be_bytes(k.as_array()[..8].try_into().unwrap()));
            ReverseIter::next(&mut iter).unwrap();
        }
        let expect: Vec<u64> = (0..n).rev().collect();
        assert_eq!(got, expect);
    }

    // seek 精确命中
    #[test]
    fn test_seek_exact_match() {
        let mut iter = make_iter(200, 256);
        let k = make_key(100);
        iter.seek(&(&k).into()).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), (&k).into());
        assert_eq!(iter.value().unwrap().as_array(), make_value(100).as_ref());
    }

    // seek 落在两个 key 之间，定位到后一个
    #[test]
    fn test_seek_between_keys() {
        let mut iter = make_iter(200, 256);
        // 构造一个介于 key(50) 和 key(51) 之间的字节序列
        let mut between = make_key(50).to_vec();
        *between.last_mut().unwrap() += 1; // 50 + 小量偏移
        iter.seek(&(&between).into()).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), (&make_key(51)).into());
    }

    // seek 小于所有 key，定位到第一个
    #[test]
    fn test_seek_before_first_key() {
        let mut iter = make_iter(200, 256);
        let before = vec![0u8; 7]; // 小于 key(0) = [0,0,0,0,0,0,0,0]
        iter.seek(&(&before).into()).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), (&make_key(0)).into());
    }

    // seek 超过最后一个 key， valid() 应为 false
    #[test]
    fn test_seek_after_last_key() {
        let mut iter = make_iter(200, 256);
        let beyond = make_key(u64::MAX);
        iter.seek(&(&beyond).into()).unwrap();
        assert!(!iter.valid());
    }
}
