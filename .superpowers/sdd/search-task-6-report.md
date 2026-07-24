# Search/Upload/Responsive Task 6 Report

## Outcome

Implemented the approved ARIA tab, roving focus, and visible-selection
contracts without weakening the existing folder-tree keyboard model or
responsive focus handoff.

## RED → GREEN evidence

- New accessibility unit tests initially failed because
  `mountTabs`, `mountRovingFocus`, and `syncVisibleSelection` were not
  exported.
- The navigation Playwright test initially failed because desktop selection
  rows exposed both a checkbox and a second activation/select button.
- The responsive/filter focus test initially demonstrated that search focus
  correctly belongs to the search input after typed filtering; it was refined
  to assert both that focus contract and retained selection.
- A dedicated desktop file-selection test was run against the prior duplicate
  activation implementation and failed before the file row was corrected.
- The full browser run reproduced an older order-dependent command-palette
  focus failure. Diagnosis showed a same-context file-list publication made a
  built-in command stale between palette render and activation. A new unit
  test failed on that exact generation race before the registry rule was
  corrected.
- Remediation tests first showed that a committed capability loss could leave
  a hidden Files or Trash tab selected, keep its panel and prior-context rows
  in the DOM, and leave focus without a visible tab owner.
- A dynamic unit case then showed that hidden, disabled, and `aria-disabled`
  tabs were not all normalized after the selected tab became unavailable.
- The exact Rust web gate initially failed after the brittle reset-call count
  was replaced, exposing a second stale assertion for the guarded Files
  renderer. Both assertions now verify the relevant implementation paths.

## Implementation

- Added and exported:
  - `mountRovingFocus(container, selector)`
  - `mountTabs(tablist)`
  - `syncVisibleSelection({ visibleIds, selectedIds })`
- Mounted tabs at the application boundary and preserved the existing guarded
  tab activation callbacks and command shortcuts.
- Arrow Left/Right wrap and activate; Home/End activate boundaries; hidden and
  disabled tabs are skipped; one available tab remains in the tab order.
- Kept the established folder-tree roving focus, hierarchy, expansion,
  selection, and keyboard behavior unchanged and covered by its existing unit
  and browser gates.
- Desktop and stacked rows now expose one semantic activation control in
  normal mode, and exactly one checkbox with no competing row activation in
  selection mode.
- Header checkboxes now name the exact visible item count and expose
  `aria-checked="mixed"` for partial visible selection.
- Escape continues to dismiss the top modal/transient before it exits
  selection mode.
- Selection and focus survive responsive renderer replacement; filtering
  retains selection while correctly leaving focus in the active search
  control.
- Built-in commands remain valid across same-context list publication races,
  while metadata item results retain operation-generation staleness and all
  results still invalidate on context-version changes.
- Updated embedded-asset assertions to the current stacked-renderer and
  selection contracts so the Rust web gate checks the shipped implementation
  rather than obsolete pre-responsive source strings.
- Committed context capability changes now synchronously clear unavailable
  Files or Trash rows, selections, errors, protected-value state, and stale
  load ownership before the available fallback tab is exposed.
- Pending and failed context transitions retain the current tab and rendered
  snapshot; only a successful committed context applies capability changes.
- Tab synchronization now normalizes every tab and panel dynamically,
  activates and focuses an available fallback exactly once, and uses a
  reentrancy guard for click-driven application callbacks.
- The bulk-selection Rust assertion now checks the clear, reconciliation,
  row-checkbox, visible-checkbox, delete, and move paths directly rather than
  relying on a hard-coded source-text call count.

## Verification

- `npm run test:unit` — 179 passed.
- Full Playwright suite — 93 passed.
- Command/context race sequence — 14 passed.
- Final focused tab/selection regression — 5 passed.
- `cargo test --features ui --lib web::tests::` — 46 passed.
- Hermetic `cargo test --features ui --lib` — 1094 passed, 1 ignored.
- `cargo clippy --features ui --all-targets -- -D warnings` — passed.
- `cargo fmt --all -- --check` — passed.
- `git diff --check` — passed.

## Files

- `src/web/assets/accessibility.js`
- `src/web/assets/accessibility.test.js`
- `src/web/assets/app.js`
- `src/web/assets/commands.js`
- `src/web/assets/commands.test.js`
- `src/web/assets/index.html`
- `src/web/assets/secrets.js`
- `src/web/assets/style.css`
- `src/web/mod.rs`
- `tests/web/ui-accessibility.spec.js`
- `tests/web/ui-navigation.spec.js`
- `tests/web/ui-context.spec.js`
