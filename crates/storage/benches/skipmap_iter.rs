use std::ops::Bound;

use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use crossbeam_skiplist::SkipMap;

fn make_sized_key(i: u64, key_size: usize) -> Bytes {
    let mut buf = vec![0u8; key_size];
    let bytes = i.to_be_bytes();
    let n = bytes.len().min(key_size);
    buf[..n].copy_from_slice(&bytes[..n]);
    // fill rest with pseudo-random pattern based on i to avoid all zeros
    for j in bytes.len()..key_size {
        buf[j] = ((i >> ((j % 8) * 8)) as u8).wrapping_add(j as u8);
    }
    Bytes::from(buf)
}

fn make_sized_value(i: u64, value_size: usize) -> Bytes {
    if value_size == 0 {
        Bytes::new()
    } else {
        let mut buf = vec![0u8; value_size];
        let prefix = format!("value_{:04}", i);
        let pb = prefix.as_bytes();
        let n = pb.len().min(value_size);
        buf[..n].copy_from_slice(&pb[..n]);
        for j in pb.len()..value_size {
            buf[j] = ((i >> ((j % 8) * 8)) as u8).wrapping_add(j as u8);
        }
        Bytes::from(buf)
    }
}

fn build_map(n: u64, key_size: usize, value_size: usize) -> SkipMap<Bytes, Bytes> {
    let map = SkipMap::new();
    for i in 0..n {
        map.insert(make_sized_key(i, key_size), make_sized_value(i, value_size));
    }
    map
}

// ── 1. varying entry count (fixed key=8, value=100) ──────────────────────────

fn bench_range_iter_by_count(c: &mut Criterion) {
    let mut group = c.benchmark_group("range_iter/count");
    for size in [100u64, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let map = build_map(size, 8, 100);
            b.iter(|| {
                let mut count = 0u64;
                let mut iter = map.range::<Bytes, (Bound<Bytes>, Bound<Bytes>)>((
                    Bound::Unbounded,
                    Bound::Unbounded,
                ));
                while let Some(entry) = iter.next() {
                    black_box(entry.key());
                    black_box(entry.value());
                    count += 1;
                }
                assert_eq!(count, size);
            });
        });
    }
    group.finish();
}

fn bench_lower_bound_iter_by_count(c: &mut Criterion) {
    let mut group = c.benchmark_group("lower_bound_iter/count");
    for size in [100u64, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let map = build_map(size, 8, 100);
            b.iter(|| {
                let mut count = 0u64;
                let mut current = map
                    .lower_bound(Bound::Included(&make_sized_key(0, 8)))
                    .map(|e| (e.key().clone(), e.value().clone()));
                while let Some((k, _v)) = current {
                    black_box(&k);
                    count += 1;
                    current = map
                        .lower_bound(Bound::Excluded(&k))
                        .map(|e| (e.key().clone(), e.value().clone()));
                }
                assert_eq!(count, size);
            });
        });
    }
    group.finish();
}

fn bench_hybrid_iter_by_count(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_iter/count");
    for size in [100u64, 1_000, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let map = build_map(size, 8, 100);
            b.iter(|| {
                let mut count = 0u64;
                let mut entry = map.lower_bound(Bound::Included(&make_sized_key(0, 8)));
                while let Some(e) = entry {
                    black_box(e.key());
                    black_box(e.value());
                    count += 1;
                    entry = e.next();
                }
                assert_eq!(count, size);
            });
        });
    }
    group.finish();
}

// ── 2. varying key size (fixed n=10k, value=100) ─────────────────────────────

fn bench_range_iter_by_keysize(c: &mut Criterion) {
    let mut group = c.benchmark_group("range_iter/keysize");
    for key_size in [8usize, 64, 256, 1024] {
        group.bench_with_input(BenchmarkId::from_parameter(key_size), &key_size, |b, &key_size| {
            let map = build_map(10_000, key_size, 100);
            b.iter(|| {
                let mut count = 0u64;
                let mut iter = map.range::<Bytes, (Bound<Bytes>, Bound<Bytes>)>((
                    Bound::Unbounded,
                    Bound::Unbounded,
                ));
                while let Some(entry) = iter.next() {
                    black_box(entry.key());
                    black_box(entry.value());
                    count += 1;
                }
                assert_eq!(count, 10_000);
            });
        });
    }
    group.finish();
}

fn bench_lower_bound_iter_by_keysize(c: &mut Criterion) {
    let mut group = c.benchmark_group("lower_bound_iter/keysize");
    for key_size in [8usize, 64, 256, 1024] {
        group.bench_with_input(BenchmarkId::from_parameter(key_size), &key_size, |b, &key_size| {
            let map = build_map(10_000, key_size, 100);
            b.iter(|| {
                let mut count = 0u64;
                let mut current = map
                    .lower_bound(Bound::Included(&make_sized_key(0, key_size)))
                    .map(|e| (e.key().clone(), e.value().clone()));
                while let Some((k, _v)) = current {
                    black_box(&k);
                    count += 1;
                    current = map
                        .lower_bound(Bound::Excluded(&k))
                        .map(|e| (e.key().clone(), e.value().clone()));
                }
                assert_eq!(count, 10_000);
            });
        });
    }
    group.finish();
}

fn bench_hybrid_iter_by_keysize(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_iter/keysize");
    for key_size in [8usize, 64, 256, 1024] {
        group.bench_with_input(BenchmarkId::from_parameter(key_size), &key_size, |b, &key_size| {
            let map = build_map(10_000, key_size, 100);
            b.iter(|| {
                let mut count = 0u64;
                let mut entry = map.lower_bound(Bound::Included(&make_sized_key(0, key_size)));
                while let Some(e) = entry {
                    black_box(e.key());
                    black_box(e.value());
                    count += 1;
                    entry = e.next();
                }
                assert_eq!(count, 10_000);
            });
        });
    }
    group.finish();
}

// ── 3. varying value size (fixed n=10k, key=8) ───────────────────────────────

fn bench_range_iter_by_valuesize(c: &mut Criterion) {
    let mut group = c.benchmark_group("range_iter/valuesize");
    for value_size in [0usize, 100, 1000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(value_size), &value_size, |b, &value_size| {
            let map = build_map(10_000, 8, value_size);
            b.iter(|| {
                let mut count = 0u64;
                let mut iter = map.range::<Bytes, (Bound<Bytes>, Bound<Bytes>)>((
                    Bound::Unbounded,
                    Bound::Unbounded,
                ));
                while let Some(entry) = iter.next() {
                    black_box(entry.key());
                    black_box(entry.value());
                    count += 1;
                }
                assert_eq!(count, 10_000);
            });
        });
    }
    group.finish();
}

fn bench_lower_bound_iter_by_valuesize(c: &mut Criterion) {
    let mut group = c.benchmark_group("lower_bound_iter/valuesize");
    for value_size in [0usize, 100, 1000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(value_size), &value_size, |b, &value_size| {
            let map = build_map(10_000, 8, value_size);
            b.iter(|| {
                let mut count = 0u64;
                let mut current = map
                    .lower_bound(Bound::Included(&make_sized_key(0, 8)))
                    .map(|e| (e.key().clone(), e.value().clone()));
                while let Some((k, _v)) = current {
                    black_box(&k);
                    count += 1;
                    current = map
                        .lower_bound(Bound::Excluded(&k))
                        .map(|e| (e.key().clone(), e.value().clone()));
                }
                assert_eq!(count, 10_000);
            });
        });
    }
    group.finish();
}

fn bench_hybrid_iter_by_valuesize(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_iter/valuesize");
    for value_size in [0usize, 100, 1000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(value_size), &value_size, |b, &value_size| {
            let map = build_map(10_000, 8, value_size);
            b.iter(|| {
                let mut count = 0u64;
                let mut entry = map.lower_bound(Bound::Included(&make_sized_key(0, 8)));
                while let Some(e) = entry {
                    black_box(e.key());
                    black_box(e.value());
                    count += 1;
                    entry = e.next();
                }
                assert_eq!(count, 10_000);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_range_iter_by_count,
    bench_lower_bound_iter_by_count,
    bench_hybrid_iter_by_count,
    bench_range_iter_by_keysize,
    bench_lower_bound_iter_by_keysize,
    bench_hybrid_iter_by_keysize,
    bench_range_iter_by_valuesize,
    bench_lower_bound_iter_by_valuesize,
    bench_hybrid_iter_by_valuesize,
);
criterion_main!(benches);
