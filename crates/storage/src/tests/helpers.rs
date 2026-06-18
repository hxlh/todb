use std::sync::Arc;

use bytes::Bytes;

use crate::{
    block::{InMemoryBlockReader, InMemoryBlockWriter},
    builder::{DefaultSstWriter, SstBuilder, SstFooter, SstOption},
    iterators::{storage_iter::{ForwardIter, IterRead}, sst_iter::SstIter},
};

pub fn make_key(i: u64) -> Bytes {
    Bytes::copy_from_slice(&i.to_be_bytes())
}

pub fn make_value(i: u64) -> Bytes {
    Bytes::from(format!("value_{:04}", i))
}

/// Build an SST with keys [start, end) and return a ready-to-use SstIter.
pub fn make_sst_iter(start: u64, end: u64) -> SstIter<InMemoryBlockReader> {
    let option = SstOption::default().block_size(256);
    let mut builder = SstBuilder::new(DefaultSstWriter::new(InMemoryBlockWriter::new(), &option), option.clone());
    for i in start..end {
        builder.add(make_key(i), make_value(i)).unwrap();
    }
    let (footer, sst_writer) = builder.finish().unwrap();
    let bytes = Bytes::from(sst_writer.into_inner().into_inner());
    let reader = Arc::new(InMemoryBlockReader::new(bytes, 256));
    SstIter::new(reader, footer, option).unwrap()
}

/// Build a raw SST buffer with n keys and return (bytes, footer, option).
pub fn build_sst(n: u64, block_size: usize) -> (Vec<u8>, SstFooter, SstOption) {
    let option = SstOption::default().block_size(block_size);
    let mut builder = SstBuilder::new(DefaultSstWriter::new(InMemoryBlockWriter::new(), &option), option.clone());
    for i in 0..n {
        builder.add(make_key(i), make_value(i)).unwrap();
    }
    let (footer, sst_writer) = builder.finish().unwrap();
    (sst_writer.into_inner().into_inner(), footer, option)
}

/// Collect all keys from an iterator into a Vec<u64>.
pub fn collect_keys<I: ForwardIter>(iter: &mut I) -> Vec<u64>
where
    for<'a> I::Key<'a>: AsRef<[u8]>,
{
    let mut keys = vec![];
    while iter.valid() {
        let bytes: Vec<u8> = iter.key().unwrap().as_ref().to_vec();
        keys.push(u64::from_be_bytes(bytes.try_into().unwrap()));
        iter.next().unwrap();
    }
    keys
}
