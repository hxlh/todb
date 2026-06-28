//! Phase 2: the flush write path seals a `.idx` SST + `.meta` per segment.
//! Append via `Wal`, sync, close (triggers finalize), then read the sealed
//! `seg_00000.idx` back through `WalIndexReader`/`SstIter` and cross-check every
//! `(lsn, offset, len)` against the on-disk `.log` frames; assert `.meta` header.

use std::os::unix::io::AsRawFd;
use std::sync::Arc;

use crate::builder::{SstFooter, SstOption};
use crate::iterators::{
    block_iter::NormalBlockIter,
    data_entry_decode_iter::DataEntryDecodeIter,
    sst_iter::SstIter,
    storage_iter::{AsArray, ForwardIter, IterRead},
};
use crate::wal::{
    DiskManager, DecodedFrame, PinGuard, Segment, Wal, WalConfig, WalIndexReader, decode_offset_len,
    key_to_lsn,
};

fn cfg(segment_size: usize, buffer_size: usize) -> WalConfig {
    WalConfig {
        segment_size,
        buffer_size,
        block_size: 4096,
        buffer_count: 2,
        read_cache_blocks: 8,
        o_direct: false, // tmpfs / CI compatible
    }
}

/// Decode the `SstFooter` from a `.idx` SST sealed by `ODirectBlockWriter`. The
/// footer block is laid out `[body][padding][trailer]` (O_DIRECT requires a
/// block-aligned write): the `body_len` trailer is the file's last 4 bytes, and
/// the body sits at the start of the final block. Reconstruct `body ++ trailer`
/// and decode.
fn read_footer(buf: &[u8], block_size: usize) -> SstFooter {
    let len = buf.len();
    let body_len = u32::from_be_bytes(buf[len - 4..len].try_into().unwrap()) as usize;
    let body_start = len - block_size;
    let mut full = buf[body_start..body_start + body_len].to_vec();
    full.extend_from_slice(&buf[len - 4..len]);
    SstFooter::decode(&full).unwrap()
}

/// Scan an index SST (served zero-copy through a fresh CLOCK cache) and collect
/// `(lsn, offset, len)` in ascending LSN order. A fresh `DiskManager` per call
/// avoids cross-segment cache-key collisions (each segment's index needs a unique
/// `file_id` namespace — production wires this in Phase 3).
fn scan_idx(block_size: usize, idx_bytes: &[u8], fd: std::os::unix::io::RawFd) -> Vec<(u64, u64, u32)> {
    let dm = Arc::new(DiskManager::new(block_size, 8).unwrap());
    let footer = read_footer(idx_bytes, block_size);
    let option = SstOption::default().block_size(block_size);
    let reader = Arc::new(WalIndexReader::new(dm, 0, fd));
    let mut iter = SstIter::<_, NormalBlockIter<PinGuard>, DataEntryDecodeIter<NormalBlockIter<PinGuard>>>::new(
        reader, footer, option,
    )
    .unwrap();
    ForwardIter::seek_to_first(&mut iter).unwrap();
    let mut out = Vec::new();
    while iter.valid() {
        let k = iter.key().unwrap();
        let v = iter.value().unwrap();
        let lsn = key_to_lsn(k.as_array());
        let (off, len) = decode_offset_len(v.as_array());
        out.push((lsn, off, len));
        ForwardIter::next(&mut iter).unwrap();
    }
    out
}

#[test]
fn flush_seals_idx_sst_and_meta_single_segment() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let block_size = 4096;
    let wal = Wal::open(&path, cfg(1 << 16, 4096)).unwrap();
    let n = 50u64;
    let payloads: Vec<Vec<u8>> = (0..n).map(|i| format!("payload-{i:04}").into_bytes()).collect();
    for p in &payloads {
        wal.append(p).unwrap();
    }
    wal.sync().unwrap();
    wal.close().unwrap();

    // `.meta` header: entry_count == n, lsn range covers all appended records.
    let seg = Segment::create(&path, 0, 1 << 16, block_size, false).unwrap();
    let header = seg.read_meta_header().unwrap();
    assert_eq!(header.seg_id, 0);
    assert_eq!(header.min_live_lsn, 0);
    assert_eq!(header.max_live_lsn, n - 1);
    assert_eq!(header.entry_count, n as u32);
    drop(seg);
    // `.meta` is exactly two block_size copies (8192 bytes).
    assert_eq!(
        std::fs::metadata(path.join("seg_00000.meta")).unwrap().len(),
        2 * block_size as u64
    );

    // `.idx` is a valid SST readable through WalIndexReader, yielding n ascending entries.
    let idx_bytes = std::fs::read(path.join("seg_00000.idx")).unwrap();
    let idx_file = std::fs::File::open(path.join("seg_00000.idx")).unwrap();
    let got = scan_idx(block_size, &idx_bytes, idx_file.as_raw_fd());
    assert_eq!(got.len(), n as usize, "SST must contain every appended record");
    for (i, (lsn, _, _)) in got.iter().enumerate() {
        assert_eq!(*lsn, i as u64, "lsns must be dense and ascending");
    }

    // Cross-check each (offset, len) against the `.log` frame: the index must point
    // at the real frame for each lsn, with the right payload.
    let log = std::fs::read(path.join("seg_00000.log")).unwrap();
    for (lsn, off, len) in &got {
        let off = *off as usize;
        let len = *len as usize;
        let decoded = DecodedFrame::decode_at(&log, off).unwrap().unwrap();
        assert_eq!(decoded.lsn.get(), *lsn, "frame at index offset must match lsn");
        assert_eq!(decoded.total_len, len, "index len must equal frame total_len");
        assert_eq!(
            &log[off + crate::wal::HEADER_LEN..off + len],
            payloads[*lsn as usize].as_slice(),
            "frame payload must match what was appended",
        );
    }
}

#[test]
fn flush_seals_one_idx_per_segment_on_rollover() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let block_size = 4096;
    // segment 8192, buffer 4096 → rollover once accumulated flushes exceed one segment.
    let wal = Wal::open(&path, cfg(8192, 4096)).unwrap();
    // ~26 B/frame × 1000 frames ≈ 26 KiB → spans multiple segments.
    for i in 0..1000u64 {
        wal.append(format!("p{i:06}").as_bytes()).unwrap();
    }
    wal.sync().unwrap();
    wal.close().unwrap();

    // Each sealed segment has the triple seg_NNNNN.{meta,idx,log}; at least 2 segments.
    assert!(path.join("seg_00000.idx").exists());
    assert!(path.join("seg_00000.meta").exists());
    assert!(path.join("seg_00001.log").exists(), "must roll over to a second segment");
    assert!(path.join("seg_00001.idx").exists());
    assert!(path.join("seg_00001.meta").exists());

    // Every segment's `.idx` SST is readable; together they cover all 1000 lsns.
    let mut all_lsns: Vec<u64> = Vec::new();
    let mut seg_id = 0u32;
    while path.join(format!("seg_{seg_id:05}.log")).exists() {
        let p = path.join(format!("seg_{seg_id:05}.idx"));
        let idx_bytes = std::fs::read(&p).unwrap();
        let idx_file = std::fs::File::open(&p).unwrap();
        for (lsn, _, _) in scan_idx(block_size, &idx_bytes, idx_file.as_raw_fd()) {
            all_lsns.push(lsn);
        }
        seg_id += 1;
    }
    assert!(seg_id >= 2, "expected rollover to produce >= 2 segments, got {seg_id}");
    all_lsns.sort_unstable();
    all_lsns.dedup();
    assert_eq!(all_lsns.len(), 1000, "all 1000 appended lsns must be indexed");
    assert_eq!(*all_lsns.last().unwrap(), 999);
}
