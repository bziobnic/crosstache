# Search and Filter Task 1 Report

## Outcome

Implemented a privacy-bounded metadata search index, deterministic ranking,
composable secret/file filters, and accessible local search/filter controls.
The implementation operates only on the committed current-context list
snapshots and leaves request generations, abort ownership, stale-snapshot
recovery, exact context routing, and opaque folder-token persistence unchanged.

The follow-up review was also addressed:

- Secret summaries now expose the backend's canonical optional expiry timestamp
  without exposing secret values. The local, Azure, and test backends populate
  it; AWS truthfully reports no expiry metadata.
- The cache schema was bumped so older cached summaries cannot masquerade as
  expiry-aware rows.
- File upload-status filtering was removed because `FileInfo` has no persisted
  upload-status contract. File filters now use only folder and MIME type.
- Successful workspace and vault commits synchronously clear search queries,
  filters, chips, dynamic options, and old indexes. Pending and failed
  transitions preserve the current committed view.
- Empty-result guidance names only searchable/filterable metadata.

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
- A real PUT/list API contract test failed because list summaries did not
  include expiry metadata.
- Transition tests failed because committed context changes retained old
  queries, filters, chips, options, and index entries.
- Markup and filter-model tests failed because the UI offered a fabricated file
  upload-status dimension.

### GREEN

- The metadata index contains only names, folders, groups, record type, and
  file MIME type. It excludes values, notes, custom/internal tags, upload
  state, and prior queries.
- Search normalizes Unicode with NFKC and performs case-insensitive matching.
  It ranks exact leaf name, prefix, word boundary, substring, searchable terms,
  then folder, with stable name/surface/source tie-breaking.
- Secret filters compose folder, group, record type, expiry, and enabled
  metadata with AND semantics. File filters compose folder and MIME type.
- Expiry classification uses absolute instants and distinguishes expired,
  expiring, and absent timestamps.
- Search indexes are rebuilt once per successful list snapshot. Active search
  preserves relevance order; clearing search restores the existing table sort.
- Committed workspace/vault transitions synchronously discard old discovery
  state before new lists load; failed transitions leave it untouched.
- Existing folder selection remains the folder authority and is exposed as a
  removable filter chip. No raw context or folder path is newly persisted.
- Both surfaces expose visible search clears, `visible / total` counts,
  labelled filter control groups, removable chips, and clear-all actions.
- Mounted route coverage proves secret notes are not searchable, canonical
  expiry filters work, chips remove filters, file MIME search works, transition
  state is correctly committed or preserved, and existing
  context/folder/error races remain green.

## Verification

- `npm run test:unit` — 141 passed.
- `cargo test --features ui web::tests --lib` — 46 passed.
- `cargo test --features ui web::api::tests --lib` — 20 passed.
- `cargo test --features ui cache:: --lib` — 34 passed.
- `cargo check --features "ui aws" --lib` — passed.
- `env PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npx playwright test tests/web/ui-folders.spec.js tests/web/ui-accessibility.spec.js`
  — 10 passed on the approved host run. The first sandboxed run was unable to
  bind localhost; it did not reach application assertions.
- Full hermetic `cargo test --features ui --lib` — 1,045 passed, 4 failed,
  1 ignored. The four failures are existing project-config/context tests whose
  assumptions depend on the invoking workspace/environment; no changed search,
  filter, cache, list API, or backend surface failed.
- `cargo clippy --features ui --all-targets -- -D warnings` — passed.
- `cargo fmt --all --check` — passed.
- `git diff --check` — passed.

## Files

- Added `src/web/assets/commands.js` and its unit tests.
- Added `src/web/assets/files.js` for shared filter-control rendering.
- Extended `ui-model.js` and tests with pure composable filters/chip models.
- Integrated snapshot indexes and controls in `secrets.js`.
- Added mounted interaction/race coverage in `secrets.routes.test.js`.
- Added accessible markup and responsive control styling.
- Registered both new native modules in the embedded web asset router.
- Extended display-safe `SecretSummary` and backend adapters with optional
  canonical expiry metadata.
- Bumped the metadata cache schema and added legacy-cache rejection coverage.
