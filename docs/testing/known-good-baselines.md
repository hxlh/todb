# Known-Good Baselines

This file tracks verification states that were confirmed working at specific commits.

Use these as reference points when debugging regressions.

## Format

| Date | Commit | Verification Command | Result | Notes |
|------|--------|---------------------|---------|-------|
| YYYY-MM-DD | hash | command | ✅/❌ | context |

## Baselines

| Date | Commit | Verification Command | Result | Notes |
|------|--------|---------------------|---------|-------|
| 2026-06-21 | ef2b96f | `cargo test -p storage` | ✅ | SST footer with first/last key |
| 2026-06-21 | e472a57 | `cargo test wal_tests` | ✅ | WAL + ReplicationGroup (rf=1) |
| 2026-06-19 | 523a62d | `cargo test -p storage` | ✅ | Disk manager, memtable flush, layering |

## How to Use

1. After a successful full verification, add an entry here
2. When debugging, find the most recent known-good baseline
3. Use `git bisect` between known-good and current state
4. Once fixed, add a new known-good entry

## Verification Commands Reference

From `docs/context/project-context.md`:
- Full test suite: `cargo test --workspace`
- Storage tests: `cargo test -p storage`
- WAL tests: `cargo test wal_tests`
- Integration tests: `cargo test --test '*' --workspace`
- Compile check: `cargo check --all-targets`
- Lint: `cargo clippy --all-targets -- -D warnings`
