use std::sync::Arc;

use crate::{
    block::{BlockHandle, BlockReader},
    builder::{SstFooter, SstOption},
    errors::{StorageError, StorageResult},
    iterators::{
        block_iter::BlockIter,
        index_tree_iter::IndexTreeIter,
        iter::{DataBlockIter, IndexBlockIter, StorageIter},
    },
};

pub struct SstIter<R, I = BlockIter, D = BlockIter>
where
    R: BlockReader,
    I: IndexBlockIter,
    for<'a> BlockHandle: From<I::Value<'a>>,
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
    for<'a> BlockHandle: From<I::Value<'a>>,
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

impl<R, I, D> StorageIter for SstIter<R, I, D>
where
    R: BlockReader,
    I: IndexBlockIter,
    for<'a> BlockHandle: From<I::Value<'a>>,
    D: DataBlockIter,
    // Index and data block must share the same key type so seek targets are compatible.
    // Key<'a> has no `where Self: 'a`, so for<'a> does not require I: 'static or D: 'static.
    for<'a> I: StorageIter<Key<'a> = D::Key<'a>>,
{
    type Key<'a> = D::Key<'a>;

    type Value<'a> = D::Value<'a> where Self: 'a;

    fn valid(&self) -> bool {
        self.data_iter.as_ref().map_or(false, |d| d.valid())
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.index_iter.seek_to_first()?;
        if self.index_iter.valid() {
            let handle: BlockHandle = self.index_iter.value().ok_or_else(|| {
                StorageError::InvalidValue("index iter valid but value is none".into())
            })?;
            let block = self.reader.read_block(&handle)?;
            let mut d = D::from_block(block)?;
            d.seek_to_first()?;
            self.data_iter = Some(d);
        }
        Ok(())
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        // target: &D::Key<'a> = &I::Key<'a> (enforced by the Key equality bound)
        self.index_iter.seek(target)?;
        if self.index_iter.valid() {
            let handle: BlockHandle = self.index_iter.value().ok_or_else(|| {
                StorageError::InvalidValue("index iter valid but value is none".into())
            })?;
            let block = self.reader.read_block(&handle)?;
            let mut d = D::from_block(block)?;
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
                let handle: BlockHandle = self.index_iter.value().ok_or_else(|| {
                    StorageError::InvalidValue("index iter valid but value is none".into())
                })?;
                let block = self.reader.read_block(&handle)?;
                let mut d = D::from_block(block)?;
                d.seek_to_first()?;
                self.data_iter = Some(d);
            }
        }
        Ok(())
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
        builder::{SstBuilder, SstFooter, SstOption},
        iterators::{block_iter::BlockIter, iter::StorageIter},
        row_key::RowKey,
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
        let mut builder = SstBuilder::new(InMemoryBlockWriter::new(), option.clone());
        for i in 0..n {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, writer) = builder.finish().unwrap();
        (writer.into_inner(), footer, option)
    }

    fn make_iter(n: u64, block_size: usize) -> SstIter<InMemoryBlockReader, BlockIter, BlockIter> {
        let (bytes, footer, option) = build_sst(n, block_size);
        let reader = Arc::new(InMemoryBlockReader::new(Bytes::from(bytes), block_size));
        SstIter::<_, BlockIter, BlockIter>::new(reader, footer, option).unwrap()
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
        assert_eq!(iter.key().unwrap(), RowKey::from_slice(&make_key(0)));
        assert_eq!(iter.value().unwrap(), make_value(0).as_ref());
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
            assert_eq!(iter.key().unwrap(), RowKey::from_slice(&make_key(i)));
            assert_eq!(iter.value().unwrap(), make_value(i).as_ref());
            iter.next().unwrap();
        }
        assert!(!iter.valid(), "expected invalid after last entry");
    }

    // seek 精确命中
    #[test]
    fn test_seek_exact_match() {
        let mut iter = make_iter(200, 256);
        let k = make_key(100);
        iter.seek(&RowKey::from_slice(&k)).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), RowKey::from_slice(&k));
        assert_eq!(iter.value().unwrap(), make_value(100).as_ref());
    }

    // seek 落在两个 key 之间，定位到后一个
    #[test]
    fn test_seek_between_keys() {
        let mut iter = make_iter(200, 256);
        // 构造一个介于 key(50) 和 key(51) 之间的字节序列
        let mut between = make_key(50).to_vec();
        *between.last_mut().unwrap() += 1; // 50 + 小量偏移
        iter.seek(&RowKey::from_slice(&between)).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), RowKey::from_slice(&make_key(51)));
    }

    // seek 小于所有 key，定位到第一个
    #[test]
    fn test_seek_before_first_key() {
        let mut iter = make_iter(200, 256);
        let before = vec![0u8; 7]; // 小于 key(0) = [0,0,0,0,0,0,0,0]
        iter.seek(&RowKey::from_slice(&before)).unwrap();
        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), RowKey::from_slice(&make_key(0)));
    }

    // seek 超过最后一个 key， valid() 应为 false
    #[test]
    fn test_seek_after_last_key() {
        let mut iter = make_iter(200, 256);
        let beyond = make_key(u64::MAX);
        iter.seek(&RowKey::from_slice(&beyond)).unwrap();
        assert!(!iter.valid());
    }
}
