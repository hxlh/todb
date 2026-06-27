# Backlog

This file tracks prioritized work items ready for AI implementation.

## Status Legend

- `ready` - Has requirement/owner doc, verification commands, and no blockers
- `blocked` - Waiting on dependency, decision, or missing context
- `in-progress` - Currently being worked on
- `done` - Completed and verified

## Autonomy Labels

See `docs/context/ai-autonomy-policy.md` for definitions.
- `implement` - AI may proceed after reading docs
- `plan-first` - AI may draft plan, implementation waits for audit
- `ask-first` - Human approval required before code changes
- `research-only` - Investigation only, no behavior changes
- `blocked` - Do not proceed until blocker resolved

## Next Ready Items

| Priority | Item | Status | Autonomy | Owner Doc | Blockers | Notes |
|----------|------|--------|----------|-----------|----------|-------|
| 1 | Integrate wal_v2 with LsmStore | blocked | plan-first | docs/architecture/wal-design.md | Design needed | Replace wal_legacy with wal_v2 |
| 2 | Add L0 → L1 compaction | blocked | plan-first | (needs design doc) | Design needed | See old_docs for OB/TiKV references |
| 3 | Metadata persistence | blocked | plan-first | (needs design doc) | Design needed | RG + shard metadata to disk |
| 4 | Full restart recovery | blocked | plan-first | (needs design doc) | Items 2+3 | WAL replay + metadata load |
| 5 | SST key range filtering | ready | implement | docs/design/storage-engine-design.md | none | Use SST footer first/last keys |

## Backlog Selection Rule

If the user asks AI to continue work without naming a task, choose the highest-priority `ready` item with autonomy `implement`.

For `plan-first` items, AI may draft the plan but must wait for audit before implementation.

## Adding New Items

When adding items:
1. Assign priority (lower number = higher priority)
2. Set initial status (usually `blocked` or `ready`)
3. Set autonomy level based on complexity and risk
4. Link to existing owner doc or mark "(needs X doc)"
5. List concrete blockers or mark `none`
