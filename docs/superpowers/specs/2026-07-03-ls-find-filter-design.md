# `--filter <GLOB>` on `xv ls` and `xv find`

**Date:** 2026-07-03
**Status:** Approved design, not yet implemented

## Motivation

Finding secrets by name pattern currently requires piping (`xv find
--names-only | grep '^test-'`) or JSON + jq. `xv migrate` already has a
`--filter` glob; `ls` and `find` should offer the same, consistently.

## Design

### Shared helper

One helper beside the existing migrate logic (e.g. in `src/cli/` shared
module or `src/utils/`): compile the pattern with `globset::Glob` (exactly
as `src/cli/migrate_ops.rs:51` does), returning
`CrosstacheError::invalid_argument("Invalid glob pattern: …")` on a bad
pattern — before any backend call. A name-match predicate implements the
matching rule below. `migrate_ops` may adopt the helper if trivial; no
behavior change there either way.

### Matching rule

A secret matches when the glob matches **either**:
- its user-facing name (`original_name` when present — what `ls` displays), or
- its backend (sanitized) name.

This mirrors the either-name convention used by `xv mv` and `xv run
--include`. Matching is case-sensitive and whole-name (`test-*` matches
`test-db`, never `latest-db`), identical to `migrate --filter`.

### `xv ls --filter <GLOB>`

- Applied client-side after listing, before pagination/rendering.
- Composes with the folder positional, `--type`, `--deleted` (deleted
  summaries carry `original_name` since #304), and every output format.
- Help text mirrors migrate's: `Filter secrets by glob pattern on the name
  (e.g., "test-*", "api-*")`.

### `xv find --filter <GLOB>`

- Hard pre-filter on the candidate set, applied before fuzzy scoring; the
  optional PATTERN then ranks within the filtered set.
- Composes with `--in`, `--folder`, `--limit`, `--min-score`,
  `--names-only`, `--all-vaults`.
- `--filter` with no PATTERN = unranked filtered list (consistent with
  today's no-pattern behavior), so
  `xv find --filter 'test-*' --names-only` is the canonical
  "names starting with test-" one-liner.

## Error handling

Invalid glob → `invalid_argument`, fail before any backend call, message
identical in shape to migrate's.

## Testing

Hermetic e2e (local backend, isolated harness): prefix anchoring
(`test-*` excludes `latest-x`); either-name matching (secret whose backend
name differs from its display name); glob specials (`?`, `[ab]`);
composition with `--type`, folder scope, `--deleted`, and a fuzzy PATTERN
on find; invalid pattern errors before listing; `--names-only` piping.

## Out of scope

Server-side filtering (no backend supports it); case-insensitivity flags;
globs on fields other than the name (`--in` already covers find's field
selection for scoring).
