//! Phase 4b-1 read primitives: `DiskManager` CLOCK buffer pool + `Segment::read_idx_header`.
//! Produces `.idx` via the Phase 4a write path (append/sync/close), then reads it back.

use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use wal_demo::wal::{DiskManager, IdxEntry, Segment, Wal, WalConfig, WalError};

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

#[test]
fn read_idx_header_after_wal_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let wal = Wal::open(&path, cfg(1 << 16, 4096)).unwrap();
    for i in 0..10u64 {
        wal.append(format!("r{i}").as_bytes()).unwrap();
    }
    wal.sync().unwrap();
    wal.close().unwrap();

    // Reopen wal-0 as a Segment (create is idempotent on an existing file: open does
    // not truncate, fallocate on already-allocated space is a no-op).
    let seg = Segment::create(&path, 0, 1 << 16, 4096, false).unwrap();
    let header = seg.read_idx_header().unwrap();
    assert_eq!(header.seg_id, 0);
    assert_eq!(header.min_live_lsn, 0);
    assert!(
        header.entry_count >= 10,
        "entry_count {} should cover 10 appended records",
        header.entry_count
    );
}

#[test]
fn disk_manager_reads_idx_entries_and_caches() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let wal = Wal::open(&path, cfg(1 << 16, 4096)).unwrap();
    for i in 0..10u64 {
        wal.append(format!("payload-{i}").as_bytes()).unwrap();
    }
    wal.sync().unwrap();
    wal.close().unwrap();

    let seg = Segment::create(&path, 0, 1 << 16, 4096, false).unwrap();
    let dm = DiskManager::new(4096, 8).unwrap();

    // block 2 = first entry block (blocks 0/1 are the two header copies)
    let first = dm.read_block(0, seg.idx_fd(), 2).unwrap();
    let cached = dm.read_block(0, seg.idx_fd(), 2).unwrap();
    assert_eq!(
        first.as_ptr(),
        cached.as_ptr(),
        "second read of the same block must hit the cached frame"
    );

    // first 10 entries are lsn 0..9 (flush writes entries in lsn order)
    for i in 0..10usize {
        let off = i * IdxEntry::SERIALIZED_LEN;
        let e = IdxEntry::deserialize(&first[off..]).unwrap();
        assert_eq!(e.lsn, i as u64, "entry {i} lsn");
        assert!(e.total_len > 0, "entry {i} total_len must be non-zero");
    }
    // slot 10 is zero-padding (total_len == 0 sentinel)
    let pad = IdxEntry::deserialize(&first[10 * IdxEntry::SERIALIZED_LEN..]).unwrap();
    assert_eq!(pad.total_len, 0, "padding slot must have total_len == 0");
}

#[test]
fn read_block_clock_evicts_unpinned_frame() {
    let dir = tempfile::tempdir().unwrap();
    // .log is preallocated to 1<<16 (16 blocks); blocks 0..=2 are readable zeros.
    let seg = Segment::create(dir.path(), 0, 1 << 16, 4096, false).unwrap();
    let dm = DiskManager::new(4096, 2).unwrap(); // 2 frames

    // Load blocks 0 and 1 (guards drop immediately → both unpinned, ref_bit set).
    let p0 = dm.read_block(0, seg.log_fd(), 0).unwrap().as_ptr();
    dm.read_block(0, seg.log_fd(), 1).unwrap();
    // Load block 2 → CLOCK first clears both ref_bits, then evicts block 0's frame.
    dm.read_block(0, seg.log_fd(), 2).unwrap();
    // Block 0 was evicted → re-reading reloads it into the other frame.
    let p0_again = dm.read_block(0, seg.log_fd(), 0).unwrap().as_ptr();
    assert_ne!(
        p0, p0_again,
        "block 0 should have been CLOCK-evicted and reloaded into a different frame"
    );
}

#[test]
fn read_block_blocks_when_all_pinned_then_wakes() {
    let dir = tempfile::tempdir().unwrap();
    let seg = Arc::new(Segment::create(dir.path(), 0, 1 << 16, 4096, false).unwrap());
    let dm = Arc::new(DiskManager::new(4096, 1).unwrap()); // 1 frame
    let dm2 = dm.clone();
    let fd = seg.log_fd();

    let g = dm.read_block(0, fd, 0).unwrap(); // pin the only frame
    let handle = thread::spawn(move || {
        // capacity 1 + the only frame pinned → blocks until `g` drops.
        dm2.read_block(0, fd, 1).unwrap();
    });
    thread::sleep(Duration::from_millis(50));
    assert!(
        !handle.is_finished(),
        "reader must block while every frame is pinned"
    );
    drop(g); // unpin → notify the blocked reader
    handle
        .join()
        .expect("blocked reader completes after its pin is released");
}

#[test]
fn read_block_pread_error_unpins_no_leak() {
    let dir = tempfile::tempdir().unwrap();
    let seg = Segment::create(dir.path(), 0, 1 << 16, 4096, false).unwrap();
    let dm = DiskManager::new(4096, 1).unwrap(); // 1 frame

    // A bad fd makes pread fail; the loader must release its pin (B2), not leak it.
    let bad_fd: RawFd = -1;
    let err = dm.read_block(0, bad_fd, 0).err().unwrap();
    assert!(matches!(err, WalError::Io(_)), "bad fd → WalError::Io");

    // Pool not leaked: a valid read on the single-frame pool must still succeed.
    let _g = dm.read_block(0, seg.log_fd(), 0).unwrap();
}
