use bytes::Bytes;

use crate::errors::StorageResult;

pub trait AsArray<'a> {
    fn as_array(&self) -> &'a [u8];
}

/// Forward scan iterator.
///
/// `Key` and `Value` are declared here only — [`ReverseIter`] and
/// [`StorageIter`] inherit them via the supertrait chain.
pub trait ForwardIter {
    type Key<'a>: Ord;
    type Value<'a>
    where
        Self: 'a;

    fn seek_to_first(&mut self) -> StorageResult<()>;
    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()>;
    fn next(&mut self) -> StorageResult<()>;
}

/// Reverse scan iterator. Inherits `Key`/`Value` from [`ForwardIter`].
pub trait ReverseIter: ForwardIter {
    fn seek_to_last(&mut self) -> StorageResult<()>;
    fn seek_for_prev(&mut self, target: &Self::Key<'_>) -> StorageResult<()>;  // position at last key <= target
    fn prev(&mut self) -> StorageResult<()>;                                     // move backward
}

/// Bidirectional storage iterator: forward + reverse + state queries.
///
/// A single iterator instance must not interleave directions (matches
/// RocksDB MergingIterator semantics). Start a forward scan with
/// `seek_to_first` or `seek`, a reverse scan with `seek_to_last` or
/// `seek_for_prev`.
pub trait StorageIter: ReverseIter {
    fn valid(&self) -> bool;
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
