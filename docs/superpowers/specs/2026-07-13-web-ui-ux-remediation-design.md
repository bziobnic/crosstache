# Web UI UX Remediation Design

**Date:** 2026-07-13<br>
**Status:** Approved

## Goal

Make `xv ui` resilient to browser reloads and overlapping asynchronous actions,
and make destructive file and secret actions predictable and accessible.

## Scope

This change remediates five confirmed concerns in the embedded web UI:

1. Reloading after the token is removed from the URL leaves the UI unauthorized.
2. Out-of-order vault loads can display data from a previously selected vault.
3. Out-of-order secret drawer loads can populate one secret's metadata while a
   different secret remains the active save/delete target.
4. The secret Delete button can remain armed when the drawer is closed and
   reopened for another secret.
5. File deletion is immediate, while file action buttons have ambiguous
   icon-only accessible names.

The server authentication model, loopback-only binding, API routes, and backend
behavior remain unchanged.

## Design

### Reload-safe session authentication

On initial navigation, the client reads `token` from the query string. When it
is present, the client writes the token to `sessionStorage` under a namespaced
key and removes it from the visible URL as it does today. On reload, the client
falls back to that per-tab session value.

The token is not written to `localStorage`, cookies, or disk by application
code. Closing the tab ends the browser session's access. If neither the query
string nor `sessionStorage` contains a token, initialization stops and the page
shows a persistent recovery message telling the user to reopen the tokenized
URL printed by `xv ui`; it must not degrade into an empty UI with a four-second
toast.

### Latest-operation-wins async state

Secret-list loads, file-list loads, and drawer loads each receive an independent
monotonically increasing operation ID. A completion may update shared state or
the DOM only if its ID is still current.

Vault selection captures the selected vault before starting both list loads.
Each request builds its query string from that captured value rather than from
the mutable global `currentVault`. Switching vaults invalidates older list and
drawer operations before issuing new requests. Stale failures are ignored so
they cannot replace a newer successful view with an error toast or placeholder.

Opening or closing the drawer invalidates the previous drawer operation. A
drawer response checks its ID before every shared-state or form update. Thus an
older secret response cannot populate or target a newer selection.

The implementation uses operation IDs rather than request cancellation.
Cancellation can be added later as an optimization, but correctness must not
depend on network cancellation timing.

### Consistent destructive actions

A small helper owns two-click destructive confirmation for reusable buttons.
Its responsibilities are to arm a button, change its visible label, expire the
armed state after three seconds, and fully reset the button and timer.

Secret drawer open and close always reset the Delete button. File deletion uses
the same two-click pattern, scoped to the individual file button, before making
the API call. Successful and failed operations also reset their relevant
button state.

The file action buttons use visible text labels (`Download` and `Delete`) rather
than glyphs alone. Their accessible names therefore describe the action without
depending on tooltip support. File deletion remains a deliberate two-click
action and does not use a blocking browser dialog.

## Error and interaction behavior

- A missing session token produces a persistent in-page recovery state.
- A current request failure retains the existing toast behavior and failed
  placeholder where applicable.
- A stale request success or failure produces no visible change.
- Save and delete operations continue to target the vault and secret captured
  by the current UI state; stale list or drawer content cannot become current.
- Confirmation expiry or drawer transitions restore the original button label
  and unarmed state.

## Testing

Implementation proceeds test-first.

Rust tests around the embedded assets will lock the authentication recovery and
accessible action contracts before production changes. Behavioral verification
will run the real embedded UI against a hermetic local backend and exercise:

- initial tokenized load followed by browser reload;
- delayed, reordered vault list responses;
- delayed, reordered drawer responses;
- arming secret deletion, closing, and opening another secret;
- file action accessible names and two-click deletion.

The final gates are `cargo fmt --check`,
`cargo clippy --features ui --all-targets`, and `cargo test --features ui`.

## Non-goals

- Sharing a live UI session across tabs or browser restarts.
- Persisting the bearer token beyond `sessionStorage`.
- Adding a JavaScript bundler, frontend framework, or production dependency.
- Changing backend delete semantics or adding an undo/restore API.
