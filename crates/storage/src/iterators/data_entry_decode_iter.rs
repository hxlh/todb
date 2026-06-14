use bytes::Bytes;

use crate::{
    errors::{StorageError, StorageResult},
    iterators::storage_iter::{AsArray, DataBlockIter, ForwardIter, ReverseIter, StorageIter},
};

const DATA_ENTRY_VERSION: u8 = 1;

/// 解码Value到具体语义(Put/Delete)
pub struct DataEntryDecodeIter<I> {
    input: I,
    entry_header: Option<EntryHeader>,
}

impl<I> DataEntryDecodeIter<I> {
    pub fn new(input: I) -> Self {
        Self {
            input,
            entry_header: None,
        }
    }

    fn decode_entry_header(&self, buf: &[u8]) -> StorageResult<EntryHeader> {
        if buf.len() < 2 {
            return Err(StorageError::InvalidValue("entry header too short".into()));
        }

        let format_version = u8::from_be_bytes(buf[0..1].try_into().unwrap());
        if format_version != DATA_ENTRY_VERSION {
            return Err(StorageError::InvalidValue(format!(
                "unknown data entry version: {format_version}"
            )));
        }

        let entry_kind = match buf[1] {
            0 => EntryKind::Put,
            1 => EntryKind::Delete,
            kind => {
                return Err(StorageError::InvalidValue(format!(
                    "invalid entry kind: {kind}"
                )));
            }
        };
        let payload_start = 2;
        Ok(EntryHeader {
            entry_kind,
            payload_start,
        })
    }
}

impl<I> DataEntryDecodeIter<I>
where
    I: StorageIter,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    fn refresh_current(&mut self) -> StorageResult<()> {
        if !self.input.valid() {
            self.entry_header = None;
            return Ok(());
        }

        let value = self.input.value().ok_or_else(|| {
            StorageError::InvalidValue("valid iterator has no entry value".into())
        })?;
        self.entry_header = Some(self.decode_entry_header(value.as_array())?);
        Ok(())
    }
}

impl<I> ForwardIter for DataEntryDecodeIter<I>
where
    I: StorageIter,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    type Key<'a> = I::Key<'a>;

    type Value<'a>
        = EntryValue<'a>
    where
        Self: 'a;

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.input.seek_to_first()?;
        self.refresh_current()
    }

    fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
        self.input.seek(target)?;
        self.refresh_current()
    }

    fn next(&mut self) -> StorageResult<()> {
        self.input.next()?;
        self.refresh_current()
    }
}

impl<I> ReverseIter for DataEntryDecodeIter<I>
where
    I: StorageIter,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    fn seek_to_last(&mut self) -> StorageResult<()> {
        self.input.seek_to_last()?;
        self.refresh_current()
    }

    fn seek_for_prev(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        self.input.seek_for_prev(target)?;
        self.refresh_current()
    }

    fn prev(&mut self) -> StorageResult<()> {
        self.input.prev()?;
        self.refresh_current()
    }
}

impl<I> StorageIter for DataEntryDecodeIter<I>
where
    I: StorageIter,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    fn valid(&self) -> bool {
        self.input.valid()
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        self.input.key()
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        if !self.valid() {
            return None;
        }

        let value = self.input.value()?;
        let entry_header = self.entry_header.as_ref()?;
        let payload = &value.as_array()[entry_header.payload_start..];
        Some(match entry_header.entry_kind {
            EntryKind::Put => EntryValue::Put(payload),
            EntryKind::Delete => EntryValue::Delete,
        })
    }
}

impl<I> DataBlockIter for DataEntryDecodeIter<I>
where
    I: DataBlockIter,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    fn from_block(block: Bytes) -> StorageResult<Self> {
        Ok(Self::new(I::from_block(block)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryValue<'a> {
    Put(&'a [u8]),
    Delete,
}

impl<'a> From<EntryValue<'a>> for crate::memtable::Entry<&'a [u8]> {
    fn from(v: EntryValue<'a>) -> Self {
        match v {
            EntryValue::Put(data) => Self::Put(data),
            EntryValue::Delete => Self::Delete,
        }
    }
}

impl<'a> AsArray<'a> for EntryValue<'a> {
    fn as_array(&self) -> &'a [u8] {
        match self {
            EntryValue::Put(buf) => buf,
            EntryValue::Delete => &[],
        }
    }
}


struct EntryHeader {
    entry_kind: EntryKind,
    payload_start: usize,
}

#[derive(Clone, Copy)]
enum EntryKind {
    Put,
    Delete,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{iterators::block_iter::RawEntry, iterators::storage_iter::StorageIter};

    struct VecEntryIter {
        entries: Vec<(&'static [u8], &'static [u8])>,
        pos: usize,
    }

    impl VecEntryIter {
        fn new(entries: Vec<(&'static [u8], &'static [u8])>) -> Self {
            Self {
                entries,
                pos: usize::MAX,
            }
        }
    }

    impl ForwardIter for VecEntryIter {
        type Key<'a> = &'a [u8];
        type Value<'a>
            = RawEntry<'a>
        where
            Self: 'a;

        fn seek_to_first(&mut self) -> StorageResult<()> {
            self.pos = if self.entries.is_empty() {
                usize::MAX
            } else {
                0
            };
            Ok(())
        }

        fn seek<'a>(&mut self, target: &Self::Key<'a>) -> StorageResult<()> {
            self.pos = self.entries.partition_point(|(key, _)| key < target);
            if self.pos >= self.entries.len() {
                self.pos = usize::MAX;
            }
            Ok(())
        }

        fn next(&mut self) -> StorageResult<()> {
            if self.valid() {
                self.pos += 1;
            }
            Ok(())
        }
    }

    impl ReverseIter for VecEntryIter {
        fn seek_to_last(&mut self) -> StorageResult<()> {
            self.pos = if self.entries.is_empty() {
                usize::MAX
            } else {
                self.entries.len() - 1
            };
            Ok(())
        }

        fn seek_for_prev(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
            let upper = self.entries.partition_point(|(key, _)| key <= target);
            self.pos = if upper == 0 {
                usize::MAX
            } else {
                upper - 1
            };
            Ok(())
        }

        fn prev(&mut self) -> StorageResult<()> {
            if self.pos == 0 {
                self.pos = usize::MAX;
            } else if self.pos < self.entries.len() {
                self.pos -= 1;
            }
            Ok(())
        }
    }

    impl StorageIter for VecEntryIter {
        fn valid(&self) -> bool {
            self.pos < self.entries.len()
        }

        fn key(&self) -> Option<Self::Key<'_>> {
            self.valid().then_some(self.entries[self.pos].0)
        }

        fn value(&self) -> Option<Self::Value<'_>> {
            self.valid()
                .then_some(RawEntry::from(self.entries[self.pos].1))
        }
    }

    fn put(payload: &'static [u8]) -> Vec<u8> {
        let mut entry = vec![1, 0];
        entry.extend_from_slice(payload);
        entry
    }

    #[test]
    fn seek_to_first_decodes_current_entry_payload() {
        let first = put(b"v1");
        let input = VecEntryIter::new(vec![(b"k1", Box::leak(first.into_boxed_slice()))]);
        let mut iter = DataEntryDecodeIter::new(input);

        iter.seek_to_first().unwrap();

        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), b"k1" as &[u8]);
        assert_eq!(iter.value().unwrap().as_array(), b"v1");
    }

    #[test]
    fn seek_decodes_target_entry_payload() {
        let first = put(b"v1");
        let second = put(b"v2");
        let input = VecEntryIter::new(vec![
            (b"k1", Box::leak(first.into_boxed_slice())),
            (b"k2", Box::leak(second.into_boxed_slice())),
        ]);
        let mut iter = DataEntryDecodeIter::new(input);

        iter.seek(&&b"k2"[..]).unwrap();

        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), b"k2" as &[u8]);
        assert_eq!(iter.value().unwrap().as_array(), b"v2");
    }

    #[test]
    fn next_decodes_next_entry_payload() {
        let first = put(b"v1");
        let second = put(b"v2");
        let input = VecEntryIter::new(vec![
            (b"k1", Box::leak(first.into_boxed_slice())),
            (b"k2", Box::leak(second.into_boxed_slice())),
        ]);
        let mut iter = DataEntryDecodeIter::new(input);

        iter.seek_to_first().unwrap();
        iter.next().unwrap();

        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), b"k2" as &[u8]);
        assert_eq!(iter.value().unwrap().as_array(), b"v2");
    }

    #[test]
    fn delete_entry_decodes_to_delete_variant() {
        let input = VecEntryIter::new(vec![(b"k1", b"\x01\x01")]);
        let mut iter = DataEntryDecodeIter::new(input);

        iter.seek_to_first().unwrap();

        assert!(iter.valid());
        assert!(matches!(iter.value().unwrap(), EntryValue::Delete));
    }

    #[test]
    fn unknown_data_entry_version_returns_error() {
        let input = VecEntryIter::new(vec![(b"k1", b"\x02\x00abc")]);
        let mut iter = DataEntryDecodeIter::new(input);

        assert!(iter.seek_to_first().is_err());
    }

    #[test]
    fn invalid_entry_kind_returns_error_when_positioning() {
        let input = VecEntryIter::new(vec![(b"k1", b"\x01\x02bad")]);
        let mut iter = DataEntryDecodeIter::new(input);

        assert!(iter.seek_to_first().is_err());
    }
}
