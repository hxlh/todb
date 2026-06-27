//! Phase 4a integration tests: append monotonicity, sync durability, segment rollover.
//! Verify target: `cargo test --test wal_write_path`.

use wal_demo::Lsn;
use wal_demo::wal::{DecodedFrame, Wal, WalConfig};

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
fn append_returns_monotonic_lsn() {
    let dir = tempfile::tempdir().unwrap();
    let wal = Wal::open(dir.path(), cfg(1 << 16, 4096)).unwrap();
    for i in 0..100u64 {
        let lsn = wal.append(format!("payload-{i}").as_bytes()).unwrap();
        assert_eq!(lsn.get(), i, "lsn must be dense and monotonic from 0");
    }
    wal.sync().unwrap();
    wal.close().unwrap();
}

#[test]
fn sync_persists_frames_to_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let wal = Wal::open(&path, cfg(1 << 16, 4096)).unwrap();
    for i in 0..50u64 {
        wal.append(format!("rec-{i}").as_bytes()).unwrap();
    }
    let durable = wal.sync().unwrap();
    wal.close().unwrap();

    // Read wal-0.log and decode frames sequentially until the zero-padded tail.
    let log = std::fs::read(path.join("wal-0.log")).unwrap();
    let mut offset = 0usize;
    let mut lsns = Vec::new();
    while offset + 16 <= log.len() {
        match DecodedFrame::decode_at(&log, offset) {
            Ok(Some(f)) => {
                lsns.push(f.lsn.get());
                offset += f.total_len;
            }
            _ => break, // zero-padded tail (crc mismatch) or torn header → valid end
        }
    }
    assert_eq!(lsns.len(), 50, "all 50 synced frames must be on disk");
    assert_eq!(*lsns.last().unwrap(), durable.get());
    // lsn set must be exactly 0..50 (order may differ across threads, but single-threaded ⇒ sorted)
    let mut sorted = lsns.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..50).collect::<Vec<_>>());
}

#[test]
fn rollover_creates_new_segment() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    // segment 8192, buffer 4096 → rollover once accumulated flushes exceed one segment.
    let wal = Wal::open(&path, cfg(8192, 4096)).unwrap();
    // ~23 B/frame × 1000 frames ≈ 23 KiB → spans multiple segments.
    for i in 0..1000u64 {
        wal.append(format!("p{i:06}").as_bytes()).unwrap();
    }
    wal.sync().unwrap();
    wal.close().unwrap();

    assert!(path.join("wal-0.log").exists());
    assert!(path.join("wal-0.idx").exists());
    assert!(
        path.join("wal-1.log").exists(),
        "segment rollover must create wal-1.log"
    );

    // wal-0.idx header (copy A) must be readable and self-consistent.
    let idx = std::fs::read(path.join("wal-0.idx")).unwrap();
    assert!(idx.len() >= 4096, "idx must hold at least the header block");
    let header_copy_b = &idx[4096..4096 + 36];
    let header_copy_a = &idx[..36];
    assert_eq!(
        header_copy_a, header_copy_b,
        "header copies A/B must match post-finalize"
    );
}

#[test]
fn append_after_close_returns_closed() {
    let dir = tempfile::tempdir().unwrap();
    let wal = Wal::open(dir.path(), cfg(1 << 16, 4096)).unwrap();
    wal.append(b"x").unwrap();
    wal.close().unwrap();
    let err = wal.append(b"y").unwrap_err();
    assert!(
        matches!(err, wal_demo::WalError::Closed),
        "append after close → Closed"
    );
    let _ = Lsn(0); // keep the Lsn import meaningful for downstream phases
}
