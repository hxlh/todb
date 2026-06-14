use std::sync::Arc;

use crate::{
    block::{Position, BlockReader},
    builder::{SstFooter, SstOption},
    errors::{StorageError, StorageResult},
    iterators::{
        block_iter::NormalBlockIter,
        data_entry_decode_iter::DataEntryDecodeIter,
        index_tree_iter::IndexTreeIter,
        storage_iter::{AsArray, DataBlockIter, ForwardIter, IndexBlockIter, ReverseIter, StorageIter},
    },
};

pub struct SstIter<R, I = NormalBlockIter, D = DataEntryDecodeIter<NormalBlockIter>>
where
    R: BlockReader,
    I: IndexBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    D: DataBlockIter,
    for<'a> D::Value<'a>: AsArray<'a>,
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
    D: DataBlockIter,
    for<'a> D::Value<'a>: AsArray<'a>,
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
    for<'a> I::Value<'a>: AsArray<'a>,
    D: DataBlockIter,
    for<'a> D::Value<'a>: AsArray<'a>,
    for<'a> I: StorageIter<Key<'a> = D::Key<'a>>,
{
    /// Load and decode the data block at the index iterator's current
    /// position. The caller is responsible for positioning the returned
    /// iterator (seek_to_first / seek / seek_to_last / seek_for_prev).
    fn load_data_block(&mut self) -> StorageResult<D> {
        let pos: Position = self.index_iter.value().ok_or_else(|| {
            StorageError::InvalidValue("index iter valid but value is none".into())
        })?;
        let block = self.reader.read_block(&pos)?;
        D::from_block(block)
    }
}

impl<R, I, D> ForwardIter for SstIter<R, I, D>
where
    R: BlockReader,
    I: IndexBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    D: DataBlockIter,
    for<'a> D::Value<'a>: AsArray<'a>,
    // Index and data block must share the same key type so seek targets are compatible.
    // Key<'a> has no `where Self: 'a`, so for<'a> does not require I: 'static or D: 'static.
    for<'a> I: StorageIter<Key<'a> = D::Key<'a>>,
{
    type Key<'a> = D::Key<'a>;
    type Value<'a> = D::Value<'a> where Self: 'a;
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.index_iter.seek_to_first()?;
        if self.index_iter.valid() {
            let mut d = self.load_data_block()?;
            d.seek_to_first()?;
            self.data_iter = Some(d);
        }
        Ok(())
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        self.index_iter.seek(target)?;
        if self.index_iter.valid() {
            let mut d = self.load_data_block()?;
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
                let mut d = self.load_data_block()?;
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
    I: IndexBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    D: DataBlockIter,
    for<'a> D::Value<'a>: AsArray<'a>,
    for<'a> I: StorageIter<Key<'a> = D::Key<'a>>,
{
    fn seek_to_last(&mut self) -> StorageResult<()> {
        self.index_iter.seek_to_last()?;
        if self.index_iter.valid() {
            let mut d = self.load_data_block()?;
            d.seek_to_last()?;
            self.data_iter = Some(d);
        }
        Ok(())
    }

    fn seek_for_prev<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        // Index uses end-key convention: each entry's key is the largest key
        // in its data block. Forward seek (first end_key >= target) lands on
        // the block that *may* contain target. If that block has no key <=
        // target (target falls in a gap between blocks), fall back to the
        // previous block whose keys are all < target.
        self.index_iter.seek(target)?;
        if !self.index_iter.valid() {
            self.index_iter.seek_to_last()?;
        }
        loop {
            if !self.index_iter.valid() {
                self.data_iter = None;
                return Ok(());
            }
            let mut d = self.load_data_block()?;
            d.seek_for_prev(target)?;
            if d.valid() {
                self.data_iter = Some(d);
                return Ok(());
            }
            // Gap: all keys in this block exceed target — try previous block.
            self.index_iter.prev()?;
        }
    }

    fn prev(&mut self) -> StorageResult<()> {
        if let Some(d) = &mut self.data_iter {
            d.prev()?;
            if !d.valid() {
                self.index_iter.prev()?;
                if !self.index_iter.valid() {
                    return Ok(());
                }
                let mut d = self.load_data_block()?;
                d.seek_to_last()?;
                self.data_iter = Some(d);
            }
        }
        Ok(())
    }
}

impl<R, I, D> StorageIter for SstIter<R, I, D>
where
    R: BlockReader,
    I: IndexBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
    D: DataBlockIter,
    for<'a> D::Value<'a>: AsArray<'a>,
    for<'a> I: StorageIter<Key<'a> = D::Key<'a>>,
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
            storage_iter::{AsArray, ForwardIter, StorageIter},
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
        let mut builder = SstBuilder::new(DefaultSstWriter::new(InMemoryBlockWriter::new(), &option), option.clone());
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
        SstIter::<_, NormalBlockIter, DataEntryDecodeIter<NormalBlockIter>>::new(reader, footer, option)
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
