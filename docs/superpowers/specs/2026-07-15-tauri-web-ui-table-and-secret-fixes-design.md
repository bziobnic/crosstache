# Tauri Web UI Table and Secret Fixes Design

## Goal

Improve the embedded Crosstache web UI used by the Tauri proof of concept so
secret metadata is accurate, protected values do not leak their lengths,
tables are easier to scan and manipulate, and file actions are simpler and
safer.

## Scope

This change modifies the existing dependency-free HTML, CSS, and JavaScript
assets in `src/web/assets/` and their UI contract tests in `src/web/mod.rs`.
The Tauri shell continues to load the same embedded Axum web UI. No new
frontend framework or package pipeline is introduced.

## Secret Drawer

### Expiration dates

- A secret without a stored expiration displays a blank `Expires` date input.
- A secret with an expiration displays the stored calendar date in
  `YYYY-MM-DD` form.
- The date is extracted from the stored timestamp literal without a local
  timezone conversion, preventing the calendar day from shifting.
- Saving an empty expiration clears an existing expiration and does not create
  a value based on the current date.

### Protected-value masking

- Every existing protected value initially displays exactly 15 literal asterisks:
  `***************`.
- This rule applies to the ordinary whole-secret value and every `secret`-kind
  field in a typed record.
- The displayed mask is independent of the underlying value, so the actual
  secret length is never represented by the number of masking characters.
- An existing ordinary secret is not fetched merely to paint the mask. Its
  actual value is fetched on `Reveal` or `Copy`, then held separately from the
  displayed text while the drawer remains open. Typed-record envelopes already
  require a value fetch to identify their fields; those field values are also
  held separately from their displayed masks.
- `Reveal` displays the actual value and changes its label to `Hide`.
- `Hide` restores the 15-asterisk mask and changes its label to `Reveal`.
- Editing a revealed value updates the value that will be saved.
- Saving an untouched masked value preserves the original value. The mask is
  never submitted as secret data.
- Copy continues to copy the actual value, not the displayed mask.
- New ordinary secrets and new protected typed-record fields start blank and
  editable; the fixed mask applies only when an actual stored value exists.
- If a protected value cannot be loaded, the existing error path remains in
  effect; the UI must not silently substitute an empty value.

## Secret Table Dates

- The `Updated` column displays valid timestamps as date-only values in
  `YYYY-MM-DD` form.
- Invalid nonempty date strings remain visible as their original text rather
  than being replaced with a misleading value.

## Table Architecture

The existing semantic `<table>` elements remain in place. Each table gains a
`<colgroup>` that owns its column widths. Header cells contain accessible sort
buttons and, for resizable data columns, resize handles. Selection checkbox
columns remain fixed-width and are neither sortable nor resizable.

No data-grid dependency is added. Sorting, resizing, persistence, and header
state are implemented in the existing vanilla JavaScript.

## Sorting

- Clicking a sortable column header selects that column in ascending order.
- Clicking the active header toggles between ascending and descending order.
- Header state is exposed with `aria-sort` and a visible direction indicator.
- The initial sort is name ascending for both secrets and files.
- Folder groups remain intact and continue to be listed alphabetically.
- The active sort is applied independently to the rows within each folder and
  to the ungrouped rows.
- Equal values use the item name as a deterministic tie-breaker.
- Text columns use locale-aware, case-insensitive comparison.
- Size uses numeric comparison.
- Date columns compare parsed timestamps, with invalid or empty values ordered
  after valid values in ascending order.
- Sort state lasts for the current app session and is not persisted across
  reloads.

Sortable secret columns are `Name`, `Folder`, `Groups`, `Note`, and `Updated`.
Sortable file columns are `Name`, `Size`, `Type`, and `Modified`.

## Column Resizing and Defaults

- A resize handle appears at the trailing edge of each resizable data-column
  header except the final visible data column.
- Pointer dragging changes the adjacent column widths while maintaining the
  table width.
- Keyboard users can focus a resize handle and use left/right arrow keys to
  adjust widths in fixed increments.
- Minimum widths prevent any column from collapsing or making its header
  unusable.
- Widths persist in local storage under separate keys for the secrets and
  files tables.
- Missing, invalid, incompatible, or out-of-range stored widths are ignored and
  replaced with the complete default width set.
- Selection checkbox columns remain fixed and do not consume persisted width
  entries.

Default proportions favor text-heavy content and reduce unnecessary ellipses:

- Secrets: Name 28%, Folder 15%, Groups 14%, Note 25%, Updated 18%.
- Files: Name 42%, Size 12%, Type 24%, Modified 22%.

Existing responsive rules may hide lower-priority columns at narrow viewport
widths. The desktop defaults apply at the Tauri app's normal and minimum window
sizes.

## File Table Actions

- Each file name is rendered as a semantic link.
- Activating the link uses the existing authenticated fetch-and-download flow;
  it does not navigate the webview to an unauthenticated API URL.
- The file action column is removed.
- Per-row `Download` and `Delete` buttons are removed.
- Outside selection mode, clicking a file-name link downloads that file.
- In selection mode, clicking a row or its name toggles selection instead of
  downloading.
- File deletion is available only in selection mode.
- The sole file `Delete` button is in the top selection toolbar next to the
  `Cancel` button. Existing bulk confirmation, pending-state, bounded
  concurrency, and partial-failure reporting behavior remains in place.

## Folder Groups and Selection

- Folder expansion state remains independent of sorting and resizing.
- Sorting does not flatten folders or move rows between groups.
- Selection continues to operate on the currently rendered visible items.
- Changing sort order does not clear selected item identifiers.
- Entering or leaving selection mode does not discard persisted column widths.

## Accessibility

- Sort controls are real buttons and expose the active direction through
  `aria-sort` on their header cells.
- Resize handles are keyboard focusable separators with labels identifying the
  affected columns.
- File-name links retain normal link keyboard activation and focus treatment.
- In selection mode, the file name uses the established selectable-row
  behavior and communicates its selection action through its accessible label.
- Loading, empty, and folder-spanning rows retain correct `colspan` values
  after removal of the file action column.

## Error Handling

- Failed secret-value loads use the existing toast/error recovery behavior and
  do not expose or overwrite secret data.
- Failed downloads continue to report through the existing error toast.
- Local-storage read or parse failures fall back to defaults without blocking
  table rendering.
- File bulk-delete failures retain the current per-item result reporting and
  refresh behavior.

## Testing and Verification

Implementation follows test-driven development. Regression tests are added
before each production change and must fail for the missing behavior before
the implementation is written.

Coverage includes:

- blank versus stored expiration display and clear semantics;
- date-only `Updated` rendering;
- fixed 15-asterisk masks for ordinary and typed-record protected fields;
- Reveal/Hide state transitions and preservation of untouched masked values;
- sortable header markup, direction state, comparisons, and folder-preserving
  sort contracts;
- default widths, resize handles, keyboard resizing, local persistence, and
  invalid-persistence fallback;
- semantic authenticated file links;
- removal of file row actions and action-column `colspan` accounting;
- file deletion being available only through selection mode's top toolbar.

Final verification runs the embedded web UI tests, relevant API tests, Rust
formatting and lint checks, and the Tauri release bundle build. The packaged
app is smoke-tested with an isolated local vault so no real user vault is read
or modified.

## Out of Scope

- Replacing the Axum loopback transport with Tauri IPC.
- Adding a frontend framework or third-party data-grid library.
- Persisting sort direction across reloads.
- Changing backend file or secret APIs.
- Redesigning the drawer, folder model, upload flow, or bulk secret actions
  beyond the behaviors described above.
