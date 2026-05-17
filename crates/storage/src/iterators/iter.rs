use crate::errors::StorageResult;

pub trait StorageIter {
    type Key<'a>: Ord;
    type Value<'a>
    where
        Self: 'a;

    /// True if positioned at a valid entry.
    fn valid(&self) -> bool;

    /// Move to the first entry.
    fn seek_to_first(&mut self) -> StorageResult<()>;

    /// Move to the first entry with key >= target.
    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()>;

    /// Advance to the next entry.
    fn next(&mut self) -> StorageResult<()>;

    /// Current key, or None when invalid.
    fn key(&self) -> Option<Self::Key<'_>>;

    /// Current value, or None when invalid.
    fn value(&self) -> Option<Self::Value<'_>>;
}
