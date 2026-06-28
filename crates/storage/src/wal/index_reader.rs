//! [`BlockReader`] adapter over the WAL [`DiskManager`], for reading an index
//! SST's blocks zero-copy through the CLOCK cache.
//!
//! `DiskManager::read_block(file_id, fd, block_idx)` returns a `'static`
//! [`PinGuard`] (it holds an `Arc` to the pool, not a borrow of `&self`). This
//! adapter exposes that through the storage-level [`BlockReader`] trait, mapping
//! a byte-offset [`Position`] to a block index. The guard is consumed directly
//! by `NormalBlockIter<PinGuard>` — no materialization, no copy.
//!
//! The reader **borrows** its `fd` (a segment's O_DIRECT `idx_fd`) — it does not
//! own or close it. Footer decode is a separate one-shot buffered read at open
//! time (see `SegmentIndex`); only full `block_size` blocks flow through here,
//! which is what O_DIRECT requires.

use std::os::unix::io::RawFd;
use std::sync::Arc;

use crate::block::{BlockReader, Position};
use crate::errors::StorageResult;
use crate::wal::{DiskManager, PinGuard};

/// `BlockReader` over a WAL `DiskManager` cache. One per index SST file.
pub struct WalIndexReader {
    dm: Arc<DiskManager>,
    file_id: u32,
    fd: RawFd,
}

impl WalIndexReader {
    /// `file_id` is the cache-key namespace for this SST (unique within the
    /// index's `DiskManager` instance). `fd` is borrowed — caller owns its
    /// lifetime (typically a `Segment`'s `idx_fd`).
    pub fn new(dm: Arc<DiskManager>, file_id: u32, fd: RawFd) -> Self {
        Self { dm, file_id, fd }
    }

    pub fn block_size(&self) -> usize {
        self.dm.block_size()
    }
}

impl BlockReader for WalIndexReader {
    type Guard<'a> = PinGuard;

    fn read_block(&self, position: &Position) -> StorageResult<PinGuard> {
        let block_size = self.dm.block_size() as u64;
        // Position offsets produced by SstBuilder are always block-aligned
        // multiples (data/index blocks are exactly block_size), so this is an
        // exact division.
        let block_idx = position.offset / block_size;
        debug_assert_eq!(
            position.offset % block_size,
            0,
            "index SST block position must be block-aligned"
        );
        Ok(self.dm.read_block(self.file_id, self.fd, block_idx)?)
    }

    fn block_size(&self) -> usize {
        self.dm.block_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::{SstBuilder, SstOption};
    use crate::wal::{SegmentIndexBlockWriter, ODirectSstWriter};
    use crate::iterators::{
        block_iter::NormalBlockIter,
        data_entry_decode_iter::DataEntryDecodeIter,
        sst_iter::SstIter,
        storage_iter::{AsArray, ForwardIter, IterRead},
    };
    use crate::wal::segment::pwrite_all;
    use bytes::Bytes;
    use std::os::unix::io::AsRawFd;
    use tempfile::tempdir;

    fn lsn_key(lsn: u64) -> Bytes {
        Bytes::copy_from_slice(&lsn.to_be_bytes())
    }
    fn encode_offset_len(offset: u64, len: u32) -> Bytes {
        let mut b = Vec::with_capacity(12);
        b.extend_from_slice(&offset.to_le_bytes());
        b.extend_from_slice(&len.to_le_bytes());
        Bytes::from(b)
    }
    fn decode_offset_len(b: &[u8]) -> (u64, u32) {
        (
            u64::from_le_bytes(b[0..8].try_into().unwrap()),
            u32::from_le_bytes(b[8..12].try_into().unwrap()),
        )
    }

    // Read one block through WalIndexReader + DiskManager; verify bytes and that
    // a second read of the same block is a cache hit (same frame pointer).
    #[test]
    fn read_block_returns_cached_pin_guard() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.idx");
        let block_size = 4096usize;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let mut buf = vec![0u8; block_size];
        buf[..5].copy_from_slice(b"hello");
        pwrite_all(file.as_raw_fd(), &buf, 0).unwrap();

        let dm = Arc::new(DiskManager::new(block_size, 4).unwrap());
        let reader = WalIndexReader::new(Arc::clone(&dm), /*file_id=*/ 7, file.as_raw_fd());

        let g1 = reader.read_block(&Position { offset: 0 }).unwrap();
        assert_eq!(&g1[..5], b"hello");
        let g2 = reader.read_block(&Position { offset: 0 }).unwrap();
        assert_eq!(g1.as_ptr(), g2.as_ptr(), "second read must be a cache hit");
        drop(file);
    }

    // Full SST round-trip through the O_DIRECT cache: build an index SST
    // (LSN→(offset,len)) buffered, then read it back zero-copy via
    // SstIter<WalIndexReader, NormalBlockIter<PinGuard>, …>. Proves the whole
    // index read pipeline composes with the generic, PinGuard-backed iterator.
    #[test]
    fn sst_roundtrip_through_cache() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("seg_0.idx");
        let block_size = 64usize; // small blocks → multi-block SST, multi-level index

        // Build side: ODirectSstWriter over ODirectBlockWriter (o_direct=false so
        // the test runs on tmpfs); the SstWriter pads the footer for O_DIRECT.
        let option = SstOption::default().block_size(block_size);
        let writer = SegmentIndexBlockWriter::create(&path, block_size, false).unwrap();
        let mut builder = SstBuilder::new(ODirectSstWriter::new(writer, &option), option.clone());
        let n = 40u64;
        for i in 0..n {
            builder
                .add(lsn_key(i), encode_offset_len(i * 100, 40))
                .unwrap();
        }
        let (footer, sst_writer) = builder.finish().unwrap();
        drop(sst_writer); // close the write fd

        // Read side: re-open buffered (test runs on tmpfs; production O_DIRECT),
        // serve blocks through the WAL DiskManager cache.
        let file = std::fs::File::open(&path).unwrap();
        let dm = Arc::new(DiskManager::new(block_size, 8).unwrap());
        let reader = Arc::new(WalIndexReader::new(Arc::clone(&dm), 0, file.as_raw_fd()));
        let mut iter = SstIter::<_, NormalBlockIter<PinGuard>, DataEntryDecodeIter<NormalBlockIter<PinGuard>>>::new(
            reader, footer, option,
        )
        .unwrap();

        ForwardIter::seek_to_first(&mut iter).unwrap();
        let mut got = Vec::new();
        while iter.valid() {
            let k = iter.key().unwrap();
            let v = iter.value().unwrap();
            let lsn = u64::from_be_bytes(k.as_array()[..8].try_into().unwrap());
            let (off, len) = decode_offset_len(v.as_array());
            got.push((lsn, off, len));
            ForwardIter::next(&mut iter).unwrap();
        }

        assert_eq!(got.len(), n as usize);
        for (i, (lsn, off, len)) in got.iter().enumerate() {
            assert_eq!(*lsn, i as u64);
            assert_eq!(*off, (i as u64) * 100);
            assert_eq!(*len, 40);
        }
        drop(file);
    }

    // LSN key encoding sanity (big-endian ⇒ lexicographic = numeric order).
    #[test]
    fn lsn_key_ordering() {
        fn k(lsn: u64) -> Bytes {
            Bytes::copy_from_slice(&lsn.to_be_bytes())
        }
        assert!(k(1) < k(2));
        assert!(k(255) < k(256));
        assert!(k(u64::MAX - 1) < k(u64::MAX));
    }
}
