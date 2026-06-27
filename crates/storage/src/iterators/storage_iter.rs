use std::ops::Deref;

use crate::errors::StorageResult;

pub trait AsArray<'a> {
    fn as_array(&self) -> &'a [u8];
}

/// Shared GAT type declarations only — no methods.
///
/// `valid/key/value` **cannot** live here: `fn key(&self) -> Option<Self::Key<'_>>`
/// forces `where Self: 'a` on the GAT in edition 2024 (rust issue #87479), which
/// cascades E0309/E0311 to every generic type parameter. The methods live one
/// level down on [`IterRead`] so the GAT stays free of the `where Self: 'a` bound.
pub trait IterBase {
    type Key<'a>: Ord;
    type Value<'a>;
}

/// State-query methods shared by both scan directions.
///
/// Lives on its own trait (not on [`IterBase`]) to keep the GAT declarations
/// separate from the methods that return them — this is what avoids the
/// `where Self: 'a` cascade. [`ForwardIter`] and [`ReverseIter`] both inherit
/// these methods via the supertrait chain.
pub trait IterRead: IterBase {
    fn valid(&self) -> bool;
    fn key(&self) -> Option<Self::Key<'_>>;
    fn value(&self) -> Option<Self::Value<'_>>;
}

/// Forward scan iterator.
///
/// `next()` moves toward larger keys.
pub trait ForwardIter: IterRead {
    fn seek_to_first(&mut self) -> StorageResult<()>;
    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()>;
    fn next(&mut self) -> StorageResult<()>;
}

/// Reverse scan iterator — sibling of [`ForwardIter`].
///
/// Same method names but mirrored: `seek_to_first` positions at the largest
/// key, `seek` positions at the last key ≤ target, and `next()` moves toward
/// smaller keys.
pub trait ReverseIter: IterRead {
    fn seek_to_first(&mut self) -> StorageResult<()>;
    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()>;
    fn next(&mut self) -> StorageResult<()>;
}

/// Abstracts the format of an index block.
/// Implementations yield (key, Position) pairs from raw block bytes.
/// Different index formats (B+tree node, prefix-compressed, etc.) implement this.
///
/// `Block` is the storage type the iterator holds (e.g. owned `Bytes` for LSM,
/// or a borrowed guard for cached reads). `from_block` takes ownership of it
/// directly — no copy when the source already owns the bytes.
pub trait IndexBlockIter: IterRead {
    type Block: Deref<Target = [u8]>;

    fn from_block(block: Self::Block) -> StorageResult<Self>
    where
        Self: Sized;

    /// Position at the first key >= target (lower bound).
    ///
    /// Locates the child block containing `target`. Direction-agnostic: both
    /// forward and reverse scans call this to find the block, then traverse
    /// within it via [`ForwardIter`] / [`ReverseIter`].
    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()>;
}

/// Abstracts the format of a data block.
/// Implementations yield (key, value) pairs from raw block bytes.
pub trait DataBlockIter: IterBase {
    type Block: Deref<Target = [u8]>;

    fn from_block(block: Self::Block) -> StorageResult<Self>
    where
        Self: Sized;
}
