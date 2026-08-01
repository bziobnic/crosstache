# Crosstache App UX Improvement Backlog

Reviewed: 2026-07-22

Scope: the current macOS desktop app and its embedded web UI, exercised with a
local backend, two vaults, five sample secrets across four folders, a typed
login record, and an encrypted file. The review covered the 820 × 560 desktop
minimum, the responsive web layout at 390 px, keyboard behavior, loading and
empty states, secret editing, vault switching, file management, and bulk
selection. The existing CLI UX review is outside this document's scope.

Priority definitions:

- **P0:** Prevent data loss or remove a serious accessibility blocker.
- **P1:** Fix a high-friction or high-risk core workflow.
- **P2:** Improve efficiency, clarity, and confidence in regular use.
- **P3:** Add polish after the core workflows are sound.

The recommendations below are in strict priority order.

## Implementation evidence status

The modernization implementation is traceable in the
[14-item evidence matrix](APP-UX-IMPLEMENTATION-EVIDENCE.md). All fourteen
items are implemented and the 29-task modernization backlog is complete. The
matrix distinguishes historical task evidence from the fresh final Rust,
JavaScript, Playwright, visual, and isolated package-smoke gates, and explicitly
identifies invalid-cloud recovery as mounted/renderer coverage rather than
claiming fresh packaged invalid-cloud launches.

## 1. P0 — Protect unsaved edits from every navigation path

**Observed:** The secret drawer can contain changed fields, but `Cancel`, a
vault switch, and app/window closure have no dirty-state warning. Switching
vaults immediately closes the drawer. In a live check, changing a note and then
switching vaults hid the drawer and discarded the edit without any prompt.

**Improve:** Track the initial form state and mark the drawer dirty only after a
meaningful change. Before closing the drawer, switching vaults or tabs, or
closing the window, offer **Keep editing** and **Discard changes**. Keep the
current draft intact when a navigation is cancelled. During save, lock context
switching and identify the destination in the progress state, for example
“Saving to `local / poc`…”.

**Done when:** No action can silently discard a changed form, and unchanged
forms still close without an unnecessary prompt.

## 2. P0 — Make the secret drawer a complete keyboard and screen-reader flow

**Observed:** Opening the drawer leaves focus on the table row behind it. The
drawer has no dialog semantics, focus containment, `Escape` handling, backdrop,
or focus restoration. Background controls remain reachable. The value field's
accessible name also absorbs its nested Reveal and Copy buttons (“Value
Protected Reveal Copy”).

**Improve:** Treat the drawer as a modal dialog: add `role="dialog"`,
`aria-modal="true"`, a labelled title, a close button, and an inert or obscured
background. Move focus to the first useful field, trap focus while open, close
on `Escape` subject to the unsaved-change guard, and restore focus to the
invoking row or button. Move Reveal and Copy outside the value's `<label>` and
give every protected field an unambiguous name and state.

**Done when:** The entire create/edit/delete flow works without a pointer and
passes a screen-reader walkthrough with predictable focus announcements.

## 3. P1 — Add safe deletion, Trash, and Undo

**Observed:** Delete is confirmed by changing the same button to “Really
delete?” for three seconds. It does not name the secret, vault, or recovery
behavior. Bulk confirmation shows only a count. The backends already expose
soft-delete and restore capabilities, but the app does not expose a trash view
or undo action.

**Improve:** Use an explicit confirmation surface that states exactly what will
be deleted and from which vault. For bulk actions, show the selected names with
overflow handling. After a soft delete, show a durable **Undo** action and add a
Trash view with restore and clearly separated permanent purge. Reserve
type-to-confirm for genuinely irreversible purge operations.

**Done when:** Users can distinguish recoverable delete from permanent purge,
can verify the target before acting, and can recover an accidental deletion in
the app.

## 4. P1 — Make copied and revealed secret values visibly time-bounded

**Observed:** Copying a value produces only a generic four-second “copied”
toast. The copied value remains on the system clipboard, unlike the CLI's
auto-clear behavior. Revealed values remain visible until manually hidden or
the drawer closes, with no timer or visible exposure state.

**Improve:** Match the CLI safety model: say which field was copied, show a
countdown, and clear the clipboard after the configured interval if it still
contains the copied value. Auto-hide revealed values after inactivity and when
the app loses focus. Offer a user setting for the timeout, with a conservative
default. Never replace a newer clipboard value.

**Done when:** The UI explains how long sensitive material remains exposed and
clears only the value it placed on the clipboard.

## 5. P1 — Add first-run setup and actionable startup recovery

**Observed:** The desktop app assumes a valid global `xv` configuration. Its
startup screen can only change from “Opening your vault” to a raw failure
message; it offers no retry, configuration, backend selection, or repair path.

**Improve:** Add a lightweight first-run flow that can create a local vault or
connect Azure/AWS, explains where data will live, and verifies access before
opening the main UI. Turn startup failures into a recovery screen with **Retry**,
**Choose backend**, **Open configuration**, and **Copy diagnostics**. Preserve a
CLI-oriented advanced path without requiring new users to know it first.

**Done when:** A user starting the desktop app with no valid configuration can
reach a working vault without leaving the app, and an experienced user can
diagnose a broken configuration from the same screen.

## 6. P1 — Make backend, vault, project, and environment scope unmistakable

**Observed:** The active backend is a small badge and the vault picker contains
bare vault names. The app does not show the active project/environment or the
multi-backend workspace model. Destructive actions repeat neither backend nor
vault context.

**Improve:** Present a persistent context line such as
`local / poc · project: crosstache · env: dev`, with tooltips explaining where
each value came from. Group vault choices by backend/account where applicable,
show connection state and permissions, and repeat the destination on create,
move, upload, and delete confirmations. Add workspace-aware switching rather
than presenting only the active backend's vaults.

**Done when:** A user can answer “where will this action happen?” without
opening another screen or inferring it from configuration.

## 7. P1 — Stop hiding most content behind collapsed folders by default

**Observed:** Every folder begins collapsed, so the populated test vault showed
five secrets but only one secret row. Four folder rows accounted for the rest.
Expansion state is memory-only and is cleared on every vault switch. Nested
paths such as `production/database` are displayed as flat folder labels.

**Improve:** Expand folders by default for small vaults, persist each vault's
expansion state, and provide **Expand all** / **Collapse all**. For larger
vaults, use a true hierarchical folder tree or a folder filter beside the
content list. Make the total and currently visible item counts distinct.

**Done when:** Opening a typical vault immediately exposes its contents, nested
folders communicate hierarchy, and users retain control of density.

## 8. P1 — Turn the secret drawer into a guided, typed editor

**Observed:** Type is a plain select with names but no descriptions and cannot
be changed after creation without explanation. Folder is free text, Groups is
comma-separated text, and Expires is a patterned text field. Rename is mixed
into the general save operation. Validation is mostly browser-native or arrives
as a transient API error.

**Improve:** Show type descriptions and representative fields before selection;
explain why an existing type is locked and provide an intentional conversion
workflow. Use folder autocomplete, group chips with existing-value suggestions,
a real date picker with clear/no-expiry states, and inline validation. Separate
Rename from Save metadata so collisions and consequences can be checked before
other changes are sent. Show field-level help for required and protected data.

**Done when:** A new user can create a correct plain or typed secret without
knowing Crosstache's tag conventions or input syntax.

## 9. P1 — Keep errors visible, contextual, and recoverable

**Observed:** Most failures appear in a toast that disappears after four
seconds. Failed list states say the vault could not be read but offer no Retry.
Form errors are not anchored to the relevant field, and bulk partial failures
can become a long, temporary toast.

**Improve:** Put errors beside the control or surface that failed, preserve the
user's inputs, and include a direct next action. Add Retry to failed lists and
uploads. For partial bulk results, show a persistent summary with succeeded and
failed items plus **Retry failed** and **Copy details**. Translate backend
messages into plain language while keeping diagnostics expandable.

**Done when:** Every failure answers what happened, what was affected, and what
the user can do next without relying on a disappearing notification.

## 10. P2 — Provide faster find, filter, and command workflows

**Observed:** Secrets have a single substring search; files have no search.
There are no filter chips for folder, group, type, expiry, or status and no
global search across vault content. Search does correctly force matching folder
contents open, but the UI does not make active filtering especially visible.

**Improve:** Add a keyboard-first command/search surface (`Cmd/Ctrl+K`) that can
find secrets and files, switch vaults, and start common actions. Add structured
filters, a prominent clear-filter action, result counts, and file search. Useful
shortcuts include `/` for search, `Cmd/Ctrl+N` for a new secret, and `Escape` to
leave selection mode.

**Done when:** A regular user can reach a known item or common action with a few
keystrokes, even in a large multi-folder vault.

## 11. P2 — Make uploads a managed queue instead of a sequence of toasts

**Observed:** Files upload sequentially, the 100 MB request limit is not stated,
and each success toast replaces the previous one. There is no per-file progress,
cancel, retry, conflict/replace choice, or aggregate completion summary. Folder
paths depend on the uploaded filename and files cannot be searched.

**Improve:** Show accepted limits before selection and use an upload queue with
per-file status, progress, cancel, retry, and a final summary. Ask how to handle
name conflicts before transfer. Let users choose a destination folder and make
the filename itself a clearly labelled Download action, with optional metadata
or preview when supported.

**Done when:** A multi-file upload remains understandable from selection through
completion and individual failures do not hide successful results.

## 12. P2 — Replace the narrow table with a responsive content pattern

**Observed:** At 390 px the secrets table still retains Name, Folder, and
Updated columns, leaving long identifiers and folder paths heavily truncated.
The desktop app's 820 px minimum also prevents its web breakpoints from being
exercised inside the native shell. Column resizing is powerful on desktop but
adds many keyboard stops and does not solve narrow layouts.

**Improve:** Use a priority-column or stacked-row pattern below the desktop
breakpoint: keep the secret/file name fully legible, move metadata to a second
line or detail sheet, and remove resize handles where columns are hidden. Test
the native window's minimum against the same breakpoints as the web UI and add
automated snapshots for desktop minimum, tablet, and phone widths.

**Done when:** Names remain identifiable without horizontal scrolling at every
supported width and the native and browser layouts share deliberate breakpoints.

## 13. P2 — Correct navigation and selection semantics

**Observed:** Secrets and Files are visually tabs but are exposed as ordinary
buttons with no `tablist`, `tab`, `aria-selected`, or panel relationships. In
selection mode, a row can expose both a checkbox and a name button with the same
accessible label. Folder rows are table cells repurposed as buttons.

**Improve:** Implement the ARIA tabs pattern with arrow-key navigation and
associated tab panels. Give selection rows one clear selection affordance and
make row activation semantics consistent. Keep sortable headers and column
resizers, but remove redundant focus stops at breakpoints where columns are
hidden. Validate the final patterns with keyboard and screen-reader testing,
not markup checks alone.

**Done when:** Interactive elements announce one role and one purpose, and focus
order matches the visual order in browse and selection modes.

## 14. P3 — Strengthen product identity and information hierarchy

**Observed:** The visual system is clean and restrained, with solid light/dark
support, but the main screens spend substantial space repeating generic safety
copy while high-value context and actions stay small. The interface could belong
to many administrative table tools and does little to communicate Crosstache's
cross-backend strength.

**Improve:** Make vault context—not decorative branding—the signature element:
use a compact, recognizable context rail that encodes backend, vault, project,
and security state. Tighten repeated headings and safety copy after first use so
more content fits above the fold. Add an explicit theme choice, app version,
keyboard shortcut reference, and a small security/session explanation under
Help or About.

**Done when:** The app is immediately recognizable as a cross-backend secrets
workspace and its most important context is more prominent than generic prose.

## Suggested delivery sequence

1. **Safety and access:** items 1–4.
2. **Onboarding and core information architecture:** items 5–9.
3. **Power-user and scale workflows:** items 10–13.
4. **Visual and product polish:** item 14.
