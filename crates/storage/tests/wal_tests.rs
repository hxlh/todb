use bytes::Bytes;
use storage::wal::file_wal_store::FileWalStore;
use storage::wal::{WalPayload, WalStore};
use storage::write_batch::WriteBatch;

fn put(k: &[u8], v: &[u8]) -> WriteBatch {
    let mut b = WriteBatch::new();
    b.put(Bytes::copy_from_slice(k), Bytes::copy_from_slice(v));
    b
}

#[test]
fn file_wal_append_sync_recover_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let wal = FileWalStore::open(tmp.path().join("rg1"), 1, 1024); // buf 1KiB
    wal.append(10, &put(b"k1", b"v1")).unwrap();
    wal.append(20, &put(b"k2", b"v2")).unwrap();
    wal.sync().unwrap();

    let mut iter = wal.recover().unwrap();
    let e1 = iter.next().unwrap().unwrap();
    assert_eq!(e1.shard_id, 10);
    assert_eq!(e1.lsn, 0);
    assert!(matches!(e1.payload, WalPayload::Write(_)));
    let e2 = iter.next().unwrap().unwrap();
    assert_eq!(e2.shard_id, 20);
    assert_eq!(e2.lsn, 1);
    assert!(iter.next().is_none());
}

#[test]
fn file_wal_recover_empty_when_not_synced() {
    let tmp = tempfile::tempdir().unwrap();
    let wal = FileWalStore::open(tmp.path().join("rg2"), 2, 1024);
    wal.append(5, &put(b"k", b"v")).unwrap();
    // not synced -> recover sees nothing on disk
    let mut iter = wal.recover().unwrap();
    assert!(iter.next().is_none());
}

#[test]
fn file_wal_rotates_segments_and_keeps_lsn() {
    let tmp = tempfile::tempdir().unwrap();
    // tiny segment_size (256B) so a few entries span multiple segments
    let wal = FileWalStore::open_with(tmp.path().join("rg"), 1, 64, 256);
    for i in 0..30u32 {
        wal.append(7, &put(&i.to_be_bytes(), b"vvvvvvvvvvvvvvvv"))
            .unwrap();
    }
    wal.sync().unwrap();

    // more than one segment file on disk
    let segs: Vec<_> = std::fs::read_dir(tmp.path().join("rg"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".wal"))
        .collect();
    assert!(segs.len() > 1, "expected rotation, got {segs:?}");

    // recover yields all 30 entries, lsn 0..30
    let entries: Vec<_> = wal.recover().unwrap().map(|r| r.unwrap()).collect();
    assert_eq!(entries.len(), 30);
    for (i, e) in entries.iter().enumerate() {
        assert_eq!(e.lsn, i as u64);
        assert_eq!(e.shard_id, 7);
    }
}

#[test]
fn file_wal_buffer_full_auto_syncs() {
    let tmp = tempfile::tempdir().unwrap();
    // buffer_size 32: each entry > 32 bytes -> every append auto-syncs
    let wal = FileWalStore::open_with(tmp.path().join("rg"), 1, 32, 1 << 20);
    wal.append(1, &put(b"k", b"v")).unwrap();
    // not manually synced, but buffer-full should have flushed it
    let mut iter = wal.recover().unwrap();
    let e = iter.next().unwrap().unwrap();
    assert_eq!(e.shard_id, 1);
}

#[test]
fn file_wal_background_sync_thread_persists() {
    use std::sync::Arc;
    use std::time::Duration;

    let tmp = tempfile::tempdir().unwrap();
    // large buffer so buffer-full never triggers; rely on the sync thread
    let wal = Arc::new(FileWalStore::open_with(
        tmp.path().join("rg"),
        1,
        1 << 20,
        1 << 20,
    ));
    wal.start_sync(Duration::from_millis(50));
    for i in 0..10u32 {
        wal.append(3, &put(&i.to_be_bytes(), b"v")).unwrap();
    }
    // wait > sync_interval for the thread to flush
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let n = wal.recover().unwrap().count();
        if n == 10 || std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let n = wal.recover().unwrap().count();
    assert_eq!(n, 10);
    // dropping `wal` stops the sync thread (no panic / hang)
}

#[test]
fn log_service_create_rg_and_get() {
    use storage::log_service::{LogService, RgOption};
    use std::time::Duration;
    let tmp = tempfile::tempdir().unwrap();
    let svc = LogService::new(tmp.path().to_path_buf());
    let opt = RgOption {
        rf: 1,
        wal_buffer_size: 64,
        wal_sync_interval: Duration::from_secs(60),
        wal_segment_size: 1 << 20,
    };
    svc.create_rg(42, &opt).unwrap();
    let wal = svc.get(42).unwrap();
    wal.append(1, &put(b"k", b"v")).unwrap();
    wal.sync().unwrap();
    let n = wal.recover().unwrap().count();
    assert_eq!(n, 1);
}

#[test]
fn log_service_get_unknown_rg_is_not_found() {
    use storage::log_service::LogService;
    let tmp = tempfile::tempdir().unwrap();
    let svc = LogService::new(tmp.path().to_path_buf());
    assert!(svc.get(999).is_err());
}
