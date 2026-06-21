use bytes::Bytes;
use std::ops::Bound;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use storage::block::{BlockReader, BlockWriter, FileBlockWriter, Position};
use storage::builder::{DefaultSstWriter, SstBuilder, SstOption};
use storage::disk_manager::{DiskManager, SstFileReader, SstFileWriter};
use storage::engine::{StorageEngine, TableOption, TableStore, DEFAULT_SHARD};
use storage::lsm_engine::LsmEngine;
use storage::lsm_state::{LsmEngineOption, LsmTableOption};
use storage::write_batch::WriteBatch;

#[test]
fn sst_file_writer_delegates_and_reports_file_size() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("0.sst");
    let inner = FileBlockWriter::create(&path, 64).unwrap();
    let mut writer = SstFileWriter::new(42, inner);

    let pos = writer.write_block(vec![1u8; 64]).unwrap();
    assert_eq!(pos.offset, 0);
    assert_eq!(writer.sst_id(), 42);
    assert_eq!(writer.file_size(), 64);
}

/// Build one real SST (single entry); return its sst_id.
fn write_one_sst(dm: &DiskManager, key: &[u8], val: &[u8]) -> u64 {
    let writer = dm.create_sst().unwrap();
    let id = writer.sst_id();
    let opt = SstOption::default().block_size(dm.block_size());
    let mut b = SstBuilder::new(DefaultSstWriter::new(writer, &opt), opt);
    b.add(Bytes::copy_from_slice(key), Bytes::copy_from_slice(val))
        .unwrap();
    b.finish().unwrap();
    id
}

/// Build one real SST with the given keys (all mapped to "v"); return sst_id.
fn write_multi_sst(dm: &DiskManager, keys: &[&[u8]]) -> u64 {
    let writer = dm.create_sst().unwrap();
    let id = writer.sst_id();
    let opt = SstOption::default().block_size(dm.block_size());
    let mut b = SstBuilder::new(DefaultSstWriter::new(writer, &opt), opt);
    for k in keys {
        b.add(Bytes::copy_from_slice(k), Bytes::from_static(b"v"))
            .unwrap();
    }
    b.finish().unwrap();
    id
}

#[test]
fn create_sst_assigns_globally_monotonic_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let dm = DiskManager::new(tmp.path().to_path_buf(), 64);

    let w0 = dm.create_sst().unwrap();
    let w1 = dm.create_sst().unwrap();
    let w2 = dm.create_sst().unwrap();

    // globally monotonic + unique
    assert!(w1.sst_id() > w0.sst_id());
    assert!(w2.sst_id() > w1.sst_id());

    // files at {data_dir}/{sst_id}.sst
    assert!(tmp.path().join(format!("{}.sst", w0.sst_id())).exists());
    assert!(tmp.path().join(format!("{}.sst", w2.sst_id())).exists());
}

#[test]
fn open_reads_footer_back_from_file_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let dm = DiskManager::new(tmp.path().to_path_buf(), 256);
    let id = write_one_sst(&dm, b"k1", b"v1");

    let SstFileReader { reader, footer } = dm.open(id).unwrap();
    assert!(footer.tree_height >= 1); // non-empty SST has a root index level
    assert_eq!(footer.first_key, Bytes::copy_from_slice(b"k1"));
    assert_eq!(footer.last_key, Bytes::copy_from_slice(b"k1"));
    let blk = reader.read_block(&Position { offset: 0 }).unwrap();
    assert_eq!(blk.len(), 256);
}

#[test]
fn open_unknown_sst_id_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let dm = DiskManager::new(tmp.path().to_path_buf(), 256);
    assert!(dm.open(999).is_err());
}

#[test]
fn open_footer_reflects_first_and_last_key_across_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let dm = DiskManager::new(tmp.path().to_path_buf(), 64);
    // 30 distinct keys span multiple data blocks (block_size 64).
    let keys: Vec<Vec<u8>> = (0..30u32).map(|i| format!("k{i:05}").into_bytes()).collect();
    let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
    let id = write_multi_sst(&dm, &key_refs);

    let SstFileReader { footer, .. } = dm.open(id).unwrap();
    assert_eq!(footer.first_key, Bytes::from_static(b"k00000"));
    assert_eq!(footer.last_key, Bytes::from_static(b"k00029"));
}

#[test]
fn list_ssts_scans_dir_and_remove_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let dm = DiskManager::new(tmp.path().to_path_buf(), 256);
    let a0 = write_one_sst(&dm, b"k", b"v");
    let a1 = write_one_sst(&dm, b"k2", b"v2");
    let a2 = write_one_sst(&dm, b"k3", b"v3");

    let mut s = dm.list_ssts().unwrap();
    s.sort();
    assert_eq!(s, vec![a0, a1, a2]);

    dm.remove(a0).unwrap();
    assert!(dm.open(a0).is_err()); // file removed
    dm.remove(a0).unwrap(); // idempotent
}

fn put(key: &[u8], val: &[u8]) -> WriteBatch {
    let mut b = WriteBatch::new();
    b.put(Bytes::copy_from_slice(key), Bytes::copy_from_slice(val));
    b
}

fn engine_option(dir: &Path, flush_interval: Duration) -> LsmEngineOption {
    LsmEngineOption {
        data_dir: dir.to_path_buf(),
        block_size: 128,
        flush_interval,
        ..Default::default()
    }
}

fn table_option(max_imm: usize) -> TableOption {
    TableOption::LsmTree(LsmTableOption {
        memtable_size_limit: 64,
        max_imm_memtables: max_imm,
        ..Default::default()
    })
}

#[test]
fn flush_oldest_imm_makes_data_readable_from_sst() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = Arc::new(LsmEngine::new(engine_option(tmp.path(), Duration::from_secs(10))));
    engine
        .create_shard(
            DEFAULT_SHARD,
            &table_option(4),
            Arc::new(storage::wal::NoopWal),
        )
        .unwrap();
    let store = engine.acquire(DEFAULT_SHARD).unwrap();

    for i in 0..20u32 {
        store
            .write(put(&i.to_be_bytes(), &format!("v{i}").into_bytes()))
            .unwrap();
    }
    store.flush_oldest_imm().unwrap();

    let mut scan = store
        .scan((Bound::Unbounded, Bound::Unbounded), false)
        .unwrap();
    let mut got = Vec::new();
    while scan.valid() {
        let k = scan.key().unwrap().to_vec();
        got.push(u32::from_be_bytes(k.try_into().unwrap()));
        scan.next().unwrap();
    }
    assert_eq!(got.len(), 20);
}

#[test]
fn write_force_flushes_when_imm_count_exceeds_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = Arc::new(LsmEngine::new(engine_option(tmp.path(), Duration::from_secs(10))));
    engine
        .create_shard(
            DEFAULT_SHARD,
            &table_option(2),
            Arc::new(storage::wal::NoopWal),
        )
        .unwrap();
    let store = engine.acquire(DEFAULT_SHARD).unwrap();

    for i in 0..200u32 {
        store.write(put(&i.to_be_bytes(), b"v")).unwrap();
    }

    let mut scan = store
        .scan((Bound::Unbounded, Bound::Unbounded), false)
        .unwrap();
    let mut n = 0;
    while scan.valid() {
        n += 1;
        scan.next().unwrap();
    }
    assert_eq!(n, 200);
    assert!(!store.disk_manager().list_ssts().unwrap().is_empty());
}

#[test]
fn lsm_engine_flush_scheduler_flushes_immutables() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = Arc::new(LsmEngine::new(engine_option(tmp.path(), Duration::from_millis(100))));
    engine
        .create_shard(
            DEFAULT_SHARD,
            &table_option(4),
            Arc::new(storage::wal::NoopWal),
        )
        .unwrap();
    let store = engine.acquire(DEFAULT_SHARD).unwrap();
    for i in 0..20u32 {
        store.write(put(&i.to_be_bytes(), b"v")).unwrap();
    }
    assert!(engine.disk_manager().list_ssts().unwrap().is_empty());

    // Background sweep flushes within ~interval once init starts the scheduler.
    let _ = engine.clone().init();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while engine.disk_manager().list_ssts().unwrap().is_empty()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!engine.disk_manager().list_ssts().unwrap().is_empty());
    // `engine` dropped here -> flush scheduler stops.
}
