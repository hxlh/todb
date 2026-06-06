use crate::{errors::StorageResult, iterators::iter::StorageIter};

/// Extension of [`StorageIter`] that defines how an iterator's native
/// key/value types are exposed to merge layers.
///
/// Most iterators implement this as an *identity* mapping (native types
/// are already what the merge layer expects). Heterogeneous sources like
/// memtable need a non-trivial mapping (e.g. `&Bytes` → `RowKey<'a>`).
///
/// # Why this trait is on `I` instead of a separate `M` parameter
///
/// Keeping the mapping as part of the iterator itself means `MapIter`
/// needs only one generic parameter (`MapIter<I>`), and `TwoMergeIter`
/// sees a single unified type on each side.
///
/// # Why GAT (`MappedKey<'a>`)
///
/// `MappedKey<'a>` can be a borrowed type like `RowKey<'a>` or `&[u8]`.
/// A plain generic parameter `K` on `MapIter<I, K>` cannot carry a
/// lifetime, so it could not represent `RowKey<'a>`. GAT hides the
/// lifetime inside the associated type, keeping `MapIter` itself
/// `'static` (required by `TwoMergeIter`'s HRTB bounds).
pub trait MappedStorageIter: StorageIter + 'static {
    type MappedKey<'a>: Ord
    where
        Self: 'a;
    type MappedValue<'a>
    where
        Self: 'a;

    fn map_key<'a>(key: Self::Key<'a>) -> Self::MappedKey<'a>;
    fn map_value<'a>(value: Self::Value<'a>) -> Self::MappedValue<'a>;
    fn seek_mapped(&mut self, target: &Self::MappedKey<'_>) -> StorageResult<()>;
}

/// Adapter iterator that exposes an inner iterator through its
/// [`MappedStorageIter`] mapping.
///
/// Like `Option::map` — purely generic. The concrete mapped types are
/// determined by the inner iterator's `MappedStorageIter` implementation.
pub struct MapIter<I: MappedStorageIter> {
    inner: I,
}

impl<I: MappedStorageIter> MapIter<I> {
    pub fn new(inner: I) -> Self {
        Self { inner }
    }
}

impl<I: MappedStorageIter> StorageIter for MapIter<I> {
    type Key<'a> = I::MappedKey<'a>;
    type Value<'a> = I::MappedValue<'a>
    where
        Self: 'a;

    fn valid(&self) -> bool {
        self.inner.valid()
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner.seek_to_first()
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        self.inner.seek_mapped(target)
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner.next()
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        self.inner.key().map(I::map_key)
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        self.inner.value().map(I::map_value)
    }
}
