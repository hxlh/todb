# SST deep-tree traversal corrupted (height ≥ 4)

**Found**: 2026-06-27 (during WAL index read-path Phase 1.2 verification)
**Severity**: high — silent data loss / wrong read results on any deep SST tree
**Status**: fixed (commit pending)
**Files**: `crates/storage/src/builder/sst.rs`, `crates/storage/src/iterators/index_tree_iter.rs`

## Summary

Two independent latent bugs in the SST builder/iterator, both invisible on trees
of height ≤ 3 (the only shape the test suite ever built). A forward scan over a
taller tree either landed near the end of the key space or dropped entries
mid-scan. Existed since the SST stack was written; uncovered when the WAL index
read path built an index SST with `block_size=64` (→ height 6).

Both affect **any** deep SST tree, including large LSM SSTs produced by
compaction — not WAL-specific.

## Bug 1 — `SstBuilder::finish` orphaned top-level index blocks

`finish()` flushed index levels bottom-up with a loop whose range was fixed to
the pre-flush builder count. A level created *during* that loop (the new
topmost) was never visited. When it accumulated more than one entry, those
entries were sibling top-level blocks with **no parent block**, but
`root_position = topmost.last_entry().child` reached only the last one.

Symptom: `seek_to_first` descended the (sole reachable) last top-level block and
landed near the end of the key space — e.g. key 32 of 40 instead of key 0.

Fix: after the bottom-up loop, flush the topmost builder when
`entry_count() > 1`. The promoted summary becomes the sole entry of a new
topmost, so a single flush always yields one root block.

## Bug 2 — `IndexTreeIter::inner_next` skipped subtrees on multi-level pop

When a forward `next()` exhausted the leaf index block **and** its parent in the
same call (the multi-level-pop case), the re-descent pushed a freshly
`seek_to_first`'d child and then the next loop iteration called `next()` on it
**before** its first entry was consumed — skipping that entry and its whole
subtree. If the skip ran past the end, iteration stopped early.

`inner_seek_to_first` descended correctly (reads each child via `value()`
without advancing); `inner_next`'s re-descent did not mirror it.

Fix: rewrote `inner_next` as two phases — (1) advance current level, walking up
while exhausted; (2) re-descend to the leaf through first-entries, never calling
`next()` on a just-pushed level.

## Why it went unnoticed

Every existing SST test used `block_size ≥ 256` with modest entry counts →
`tree_height ≤ 3`. At height 3 the only non-leaf level is the root, so a pop
from the leaf always goes directly to root and re-descends a single level —
neither bug's trigger condition (unflushed topmost with >1 entry; multi-level
pop) ever occurs.

## Regression tests

- `iterators::sst_iter::tests::test_deep_tree_full_scan_reaches_all_keys` —
  `block_size=64`, 40 entries (asserts `tree_height ≥ 4`), full forward scan of
  all keys in order. Fails on either bug 1 or bug 2; passes with both fixes.
- `iterators::sst_iter::tests::test_reverse_deep_tree_full_scan_reaches_all_keys`
  — same tree, full reverse scan, expects all 40 keys descending. Guards bug 2's
  mirror in `inner_prev` (see below).

## Bug 2 also affected the reverse path (`inner_prev`) — fixed

`IndexTreeIter::inner_prev` (the reverse `next`) had the **same** single-loop
structure as the old `inner_next`, so the same multi-level-pop skip applied: a
reverse step that exhausted a leaf block *and* its parent in one call re-descended
by stepping a freshly-positioned child, skipping its last entry and subtree.
Empirically, a reverse scan of the 40-entry/tree-height-6 SST returned
`[39,38,37,36,33,32,1,0]` instead of all 40 descending.

> Correction of an earlier note: the reverse path is **not** a forward-semantics
> stub — it works correctly on shallow trees (existing `reverse_iter_tests` pass,
> e.g. `[4,3,2,1,0]`). The `.next()`/`.seek_to_first()` calls resolve to
> `ReverseIter` methods inside the reverse impl block (`I: ReverseIter`). The
> only defect was the deep-tree skip, symmetric to the forward one.

Fix: rewrote `inner_prev` with the same two-phase structure as `inner_next` —
(1) step current backward, walking up while exhausted; (2) re-descend to the leaf
through **last** entries (`ReverseIter::seek_to_first`), never stepping a
just-pushed level. Verified by the reverse regression test above.

> Note: the WAL index read path is forward-only (`get` + forward `scan`), so this
> reverse fix is not required by it — done for correctness/consistency since the
> root cause is identical and LSM reverse range scans over large SSTs would hit
> it too.

