# Web UI Tweaks: Dates, Record Editing, Loading Indicator

**Date:** 2026-07-09
**Status:** Approved design, not yet implemented

## Motivation

Three usability gaps in the embedded web UI (`xv ui`, shipped v0.23.0):

1. Datetime columns show full timestamps; dates alone are enough.
2. Typed records (record-types plan, `docs/superpowers/specs/2026-07-03-record-types-design.md`)
   are invisible in the UI: the detail drawer shows none of a record's
   fields, Reveal dumps the raw envelope JSON (every secret field at
   once), and typing into Value silently overwrites the envelope.
3. On launch (and vault switch) the secrets table is blank for several
   seconds with no signal that loading is in progress.

## 1. Dates only, everywhere

- New `fmtDate()` helper in `app.js`: parse the datetime string with
  `new Date()`, render `YYYY-MM-DD`; if unparseable, show the raw string
  unchanged. Applied to the secrets **Updated** column and the files
  **Modified** column.
- The drawer's **Expires** field becomes a native `<input type="date">`.
  Loading an existing secret shows the date portion of `expires_on`;
  saving sends `<date>T00:00:00Z` (blank still clears). No server change
  — the API keeps accepting RFC3339.

## 2. Full record editing

### Server

- New endpoint `GET /api/types` returning the resolved record types
  (builtin `login`/`api-key`/`database` + `[types.*]` from global and
  project config), shape `{ "types": [RecordType...] }`. `RecordType`
  is already `Serialize`. Types are resolved once at startup in
  `run_web` via `config.resolve_record_types()` and stored on
  `WebState`. A broken `[types.*]` config fails `xv ui` startup with
  the config error (same fail-loud stance as the CLI's eager paths).
  The test-only `WebState` constructor supplies `builtin_types()`.
- No other server changes: the existing PUT already accepts arbitrary
  `content_type` and `tags`, which is all a record save needs.

### Frontend

**Detecting a record** (same rule as the TUI): the secret has an
`xv-type` tag, or content-type is exactly `application/vnd.xv.record`.

**Editing an existing record:**

- The raw Value textarea and whole-value Reveal/Copy are hidden — the
  envelope JSON is never displayed and can't be clobbered.
- The drawer shows `Type: <name>` (read-only) and one input per field:
  the union of the type's declared fields, the secret's `f.*` metadata
  tags, and the envelope keys. Metadata-kind fields are plain text
  inputs; secret-kind fields are `type="password"` inputs (masked) with
  per-field reveal (toggles the input to `type="text"`) and copy
  buttons.
- Field kind is decided by where the value lives on the secret
  (envelope key → secret, `f.*` tag → metadata); a declared field not
  yet present on the secret uses its `FieldDef.kind`. An unknown type
  name (no `[types.*]` definition) still renders the union of envelope
  keys and `f.*` tags — editing existing fields works; no new declared
  fields are offered.
- Opening a record fetches the envelope once via the existing
  `POST /api/secrets/{name}/value` endpoint so secret fields are
  editable. Values live in JS memory but display masked — the same
  exposure as today's Reveal button.
- If the envelope fails to parse (content-type says record but value
  isn't a valid JSON string map): error toast, and the drawer opens
  read-only in the plain-secret view (no Save) rather than pretending
  fields are empty.

**Saving a record:**

- Secret-field inputs re-encode into the JSON envelope (sorted keys via
  plain `JSON.stringify` of an object built in sorted order —
  determinism matching `encode_envelope` is cosmetic, not required).
- Metadata-field inputs route to `f.<name>` tags. `xv-type` and
  non-field custom tags are preserved.
- PUT with `content_type: application/vnd.xv.record`, echoing
  `enabled`/`not_before` as the drawer already does. Records always use
  the full PUT path — no metadata-only PATCH branch, since field edits
  change the value.
- Empty secret-field inputs are omitted from the envelope; empty
  metadata inputs drop the `f.*` tag. Required-field enforcement is
  advisory only (HTML `required` on declared required fields for new
  records); the server does not validate.

**Creating a record:**

- The New-secret drawer gains a **Type** dropdown: `plain secret`
  (default, current behavior) plus each resolved type from
  `/api/types`.
- Picking a type swaps the Value textarea for generated field inputs
  from the type's `FieldDef` list: required fields marked, primary
  field last (matching the CLI prompt order: non-primary declared
  order, then primary).

**Out of scope (YAGNI):** ad-hoc fields not declared by the type,
changing a record's type after creation, a Type column in the secrets
table, per-field expiry.

**Alternative considered:** a server-side
`/api/secrets/{name}/record` endpoint parsing and masking fields in
Rust. Rejected: the client needs actual values for editing anyway, and
the envelope is a flat JSON string map, trivially parsed client-side.

## 3. Background-loading indicator

- `loadSecrets()`/`loadFiles()` render a single placeholder row
  ("Loading secrets…" / "Loading files…") in the table body while the
  fetch is in flight, replaced by results — or an empty-state row
  ("no secrets") when the result is empty. Covers launch and vault
  switch.
- A thin indeterminate progress bar fixed to the top of the page,
  shown whenever any API call is in flight: an in-flight counter
  incremented/decremented inside the `api()` wrapper (decrement in
  `finally`), bar visible while counter > 0. Saves, deletes, uploads,
  and reveals get feedback for free.

## Error handling

- `/api/types` has no failure mode at request time (types are resolved
  at startup); startup resolution failure aborts `xv ui` with the
  config error.
- Record envelope parse failure on open: toast + read-only drawer (see
  above).
- Date parse failure in `fmtDate()`: raw string passthrough.

## Testing

- Rust (axum, `src/web/api.rs` tests): `/api/types` returns builtin
  types; record round-trip — PUT a record (envelope value +
  `xv-type`/`f.*` tags + record content type), GET it back, confirm
  tags/content-type survive and the list never leaks the envelope.
- Frontend: no JS test harness exists (matching the existing code);
  verified manually by driving `cargo run --features ui -- ui` against
  the local backend with builtin and custom types.
