# Crosstache App UX Modernization Design

**Date:** 2026-07-22 · **Status:** Approved design

## Goal

Implement every recommendation and every “Done when” condition in
`docs/APP-UX-IMPROVEMENTS.md` for the shared embedded web UI and the macOS
desktop shell without duplicating Crosstache domain behavior or introducing a
production frontend build pipeline.

## Approved product decisions

- Evolve the existing dependency-free embedded frontend rather than adopting a
  production JavaScript framework or replacing Axum with Tauri IPC.
- Use the same embedded interface for `xv ui` and the Tauri desktop app.
- Permit desktop onboarding to write backend configuration through shared Rust
  setup services. Keep `xv ui` read-only with respect to backend configuration.
- Permit both shells to persist presentation-only preferences in a dedicated,
  non-secret UI preference store.
- Use the approved **vault workspace** layout: persistent context rail,
  hierarchical folder navigation, and one primary content surface.
- Use stacked item rows on narrow screens rather than a squeezed table.
- Use a full-height right-side modal sheet for create/edit flows; it becomes a
  full-screen sheet on phones.
- Retain the forest-green product palette, but make effective context the
  signature visual element instead of decorative branding.

## Project charter compliance

This design follows `docs/design-principles.md`:

- Domain operations continue to live behind existing backend traits and shared
  Rust services. Frontends do not recreate validation, configuration
  precedence, record conversion, deletion, restore, or upload rules.
- Trash, restore, purge, record conversion, configuration setup, and preference
  outcomes either already have CLI equivalents or remain presentation-only.
- Backend capability differences are explicit in API responses and UI copy.
- Destructive actions identify backend, vault, target, recoverability, partial
  completion, and remediation.
- Context displays backend, vault, project, environment, source, connection
  state, and applicable capability limitations.

No charter exception is required.

## Architecture

The application keeps one Rust behavioral core and two local presentation
shells:

```text
shared config/backend/record/file services
                 │
        ┌────────┴────────┐
        │                 │
  Axum loopback API   desktop setup adapter
        │                 │
        └──── shared embedded UI ────┘
```

The browser UI never shells out to `xv`. Axum handlers and Tauri setup commands
call the same Rust services used by CLI commands.

### Frontend modules

The current `src/web/assets/app.js` is decomposed into dependency-free modules:

- `app.js`: bootstrap, module wiring, and initial route.
- `api-client.js`: authenticated requests, cancellation, structured errors,
  downloads, and XMLHttpRequest-based upload transport with real progress.
- `store.js`: authoritative UI state, immutable snapshots, and subscriptions.
- `dialogs.js`: modal lifecycle, focus containment, confirmations, and guarded
  navigation.
- `secrets.js`: secret list, Trash, typed editor, rename, record conversion,
  protected values, and destructive actions.
- `files.js`: file list, search, upload queue, conflict resolution, progress,
  cancellation, retry, and download.
- `commands.js`: command palette, action registry, and keyboard shortcuts.
- `preferences.js`: theme, exposure timeout, folder expansion, table widths,
  density, and persisted UI settings.
- `accessibility.js`: ARIA tabs, roving focus, live announcements, inert
  background behavior, and responsive semantics.
- `ui-model.js`: pure formatting, sorting, filtering, and view-model helpers.

All modules are native browser ES modules embedded with `include_str!` and
served as same-origin assets. There is no production Node/Vite build.

### Rust modules

The web layer is split by responsibility while preserving thin handlers:

- context: effective backend/vault/project/environment/workspace and
  capabilities;
- secrets: metadata, value reveal, record conversion, rename, delete, deleted
  listing, restore, and purge;
- files: listing, download, upload preflight, upload, conflict handling, and
  delete;
- preferences: load/save non-secret UI preferences;
- errors: stable structured API errors;
- setup: desktop-only adapter over shared configuration initialization.

## State and data-flow contract

The frontend store is the sole source of truth for visible UI state. Views
render from snapshots. Asynchronous work dispatches explicit started,
succeeded, partially-succeeded, cancelled, and failed events. Existing
generation guards remain; `AbortController` additionally cancels obsolete
reads and uploads when safe.

Secret drafts contain an immutable normalized baseline and a normalized working
copy. Every close, tab switch, vault switch, window close, and competing edit
passes through one navigation guard. Dirty drafts offer **Keep editing** and
**Discard changes**. Clean drafts close without a prompt. Saving locks context
switching and names the target backend and vault in progress copy.

Presentation preferences are stored in a versioned `ui.json` under the normal
Crosstache configuration directory. The file contains no secret names, values,
notes, search history, or other vault data. Backend configuration remains in
its existing files and is not writable through `xv ui`.

## Information architecture

The primary navigation is an ARIA tab set containing **Secrets**, **Files**,
and **Trash**. A persistent context rail shows:

- backend and backend kind;
- active vault and attached workspace alias where applicable;
- project name/path summary and active environment;
- connection state and capability limitations;
- command palette, Help, Settings, theme, and app version entry points.

Desktop uses a hierarchical folder sidebar. Vaults with 50 or fewer items
expand folders by default. Larger vaults restore per-vault expansion state and
otherwise begin collapsed. Expand all and Collapse all remain available at all
sizes. Narrow screens replace the sidebar with filter sheets and stacked item
rows whose primary identifiers never ellipsize into ambiguity.

## Error model

API failures use a stable shape:

```json
{
  "error": {
    "code": "xv-secret-not-found",
    "message": "Secret 'DB_URL' was not found.",
    "hint": "Refresh the vault or choose another secret.",
    "details": null
  }
}
```

Errors remain attached to their surface until resolved or dismissed. Lists
offer Retry. Forms preserve input and attach validation to fields. Bulk and
upload results name successes and failures and offer Retry failed and Copy
details. Stale-operation failures are ignored. Partial success is never shown
as complete success.

## Test architecture

- Rust unit and Axum integration tests cover every new service, route, stable
  error, capability branch, and setup path.
- Node’s built-in test runner covers pure frontend modules and state machines.
- Playwright is a test-only dependency. It does not participate in production
  asset generation. It covers focus, keyboard, responsive interaction,
  clipboard behavior, destructive actions, and uploads.
- Axe runs inside Playwright for automated accessibility checks.
- Visual snapshots cover 1180×760, 820×560, 768×700, and 390×844 in light and
  dark themes.
- Isolated local-vault end-to-end tests and desktop smoke tests never read or
  mutate a real user vault.

## Delivery decomposition

The work is too large for one implementation plan. It is divided into four
independently testable specifications:

1. `2026-07-22-app-safety-accessibility-design.md`
2. `2026-07-22-app-context-workflows-design.md`
3. `2026-07-22-app-search-upload-responsive-design.md`
4. `2026-07-22-desktop-onboarding-polish-design.md`

Their implementation order is binding because later slices consume state,
dialog, error, preference, and test infrastructure from earlier slices.

## Backlog coverage matrix

| Item | Priority | Outcome | Owning specification |
| ---: | :---: | --- | --- |
| 1 | P0 | Guard unsaved edits across every navigation path | App Safety and Accessibility |
| 2 | P0 | Complete keyboard and screen-reader modal-sheet flow | App Safety and Accessibility |
| 3 | P1 | Recoverable deletion, Trash, restore, Undo, and explicit purge | App Safety and Accessibility |
| 4 | P1 | Time-bound copied and revealed values | App Safety and Accessibility |
| 5 | P1 | First-run setup and actionable startup recovery | Desktop Onboarding and Product Polish |
| 6 | P1 | Unmistakable backend, vault, project, and environment scope | App Context and Core Workflows |
| 7 | P1 | Discoverable nested folders with practical default expansion | App Context and Core Workflows |
| 8 | P1 | Guided typed secret creation, editing, rename, and conversion | App Context and Core Workflows |
| 9 | P1 | Persistent, contextual, recoverable errors | App Context and Core Workflows, with Safety foundations |
| 10 | P2 | Fast search, filters, shortcuts, and command palette | App Search, Upload, Responsive, and Navigation |
| 11 | P2 | Managed upload queue with conflict, progress, cancel, and retry states | App Search, Upload, Responsive, and Navigation |
| 12 | P2 | Legible responsive content pattern | App Search, Upload, Responsive, and Navigation |
| 13 | P2 | Correct tab, tree, activation, and selection semantics | App Search, Upload, Responsive, and Navigation, with Accessibility foundations |
| 14 | P3 | Stronger product identity and information hierarchy | Desktop Onboarding and Product Polish, informed by Context architecture |

Every item has one primary owning specification. Cross-cutting foundations are
named where a later slice depends on an earlier one; that dependency does not
split accountability for acceptance evidence.

## Completion evidence

The final implementation maintains a requirement matrix mapping all 14 backlog
items and every “Done when” sentence to:

- production files;
- automated tests that were observed failing before implementation;
- passing verification commands;
- runtime evidence from the packaged desktop app and `xv ui`;
- accessibility and responsive evidence.

The modernization is not complete while any row is missing direct evidence.
