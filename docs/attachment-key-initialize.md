# Initialize an attachment key ring

Create or configure the destination vault first, then preview its attachment-key
initialization:

```sh
xv attachment-key initialize --vault stage --format json
```

For an empty ring, the report says `would_initialize` and has no key ID. Preview
does not generate or publish an attachment key. Stop other writers before applying:

```sh
xv attachment-key initialize --vault stage --apply --offline --format json
```

The result includes a public `active_key_id` and the retained record's provider
version. Use that ID as the transfer's `--to-key-id`. Initialization does not upload
a dummy attachment or create the vault's file-storage directory. A healthy V2 ring
returns `already_initialized` with its existing binding, without rotating it.

Initialization requires complete custody visibility and a readable file inventory.
It refuses a missing pointer when managed files or retained keys already exist;
generating a new key would not recover those files. Inspect `attachment-key status`
and `attachment-key keys`, then use the explicit `attachment-key recover` workflow.
An existing V1 identity requires `attachment-key upgrade`. Malformed or disabled
pointers also require inspection and recovery.

If initialization stops after committing a retained key but before publishing its
pointer, that key is preserved. A retry reports the retained-only state and requires
an explicit recovery choice. Keep its key record and provider versions intact.

The offline acknowledgement is required for writes. It asserts that other writers
have stopped; the retained-key commit and pointer publication are separate provider
operations.
