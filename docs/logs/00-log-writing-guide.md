# Log Writing Guide

## Purpose

Daily dev logs provide a lightweight, append-only record of what changed, why, and what was verified. They are searchable implementation memory, not polished documentation.

## File Structure

- Path: `docs/logs/{year}/{month}-{day}.md` (e.g., `docs/logs/2026/06-27.md`)
- Format: Reverse chronological (newest entries at top)
- One file per day

## Entry Format

```markdown
## HH:MM - Brief description

**What**: What was changed (files, features, fixes)
**Why**: Reason for the change (requirement, bug, refactor)
**How**: Key implementation decisions (if non-obvious)
**Verification**: Commands run and results (pass/fail)
**Notes**: Gotchas, open questions, follow-up needed

Commit: <commit-hash> (if committed)
```

## When to Write

After completing ANY significant code change:
- Feature implementation
- Bug fix
- Refactoring that touches multiple files
- Design decision that affects future work

Do NOT wait until end of day. Write immediately after verifying the change.

## Verification Status

When verification passes completely (full green), include the status in:
1. The log entry ("Verification: ✅ All tests passed")
2. The git commit message

This provides reliable known-good baselines for future debugging.

## What to Include

✅ DO include:
- Files changed (list major ones)
- Why the change was needed
- Non-obvious implementation choices
- Verification commands and results
- Known issues or follow-up needed
- Links to related specs/plans

❌ DO NOT include:
- Full code snippets (link to commit instead)
- Detailed API docs (belongs in code comments)
- Future roadmap (belongs in backlog)
- Long explanations (keep it concise)

## Example Entry

```markdown
## 14:30 - Add SST footer with first/last key metadata

**What**: Modified SST footer from 8 bytes to 12 bytes, added first_key and last_key fields
**Why**: Enable key range filtering to skip SSTs during scans (requirement: efficient range queries)
**How**: Extended SstFooter struct, updated encode/decode, modified SstBuilder to track keys
**Verification**: ✅ `cargo test -p storage` - all pass, `cargo test wal_tests` - pass
**Notes**: Footer backward incompatible, existing SST files need rebuild

Files: crates/storage/src/builder/sst_builder.rs, crates/storage/src/block.rs
Commit: ef2b96f
```

## Tips

- Write while the context is fresh (don't wait)
- Be concise but complete
- Link to related issues/specs/plans
- Mark verification status clearly (✅/❌)
- Note any surprising behavior or gotchas
