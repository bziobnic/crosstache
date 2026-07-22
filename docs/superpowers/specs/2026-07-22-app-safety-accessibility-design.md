# App Safety and Accessibility Design

**Date:** 2026-07-22 · **Status:** Approved design

**Backlog coverage:** items 1–4 and the safety foundation for items 9 and 13

## Goal

Prevent silent edit loss, make the secret sheet fully operable by keyboard and
screen reader, make destructive actions recoverable and explicit, and bound the
exposure of copied and revealed values.

## Shared frontend foundation

This slice introduces `store.js`, `api-client.js`, `dialogs.js`,
`accessibility.js`, and the initial `secrets.js` extraction. Existing behavior
moves behind these boundaries before new behavior is added. The store exposes
subscribe, snapshot, and dispatch contracts. Feature modules may not mutate
another module’s DOM or state directly.

## Drafts and guarded navigation

Opening a secret captures a normalized baseline after metadata has loaded.
Normalization trims only fields whose server contract trims; it preserves
secret values, note whitespace, field ordering semantics, and the distinction
between absent and explicit clear. A draft becomes dirty only when its
normalized working state differs from its baseline.

The navigation guard handles:

- Cancel and close-button activation;
- Escape;
- backdrop activation;
- Secrets/Files/Trash tab changes;
- vault and workspace changes;
- opening another secret;
- browser `beforeunload`;
- Tauri window-close requests.

Dirty drafts show a modal with **Keep editing** as the initial focus and
**Discard changes** as the destructive action. Save-in-progress disables
context switching and closing. A cancelled navigation restores the exact draft
and focus. A completed close clears protected values from frontend state and
returns focus to the invoking control when it still exists.

## Modal sheet accessibility

The sheet uses `role="dialog"`, `aria-modal="true"`, and `aria-labelledby`.
Background content becomes inert; an `aria-hidden` fallback supports engines
without native `inert`. Focus moves to the first invalid field or otherwise to
the first editable field. Tab and Shift+Tab remain inside the sheet. Escape
uses the navigation guard. Nested confirmations return focus to their invoking
control. The phone layout is full-screen but keeps the same semantic contract.

Reveal and Copy controls are outside field labels. Protected inputs expose a
stable field label, protected/revealed state, and timer status without speaking
the secret value through live regions.

## Deletion and Trash

The context capability response includes soft-delete, restore, purge, and
scheduled-purge support. Secret deletion confirmation shows backend, vault,
targets, and whether recovery is available. Bulk confirmation lists up to five
names and summarizes overflow.

Recoverable deletion opens a durable action notice with Undo and places the
secret in the Trash tab. Trash lists deletion date and scheduled purge date
when available. Restore reports name conflicts and never overwrites a live
secret silently. Permanent purge requires the user to type the exact secret
name; bulk purge is deliberately not introduced.

File deletion and hard-delete-only backends explicitly say recovery is not
available. Their confirmations do not imply Trash support.

## Sensitive-value lifecycle

The effective clipboard timeout comes from existing Crosstache configuration,
with a presentation preference allowed to choose a shorter value but never a
longer value than a nonzero security policy. Copy identifies the field and
shows a countdown. At expiry the UI reads the clipboard and clears it only if
it still matches the value written by Crosstache. If safe verification is not
available, the notice states that automatic clearing could not be confirmed;
the UI never overwrites a potentially newer clipboard value.

Revealed values hide when their timer expires, when the document becomes
hidden, when focus leaves the app, when the sheet closes, or after a successful
save. Any interaction with the protected field resets its inactivity timer.
The default is the configured clipboard timeout, falling back to 30 seconds.

## Error behavior

Guard, reveal, copy, delete, restore, and purge errors remain in the relevant
modal or action notice. A failed destructive operation returns controls to a
retryable state without closing the sheet or clearing selection. Partial bulk
delete results identify each failed item and retain those items as selected.

## Acceptance evidence

- Unit tests prove draft normalization, dirty transitions, guard outcomes,
  timer resets, clipboard match checks, and delete capability decisions.
- Browser tests prove initial focus, focus containment, Escape, restoration,
  background inertness, accessible field names, window-close guarding, and the
  complete delete/Undo/Trash/restore/purge flow.
- Axum tests prove deleted listing, restore, purge, structured errors, and
  capability gating for local, Azure, AWS, and unsupported backends.
- Axe reports no serious or critical violations in list, sheet,
  confirmation, selection, and Trash states.
