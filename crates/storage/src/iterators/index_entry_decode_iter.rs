use crate::{
    block::Position,
    errors::{StorageError, StorageResult},
    iterators::storage_iter::{AsArray, ForwardIter, IndexBlockIter, IterBase, IterRead, ReverseIter},
};

const INDEX_VALUE_VERSION: u8 = 1;
const POSITION_LEN: usize = size_of::<u64>();

pub struct IndexEntryDecodeIter<I> {
    input: I,
    header: Option<IndexEntryHeader>,
}

impl<I> IndexEntryDecodeIter<I> {
    pub fn new(input: I) -> Self {
        Self {
            input,
            header: None,
        }
    }

    fn decode_header(&self, buf: &[u8]) -> StorageResult<IndexEntryHeader> {
        if buf.is_empty() {
            return Err(StorageError::InvalidValue(
                "index entry header too short".into(),
            ));
        }

        let format_version = buf[0];
        if format_version != INDEX_VALUE_VERSION {
            return Err(StorageError::InvalidValue(format!(
                "unknown index entry version: {format_version}"
            )));
        }

        if buf.len() != 1 + POSITION_LEN {
            return Err(StorageError::InvalidValue(format!(
                "invalid index entry payload length: {}",
                buf.len() - 1
            )));
        }

        Ok(IndexEntryHeader {
            _format_version: format_version,
            payload_start: 1,
        })
    }
}

impl<I: IterBase> IterBase for IndexEntryDecodeIter<I>
where
    for<'a> I::Value<'a>: AsArray<'a>,
{
    type Key<'a> = I::Key<'a>;
    type Value<'a> = IndexEntryValue<'a>;
}

impl<I: IterRead> IterRead for IndexEntryDecodeIter<I>
where
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
        let header = self.header.as_ref()?;
        Some(IndexEntryValue {
            buf: &value.as_array()[header.payload_start..],
        })
    }
}

// ── Shared helpers (direction-agnostic) ──

impl<I: IterRead> IndexEntryDecodeIter<I>
where
    for<'a> I::Value<'a>: AsArray<'a>,
{
    /// Decode the header for the entry the input currently points at.
    /// Shared by forward and reverse paths — it only reads value/valid,
    /// not direction.
    fn refresh(&mut self) -> StorageResult<()> {
        if !self.input.valid() {
            self.header = None;
            return Ok(());
        }

        let value = self.input.value().ok_or_else(|| {
            StorageError::InvalidValue("valid iterator has no index entry value".into())
        })?;
        self.header = Some(self.decode_header(value.as_array())?);
        Ok(())
    }

    /// Direction-agnostic positioning (lower bound) for index-tree
    /// navigation. Used by `IndexTreeIter::locate` regardless of scan
    /// direction; in-block traversal still goes through Forward/ReverseIter.
    pub(crate) fn seek_lower_bound(&mut self, target: &I::Key<'_>) -> StorageResult<()>
    where
        I: IndexBlockIter,
    {
        self.input.seek(target)?;
        self.refresh()
    }
}

// ── Forward direction ──

impl<I> ForwardIter for IndexEntryDecodeIter<I>
where
    I: ForwardIter,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.input.seek_to_first()?;
        self.refresh()
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        self.input.seek(target)?;
        self.refresh()
    }

    fn next(&mut self) -> StorageResult<()> {
        self.input.next()?;
        self.refresh()
    }
}

// ── Reverse direction ──

impl<I> ReverseIter for IndexEntryDecodeIter<I>
where
    I: ReverseIter,
    for<'a> I::Value<'a>: AsArray<'a>,
{
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.input.seek_to_first()?;
        self.refresh()
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        self.input.seek(target)?;
        self.refresh()
    }

    fn next(&mut self) -> StorageResult<()> {
        self.input.next()?;
        self.refresh()
    }
}

pub struct IndexEntryValue<'a> {
    buf: &'a [u8],
}

impl<'a> AsArray<'a> for IndexEntryValue<'a> {
    fn as_array(&self) -> &'a [u8] {
        self.buf
    }
}

impl From<IndexEntryValue<'_>> for Position {
    fn from(value: IndexEntryValue<'_>) -> Self {
        assert_eq!(value.buf.len(), POSITION_LEN);
        Self {
            offset: u64::from_be_bytes(value.buf.try_into().unwrap()),
        }
    }
}

struct IndexEntryHeader {
    _format_version: u8,
    payload_start: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        block::Position,
        iterators::{
            block_iter::RawEntry,
            storage_iter::{ForwardIter, IterBase, IterRead, ReverseIter},
        },
    };

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

    impl IterBase for VecEntryIter {
        type Key<'a> = &'a [u8];
        type Value<'a> = RawEntry<'a>;
    }

    impl IterRead for VecEntryIter {
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

    impl ForwardIter for VecEntryIter {
        fn seek_to_first(&mut self) -> crate::errors::StorageResult<()> {
            self.pos = if self.entries.is_empty() {
                usize::MAX
            } else {
                0
            };
            Ok(())
        }

        fn seek<'a>(&mut self, target: &Self::Key<'a>) -> crate::errors::StorageResult<()> {
            self.pos = self.entries.partition_point(|(key, _)| key < target);
            if self.pos >= self.entries.len() {
                self.pos = usize::MAX;
            }
            Ok(())
        }

        fn next(&mut self) -> crate::errors::StorageResult<()> {
            if self.valid() {
                self.pos += 1;
                if self.pos >= self.entries.len() {
                    self.pos = usize::MAX;
                }
            }
            Ok(())
        }
    }

    impl ReverseIter for VecEntryIter {
        fn seek_to_first(&mut self) -> crate::errors::StorageResult<()> {
            self.pos = if self.entries.is_empty() {
                usize::MAX
            } else {
                self.entries.len() - 1
            };
            Ok(())
        }

        fn seek<'a>(&mut self, target: &Self::Key<'a>) -> crate::errors::StorageResult<()> {
            let upper = self.entries.partition_point(|(key, _)| key <= target);
            self.pos = if upper == 0 { usize::MAX } else { upper - 1 };
            Ok(())
        }

        fn next(&mut self) -> crate::errors::StorageResult<()> {
            if self.pos == 0 {
                self.pos = usize::MAX;
            } else if self.pos < self.entries.len() {
                self.pos -= 1;
            }
            Ok(())
        }
    }

    fn index_entry(offset: u64) -> Vec<u8> {
        let mut entry = vec![1u8];
        entry.extend_from_slice(&offset.to_be_bytes());
        entry
    }

    #[test]
    fn seek_to_first_decodes_index_payload() {
        let first = index_entry(0xAB);
        let input = VecEntryIter::new(vec![(b"k1", Box::leak(first.into_boxed_slice()))]);
        let mut iter = IndexEntryDecodeIter::new(input);

        ForwardIter::seek_to_first(&mut iter).unwrap();

        assert!(iter.valid());
        assert_eq!(iter.key().unwrap(), b"k1" as &[u8]);
        assert_eq!(Position::from(iter.value().unwrap()).offset, 0xAB);
    }

    #[test]
    fn seek_decodes_target_entry_payload() {
        let first = index_entry(0x10);
        let second = index_entry(0x20);
        let input = VecEntryIter::new(vec![
            (b"k1", Box::leak(first.into_boxed_slice())),
            (b"k2", Box::leak(second.into_boxed_slice())),
        ]);
        let mut iter = IndexEntryDecodeIter::new(input);

        ForwardIter::seek(&mut iter, &&b"k2"[..]).unwrap();

        assert!(iter.valid());
        assert_eq!(Position::from(iter.value().unwrap()).offset, 0x20);
    }

    #[test]
    fn unknown_version_returns_error_when_positioning() {
        let input = VecEntryIter::new(vec![(b"k1", b"\x02abcdefgh")]);
        let mut iter = IndexEntryDecodeIter::new(input);

        assert!(ForwardIter::seek_to_first(&mut iter).is_err());
    }

    #[test]
    fn short_payload_returns_error_when_positioning() {
        let input = VecEntryIter::new(vec![(b"k1", b"\x01abc")]);
        let mut iter = IndexEntryDecodeIter::new(input);

        assert!(ForwardIter::seek_to_first(&mut iter).is_err());
    }
}
