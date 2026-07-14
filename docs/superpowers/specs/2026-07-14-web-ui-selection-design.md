# Web UI Selection and Folder Indentation Design

**Date:** 2026-07-14  
**Status:** Approved

## Goal

Make grouped tables visually hierarchical and add an explicit, accessible
selection mode for bulk secret and file operations.

## Scope

- Expanded folder contents are indented beneath their folder header in both the
  Secrets and Files tables.
- Each table gains a `Select` control that enables per-item checkboxes.
- The table-header checkbox selects or clears only the currently visible items.
- Both tables support bulk deletion.
- Secrets additionally support bulk movement to a destination folder.
- File movement is not included because `FileBackend` has no rename/move
  operation.

## Interaction design

Selection mode is explicit so ordinary secret-row clicks continue to open the
drawer and the tables remain uncluttered during normal browsing. Entering the
mode reveals a checkbox column and a compact action bar. The action bar shows
the selected count, destructive or move actions, and `Cancel`.

Checkbox clicks stop row activation. Secret rows do not open the drawer while
selection mode is active; clicking a row toggles its selection instead. File
download and per-file delete controls are hidden in selection mode to keep the
interaction unambiguous.

The header checkbox reflects the visible rows:

- checked when every visible item is selected;
- indeterminate when only some visible items are selected;
- unchecked when none are selected.

When a secret search is active, `Select all` affects only matching rows.
Selections outside the filter remain selected and are included in the selected
count and subsequent bulk action.

Selection is cleared when changing vaults, switching tabs, or cancelling
selection mode. Reloading a table prunes identifiers that no longer exist.

## Folder hierarchy

`renderGrouped` marks rows emitted beneath an expanded folder as folder
children. The item-name cell receives additional left padding and a subtle
guide line. Loose items are not indented. Existing grouping remains flat:
folder strings such as `proj/db` are still one group, not a nested tree.

## Bulk operations

Bulk operations reuse existing per-item API routes with at most four requests
in flight:

- secret delete: `DELETE /api/secrets/:name`;
- file delete: `DELETE /api/files/:name`;
- secret move: `POST /api/secrets/:name/move` with `{ "folder": "..." }`.

Bulk deletion uses the existing two-click confirmation pattern. Controls are
disabled while an operation is pending. After completion, successful items are
removed from selection, failed items remain selected, and the current table is
reloaded. A summary toast reports the success count and names each failure.

## Accessibility

- Selection checkboxes have item-specific accessible labels.
- Header checkboxes identify their visible-item scope.
- Selection counts use an `aria-live` region.
- Indentation is visual only; folder disclosure rows retain their existing
  keyboard behavior and `aria-expanded` state.
- Pending actions expose visible text and disabled state.

## Testing

Implementation proceeds test-first. Embedded-asset contract tests cover the
new controls, selection state, visible-only select-all behavior, folder-child
styling hooks, bounded bulk execution, bulk API routes, and confirmation.
Existing UI tests continue to cover authentication, stale response guards, and
single-item destructive actions.

Final verification runs:

- `cargo fmt --check`
- `cargo clippy --features ui --all-targets`
- `cargo test --features ui`

