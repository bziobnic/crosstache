# Search / Upload / Responsive Task 5 Report

## Outcome

Implemented the responsive content pattern for secret and file lists:

- Added the pure `contentMode(width)` boundary: table above 768 px and stacked
  at 768 px and below.
- Added one shared `contentRows` view model for secrets and files. It retains
  the complete identifier, folder grouping, source record, and ordered
  display-safe metadata without mutating source data.
- Preserved the existing semantic desktop tables, sorting, persisted column
  widths, pointer resizing, and keyboard resizing above the breakpoint.
- Added separate semantic stacked renderers below the breakpoint. Each item is
  one list item with the full identifier first, priority metadata second, and
  one clear Edit, Download, or Select activation control.
- Added deterministic semantic folder sections: one real heading and labelled
  list per folder, Unfiled first, folders collated consistently, and the
  current row sort retained within each group.
- Added full-width folder headings and untruncated, wrapping identifiers and
  metadata.
- Kept selection checkboxes out of non-selection mode. In selection mode the
  checkbox is the sole actionable control and visible identifier/metadata
  remain inert labelled and described content.
- Built accessible control names from a hidden action verb plus the visible
  complete identifier, and linked visible priority metadata as the accessible
  description. No `aria-label` masks visible descendants.
- Hid the complete desktop table in stacked mode, removing its sort controls,
  columns, and resize separators from the accessibility tree and focus order.
- Preserved loading, empty, filtered, and failed list states in both renderers.
  Narrow empty lists retain their New secret or Browse files action.
- Made narrow toolbars wrap without horizontal scrolling and made create/edit
  drawers and folder sheets full-screen below 544 px.
- Lowered and pinned the Tauri window minimum width to 390 px so the approved
  768 px tablet and 390 px phone layouts are exercisable.

## TDD evidence

The first unit run failed because `contentMode` and `contentRows` did not exist:

- `content mode changes at the approved breakpoint` — `model.contentMode is
  not a function`.
- `responsive content rows preserve complete identifiers and priority
  metadata` — `model.contentRows is not a function`.

The initial browser run was blocked by the sandbox's loopback restriction.
After rerunning with approved host access, the initial five responsive browser
scenarios passed. A subsequent narrow empty-list regression test was written
and observed failing because the empty stacked surface had no visible state or
action. The implementation then introduced a shared loading/empty/filtered/
failed renderer, and the regression passed.

Review remediation was also test-first:

- `stacked groups use one deterministic heading per folder...` failed because
  `groupContentRows` did not exist.
- The updated selection test failed because the checkbox had no metadata
  description and exposed a second Select button.
- Expanded state coverage found a critical axe violation where a failed-state
  `listitem` lacked a list parent. State content now uses ordinary semantics
  and loading skeletons are hidden from assistive technology.

## Verification

- `node --test src/web/assets/ui-model.test.js` — 38 passed.
- `node --test src/web/assets/*.test.js` — 174 passed.
- `env PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npx playwright test tests/web/ui-responsive.spec.js --reporter=dot`
  — 10 passed.
- Responsive plus accessibility, folder, command, and upload Playwright
  regression group — 47 passed.
- `cargo test -p xv-desktop` — 2 passed.
- `cargo fmt --all -- --check` — passed.
- `cargo clippy --features ui --all-targets -- -D warnings` — passed.
- `git diff --check` — passed.

## Files

- `src/web/assets/ui-model.js`
- `src/web/assets/ui-model.test.js`
- `src/web/assets/secrets.js`
- `src/web/assets/index.html`
- `src/web/assets/style.css`
- `desktop/src-tauri/tauri.conf.json`
- `tests/web/ui-responsive.spec.js`
