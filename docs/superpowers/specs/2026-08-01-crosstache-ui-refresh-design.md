# Crosstache UI Refresh: Refined Command Center

**Date:** 2026-08-01

**Status:** Approved design

**Applies to:** Shared embedded UI used by `xv ui` and the desktop app

## Summary

Refresh the current vault workspace into a cleaner **Refined Command Center**.
The persistent context rail becomes Crosstache's defining element, Secrets,
Files, and Trash move into that rail, and the main canvas is simplified around
one page title, one primary action, one search field, and progressively
disclosed secondary controls.

This design supersedes the shell, navigation, responsive, and editor-layout
portions of the 2026-07-14 visual refresh and 2026-07-22 modernization designs.
It does not replace their safety, accessibility, error, upload, preference, or
backend contracts. No Rust API or domain-model change is required.

## Goals

- Make Crosstache feel like a focused product rather than a generic admin
  table.
- Keep the effective backend, workspace, vault, project, and connection state
  understandable at a glance.
- Remove redundant navigation and reduce the number of simultaneously visible
  controls.
- Preserve efficient folder-tree browsing and data density on desktop.
- Give create/edit flows a clear field hierarchy without hiding protected-value
  safety.
- Keep light, dark, loading, empty, selection, failure, and narrow-screen states
  visually coherent.
- Preserve the existing no-build-step frontend, API, security model, and tested
  behavior.

## Non-goals

- No frontend framework, bundler, remote font, analytics, or network asset.
- No dashboard, metric cards, or decorative home screen.
- No new backend operations or API endpoints.
- No change to bearer-token handling, loopback-only serving, or secret-value
  transport.
- No replacement of the hierarchical tree grid with a card library or a
  separate folder browser.
- No change to Trash semantics, upload behavior, protected-value timers, dirty
  draft guards, typed-record rules, or confirmation requirements.

## Approved Direction

Three alternatives were reviewed:

1. **Refined Command Center** — persistent context rail and compact data
   workspace.
2. **Quiet Workspace** — top navigation with no permanent rail.
3. **Library Navigator** — split folder navigation and card-based content.

The Refined Command Center was selected because it keeps Crosstache's
cross-backend context visible, preserves the efficient tree-grid interaction,
and requires less behavioral change than the library model. The selected
direction was then approved in three sections: application hierarchy,
desktop/mobile editor behavior, and visual/operational states.

## Information Architecture

### Desktop shell

At widths above 768px, the application uses a two-column shell:

- A 13rem-wide context rail.
- A flexible main workspace with no second application-level navigation bar.

The rail contains, in order:

1. `xv` product mark and “Crosstache.”
2. Active workspace control showing workspace, backend, project or vault, and
   connection/capability summary.
3. Primary navigation: Secrets, Files, and Trash, with item counts when known.
4. Quick access links that map only to existing client-side state:
   “Recently updated” routes to Secrets and applies descending updated sort,
   and “Expiring soon” routes to Secrets and applies the existing expiry
   filter.
5. Commands, Settings, and Help.
6. Version information at the bottom.

Secrets, Files, and Trash no longer appear as a second tab strip in the page
header. They retain ARIA tab semantics even though they are rendered vertically
in the rail. Arrow-key, Home/End, `aria-selected`, and panel relationships
continue to follow the existing tabs implementation.

The workspace control remains the existing native select, visually grouped
inside the context card. Selecting it changes workspace through the existing
guarded context workflow. A separate context-details disclosure exposes full
provenance and capability information. The compact top breadcrumb is
display-only and does not duplicate Settings or Help.

### Main workspace

The main workspace uses this stable order:

1. Compact breadcrumb with workspace, project/vault, and backend status.
2. Page heading with eyebrow, direct title, one sentence of context, live item
   count, and one filled primary action.
3. Control row with dominant search, one Filters button, Select, and compact
   refresh.
4. Active filter chips, shown only when filters are active.
5. Persistent error or bulk-selection surface when applicable.
6. Tree grid or stacked responsive list.
7. Quiet result/security summary.

The primary action is contextual: New secret in Secrets, upload/browse in
Files, and no invented creation action in Trash. Search is always the widest
control. Group, type, expiry, status, and file-type controls move behind one
Filters button; active values return to the page as individually removable
chips. The Filters button toggles an inline panel immediately below the control
row using `aria-expanded` and `aria-controls`; the panel stacks at narrow
widths. Expand all and Collapse all move into that panel's footer rather than
occupying the normal toolbar.

### Content views

The existing tree-grid model remains authoritative. Folder rows use disclosure,
name, count, and depth indentation. Item rows keep Name as the dominant column,
with surface-specific metadata in secondary columns. The currently implemented
sorting, expansion memory, filtered ancestor reveal, selection semantics, and
keyboard navigation do not change.

Files and Trash reuse the same shell, heading, control hierarchy, data-surface
geometry, and feedback patterns. Upload queues and Trash recovery actions keep
their current behavior and appear beneath the Files or Trash heading as local
workflow surfaces.

## Responsive Behavior

The existing content breakpoint remains binding:

- **Above 768px:** persistent rail and full tree grid.
- **768px and below:** stacked content rows whose identifiers do not truncate.

At 768px and below, the persistent rail becomes two compact surfaces:

- A top context bar with the active workspace/backend summary and access to the
  workspace switcher.
- A sticky bottom primary navigation bar for Secrets, Files, and Trash. Its
  height is reserved in layout so it never covers list or editor content.

Commands, Settings, Help, context details, and version move into the existing
command or utility sheet; they do not compete with the three primary
destinations. The bottom bar participates in normal layout rather than covering
content, and safe-area padding is applied when supported. Dialog and sheet
focus containment must treat both compact surfaces as background content.

Main-workspace controls wrap by priority: search takes a full row first,
followed by Filters, Select, refresh, and the primary action. Folder/item names
stay complete and secondary metadata becomes a second line. No supported width
may require horizontal page scrolling.

## Secret Editor

### Desktop

Create and edit remain full-height, 30rem-maximum modal drawers on the trailing edge. The
underlying list stays visible for orientation but is inert and obscured. The
drawer is divided into three stable regions:

- Header: operation and record type, secret name, update metadata, and Close.
- Scrollable body: destination context, typed fields, organization metadata,
  attachments, and advanced workflows.
- Sticky footer: Delete at the leading edge for existing records, then Cancel
  and the primary Save action at the trailing edge.

The body groups fields in this order:

1. Destination context naming backend, workspace/vault, and project.
2. Record-type fields, with required and protected status next to labels.
3. Folder, groups, expiry, note, and enabled state.
4. Attachments.
5. Collapsible advanced workflows such as rename or type conversion.

Reveal and Copy remain adjacent to the protected field but outside its label.
Their accessible names, exposure countdown, clipboard ownership checks,
auto-hide behavior, and focus behavior remain unchanged.

### Narrow screens

At phone widths, the same editor becomes a full-screen sheet. It uses the same
field order and terminology, a compact top bar, a visible destination context
line, a scrollable body, and a sticky two-action footer. Delete remains
available but separated from Save and still opens the existing explicit
confirmation flow.

Dirty-state navigation guards cover Close/Back, Escape, navigation, workspace
switching, and window close. Saving locks context switching and names the exact
destination. Form errors stay beside the relevant field and never erase the
draft.

## Visual System

The existing semantic color tokens remain the foundation. The refresh adjusts
usage and hierarchy rather than introducing a second palette.

- Warm neutral canvas in light mode; neutral near-black canvas in dark mode.
- Opaque content surfaces with fine borders and limited, soft elevation.
- Forest green for the active destination, primary action, selection, focus,
  and positive state.
- Red only for destructive actions and failures.
- The dark rail is slightly differentiated from the main dark canvas rather
  than becoming pure black.
- Operating-system UI font stack; monospace only for technical values.
- 4px spacing base with 8, 12, 16, 24, and 32px as common intervals.
- 8–12px radii for controls and surfaces; pills only for compact status and
  tags.
- One filled primary action per page or modal. Secondary, ghost, and danger
  actions retain explicit roles.
- Standard table rows remain approximately 40–45px high; compact density uses
  the existing preference rather than a separate design.

Both explicit theme preferences and system-following behavior remain. Every
new token or component state must pass WCAG AA contrast in both effective
themes. Motion remains brief and functional and honors reduced motion.

## Operational States

### Selection

Entering selection mode reveals checkboxes and replaces the ordinary filter
context with an accent-quiet bulk command surface. It names the selected count
and current destination, exposes only supported bulk actions, and keeps Cancel
lowest emphasis. Selected rows use both controls and surface tint.

### Errors and stale data

Errors remain attached to the surface that failed. A failed refresh preserves
stale results and displays a persistent panel with what happened and direct
Retry, Copy details when relevant, and Dismiss actions. Partial bulk or upload
results remain persistent and distinguish succeeded and failed items. Toasts
remain appropriate only for short-lived success acknowledgement and Undo.

### Loading and empty results

Loading uses existing stable skeleton rows inside the affected surface. Empty
vaults offer the applicable primary action. A filtered empty state names the
active constraint and offers Clear search or Clear filters rather than a create
action. Reduced motion leaves skeletons static.

## Frontend Architecture

The existing dependency-free module boundaries remain:

- `app.js`: bootstrap and route ownership.
- `context.js`: workspace context, activation, and capability presentation.
- `secrets.js`, `files.js`: view-specific rendering and workflows.
- `tree-grid.js`: hierarchical navigation and selection semantics.
- `dialogs.js`: editor, confirmation, focus containment, and dirty guards.
- `commands.js`: command palette and shortcuts.
- `settings.js`, `preferences.js`: appearance and density preferences.
- `store.js`: authoritative application state.
- `ui-model.js`: pure responsive, sorting, filtering, and formatting helpers.
- `accessibility.js`: ARIA tabs, focus mapping, inert background, and live
  announcements.

`index.html` changes shell and semantic placement. `style.css` gains the rail,
compact context bar, bottom navigation, consolidated toolbar/filter surface,
and editor hierarchy. JavaScript changes should be limited to moving existing
controls, mapping quick-access actions to existing sort/filter state, and
maintaining responsive focus/state ownership. No domain rule belongs in CSS or
DOM event handlers.

## Data Flow

The store remains the single source of truth:

1. Existing API clients fetch context and view data.
2. Store events update route, context, filters, selection, operation, and
   dialog state.
3. Renderers project that snapshot into the desktop or narrow shell.
4. Responsive transitions preserve the active route, filter, selection,
   expansion, dialog, and meaningful focus target.
5. Existing generation guards and cancellation ignore stale results.

The redesign introduces no secret-bearing preference or storage entry. Rail
state is derived from existing context and counts. Quick access changes only
the normal view model. The bearer token remains in per-tab `sessionStorage` and
all secret values continue to travel only through authenticated request bodies.

## Accessibility Requirements

- Vertical primary navigation preserves the existing ARIA tabs contract.
- At narrow widths, the same tab identities move to bottom navigation without
  duplicate focusable tab controls.
- Workspace selection remains keyboard- and screen-reader-operable.
- Tree-grid and stacked-list focus mapping survives responsive transitions.
- All modal sheets set dialog semantics, inert the shell, contain focus,
  restore focus, and honor dirty-state confirmation.
- Icon-only controls have stable accessible names; icons never replace the
  visible name of a destructive or high-risk action.
- Selected, pending, success, failure, and connection states never rely on
  color alone.
- Focus indicators remain clearly visible on both the rail and content
  surfaces.
- The page has no overlapping controls or horizontal overflow at the supported
  test widths.

## Verification

### Automated

- Run `npm run test:unit` for store, model, dialog, command, context, settings,
  and view contracts.
- Run the complete Playwright suite for routed views, keyboard navigation,
  focus restoration, responsive transitions, selection, upload, Trash,
  protected values, errors, and accessibility.
- Keep visual snapshots at 1180×760, 820×560, 768×700, and 390×844 in light and
  dark appearances. Update them only after independent inspection.
- Run `cargo test --features ui web:: --lib` to protect embedded assets, routes,
  context, preferences, and structured errors.
- Run relevant desktop startup/smoke coverage because the same embedded shell
  is used by the native app.

### Manual and visual

- Verify all three routed views in browse and selection modes.
- Exercise workspace switching, quick access, filter opening/removal, folder
  expansion, editor open/save/cancel/delete, and command-palette access.
- Verify clean and dirty editor closure, protected reveal/copy timers, stale
  list errors, partial results, loading, empty vaults, and filtered empties.
- Inspect light/dark desktop, desktop minimum, breakpoint, and phone layouts.
- Complete keyboard-only and screen-reader walkthroughs and confirm reduced
  motion.

## Acceptance Criteria

- Secrets, Files, and Trash have exactly one visible primary navigation system
  at every width.
- Effective context is immediately visible without dominating the content.
- Each page has one clear primary action and one dominant search control.
- Secondary filters are progressively disclosed and active constraints remain
  visible and individually removable.
- The existing tree-grid, upload, Trash, error, protected-value, typed-record,
  and safety behaviors remain functionally equivalent.
- Desktop drawers and narrow full-screen editors share terminology, field
  order, context, and guarded save/close behavior.
- Light, dark, selection, loading, empty, and error states use one coherent
  visual system.
- The interface remains usable without horizontal page scrolling at 1180,
  820, 768, and 390px test widths.
- Automated unit, Rust, Playwright, axe, responsive, and visual verification
  passes without serious or critical accessibility findings.
- No production frontend dependency, backend endpoint, security-model change,
  or secret-bearing preference is introduced.
