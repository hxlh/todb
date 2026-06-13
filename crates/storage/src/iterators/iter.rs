use bytes::Bytes;

use crate::errors::StorageResult;

pub trait AsArray<'a> {
    fn as_array(&self) -> &'a [u8];
}

pub trait StorageIter {
    type Key<'a>: Ord;
    type Value<'a>
    where
        Self: 'a;

    fn valid(&self) -> bool;
    fn seek_to_first(&mut self) -> StorageResult<()>;
    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()>;
    fn next(&mut self) -> StorageResult<()>;
    fn key(&self) -> Option<Self::Key<'_>>;
    fn value(&self) -> Option<Self::Value<'_>>;
}

/// Abstracts the format of an index block.
/// Implementations yield (key, Position) pairs from raw block bytes.
/// Different index formats (B+tree node, prefix-compressed, etc.) implement this.
pub trait IndexBlockIter: StorageIter + Sized {
    fn from_block(block: Bytes) -> StorageResult<Self>;
}

/// Abstracts the format of a data block.
/// Implementations yield (key, value) pairs from raw block bytes.
pub trait DataBlockIter: StorageIter + Sized {
    fn from_block(block: Bytes) -> StorageResult<Self>;
}
