use std::ops::Bound;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use storage::{
    engine::TableOption,
    iterators::ScanIter,
    lsm_state::{LsmEngineOption, LsmTableOption},
    meta_manager::MetaManager,
    storage_layer::{Engine, StorageLayer},
    write_batch::WriteBatch,
};

fn key(s: &str) -> Bytes {
    Bytes::from(s.as_bytes().to_vec())
}

fn collect_scan(scanner: &mut dyn ScanIter) -> Vec<(Bytes, Option<Bytes>)> {
    let mut out = Vec::new();
    while scanner.valid() {
        let k = Bytes::copy_from_slice(scanner.key().unwrap());
        let v = match scanner.value().unwrap() {
            storage::memtable::Entry::Put(v) => Some(Bytes::copy_from_slice(v)),
            storage::memtable::Entry::Delete => None,
        };
        out.push((k, v));
        scanner.next().unwrap();
    }
    out
}

fn engine_option(dir: &Path) -> LsmEngineOption {
    LsmEngineOption {
        data_dir: dir.to_path_buf(),
        ..Default::default()
    }
}

fn table_option() -> TableOption {
    TableOption::LsmTree(LsmTableOption::default())
}

/// Build a StorageLayer + MetaManager over a temp data_dir.
fn make_layer(dir: &Path) -> (Arc<StorageLayer>, MetaManager) {
    let storage = Arc::new(StorageLayer::new(engine_option(dir)));
    let meta = MetaManager::new(storage.clone());
    (storage, meta)
}

#[test]
fn test_write_then_scan_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, meta) = make_layer(dir.path());
    meta.create_table("t1", Engine::LsmTree, table_option()).unwrap();
    let shard = meta.shard_for("t1").unwrap();

    let mut batch = WriteBatch::new();
    batch.put(key("k1"), key("v1"));
    batch.put(key("k2"), key("v2"));
    batch.put(key("k3"), key("v3"));
    storage.write(&shard.engine, shard.shard_id, batch).unwrap();

    let range = (Bound::Unbounded, Bound::Unbounded);
    let mut scanner = storage.scan(&shard.engine, shard.shard_id, range, false).unwrap();
    let rows = collect_scan(&mut *scanner);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, key("k1"));
    assert_eq!(rows[0].1.as_ref().unwrap(), &key("v1"));
    assert_eq!(rows[2].0, key("k3"));
}

#[test]
fn test_delete_tombstone() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, meta) = make_layer(dir.path());
    meta.create_table("t1", Engine::LsmTree, table_option()).unwrap();
    let shard = meta.shard_for("t1").unwrap();

    let mut batch = WriteBatch::new();
    batch.put(key("k1"), key("v1"));
    batch.put(key("k2"), key("v2"));
    storage.write(&shard.engine, shard.shard_id, batch).unwrap();

    // Delete k1.
    let mut del = WriteBatch::new();
    del.delete(key("k1"));
    storage.write(&shard.engine, shard.shard_id, del).unwrap();

    let range = (Bound::Unbounded, Bound::Unbounded);
    let mut scanner = storage.scan(&shard.engine, shard.shard_id, range, false).unwrap();
    let rows = collect_scan(&mut *scanner);
    // k1 is a deleted tombstone, k2 still present.
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, key("k1"));
    assert!(rows[0].1.is_none()); // tombstone
    assert_eq!(rows[1].1.as_ref().unwrap(), &key("v2"));
}

#[test]
fn test_range_scan_lower_bound() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, meta) = make_layer(dir.path());
    meta.create_table("t1", Engine::LsmTree, table_option()).unwrap();
    let shard = meta.shard_for("t1").unwrap();

    let mut batch = WriteBatch::new();
    for i in 0..10 {
        batch.put(key(&format!("k{i:02}")), key(&format!("v{i:02}")));
    }
    storage.write(&shard.engine, shard.shard_id, batch).unwrap();

    // Scan [k03, unbounded).
    let range = (Bound::Included(key("k03")), Bound::Unbounded);
    let mut scanner = storage.scan(&shard.engine, shard.shard_id, range, false).unwrap();
    let rows = collect_scan(&mut *scanner);
    assert_eq!(rows.len(), 7); // k03..k09
    assert_eq!(rows[0].0, key("k03"));
    assert_eq!(rows.last().unwrap().0, key("k09"));
}

#[test]
fn test_range_scan_upper_bound() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, meta) = make_layer(dir.path());
    meta.create_table("t1", Engine::LsmTree, table_option()).unwrap();
    let shard = meta.shard_for("t1").unwrap();

    let mut batch = WriteBatch::new();
    for i in 0..10 {
        batch.put(key(&format!("k{i:02}")), key(&format!("v{i:02}")));
    }
    storage.write(&shard.engine, shard.shard_id, batch).unwrap();

    // Scan [unbounded, k05).
    let range = (Bound::Unbounded, Bound::Excluded(key("k05")));
    let mut scanner = storage.scan(&shard.engine, shard.shard_id, range, false).unwrap();
    let rows = collect_scan(&mut *scanner);
    assert_eq!(rows.len(), 5); // k00..k04
    assert_eq!(rows.last().unwrap().0, key("k04"));
}
