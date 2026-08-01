# App Search, Upload, Responsive, and Navigation Design

**Date:** 2026-07-22 · **Status:** Approved design

**Backlog coverage:** items 10–13

## Goal

Make large vaults fast to navigate, make multi-file uploads understandable and
recoverable, replace narrow tables with legible responsive rows, and implement
correct navigation and selection semantics.

## Search, filters, and commands

Each surface has local search with a visible clear action and `visible / total`
result count. Secret filters include folder, group, record type, expiry, and
enabled status. File filters include folder, type, and upload status. Active
filters render as removable chips and are announced as filters, not content.

The command palette opens with Cmd/Ctrl+K. It searches loaded secret and file
metadata, folders, vault/workspace targets, and registered commands. It never
indexes or persists secret values, notes, clipboard contents, or previous
queries. Results identify their surface and backend/vault. Activating a result
switches context only after guarded-navigation approval.

The command registry also owns `/` to focus local search, Cmd/Ctrl+N to create a
secret, Escape to close the topmost transient surface or exit selection mode,
and arrow-key tab navigation. Shortcuts do not fire while typing in compatible
form controls.

## Managed upload queue

The file surface states the 100 MB per-request limit and configured concurrency
before selection. Users select a destination folder before transfer. Preflight
checks validate names, sizes, capabilities, and conflicts without uploading
content.

Conflicts are resolved per file or with an Apply to all choice: Skip, Replace,
or Rename. Replace is never implicit. Queue entries expose queued, uploading,
completed, failed, cancelled, and retrying states. Uploads use
`XMLHttpRequest.upload` progress events so every local request reports bytes
sent and total bytes. Provider-side finalization remains an indeterminate
“Finishing…” state because client upload completion does not prove provider
commit.

Cancellation aborts the in-flight XMLHttpRequest. The server writes through the
existing backend operation and reports whether cancellation occurred before or
after provider commit. Ambiguous completion is refreshed and labelled rather
than reported as failure or success without evidence. Retry targets failed or
cancelled entries only. The final persistent summary names all outcomes.

## Responsive content pattern

At widths above 768 px, semantic tables remain available with sorting and
resizing. At 768 px and below, data renders as stacked semantic rows: full
identifier on the first line, priority metadata on the second, and a clear
activation affordance. Folder headers span the list. Hidden desktop columns and
resize separators are removed from the accessibility tree and focus order.

The desktop shell’s minimum size and web breakpoints are tested together. The
right modal sheet becomes full-screen below 544 px. Toolbars wrap into ordered
rows without moving primary actions behind horizontal scrolling.

## ARIA and selection contracts

Secrets, Files, and Trash use the ARIA tabs pattern with `tablist`, `tab`,
`aria-selected`, roving tabindex, and labelled `tabpanel` elements. Left/right
arrows move tabs; Home and End jump to boundaries.

Folder nodes use a tree pattern on desktop and filter controls on mobile. Item
activation has one semantic control. Selection mode exposes one checkbox per
item; the name is no longer a second control with the same accessible label.
The header checkbox labels its visible-item scope and mixed state.

## Acceptance evidence

- Pure tests cover search indexes, filter combinations, command ranking,
  shortcut suppression, queue state transitions, conflict policies,
  cancellation ambiguity, and responsive view models.
- Axum tests cover upload preflight, stable conflict errors, size enforcement,
  cancellation-safe responses, and partial results.
- Playwright tests cover every shortcut, ARIA tab behavior, selection focus
  order, filters, file search, conflict decisions, cancellation, retry, and
  responsive rows.
- Visual snapshots cover 1180×760, 820×560, 768×700, and 390×844 in both
  themes. Snapshot fixtures use realistic long secret names and folder paths.
