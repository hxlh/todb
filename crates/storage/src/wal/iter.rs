//! WAL scan iterator: merges sealed segments' index SSTs + active segment's memtable,
//! then reads frames from `.log` on demand via DiskManager cache.

use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;

use crate::builder::SstOption;
use crate::errors::StorageResult;
use crate::iterators::block_iter::NormalBlockIter;
use crate::iterators::concat_iter::ConcatIter;
use crate::iterators::data_entry_decode_iter::{DataEntryDecodeIter, EntryValue};
use crate::iterators::sst_iter::SstIter;
use crate::iterators::storage_iter::{AsArray, ForwardIter, IterBase, IterRead};
use crate::iterators::two_merge_iter::TwoMergeIter;
use crate::memtable::{Entry, OwnedMemTableIter};
use crate::row_key::BinaryKey;
use crate::wal::{
    DecodedFrame, DiskManager, HEADER_LEN, Lsn, PinGuard, Record, Segment, WalError, WalIndexReader,
    lsn_to_key,
};

/// Index entry: points to a frame in a segment's `.log`.
#[derive(Clone)]
pub struct IndexEntry {
    pub segment: Arc<Segment>,
    pub offset: u64,
    pub len: u32,
}

/// Wrapper that maps an index iterator's output to (BinaryKey, IndexEntry).
/// Holds segment Arc for IndexEntry construction.
pub struct SegmentIndexIter<I> {
    inner: I,
    segment: Arc<Segment>,
}

impl<I> SegmentIndexIter<I> {
    pub fn new(inner: I, segment: Arc<Segment>) -> Self {
        Self { inner, segment }
    }
}

impl SegmentIndexIter<SstIterInner> {
    /// Create an SST index iterator from a sealed segment.
    /// Reads the .idx footer via DiskManager and constructs the SstIter.
    pub fn from_sealed_segment(
        segment: Arc<Segment>,
        dm: Arc<DiskManager>,
    ) -> Result<Self, WalError> {
        // Get file size via fstat
        let fd = segment.meta_fd();
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::fstat(fd, &mut stat) };
        if ret != 0 {
            return Err(WalError::Io(std::io::Error::last_os_error()));
        }
        let file_size = stat.st_size as u64;

        if file_size < 4 {
            return Err(WalError::Io(std::io::Error::other("idx file too small")));
        }

        // Read body_len trailer (last 4 bytes)
        let trailer = dm.raw_read(fd, file_size - 4, 4)?;
        let body_len = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]) as u64;
        let footer_total = body_len + 4;

        if file_size < footer_total {
            return Err(WalError::Io(std::io::Error::other("idx file too small for footer")));
        }

        // Read full footer
        let footer_buf = dm.raw_read(fd, file_size - footer_total, footer_total as usize)?;
        let footer = crate::builder::SstFooter::decode(&footer_buf)
            .map_err(|e| WalError::Io(std::io::Error::other(format!("decode footer: {e}"))))?;

        // Create WalIndexReader + SstIter
        let block_size = dm.block_size();
        let reader = Arc::new(WalIndexReader::new(dm, segment.seg_id(), segment.meta_fd()));
        let option = SstOption::default().block_size(block_size);
        let sst_iter: SstIterInner = SstIter::new(reader, footer, option)
            .map_err(|e| WalError::Io(std::io::Error::other(format!("create sst iter: {e}"))))?;

        Ok(Self {
            inner: sst_iter,
            segment,
        })
    }
}

impl<I: IterBase> IterBase for SegmentIndexIter<I> {
    type Key<'a> = BinaryKey<'a>;
    type Value<'a> = IndexEntry;
}

// Memtable iter specialization
impl IterRead for SegmentIndexIter<OwnedMemTableIter<Bytes, Bytes>> {
    fn valid(&self) -> bool {
        self.inner.valid()
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        self.inner.key().map(|k| BinaryKey::from(k.as_ref()))
    }

    fn value(&self) -> Option<IndexEntry> {
        let entry = self.inner.value()?;
        let bytes = match entry {
            Entry::Put(b) => b.as_ref(),
            Entry::Delete => return None,
        };
        let (offset, len) = crate::wal::decode_offset_len(bytes);
        Some(IndexEntry {
            segment: Arc::clone(&self.segment),
            offset,
            len,
        })
    }
}

impl ForwardIter for SegmentIndexIter<OwnedMemTableIter<Bytes, Bytes>> {
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner.seek_to_first()
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        let bytes_key = Bytes::copy_from_slice(target.as_bytes());
        self.inner.seek(&&bytes_key)
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner.next()
    }
}

// SST iter specialization
type SstIterInner = SstIter<WalIndexReader, NormalBlockIter<PinGuard>, DataEntryDecodeIter<NormalBlockIter<PinGuard>>>;

impl IterRead for SegmentIndexIter<SstIterInner> {
    fn valid(&self) -> bool {
        self.inner.valid()
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        self.inner.key().map(|k| BinaryKey::from(k.as_array()))
    }

    fn value(&self) -> Option<IndexEntry> {
        let val = self.inner.value()?;
        let bytes = match val {
            EntryValue::Put(b) => b,
            EntryValue::Delete => return None,
        };
        let (offset, len) = crate::wal::decode_offset_len(bytes);
        Some(IndexEntry {
            segment: Arc::clone(&self.segment),
            offset,
            len,
        })
    }
}

impl ForwardIter for SegmentIndexIter<SstIterInner> {
    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.inner.seek_to_first()
    }

    fn seek(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        let binary_key = BinaryKey::from(target.as_bytes());
        self.inner.seek(&binary_key)
    }

    fn next(&mut self) -> StorageResult<()> {
        self.inner.next()
    }
}

/// Concrete SST index iterator type
type SstIndexIter = SegmentIndexIter<SstIterInner>;

/// Concrete memtable index iterator type
type MemTableIndexIter = SegmentIndexIter<OwnedMemTableIter<Bytes, Bytes>>;

/// WAL scan iterator: merges sealed SST indices + active memtable index via TwoMergeIter.
pub struct WalIter {
    /// Merged index iterator
    index_iter: TwoMergeIter<ConcatIter<SstIndexIter>, MemTableIndexIter>,
    dm: Arc<DiskManager>,
    buffer: Vec<u8>,
}

impl WalIter {
    pub fn new(
        sealed: Vec<Arc<Segment>>,
        active_seg: Arc<Segment>,
        active_mem: Arc<crate::memtable::MemTable<Bytes, Bytes>>,
        dm: Arc<DiskManager>,
        _range: (Bound<Lsn>, Bound<Lsn>),
    ) -> Result<Self, WalError> {
        // Build sealed segment SST iterators
        let mut sealed_iters = Vec::new();
        for seg in sealed {
            let iter = SegmentIndexIter::from_sealed_segment(seg, dm.clone())?;
            sealed_iters.push(iter);
        }

        let sealed_concat = ConcatIter::new(sealed_iters);

        // Build active segment memtable iterator
        let active_iter = SegmentIndexIter::new(active_mem.iter(), active_seg);

        // Merge sealed + active
        let index_iter = TwoMergeIter::new(sealed_concat, active_iter)
            .map_err(|e| WalError::Io(std::io::Error::other(format!("merge iter: {e}"))))?;

        Ok(Self {
            index_iter,
            dm,
            buffer: Vec::new(),
        })
    }

    pub fn valid(&self) -> bool {
        self.index_iter.valid()
    }

    pub fn key(&self) -> Option<Lsn> {
        let k = self.index_iter.key()?;
        Some(Lsn(crate::wal::key_to_lsn(k.as_ref())))
    }

    pub fn next(&mut self) -> Result<(), WalError> {
        self.index_iter
            .next()
            .map_err(|e| WalError::Io(std::io::Error::other(format!("iter next: {e}"))))
    }

    pub fn seek_to_first(&mut self) -> Result<(), WalError> {
        self.index_iter
            .seek_to_first()
            .map_err(|e| WalError::Io(std::io::Error::other(format!("seek_to_first: {e}"))))
    }

    pub fn seek(&mut self, lsn: Lsn) -> Result<(), WalError> {
        let key = lsn_to_key(lsn.0);
        let binary_key = BinaryKey::from(key.as_ref());
        self.index_iter
            .seek(&binary_key)
            .map_err(|e| WalError::Io(std::io::Error::other(format!("seek: {e}"))))
    }

    /// Get current record (lsn + payload).
    pub fn value(&mut self) -> Result<&[u8], WalError> {
        if !self.valid() {
            return Err(WalError::Io(std::io::Error::other("iterator not valid")));
        }
        let decoded = self.load_current_frame()?;

        // Extract payload from buffer (skip HEADER_LEN bytes)
        let payload_len = decoded.total_len - HEADER_LEN;
        let payload = &self.buffer[HEADER_LEN..HEADER_LEN + payload_len];

        Ok(payload)
    }

    /// Load the frame pointed to by the current index entry.
    fn load_current_frame(&mut self) -> Result<DecodedFrame, WalError> {
        let entry = self
            .index_iter
            .value()
            .ok_or_else(|| WalError::Io(std::io::Error::other("index iterator not valid")))?;

        let block_size = self.dm.block_size() as u64;
        let start_block = entry.offset / block_size;
        let end_block = (entry.offset + entry.len as u64 - 1) / block_size;

        // Read all blocks covering this frame
        let mut guards = Vec::new();
        for block_idx in start_block..=end_block {
            guards.push(self.dm.read_block(
                entry.segment.seg_id(),
                entry.segment.log_fd(),
                block_idx,
            )?);
        }

        // Assemble frame bytes into buffer
        self.buffer.clear();
        self.buffer.reserve(entry.len as usize);

        let block_offset = (entry.offset % block_size) as usize;
        let mut remaining = entry.len as usize;
        let mut guard_idx = 0;

        // First block: may start at block_offset and may not fill the block
        let first_block_available = block_size as usize - block_offset;
        let first_copy_len = first_block_available.min(remaining);
        self.buffer.extend_from_slice(&guards[guard_idx][block_offset..block_offset + first_copy_len]);
        remaining -= first_copy_len;
        guard_idx += 1;

        // Subsequent blocks: start at offset 0, copy up to remaining bytes
        while remaining > 0 && guard_idx < guards.len() {
            let copy_len = remaining.min(block_size as usize);
            self.buffer.extend_from_slice(&guards[guard_idx][..copy_len]);
            remaining -= copy_len;
            guard_idx += 1;
        }

        // Decode frame at offset 0 in buffer
        DecodedFrame::decode_at(&self.buffer, 0)?
            .ok_or_else(|| WalError::Io(std::io::Error::other("incomplete frame")))
    }
}
