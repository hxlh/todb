use crate::{
    errors::StorageResult,
    iterators::storage_iter::{AsArray, ForwardIter, ReverseIter, StorageIter},
    memtable::Entry,
};

/// Object-safe iterator trait for engine API boundaries.
///
/// Unlike [`StorageIter`], this trait has no GATs and can be used as
/// `Box<dyn ScanIter>`. Internally, each engine uses [`StorageIter`] with
/// typed GAT keys/values for zero-copy; [`ScanAdapter`] bridges the two.
pub trait ScanIter: Send {
    fn valid(&self) -> bool;
    fn seek_to_first(&mut self) -> StorageResult<()>;
    fn seek(&mut self, target: &[u8]) -> StorageResult<()>;
    fn next(&mut self) -> StorageResult<()>;
    fn key(&self) -> Option<&[u8]>;
    fn value(&self) -> Option<Entry<&[u8]>>;
}

/// Adapter that wraps a [`StorageIter`] and exposes it as [`ScanIter`].
///
/// Projects GAT key/value types to `&[u8]` / [`Entry`] at the boundary.
/// Zero-copy: `as_array()` returns the inner byte slice without copying.
pub struct ScanAdapter<I: StorageIter> {
    inner: I,
}

impl<I: StorageIter> ScanAdapter<I> {
    pub fn new(inner: I) -> Self {
        Self { inner }
    }
}

impl<I> ScanIter for ScanAdapter<I>
where
    I: StorageIter + Send,
    for<'a> I::Key<'a>: AsArray<'a> + From<&'a [u8]>,
    for<'a> I::Value<'a>: Into<Entry<&'a [u8]>>,
{
    fn valid(&self) -> bool {
        self.inner.valid()
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner.seek_to_first()
    }

    fn seek(&mut self, target: &[u8]) -> StorageResult<()> {
        let key = I::Key::from(target);
        self.inner.seek(&key)
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner.next()
    }

    fn key(&self) -> Option<&[u8]> {
        self.inner.key().map(|k| k.as_array())
    }

    fn value(&self) -> Option<Entry<&[u8]>> {
        self.inner.value().map(|v| v.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iterators::data_entry_decode_iter::EntryValue;

    /// A minimal StorageIter for testing: yields one entry.
    struct SingleEntry {
        key: Vec<u8>,
        value: Vec<u8>,
        valid: bool,
    }

    impl ForwardIter for SingleEntry {
        type Key<'a> = crate::row_key::BinaryKey<'a>;
        type Value<'a> = EntryValue<'a>
        where
            Self: 'a;

        fn seek_to_first(&mut self) -> StorageResult<()> {
            self.valid = true;
            Ok(())
        }
        fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
            self.valid = self.key.as_slice() >= target.as_bytes();
            Ok(())
        }
        fn next(&mut self) -> StorageResult<()> {
            self.valid = false;
            Ok(())
        }
    }

    impl ReverseIter for SingleEntry {
        fn seek_to_last(&mut self) -> StorageResult<()> {
            self.valid = true;
            Ok(())
        }
        fn seek_for_prev<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
            self.valid = self.key.as_slice() <= target.as_bytes();
            Ok(())
        }
        fn prev(&mut self) -> StorageResult<()> {
            self.valid = false;
            Ok(())
        }
    }

    impl StorageIter for SingleEntry {
        fn valid(&self) -> bool {
            self.valid
        }
        fn key(&self) -> Option<Self::Key<'_>> {
            if self.valid {
                Some(crate::row_key::BinaryKey::from(self.key.as_slice()))
            } else {
                None
            }
        }
        fn value(&self) -> Option<Self::Value<'_>> {
            if self.valid {
                Some(EntryValue::Put(&self.value))
            } else {
                None
            }
        }
    }

    /// Verify ScanAdapter projects key/value correctly from a simple StorageIter.
    #[test]
    fn test_scan_adapter_projects_key_and_value() {
        let inner = SingleEntry {
            key: b"k1".to_vec(),
            value: b"v1".to_vec(),
            valid: false,
        };

        let mut adapter = ScanAdapter::new(inner);
        adapter.seek_to_first().unwrap();

        assert!(adapter.valid());
        assert_eq!(adapter.key(), Some(&b"k1"[..]));
        assert_eq!(adapter.value(), Some(Entry::Put(&b"v1"[..])));

        adapter.next().unwrap();
        assert!(!adapter.valid());
        assert_eq!(adapter.key(), None);
        assert_eq!(adapter.value(), None);
    }

    /// Verify ScanAdapter is dyn-compatible (can be boxed).
    #[test]
    fn test_scan_adapter_is_dyn_compatible() {
        let inner = SingleEntry {
            key: b"dyn".to_vec(),
            value: b"ok".to_vec(),
            valid: false,
        };
        let adapter = ScanAdapter::new(inner);
        let _boxed: Box<dyn ScanIter> = Box::new(adapter);
    }
}
