# Filesystem Verbs, Round 1: `ls` Aliases Everywhere + `xv mv`

**Date:** 2026-07-02
**Status:** Approved design, not yet implemented
**Depends on:** trait-level rename (issue #295, shipped in PR #298 — merged to main)

## Motivation

The v0.17.0 list-UX overhaul gave xv a filesystem mental model: folder paths,
an ls-style grid, `-l`/`-r`. Users who adopt that model immediately reach for
two things that don't exist yet:

1. `ls` on nested list subcommands — `xv ls` works, but `xv vault ls` errors.
   Muscle memory guarantees users will type it.
2. A way to move things — changing a secret's folder or name today requires
   knowing about `xv update --folder` / `xv update --rename`, which is the
   flag-soup version of what a filesystem user would express as `mv`.

Deferred from this round: `cp` (copy secret), cross-vault moves, `mkdir`
(meaningless — folders are virtual and cannot be empty).

## Part 1: `ls` aliases

Add a hidden `#[command(alias = "ls")]` to the `List` variant of every
subcommand enum that has one:

| Enum | Command gained |
|---|---|
| `VaultCommands` | `xv vault ls` |
| `GroupCommands` | `xv group ls` |
| `ShareCommands` | `xv share ls` |
| `VaultShareCommands` | `xv vault share ls` |
| `ContextCommands` | `xv context ls` |
| `EnvCommands` | `xv env ls` |
| `FileCommands` (`src/cli/file.rs`) | `xv file ls` |

Hidden alias (not `visible_alias`), matching the top-level `xv ls` precedent,
so help output stays clean. No behavior change. No conflicts — none of these
enums has an existing `ls` variant or alias.

## Part 2: `xv mv`

### Command surface

```
xv mv <SOURCE> <DEST> [--dry-run] [--yes]
```

Plus the standard global/context flags (`--vault`, etc.) resolved the same way
`xv update` resolves them. Both operands are within the one resolved vault.

### Path grammar (trailing-slash convention)

A trailing slash marks a folder; anything else is a secret path whose last
segment is the name.

| Command | Meaning |
|---|---|
| `xv mv db/pass app/` | move secret into folder `app`, keep name `pass` |
| `xv mv db/pass app/pw` | move to folder `app` and rename to `pw` |
| `xv mv db/pass newname` | rename to `newname` at root |
| `xv mv app/pass /` | move to root (clears the folder tag), keep name |
| `xv mv app/ svc/` | bulk: re-folder every secret under `app/` to `svc/` |

Rules:

- Folder source (trailing slash) with a non-slash destination is an error with
  a corrective hint ("folder moves require a folder destination ending in /"),
  never a guess.
- `/` as destination means the vault root; as part of a bulk move source it is
  rejected (moving the whole root is not a supported operation this round).
- Nested prefixes match: `xv mv app/ svc/` moves secrets with folder `app`
  *and* folders nested under `app/…`, preserving the remainder of the path
  (`app/db/x` → `svc/db/x`).

### Execution semantics

The two underlying operations have very different costs; `mv` routes by what
actually changed:

- **Folder-only change** (name unchanged): metadata-only tag update through
  the existing `update --folder` code path. Version history preserved, cheap,
  supported on all backends.
- **Name change** (with or without folder change): routes through the
  trait-level rename machinery shipped for #295, inheriting its cache
  invalidation and partial-rename messaging. Before touching anything, `mv`
  checks the destination name for a collision with an existing secret and
  errors out if one exists.
- **No-op** (`mv x x`): say so, exit 0.

### Bulk folder move

1. Enumerate secrets whose folder equals the source prefix or is nested under
   it. Disabled secrets are included — folder is metadata.
2. Print the count plus a sample of the first ~10 old → new mappings, with a
   "use --dry-run to list all" hint when truncated. `--dry-run` prints the
   full plan and touches nothing.
3. Confirm interactively unless `--yes`. When stdin is not a TTY, `--yes` is
   required; otherwise the command aborts with a message saying so.
4. Apply the tag updates, reporting each failure and continuing.
5. Exit non-zero if any secret failed; invalidate the secret-list cache
   (once, at the end — mirroring the rename fix's cache discipline).

Names never change in a bulk move, so collisions are impossible.

### Errors and edge cases

- Source secret not found → closest-match suggestion, same pattern as
  `attach_vault_suggestion` in `src/cli/vault_ops.rs`.
- Empty folder prefix → "no secrets under 'app/'".
- Destination names pass through the existing `name_manager` sanitization
  rules; a destination that sanitizes to something different is handled
  exactly as `xv set` handles it today (original name preserved in the
  `original_name` tag).

### Implementation approach

**Chosen: (A) thin CLI verb over existing internals.** A new `Commands::Mv`
whose handler is a pure path-resolution step plus dispatch to the existing
folder-update and rename code paths. The path grammar becomes one pure,
table-testable function; no backend or trait changes are needed.

Rejected:

- (B) A new `move_secret` trait method per backend — rename and tag-update
  already exist at the trait level; this duplicates plumbing.
- (C) Sugar-only alias onto `xv update --rename --folder` — cannot express
  bulk moves or the path grammar, which is the point of the feature.

## Testing

- Table-driven unit tests for the path-resolution function: every row of the
  grammar table above, plus the error rows (folder source → non-slash dest,
  root as bulk source, empty operands).
- Integration tests for single-secret folder move, move+rename, and bulk move
  per backend, mirroring the existing rename test structure (LocalStack for
  AWS, live e2e for Azure, in-process for local).
- CLI-level tests for `--dry-run` (no writes) and the non-TTY `--yes`
  requirement.

## Delivery

Two PRs: PR 1 carries this spec plus the ls aliases (trivial, mergeable on
sight); PR 2 carries `xv mv`, stacked on PR 1. Verification for PR 2 is the
full ladder: unit + integration tests, LocalStack for AWS, and a live Azure
mv roundtrip against kv-scottzionic mirroring the #298 rename e2e (scratch
secrets cleaned up; soft-deleted remnants auto-purge like existing fixtures).

## Documentation

- `README.md` command reference gains `mv`.
- `CHANGELOG.md` entry under Unreleased.
- Help-text hint on `xv ls`: mention that `--format table` gives the classic
  table view (small discoverability follow-up from the list-UX review).
