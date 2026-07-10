# Web UI Folder Grouping, Collapsible Sections, Human-Readable Sizes

**Date:** 2026-07-09
**Status:** Approved design, not yet implemented

## Motivation

In `xv ui` (v0.24.0), the secrets table and the Files tab render flat rows.
Vaults organized with folders read poorly: folder membership is just a
column, related entries don't sit together, and large vaults scroll.
Additionally, file sizes render as raw byte counts.

## Scope

All changes are presentational, client-side only (`src/web/assets/app.js`,
`style.css`) — both lists already load their full datasets, so no API or
server changes. (Alternative considered and rejected: server-side
delimiter/prefix queries with per-folder lazy loading — needless for a
localhost single-user UI that holds the whole list in memory.)

## 1. Folder grouping (secrets table and files table)

**Grouping rules:**

- **Secrets** group by the full `folder` tag string. `proj/db` is one
  group named `proj/db` — flat groups, no nested tree, matching the
  folder tag being a single value.
- **Files** group by the dirname of the file name's `/` path:
  `a/b/c.txt` groups under `a/b`; names without `/` are loose files.
- Folder groups render first, sorted alphabetically by folder name
  (case-sensitive lexicographic, consistent with `Array.sort`). Loose
  items (no folder) follow below with no header.
- Within a group (and among loose items), rows keep the current order
  (as returned by the API).

**Header rows:**

- Each group gets one full-width header row (`colspan` = table width):
  a disclosure indicator (`▸` collapsed / `▾` expanded), the folder
  name, and an item count like `(3)`.
- Clicking the header toggles that group. Header rows are visually
  distinct (muted background, pointer cursor).

**Collapse state:**

- **Collapsed by default.** State is in-memory only: one `Set` of
  expanded folder names per table (`expandedSecretFolders`,
  `expandedFileFolders`).
- Cleared on vault switch (fresh vault starts fully collapsed). NOT
  cleared on reload-after-save/delete, so saving a secret inside an
  open folder doesn't snap it shut.
- No persistence across page loads.

**Search interaction (secrets):**

- While the filter box is non-empty, collapse state is ignored:
  matching rows always render under their group headers, and headers
  whose groups have zero matches are omitted. Clearing the filter
  restores the collapsed view. Rationale: hits hidden inside collapsed
  folders would read as "no results."
- The existing filter haystack (name, folder, groups, note) is
  unchanged.

**Row behavior unchanged:** secret rows open the drawer; file rows keep
their download/delete buttons. The secrets Folder column remains
(redundant under a header but harmless, and it keeps filter behavior
unchanged). Empty/loading/failed placeholder states are unchanged.

## 2. Human-readable file sizes

- New `fmtSize(bytes)` helper in `app.js`, mirroring the CLI's
  `format_size` (`src/utils/format.rs`) exactly: binary (1024) steps,
  units `B, KB, MB, GB, TB`; `0 B` for zero; whole bytes with no
  decimals; larger units with two decimals (e.g. `3.00 MB`,
  `1.46 KB`).
- Applied to the files table Size column only (no other size renders
  exist in the UI).

## Error handling

- A missing/empty folder tag and a file name with no `/` are the
  "loose" bucket — never a group named `""`.
- Non-numeric/absent `size` renders as an empty cell (current
  behavior for falsy cells).

## Testing

- No Rust changes → no new server tests.
- Browser e2e (playwright + headless Chrome recipe): folders listed
  before loose rows in both tables; groups collapsed by default; header
  click toggles rows; search reveals matches inside collapsed folders
  and hides empty groups; expanded state survives a save-triggered
  re-render and resets on vault switch; Size column shows `1.46 KB`
  style values matching `format_size` semantics.
