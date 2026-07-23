# Search/Upload/Responsive Task 1 Report

## Outcome

Implemented a privacy-bounded metadata search index, deterministic ranking,
composable secret/file filters, and accessible local search/filter controls.
The implementation operates only on the current loaded list snapshots and
leaves request generations, abort ownership, stale-snapshot recovery, exact
context routing, and opaque folder-token persistence unchanged.

## TDD evidence

### RED

- `node --test src/web/assets/commands.test.js src/web/assets/ui-model.test.js`
  failed because `commands.js`, `filterSecrets`, and `filterFiles` did not
  exist.
- The markup/control test then failed because local clear actions and labelled
  filter/chip groups did not exist.
- `cargo test --features ui web::tests::ui_serves_native_module_graph --lib`
  failed with `missing /commands.js`, proving the new native modules were not
  yet served.
- A file-path ranking test failed because a folder prefix was incorrectly
  treated as a leaf-name prefix.

### GREEN

- The metadata index contains only names, folders, groups, record type, and
  file MIME type. It excludes values, notes, custom/internal tags, upload
  state, and prior queries.
- Search normalizes Unicode with NFKC and performs case-insensitive matching.
  It ranks exact leaf name, prefix, word boundary, substring, searchable terms,
  then folder, with stable name/surface/source tie-breaking.
- Secret filters compose folder, group, record type, expiry, and enabled
  metadata with AND semantics. File filters compose folder, MIME type, and
  upload status.
- Search indexes are rebuilt once per successful list snapshot. Active search
  preserves relevance order; clearing search restores the existing table sort.
- Existing folder selection remains the folder authority and is exposed as a
  removable filter chip. No raw context or folder path is newly persisted.
- Both surfaces expose visible search clears, `visible / total` counts,
  labelled filter control groups, removable chips, and clear-all actions.
- Mounted route coverage proves secret notes are not searchable, status
  filters work, chips remove filters, file MIME search works, and existing
  context/folder/error races remain green.

## Verification

- `npm run test:unit` — 136 passed.
- `cargo test --features ui web::tests --lib` — 46 passed.
- `env PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npx playwright test tests/web/ui-folders.spec.js tests/web/ui-accessibility.spec.js`
  — 9 passed on the approved host run. The first sandboxed run was unable to
  bind localhost; it did not reach application assertions.
- `cargo clippy --features ui --all-targets -- -D warnings` — passed.
- `cargo fmt --check` — passed.
- `git diff --check` — passed.

## Files

- Added `src/web/assets/commands.js` and its unit tests.
- Added `src/web/assets/files.js` for shared filter-control rendering.
- Extended `ui-model.js` and tests with pure composable filters/chip models.
- Integrated snapshot indexes and controls in `secrets.js`.
- Added mounted interaction/race coverage in `secrets.routes.test.js`.
- Added accessible markup and responsive control styling.
- Registered both new native modules in the embedded web asset router.
