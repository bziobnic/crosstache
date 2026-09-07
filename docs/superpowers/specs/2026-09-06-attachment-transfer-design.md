# Attachment transfer and recovery

> **Status:** ✅ Shipped in **v0.39.0** (PRs #436, #437, #438). Operator
> guide: [`docs/attachments.md`](../../attachments.md#rename-and-move).
> Remaining Azure destinations, AWS/Azure source moves, and
> `xv update --rename` live in [`ROADMAP.md`](../../../ROADMAP.md).

## Approved scope

Deliver three sequential PRs: transfer safety and recovery foundation, attachment-aware
rename, then cross-vault/backend copy, move and migration. Each PR must pass local
verification and current-head CI and actual Bugbot review before merging. Begin the
next implementation only after the preceding PR merges.

## Contract

Transfers include the selected secret and every object under its exact attachment
prefix. Folder-only moves keep the existing prefix. Source and destination discovery
must fail closed on listing errors, unsupported visibility, ambiguous names, and
destination secret or attachment collisions. Force flags cannot bypass attachment
safety. Generic operations without the transfer engine must refuse attached sources
and destinations before modifying secrets, including combined rename/metadata updates
and bulk migration. Feature-disabled builds must not silently assume no attachments.

Preview reports endpoints, names, attachment count and bytes, key bindings,
collisions and any unsupported capability. It does not provision stores, keys, vaults or Git,
or perform planned secret/file mutations. Existing read auditing, operational locks
and recovery of earlier local transactions retain their normal behavior.
Apply requires an explicit stopped-writers assertion. This is a recoverable offline operation, not a portable
transaction across secrets and blob stores. Provider permissions must remain enforced;
generic transfer cannot copy reserved key records or expose raw custody interfaces.

The recovery manifest contains validated endpoint/name intent, source versions, object
metadata and ciphertext digests, expected destination generations and progress. It
contains no secret values, private keys or decrypted attachments. Persist it privately
and atomically before mutations, bound reads and reject malformed/unknown schemas,
duplicate or out-of-prefix objects, and mismatched resume intent. A progress flag alone
never authorizes deletion: independently verify current source and destination data
and detect drift before cleanup. Recovery moves forward; it never blindly rolls back
by deleting a destination. Interrupted writes and readback failures retain the source.

Create the destination secret, transfer and verify all attachments, verify the complete
destination and unchanged source, then remove source attachments and finally the source
secret for moves. Copies retain the source. Persist progress around each step and make
retry safe after a committed write whose response was lost. Never overwrite unrelated
destination data, silently adopt a different source generation, or delete newly added
source attachments. Same-vault rename preserves ciphertext and crypto references;
cross-vault transfers authenticate source data and encrypt for an explicitly verified
destination active key. Preserve all non-crypto file metadata, tags, groups and content
type. No plaintext file artifacts. Unsupported provider primitives or physical storage
aliasing must be detected before mutation and explained truthfully.

## Delivery boundaries

PR 1 provides shared detection, generic-operation guards, read-only transfer planning
and the validated manifest/recovery contract. It does not enable attached mutations.
PR 2 adds same-vault execution and CLI/web entry points with interruption recovery.
PR 3 adds destination key bindings and cross-vault/backend execution and integration.
The final documentation must distinguish supported operations from explicit refusals.
