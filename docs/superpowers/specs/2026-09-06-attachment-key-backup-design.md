# Encrypted attachment-key backup and offline restore

Status: implementation contract, incorporating the design merged in PR #431.

## Outcome and scope

Users can export the attachment identities needed by the current visible files
to a separately encrypted key bundle, then restore those identities and file
references into the same or another vault. Successful restore proves the current
manifest files decrypt with destination custody through the normal download path.

This is a key backup plus a recovery manifest. It contains no attachment
payloads. Users must separately preserve and restore the encrypted files,
including their metadata, at the manifest's original names. It does not back up
ordinary secrets or historical object versions. Reports describe visible scope;
agent policy may make that scope smaller than the whole vault.

Destination files are updated in place. Azure file storage is container-scoped
and ignores the vault argument; a separate recovery copy requires an independently
configured destination container/account, not merely another vault name. Backend
labels in a bundle are provenance and cannot prove physical storage separation.

Rotation, changing the identities that encrypt files, key deletion, retirement,
renaming files, and transparent fallback during normal downloads are excluded.
The file-rebinding step preserves ciphertext bytes and only changes their key
references. It is distinct from re-encrypting files under a different identity.

## Architecture decision

Three approaches were considered:

1. Import keys and return a version mapping. Smallest implementation, but files
   remain unreadable until another recovery tool applies that mapping.
2. Add a persistent version-alias lookup to normal downloads. Avoids file writes,
   but changes the exact-version trust contract and introduces another custody
   record requiring protection, migration, and long-term support.
3. Explicitly rebind verified file metadata during offline restore. Preserves
   strict normal reads and delivers working recovery; requires file writes and
   an interruption-safe operation.

Use option 3. Never fabricate source provider versions, silently use a current
key when an exact version is missing, or search candidate keys during a download.
The source reference and destination reference are separate, explicit values.

## CLI contract

Proposed commands:

```text
xv attachment-key export --vault SOURCE --recipient age1... --output keys.age --offline
xv attachment-key restore --vault DEST --input keys.age --identity-file recovery.agekey
xv attachment-key restore --vault DEST --input keys.age --identity-file recovery.agekey --apply --offline
```

Export requires a caller-supplied X25519 age recipient, independent of the vault
being backed up. It does not export unencrypted private values or implicitly use
the source vault's own encryption key. The recipient is public; the recovery
identity is supplied through a file, never a command-line secret argument.
Passphrase and SSH recipients are outside this first format.

Export is read-only against providers but requires stopped writers to collect a
coherent recovery set. The output is created without replacing any existing
path. Write ciphertext to a sibling temporary file, flush it, and publish with
no-clobber semantics. No plaintext temporary files. Use restrictive file
permissions where supported.

Restore previews by default. Preview decrypts and validates the bundle and reads
the destination, including ciphertext verification, but performs no provider
writes. Apply requires both --apply and --offline. Both modes use the existing
backend/workspace resolver and policy-wrapped custody/file interfaces.

An additional --repair-pointer flag explicitly permits replacing a malformed
destination pointer with the bundle's verified bindings. Use it in preview and
again when applying the repair. It does not authorize replacing a valid V1 or
conflicting V2 pointer, bypassing provider/policy errors, or overwriting retained
key collisions. No separate recover command is needed when restoring lost keys.

JSON/YAML/human reports contain operation, outcome, source context, destination
context, counts, key IDs, per-file outcomes, and source/destination version
mappings. They never serialize private material or decrypted file contents.
Applying does not imply all provider operations were transactional.

## Bundle format and confidentiality

Use the existing age library for authenticated encryption. Inside the encrypted
envelope, a versioned JSON document carries:

- Format discriminator and schema version 1.
- Source backend label, source vault, and creation timestamp, as provenance only.
- Active key ID and optional explicit legacy fallback key ID.
- Identities indexed by their derived portable key ID.
- Verified source references: slot, portable key ID, opaque provider version.
- Current managed-file manifest: exact logical name, ciphertext SHA-256, source
  crypto reference or explicit pre-schema legacy binding.

Private DTOs are confined to the codec module and have no Debug implementation.
Secret strings and serialized/decrypted buffers are zeroized on drop. Error
messages never include the decrypted document, identity parser input, provider
values, or arbitrary JSON fragments. Public report types contain no secrets.

Reject unknown schemas, unknown fields, duplicate IDs/references/file names,
invalid names or key IDs, mismatched derived identities, dangling bindings,
unsupported reference slots, empty/invalid versions, and inconsistent legacy
bindings. Reject trailing data and incomplete authenticated decryption. Bound
ciphertext and plaintext bundle input to 16 MiB each, identities to 10,000 and
manifest entries to 100,000; exceeding limits fails explicitly. Stream-limit
decryption rather than checking only after allocating the entire plaintext.

Encryption establishes confidentiality and tamper detection, not the sender's
identity: anyone with the public recipient can create a bundle. Treat decrypted
contents as untrusted input and do not accept bundle-supplied backend credentials,
destination paths, or instructions. The user explicitly selects the input and
destination.

## Export collection and verification

1. Read and validate the source pointer. Accept healthy V1 or V2. Represent V1
   as active=legacy=the verified original identity without modifying its pointer.
   A missing or invalid pointer fails; pointer repair remains the recover command.
2. Enumerate all visible marked retained records and include active/legacy
   identities. Read their exact versions and verify each ID from its identity.
   An inaccessible, disabled, invalid, or mismatched required record fails the
   export rather than producing a misleading successful recovery bundle.
3. Enumerate current visible files without a result limit. Use existing managed
   file classification. Ordinary files are excluded. Malformed managed crypto
   metadata fails export. Read coherent bytes-and-metadata snapshots for managed
   files, resolve the exact referenced identity (including historical V1 pointer
   versions), decrypt to authenticate, discard plaintext, and record the
   ciphertext hash and source binding. Pre-schema files use only the explicitly
   established legacy identity.
4. Include referenced historical identities even if not currently selected by a
   pointer. Deduplicate identities by derived ID, but preserve every referenced
   source slot/version binding. Never enumerate arbitrary ordinary secret values.
5. Recheck pointer, retained references, file set, and recorded file snapshots
   for observed drift before finalizing. These checks detect changes; they do not
   create a portable transaction. Offline operation remains a precondition.
6. Encrypt the fully validated bundle and publish its output only on success.

## Restore preflight

Validate the complete bundle before provider access. Destination name validation
and existing policy checks apply to every record and file. Fail on any policy or
provider error rather than skipping denied objects and reporting full success.
Preflight locally configured Set policy for every planned custody write before
any mutation; remote provider write permissions are still checked by the actual
write and may fail after earlier successful steps. Read fresh file metadata when
classifying destination files because provider listings may omit it.

Require every manifest file to exist at its original logical name in destination
storage, with ciphertext matching the bundle hash. Authenticate each file using
its declared identity from the bundle. Accept original source crypto metadata,
or already rebound metadata whose exact destination key verifies to the same ID.
Other bindings, malformed metadata, missing files, or changed bytes fail.

Reject additional managed destination files outside the bundle manifest; this
avoids changing an existing ring's meaning for files the operation cannot verify.
Ordinary unrelated files and secrets are unaffected. Empty manifests support
restoring a ring before any encrypted files were created.

Before any mutation, check every destination retained-key name. An unmarked
collision, different identity, disabled key, or invalid existing record fails.
An existing marked record with the expected identity is reusable after an exact
version read. Destination pointers may be absent or already carry the identical
V2 active/legacy bindings. Refuse a V1 pointer (upgrade it first) or any parsed
V2 pointer with different active/legacy bindings, even if its keys are missing.
This first restore operation never implicitly rotates a destination or changes
a known legacy binding.

A malformed pointer fails by default with guidance to preview using
--repair-pointer. With that flag, accept the malformed pointer as a repair target
and capture its exact preflight version and value privately. Classify a pointer
as malformed only after a successful value read and parse failure; an omitted
value or provider/policy failure is an error, not permission to repair. The
validated bundle supplies the replacement bindings. Missing retained records
are imported by restore before pointer publication, so repair remains possible
when neither recover nor generic reserved-record mutation can help. Preview
reports the planned pointer repair without exposing its original value.

Preview reports records to create/reuse and files to rebind/already verified.
New provider versions cannot be known in preview and are reported only after
commit; do not invent a preview mapping.

## Apply ordering, verification, and retries

1. Complete all preflight checks before the first write.
2. Commit missing marked retained records through commit_retained_key. Reuse
   validated existing records. If a create conflict occurs, reread and verify the
   winner. Read each committed exact provider version and verify its identity.
3. Build destination references with slot=retained and the actual verified
   destination version. Historical source V1 references also become retained
   references; source version tokens are never claimed to exist at destination.
4. For each manifest file, reread and compare its snapshot before mutation.
   Authenticate its ciphertext, replace only reserved crypto-reference metadata,
   and preserve ciphertext, user metadata, tags, content type, and groups. Do not
   use decrypted bytes as the upload payload. Skip already correctly rebound
   files. Preserve provider errors and never claim rollback of completed writes.
5. Confirm every write by reading a coherent snapshot and validating unchanged
   ciphertext, expected metadata, and normal exact-key decryption. Report an
   unconfirmed write as failure. Repeating restore safely recognizes verified
   retained records and already rebound files.
6. Recheck the destination pointer against its preflight state (absence, or exact
   version and value), including an explicitly authorized malformed-pointer
   repair. Any observed change aborts publication. Publish the bundle's V2
   active/legacy pointer only after all retained keys and file references are
   verified, then verify its readback. Skip publication if the pointer already
   has the required bindings. An interruption leaves retained keys and
   independently readable rebound files; a retry completes the remaining work
   without deleting keys. Retrying after repair recognizes the valid matching
   pointer even if --repair-pointer is still supplied.
7. Run a final manifest/readability verification before reporting success.

FileBackend currently has coherent snapshots but no portable conditional
replacement. Rechecking immediately before upload is a drift detector, not
compare-and-swap. All writers and older clients must remain stopped throughout
apply. A future online restore requires a separate conditional-write interface.
Existing historical blob versions retain their original metadata and are not
covered by this operation's success statement.

## Implementation boundaries

- src/secret/attachment_backup.rs: private validated bundle model, collection,
  bounded age codec, and safe public reports; split codec into its own module if
  the orchestration would otherwise mix serialization and provider operations.
- src/secret/attachment_restore.rs: full preflight, retained-record commit,
  reference mapping, file rebinding, retry, and pointer publication.
- src/cli/attachment_key_ops.rs: export/restore options, file I/O, destination
  resolution, offline acknowledgement, safe report rendering.
- src/secret/attachment_lifecycle.rs: share narrowly scoped exact-read and pointer
  verification helpers where useful, without exposing a raw secret backend.
- AttachmentKeyStore and FileBackend remain the authority boundary. Add a custody
  write-policy preflight and explicit file restore methods: full metadata reads
  propagate tag errors, and replacement preserves supplied metadata and tags.
  Backends without these restore methods fail closed before file mutation.
  Display-oriented metadata reads and ordinary upload bookkeeping stay unchanged;
  normal download semantics and generic custody guards remain strict.
- docs/attachments.md, README command reference, CHANGELOG.md, and ROADMAP.md:
  document commands, separate payload backups, visible scope, offline semantics,
  current-object limitation, and recovery examples.

## Required verification

Codec tests: wrong recovery key, truncated/tampered ciphertext, malformed or
oversized decrypted data, duplicates, ID mismatches, unsupported schemas, and
secret-free errors/debug/report output. Output tests: no overwrite, no partial
final bundle, no plaintext artifacts.

Lifecycle tests: V1 raw identity, schema-1 legacy pins, V2 retained pins, distinct
active/legacy identities, unreferenced retained keys, historical references,
missing/disabled keys, collision preflight with zero writes, unrelated destination
managed files, preview with zero writes, and policy denial before mutation.

Malformed-pointer recovery tests: remove the retained records and corrupt the
pointer after creating a valid bundle. Default restore must refuse with zero
writes; --repair-pointer preview must validate without mutation; explicit offline
apply must import keys, rebind files, and finally publish a working pointer. Cover
empty manifests, pointer drift before publication, interruption before/after
publication, idempotent retry, denied/omitted pointer values, and continued refusal
of valid V1 or conflicting parsed V2 pointers even with the repair flag.

Cross-vault tests must use different source and destination version tokens. Copy
the original ciphertext and metadata separately, restore, and prove ordinary
download returns the original plaintext while ciphertext bytes are unchanged.
Verify source custody is unchanged and new uploads use the restored active key.

Inject interruptions after retained commit, file replacement, and before/after
pointer publication. Retry must succeed without extra retained versions or
rewriting already rebound files. Inject pointer/file drift and mismatched readback;
fail with no false success. Verify user tags, metadata, groups, and content type.

Exercise Local end-to-end plus injected Azure/AWS provider behavior, including
AWS tag markers and Azure immutable version creation. Run feature-gated compile
checks, formatting, Clippy, and the workspace suite before submitting the PR.
