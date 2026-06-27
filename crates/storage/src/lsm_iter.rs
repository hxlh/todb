use std::ops::Bound;

use bytes::Bytes;

use crate::{
    errors::StorageResult,
    iterators::{
        scan_iter::ScanIter,
        storage_iter::{AsArray, ForwardIter, ReverseIter},
    },
    memtable::Entry,
};
// ── Forward range-limited scan ──

/// Forward scan: lower bound positions via seek, upper bound enforced in `valid()`.
pub struct LsmForwardScan<I: ForwardIter> {
    inner: I,
    lower: Bound<Bytes>,
    upper: Bound<Bytes>,
}

impl<I> LsmForwardScan<I>
where
    I: ForwardIter,
    for<'a> I::Key<'a>: From<&'a [u8]>,
{
    pub(crate) fn new(inner: I, lower: Bound<Bytes>, upper: Bound<Bytes>) -> Self {
        Self { inner, lower, upper }
    }

    pub(crate) fn init(&mut self) -> StorageResult<()> {
        match &self.lower {
            Bound::Included(start) => self.inner.seek(&start.as_ref().into()),
            Bound::Excluded(start) => {
                self.inner.seek(&start.as_ref().into())?;
                while self.inner.valid() {
                    let is_equal = self.inner.key().map_or(false, |k| k == start.as_ref().into());
                    if is_equal {
                        self.inner.next()?;
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            Bound::Unbounded => self.inner.seek_to_first(),
        }
    }
}

impl<I> ScanIter for LsmForwardScan<I>
where
    I: ForwardIter + Send,
    for<'a> I::Key<'a>: AsArray<'a> + From<&'a [u8]>,
    for<'a> I::Value<'a>: Into<Entry<&'a [u8]>>,
{
    fn valid(&self) -> bool {
        if !self.inner.valid() {
            return false;
        }
        let key = self.inner.key().unwrap();
        match &self.upper {
            Bound::Included(end) => key <= (&end[..]).into(),
            Bound::Excluded(end) => key < (&end[..]).into(),
            Bound::Unbounded => true,
        }
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner.seek_to_first()
    }

    fn seek(&mut self, target: &[u8]) -> StorageResult<()> {
        self.inner.seek(&target.into())
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner.next()
    }

    fn key(&self) -> Option<&[u8]> {
        if !self.valid() {
            return None;
        }
        self.inner.key().map(|k| k.as_array())
    }

    fn value(&self) -> Option<Entry<&[u8]>> {
        if !self.valid() {
            return None;
        }
        self.inner.value().map(|v| v.into())
    }
}

// ── Reverse range-limited scan ──

/// Reverse scan: upper bound positions via seek, lower bound enforced in `valid()`.
pub struct LsmReverseScan<I: ReverseIter> {
    inner: I,
    lower: Bound<Bytes>,
    upper: Bound<Bytes>,
}

impl<I> LsmReverseScan<I>
where
    I: ReverseIter,
    for<'a> I::Key<'a>: From<&'a [u8]>,
{
    pub(crate) fn new(inner: I, lower: Bound<Bytes>, upper: Bound<Bytes>) -> Self {
        Self { inner, lower, upper }
    }

    pub(crate) fn init(&mut self) -> StorageResult<()> {
        match &self.upper {
            Bound::Included(end) => self.inner.seek(&end.as_ref().into()),
            Bound::Excluded(end) => {
                self.inner.seek(&end.as_ref().into())?;
                while self.inner.valid() {
                    let is_equal = self.inner.key().map_or(false, |k| k == end.as_ref().into());
                    if is_equal {
                        self.inner.next()?;
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            Bound::Unbounded => self.inner.seek_to_first(),
        }
    }
}

impl<I> ScanIter for LsmReverseScan<I>
where
    I: ReverseIter + Send,
    for<'a> I::Key<'a>: AsArray<'a> + From<&'a [u8]>,
    for<'a> I::Value<'a>: Into<Entry<&'a [u8]>>,
{
    fn valid(&self) -> bool {
        if !self.inner.valid() {
            return false;
        }
        let key = self.inner.key().unwrap();
        match &self.lower {
            Bound::Included(start) => key >= (&start[..]).into(),
            Bound::Excluded(start) => key > (&start[..]).into(),
            Bound::Unbounded => true,
        }
    }

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner.seek_to_first()
    }

    fn seek(&mut self, target: &[u8]) -> StorageResult<()> {
        self.inner.seek(&target.into())
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner.next()
    }

    fn key(&self) -> Option<&[u8]> {
        if !self.valid() {
            return None;
        }
        self.inner.key().map(|k| k.as_array())
    }

    fn value(&self) -> Option<Entry<&[u8]>> {
        if !self.valid() {
            return None;
        }
        self.inner.value().map(|v| v.into())
    }
}
