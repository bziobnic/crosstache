# `xv ui` Visual Refresh

**Date:** 2026-07-14
**Status:** Approved design

## Motivation

The embedded `xv ui` web application is functional and now handles its core
workflows reliably, including secret and file browsing, record editing,
selection, bulk actions, and session recovery. Its presentation still feels
like an unstyled administrative page: the hierarchy is flat, most controls
look interchangeable, tables sit directly on the page, and temporary states do
not feel like parts of one coherent product.

This refresh should make the interface feel calm, trustworthy, and deliberate
without turning a local command-line companion into a large SaaS dashboard.
The approved direction is a **calm premium vault** with **balanced density**:
generous spacing around the application shell and controls, paired with
efficient 40–45px data rows.

## Goals

- Give the entire application one recognizable visual language.
- Establish clear hierarchy among context, navigation, page content, and
  actions.
- Make secrets, files, selection mode, the editor drawer, and feedback states
  feel intentionally related.
- Provide equally polished automatic light and dark themes.
- Improve narrow-screen behavior and keyboard focus visibility.
- Preserve the embedded, offline, no-build-step frontend.
- Preserve all existing API, authentication, storage, and workflow behavior.

## Non-goals

- No frontend framework, bundler, package manager, or external font.
- No remote images, icon service, analytics, or other network asset.
- No new application sidebar or dashboard views.
- No manual theme switch in this iteration; the UI follows
  `prefers-color-scheme`.
- No changes to the Rust API, bearer-token flow, `sessionStorage`, or backend
  behavior.
- No redesign of secret/file information architecture beyond the modest page
  hierarchy and component grouping described here.

## Visual Direction

### Personality

The interface should feel secure through restraint rather than through dark,
aggressive, or terminal-themed styling. Warm neutral surfaces, a forest-green
accent, fine borders, and quiet elevation create trust without making the UI
corporate or sterile.

### Color system

`style.css` will use semantic custom properties instead of component-specific
color literals. The following values define the target palette. If a listed
foreground/background pairing misses WCAG AA in browser verification, adjust
that token by the smallest amount necessary to pass while preserving its role.

| Role | Light | Dark |
| --- | --- | --- |
| Canvas | `#f3f1eb` | `#121814` |
| Primary surface | `#ffffff` | `#19211c` |
| Subtle surface | `#f8f8f5` | `#202922` |
| Primary text | `#18221c` | `#dce5df` |
| Muted text | `#68726b` | `#8f9d94` |
| Border | `#d7dad5` | `#303b34` |
| Accent | `#216446` | `#65c68e` |
| Accent-quiet | `#e7f0e9` | `#1f3428` |
| Danger | `#9f332e` | `#f18e85` |

Color communicates meaning sparingly. Green identifies the primary action,
the active view, focus, selected items, and positive feedback. Red is reserved
for destructive actions and errors. Neutral controls remain neutral so primary
and destructive actions are unmistakable.

### Typography, spacing, and depth

- Use the operating system's UI font stack. A small monospace stack is allowed
  only where it helps identify secret values or technical identifiers.
- Page titles use a tighter tracking and heavier weight than body text; table
  headers use small uppercase labels with modest letter spacing.
- Base the layout on a 4px spacing scale, using 8, 12, 16, 24, and 32px as the
  common intervals.
- Use 8–12px radii for controls and surfaces. Pill radii are limited to badges
  and tags.
- Use fine borders as the primary separation method. Shadows are subtle and
  reserved for elevated surfaces such as the data card, drawer, toast, and
  primary action.
- Motion is brief and functional. It must respect `prefers-reduced-motion`.

## Application Shell

### Header

The full-width header becomes a compact application bar:

- An `xv` mark and the label “Crosstache Vault” establish identity.
- Backend and current-vault context remain adjacent to the brand but visually
  secondary. The backend appears as a quiet badge; the vault remains a select
  control with a status indicator.
- Secrets and Files become a small segmented navigation control at the trailing
  edge.
- Header content aligns with the page workspace and collapses cleanly on narrow
  screens.

### Page heading and toolbar

Each view gains a heading block between the application bar and toolbar:

- A small contextual eyebrow (“Vault contents” or “Vault files”).
- A direct title (“Your secrets” or “Your files”).
- One short description and a live item count.

The search field is the dominant toolbar control. Selection is secondary, and
New secret is the single filled primary action. The Files view uses the same
toolbar proportions even when it has fewer actions.

### Main workspace

The main content is centered within a 76rem maximum width. Data
tables sit inside a bordered, slightly elevated surface rather than directly on
the canvas. When a list contains items, a quiet footer below the surface
summarizes visible item and folder counts and reiterates that values remain
hidden.

## Component Design

### Tables and folders

- Column headers sit on a subtle surface distinct from item rows.
- Standard rows are 40–45px high and gain a quiet accent-tinted hover state.
- Secret names receive the strongest weight; folder, group, note, and date
  metadata remain secondary.
- Folder rows use a chevron, folder name, and item-count badge. Expanded child
  rows retain the current visual indentation and guide line.
- Groups render as quiet tags when the value is short enough; long values still
  truncate safely.
- Selection checkboxes appear only in selection mode. Selected rows use both a
  checked control and a tinted surface so the state never relies on color alone.

### Buttons and controls

The UI uses four explicit button variants:

1. Primary: filled accent surface for New, Save, and bulk Move.
2. Secondary: bordered surface for ordinary actions such as Select, Reveal,
   Copy, and Download.
3. Ghost: unfilled low-emphasis actions such as Cancel and Close.
4. Danger: red text and quiet red surface/border for Delete actions.

Inputs, textareas, and selects share height, border, radius, typography, and
focus behavior. Labels sit above fields; required fields display “Required,” and
concealed value fields display “Protected.” Focus uses a visible accent border
and halo, not a browser-outline removal without replacement.

### Bulk selection

Selection mode appears as an accent-quiet command surface directly above the
table. It contains the selected count, destination folder where applicable,
Move/Delete actions, and Cancel. The selected count anchors the surface; Cancel
is the lowest-emphasis action. Pending and confirmation states retain their
existing behavior and receive clear disabled/progress styling.

### Secret drawer

The editor remains a right-side drawer rather than becoming a separate page or
modal dialog.

- A structured header identifies New/Edit mode, the secret name, and the
  protected-value context.
- The form body groups fields with consistent labels, hints, spacing, and value
  actions.
- The footer remains visible while the form scrolls. Delete sits at the leading
  edge; Cancel and Save changes sit at the trailing edge, with Save as primary.
- Desktop presentation uses a quiet border and shadow. At phone widths the
  drawer becomes full-width.
- Existing reveal, copy, save, delete, stale-request, and typed-record behavior
  does not change.

### Files and upload

The file table uses the same data-surface treatment as the secrets table. File
actions use compact secondary or ghost buttons. The upload dropzone becomes a
contained, accent-quiet surface with a simple upload icon, a strong instruction,
and a secondary browse hint. Drag-over state strengthens the accent border and
surface without moving the layout.

### Icons

A small inline SVG symbol set supplies only the icons used by the interface
(search, add, secret, folder/chevrons, file, upload, reveal, copy, download,
close, and status). Symbols are embedded with the existing assets and rendered
through a small helper for dynamically created rows. Icons never replace a
necessary accessible name and do not introduce a network or build dependency.

## Feedback and Exceptional States

- Loading keeps the current request/data flow but renders three lightweight
  skeleton rows within the affected surface. Animation is disabled when reduced
  motion is requested, leaving static skeleton rows.
- Empty Secrets and Files views replace a bare table message with a concise
  explanation and one action wired to the existing create or browse handler.
- Load failures remain inside the affected data surface so the surrounding
  vault context does not disappear.
- Success toasts appear near the lower trailing corner as compact elevated
  notices. Errors use the danger palette and remain readable in both themes.
- The global progress bar keeps its existing in-flight counter behavior and
  uses the new accent tokens.
- The session-recovery state uses the same centered page hierarchy and provides
  no false affordances; its instruction continues to direct the user back to
  the terminal-generated URL.

## Responsive Behavior

- **Wide screens:** centered workspace, complete table columns, and a fixed
  right drawer.
- **Tablet widths:** the product label and backend badge hide, leaving the `xv`
  mark, vault selector, and navigation on one line. Toolbar actions remain
  visible, and lower-priority table columns begin to collapse.
- **Phone widths:** the navigation remains reachable, toolbar controls wrap,
  Note and Groups columns are hidden before Name/Folder/Updated, bulk controls
  stack, and the drawer fills the viewport width.
- Metadata hidden from a narrow table remains available in the detail drawer.
- The design must avoid horizontal page scrolling at supported widths.

## Frontend Architecture and Data Flow

The frontend remains three embedded files with no compilation step:

- `index.html` gains semantic wrappers for the application bar, page headings,
  data surfaces, empty states, and drawer sections. It also owns the inline SVG
  symbols.
- `style.css` is reorganized into documented sections: tokens/reset, layout,
  controls, data surfaces, drawer, feedback/states, dark theme, responsive
  rules, and reduced-motion overrides.
- `app.js` adds only presentation hooks: item-count updates, empty-state action
  wiring to existing handlers, component state classes, and the SVG icon
  helper.

The request flow remains unchanged:

1. Existing functions request context, secrets, files, or values.
2. Existing generation guards and selection state decide whether results are
   current.
3. Render functions construct the same user-visible records while applying the
   new semantic classes and icons.
4. Existing event handlers perform create, edit, reveal, copy, move, delete,
   upload, and download operations.

No new API endpoint, persistent preference, or client-side data model is
introduced. The session token continues to live in `sessionStorage` and is not
exposed to the presentation layer.

## Accessibility

- Text and interactive states must meet WCAG AA contrast in both themes.
- Every interactive element receives a clear `:focus-visible` treatment.
- Icon-only controls require an accessible label and tooltip where helpful.
- Selected, pending, successful, and failed states use text or symbols in
  addition to color.
- Interactive targets should be at least 36px in the standard layout and must
  not overlap at narrow widths.
- Existing semantic controls and table structure are preserved. Decorative SVG
  content is hidden from assistive technology.
- Motion and progress animation honor `prefers-reduced-motion`.

## Verification

Automated verification:

- Run `cargo test --features ui` to protect the embedded asset server and web
  API behavior.
- Extend existing embedded-asset/markup assertions for any structural hooks
  whose absence would break rendering.
- Keep JavaScript behavior covered by the existing Rust integration contracts;
  do not add a package manager solely for visual tests.

Manual browser verification:

- Check Secrets, Files, selection mode, bulk move/delete, and the editor drawer.
- Exercise create, edit, reveal, copy, delete, upload, and download workflows.
- Check loading, empty, failure, success-toast, and session-recovery states.
- Check automatic light and dark themes.
- Check representative desktop, tablet, and phone widths.
- Navigate all controls by keyboard and confirm visible focus order.
- Confirm reduced-motion behavior.

## Acceptance Criteria

- The UI matches the approved calm-premium direction and balanced density.
- Light and dark themes both appear intentional and remain readable.
- Existing workflows and security behavior are unchanged.
- The layout is usable without horizontal page scrolling from desktop through
  phone widths.
- All permanent and temporary states use the same tokenized component system.
- No external runtime asset, frontend dependency, or build step is added.
- Tests pass with the `ui` feature enabled, and the manual verification matrix
  exposes no functional regressions.
