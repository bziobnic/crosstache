# `--filter <GLOB>` on `xv mv`

**Date:** 2026-07-03
**Status:** Approved design, not yet implemented
**Depends on:** the shared glob helper from #326 (`compile_name_glob` / `glob_matches_either_name` in `src/utils/helpers.rs`); `xv mv` bulk machinery (v0.18.0, PR #300)

## Motivation

Moving secrets that match a name pattern into a folder currently requires a
shell loop (`xv find --filter 'test-*' --names-only | while read -r n; do
xv mv "$n" archive/ ; done`) — N invocations, no unified plan/confirmation,
no single dry-run. `xv mv` already has bulk-move machinery for folder
moves; a `--filter` glob should reuse it.

## CLI shape

```
xv mv --filter 'test-*' archive/     # move all matching secrets into archive/
xv mv --filter 'test-*' /            # move matches to the vault root
```

- With `--filter`, the `SOURCE` positional must be omitted: exactly one of
  (`SOURCE`, `--filter`) — violations are a usage error naming both forms.
- `DEST` must be a folder destination (`folder/` or `/`): renames are
  impossible for a multi-secret move. A non-folder dest errors with the
  same shape as the existing "folder moves require a folder destination
  ending in / (got '…'); did you mean '…/'?" message.
- Composes with the existing `--yes` and `--dry-run` flags.

## Semantics

- **Matching** is identical to `ls`/`find --filter` (#326): `globset`,
  case-sensitive, whole-name, matched against **either** the user-facing
  name (`original_name`) or the backend (sanitized) name, bare names (not
  folder-qualified), whole-vault scope.
- Matched secrets keep their names; only the `folder` metadata is
  rewritten to `DEST` — metadata-only, exactly like folder moves.
- **Plan variant**: a new `MvPlan::Filter { pattern: String, dest_prefix:
  Option<String> }` in `src/cli/mv_ops.rs` flowing through the existing
  bulk path (`execute_folder_mv`'s machinery, generalized or mirrored):
  count + sample plan confirmation, `--yes` bypass, non-TTY without
  `--yes` refuses (same exit as bulk folder moves today), `--dry-run`
  previews the full plan without moving, collision pre-check on backend
  names before any move, attempt-all-report-failures partial-failure
  behavior.
- **Already-in-dest** secrets are skipped and noted in the plan output
  (not errors, not counted as moves).
- **Zero matches** → fail loud: `no secrets matched --filter 'test-*'`
  (non-zero exit), consistent with `run`'s fail-loud posture.
- **Invalid glob** → `invalid_argument` before any backend call (same
  message shape as `ls`/`find`/`migrate`).

## Rejected alternatives

- Looping per-secret moves internally: loses the unified
  plan/confirm/dry-run/partial-failure semantics — no better than the
  shell loop.
- A glob parameter on the existing `MvPlan::Folder` variant: muddies the
  trailing-slash grammar; folder-scoped filtering can compose later if
  ever needed (out of scope).

## Testing

Hermetic e2e (local backend, existing harness idioms): filter move
relocates matches and only matches; either-name matching; dest-must-be-
folder error; SOURCE-xor-`--filter` usage error; `--dry-run` previews
without moving; non-TTY without `--yes` refuses with nothing moved;
already-in-dest skip note; zero-match failure; invalid glob fails before
listing; collision pre-check refuses before any move.

## Docs

`mv` help text; README bulk-move example replaces the shell loop with
`xv mv --filter 'test-*' archive/`; CHANGELOG Added entry under
Unreleased.

## Out of scope

Folder-scoped filtered moves (`xv mv src/ dest/ --filter 'x-*'`);
cross-vault filtered moves; filters on fields other than the name.
