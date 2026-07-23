# Context/Workflows Task 6 Report

## Outcome

Implemented the guided typed secret editor and integrated it with the current
Task 5 conversion and atomic rename contracts.

- New-secret creation starts with an accessible Plain card followed by resolved
  type cards. Selecting a card renders only that type's fields.
- Field help identifies required/optional, protected/metadata, and primary
  semantics. Protected fields retain the existing masking, reveal, copy, timer,
  and draft-safety behavior.
- Existing secrets display their current type. Conversion and rename are
  separate nested workflows and are shown only when the effective context
  advertises the corresponding guarded capability.
- Conversion uses the current tagged target body, preserves supplied fields
  through re-preview, shows a value-free impact summary, requires a preview
  revision, and explicitly confirms apply.
- Rename uses the current atomic `/rename` route; the primary name field is
  read-only while editing so saving metadata cannot implicitly rename.
- Groups use removable chips with existing-group suggestions. Folders use
  autocomplete from current folder paths. Expiry uses a native date control
  with No expiry and Clear actions.
- Typed saves preserve custom tags, enabled state, not-before state, and
  untouched protected fields.
- Structured field errors remain durable, add `aria-invalid` and a description
  link without clearing the draft, and focus ordinary, conversion, or nested
  rename controls as appropriate.
- The layout collapses cards and workflow controls responsively at 48rem.

## TDD Evidence

1. Added `typed-editor.test.js` first and ran it before model implementation.
   It failed with the expected module `SyntaxError` because
   `buildTypedDraft` was not exported.
2. Implemented the minimal `typeCards`, `buildTypedDraft`,
   `groupSuggestions`, and `conversionSummary` model and reached 5/5 green.
3. Added the Playwright acceptance suite before markup/behavior changes.
   The focused guided-card test failed because the Plain radio did not exist.
4. Implemented the UI in slices and repeatedly ran the focused browser suite.
5. Added a supplied-conversion-field persistence assertion. It failed because
   successful preview cleared the generated field; the preview renderer was
   fixed to retain it through confirmation.
6. Added a nested rename error-focus assertion. It failed because `field:
   "name"` focused the read-only primary name; error mapping was fixed to focus
   the active rename input.
7. Review remediation began with three focused failing browser assertions:
   an older login preview replaced a newer API-key preview, a required
   conversion password was rendered without protected-field lifecycle state,
   and changing the conversion target did not participate in the Escape
   discard guard.
8. Added failing race, immutable-request, protected-lifecycle, apply-failure,
   close, and context-switch cases before hardening the workflow. Conversion
   previews now capture the complete request and drawer/context generations;
   stale responses are ignored and apply reuses the exact successful snapshot.
9. Supplied secret fields now use the same masking, reveal, copy, expiry, and
   scrubbing lifecycle as other protected values. They are scrubbed on target
   changes, failures, closes, successful completion, and context changes.
10. Rename and conversion inputs are now part of the drawer draft, so Escape,
    close, and context navigation consistently offer Keep editing or Discard.

## Verification

- `node --test src/web/assets/*.test.js`: **110 passed**
- `node --test src/web/assets/typed-editor.test.js src/web/assets/secrets.routes.test.js`:
  **27 passed**
- `cargo test --features ui web:: --lib`: **143 passed**
- `cargo clippy --features ui --all-targets -- -D warnings`: **passed**
- `cargo fmt -- --check`: **passed**
- `cargo check --features ui`: **passed**
- `git diff --check`: **passed**
- `npx playwright test tests/web/ui-typed-editor.spec.js`: **9 passed**
- `npx playwright test`: **32 passed**
- Axe scans in new-secret metadata controls, open conversion preview, and
  rename-error states: **no serious or critical violations**

The first hermetic Rust test attempt failed before tests ran because changing
`HOME` hid rustup's configured toolchain. It was rerun with explicit
`RUSTUP_HOME` and `CARGO_HOME`; the web suite then ran normally.

## Deviations from the older plan examples

- The current Task 5 API uses
  `target: {kind: "typed", target_type: ...} | {kind: "plain"}`,
  `source_revision`, `conditional_conversion`, and `atomic_rename`. The UI uses
  these current contracts instead of the older `target_type`-only and `/move`
  examples.
- Updated existing static and protected-value assertions to cover the new
  native date input and the richer help + protection + live-status
  `aria-describedby` chain. The assertions were strengthened rather than
  removed.
- No backend behavior was changed in this task.
