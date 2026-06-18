use crate::{
    errors::StorageResult,
    memtable::Entry,
};

/// Object-safe iterator trait for engine API boundaries.
///
/// Unlike `ForwardIter`/`ReverseIter`, this trait has no GATs and can be used as
/// `Box<dyn ScanIter>`. Internally, each engine uses `ForwardIter` with
/// typed GAT keys/values for zero-copy; `ScanAdapter` bridges the two.
pub trait ScanIter: Send {
    fn valid(&self) -> bool;
    fn key(&self) -> Option<&[u8]>;
    fn value(&self) -> Option<Entry<&[u8]>>;
    fn seek_to_first(&mut self) -> StorageResult<()>;
    fn seek(&mut self, target: &[u8]) -> StorageResult<()>;
    fn next(&mut self) -> StorageResult<()>;
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        iterators::{
            data_entry_decode_iter::EntryValue,
            storage_iter::{ForwardIter, IterBase, IterRead, ReverseIter},
        },
        row_key::BinaryKey,
    };

    /// A minimal iterator for testing: yields one entry.
    struct SingleEntry {
        key: Vec<u8>,
        value: Vec<u8>,
        valid: bool,
    }

    impl IterBase for SingleEntry {
        type Key<'a> = BinaryKey<'a>;
        type Value<'a> = EntryValue<'a>;
    }

    impl IterRead for SingleEntry {
        fn valid(&self) -> bool {
            self.valid
        }
        fn key(&self) -> Option<Self::Key<'_>> {
            self.valid
                .then(|| BinaryKey::from(self.key.as_slice()))
        }
        fn value(&self) -> Option<Self::Value<'_>> {
            self.valid.then(|| EntryValue::Put(&self.value))
        }
    }

    impl ForwardIter for SingleEntry {
        fn seek_to_first(&mut self) -> StorageResult<()> {
            self.valid = true;
            Ok(())
        }
        fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
            self.valid = self.key.as_slice() >= target.as_bytes();
            Ok(())
        }
        fn next(&mut self) -> StorageResult<()> {
            self.valid = false;
            Ok(())
        }
    }

    impl ReverseIter for SingleEntry {
        fn seek_to_first(&mut self) -> StorageResult<()> {
            self.valid = true;
            Ok(())
        }
        fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
            self.valid = self.key.as_slice() <= target.as_bytes();
            Ok(())
        }
        fn next(&mut self) -> StorageResult<()> {
            self.valid = false;
            Ok(())
        }
    }
}
