# Context/Workflows Task 7 Report

## Outcome

Implemented durable, context-safe failure handling for list refreshes, forms,
workspace activation, and bulk operations.

- API requests publish `started`, `succeeded`, `cancelled`, or `failed` with a
  stable `operationId`.
- Logical workspace and bulk operations publish the same vocabulary, with
  `partially-succeeded` reserved for mixed aggregate results.
- Superseded secret and file reads are aborted, and generation checks continue
  to ignore late completions.
- A refresh failure retains the last successful list snapshot and displays a
  persistent Stale marker with Retry. Retry remains visible while the retry is
  pending and clears only after recovery or explicit dismissal.
- Partial bulk results retain Retry failed and Copy details. Retry is restricted
  to the captured backend/vault scope.
- Diagnostic DTOs whitelist only code, safe message, hint, backend, vault, and
  failed names. Values, notes, authentication material, request headers, and
  raw error details are excluded.
- Form failures continue to preserve the central draft and focus/describe the
  server-named field.
- Error controls wrap responsively and retain existing accessible alert
  semantics.

## Review remediation

- Split per-kind refresh ownership (`secret-refresh-error` and
  `file-refresh-error`) from action/bulk ownership. Refresh and partial-result
  failures now coexist without sharing handlers or clearing each other.
- Added keyed owner generations. Replacement and dismissal null Retry, Copy,
  and Dismiss handlers, release captured retry/diagnostic/scope/name state, and
  invalidate late completions.
- Retry-time list loads carry the owning generation so a failure arriving
  after dismissal cannot reopen the panel. Bulk retry completions make the same
  owner check before rendering replacement results.
- Routine terminal operations are capped at 50 with 100 bounded terminal
  tombstones. Active operations and actionable durable logical failures are
  retained, double terminal events are ignored even after dismissal, and
  dismissal removes durable diagnostic/action state.
- Added browser race coverage for bulk-then-refresh, refresh-then-bulk, refresh
  failure while bulk deletion is gated, bulk retry dismissal, and stale retry
  dismissal. Added store coverage for caps, terminal observability, double
  terminals, durable dismissal, owner replacement, handler cleanup, and
  generation invalidation.
- Committed scope transitions synchronously abort and invalidate both list
  loads, release both refresh owners, clear diagnostics and handlers, and
  restore Retry controls to an enabled baseline before any new-scope load
  starts. This applies at workspace/context commit and to the legacy vault
  selector's immediate commit.
- List retry closures capture the original vault and cloned scope that failed;
  they never retarget through the current vault. A delayed new-vault file-load
  unit test proves both stale refresh owners are already inert before the new
  load settles.
- Generic and bulk retries share an owner-generation-aware binder. Replacement
  initializes the new control as enabled; duplicate clicks are blocked; and a
  superseded retry cannot publish, reject, or re-enable the current owner's
  pending control.
- Added a Playwright workspace-transition scenario that delays the new-scope
  file request and verifies synchronous owner/handler/disabled-state cleanup
  plus inert saved old-scope retry closures. It is committed as browser
  coverage but could not be executed because the previously reported browser
  approval/usage quota remains exhausted.
- A pending workspace activation no longer clears or aborts current-scope list
  state. Existing secret and file stale warnings, diagnostics, generations, and
  retry handlers remain owned while the context-scoped UI is inert. Failed and
  activity-revision-cancelled activation restores normal interactivity with
  those exact owners intact.
- Current-scope refreshes continue settling while activation is pending. A unit
  race rejects activation before the old-scope refresh fails, then proves the
  retained snapshot never enters loading limbo and the resulting stale warning
  retries successfully. Successful activation still clears both owners at the
  atomic commit before its delayed new-scope file load settles.
- Added browser coverage for failed activation owner preservation, in-flight
  refresh recovery after activation failure, pending owner preservation, and
  successful commit cleanup. Unit coverage additionally exercises explicit
  activation cancellation.

## TDD evidence

RED was observed before implementation:

- `node --test src/web/assets/api-client.test.js src/web/assets/store.test.js`
  failed because operation callbacks and safe event/diagnostic helpers did not
  exist.
- `node --test src/web/assets/context.test.js` failed because workspace
  activation did not publish operation lifecycle events.
- `npx playwright test tests/web/ui-errors.spec.js` failed because refresh
  discarded rows and bulk partial results had neither Retry failed nor Copy
  details.
- The browser cancellation assertion failed until cancelled bulk confirmation
  published a terminal `cancelled` status.
- The transition lifecycle tests failed while `context/switch-started` cleared
  both refresh panels and invalidated the in-flight current-scope refresh.
  Moving cleanup exclusively to `context/switch-succeeded` made the failure,
  cancellation, in-flight recovery, and successful commit cases pass.

Each focused suite was rerun GREEN before broad verification.

## Verification

- `cargo fmt --check` — PASS
- `cargo clippy --features ui --all-targets -- -D warnings` — PASS
- `cargo test --features ui web:: --lib` — PASS, 143 tests
- `cargo test --features ui --test e2e_record_types` — PASS, 71 tests
- `node --test src/web/assets/*.test.js` — PASS, 124 tests
- `PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npx playwright test tests/web/ui-errors.spec.js`
  — PASS, 6 remediation tests
- The complete four-file Playwright plan gate passed 31 tests before the review
  remediation. Its final 35-test rerun was requested but rejected by the
  escalation reviewer because the browser approval/usage quota was exhausted.
  No workaround or unapproved browser launch was attempted.
- Axe reported no serious or critical violations in the stale, partial-result,
  context rail, folder tree, editor, conversion, or rename states covered by
  the plan gate.
- `git diff --check` — PASS

The record-type test target emitted its existing non-fatal dead-code warnings;
the strict all-target clippy gate remained warning-free.

The remediation raises the JavaScript total to 124 tests. The focused error
suite now contains 9 tests; its first 6 tests passed before browser execution
became unavailable, and the 3 new transition lifecycle tests remain unexecuted
solely because of the external browser quota. Playwright discovery and syntax
validation pass for all 9.
