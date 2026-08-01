# App Context and Core Workflows Design

**Date:** 2026-07-22 · **Status:** Approved design

**Backlog coverage:** items 6–9 and the information architecture required by 14

## Goal

Make operation scope unmistakable, make foldered content immediately
discoverable, turn secret editing into a guided typed workflow, and make every
failure contextual and recoverable.

## Effective context

The context response includes display-safe effective context:

- backend registry name and backend kind;
- current vault and workspace alias;
- attached workspace entries available to the interface;
- discovered project path summary and active environment;
- source of backend, vault, project, and environment selections;
- connection status;
- secret, file, vault, Trash, conversion, and metadata capabilities;
- app version and security timeout policy.

No credential material, token, secret name, note, or search history is returned.
The context rail renders `backend / vault · project · environment`, with a
details popover for sources and capability limitations. Mutation sheets,
confirmations, uploads, and progress states repeat backend and vault.

Workspace switching materializes the selected backend lazily through the
existing registry and workspace resolution layer. The UI never silently falls
back to a different backend, vault, or credential source.

## Vault workspace layout

Desktop navigation contains Secrets, Files, Trash, and a hierarchical folder
tree. Nested slash-delimited folders render as nested nodes, not flattened
labels. A selected folder filters the current content surface. Unfiled content
has a stable node.

Vaults with at most 50 current items expand all folders on first visit. Larger
vaults restore persisted expansion state; without saved state they begin
collapsed. Expansion is stored per backend registry name, vault, and surface.
Expand all and Collapse all are always available. The visible count and total
count are separately labelled.

## Guided secret editor

Creation begins with type cards for Plain secret and each resolved record type.
Cards show description, required fields, protected fields, and the primary
field. Selecting a type renders only that type’s fields and explains protected
storage.

Existing records show their current type and a separate Convert action. A new
shared record-conversion service handles plain-to-record, record-to-plain, and
record-to-record conversion in one backend update. The UI and CLI both consume
it. The CLI expands `xv update --type <type>` to accept an existing record and
requires `--yes` when conversion drops fields, matching `--untype` safety.
This is an intentional compatible expansion of a command that previously
returned a usage error for an existing record. The workflow previews retained,
renamed, and dropped fields and never exposes an intermediate untyped state.

Folder uses autocomplete over existing folders. Groups use removable chips and
existing-group suggestions. Expiry uses a real date control with No expiry and
Clear states. Inline help distinguishes required, optional, protected, and
metadata fields. Server validation maps stable field identifiers back to the
relevant controls.

Rename is a separate workflow. It preflights collisions, shows the target
backend/vault, and does not submit unrelated metadata changes. Save preserves
custom tags, disabled state, not-before state, and untouched protected values.

## Durable errors

The API error envelope contains stable code, safe message, optional hint,
optional field, and optional structured details. List failures replace rows
with a persistent error state and Retry. Form failures preserve the draft and
focus the first invalid field. Connection errors keep the prior successfully
loaded view marked stale rather than replacing it with an empty vault.

Bulk result panels persist until dismissed and provide Retry failed and Copy
details. Diagnostics omit secret values and authentication material.

## Acceptance evidence

- Axum tests cover context sources, workspace entries, capability differences,
  conversion, rename collision, field errors, and stale-safe failures.
- Pure frontend tests cover nested folder models, threshold behavior,
  persistence keys, type cards, chips, date states, rename state, and error
  placement.
- Browser tests cover scope visibility in every mutation, workspace switching,
  folder navigation, type conversion, rename isolation, inline validation,
  retry, and persistent bulk results.
- Runtime tests exercise local and at least one mocked capability-limited
  backend so unsupported features are visible rather than hidden ambiguously.
