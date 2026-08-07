# Keeper import / export

`xv` reads and writes [Keeper Security's JSON import format][keeper-docs], so you
can migrate a Keeper vault into `xv` and hand a vault back to Keeper.

```bash
# Keeper -> xv
xv vault import myvault --fmt keeper -i keeper-export.json

# xv -> Keeper
xv vault export myvault --fmt keeper --include-values -o keeper-import.json
```

Both work on every backend (Azure Key Vault, AWS Secrets Manager, and the local
age-encrypted store).

[keeper-docs]: https://docs.keeper.io/user-guides/import-records-1/import-json

## Preview before you write

`--dry-run` reports exactly what a real run would do — including every record it
would refuse — and exits non-zero if anything is unimportable, so it works as a
gate:

```bash
xv vault import myvault --fmt keeper -i keeper-export.json --dry-run
```

## How records map

A Keeper record with both a `login` and a `password` becomes a typed
[`login` record](#record-types): its username and URL are listable metadata,
and its password is encrypted secret material.

| Keeper                       | xv                                    |
|------------------------------|---------------------------------------|
| `title`                      | secret name (original kept in `original_name`) |
| `login`                      | `f.username` tag                      |
| `password`                   | record envelope, primary field         |
| `login_url`                  | `f.url` tag                           |
| `notes` + `$note` fields     | `note` tag (merged)                   |
| `folders[].folder`           | `folder` tag, `\` rewritten to `/`    |
| scalar `custom_fields`       | ordinary tags                         |
| object `custom_fields`       | record envelope, one field per sub-key |
| `custom_fields.$oneTimeCode` | record envelope, `one-time-code` field |

A TOTP seed is a second authentication factor, so `$oneTimeCode` is stored as
encrypted secret material rather than as a tag. It never appears in `xv ls`:

```bash
xv get Facebook --field one-time-code
```

Titles pass through `xv`'s usual name sanitization — `Dev Server 1` is stored
under the secret name `Dev-Server-1` with the original title preserved in the
`original_name` tag, so it still displays and round-trips as `Dev Server 1`.

### Object fields hold the real secrets

Keeper stores its most sensitive values inside *object-valued* custom fields:

| Keeper field    | Contains                                        |
|-----------------|-------------------------------------------------|
| `$keyPair`      | `privateKey`, `publicKey`                       |
| `$paymentCard`  | `cardNumber`, `cardSecurityCode`, expiry        |
| `$passkey`      | `privateKey`, `credentialId`, …                 |
| `$pamSettings`  | connection and port-forward settings            |

Every sub-key is flattened into the **encrypted envelope**, never a tag —
tags are unencrypted metadata capped at 256 characters, so a private key
there would be both exposed and rejected by the backend:

```bash
xv get "BMO SFTP key" --field private-key
xv get "BMO SFTP key" --record        # every field as JSON
```

### Record types

A record must have exactly one primary field, and most Keeper credentials
have no password — so imports pick a type to suit:

| Keeper record has        | xv type       | Primary field |
|--------------------------|---------------|---------------|
| a `$keyPair` private key | `ssh-key`     | `private-key` |
| a `$paymentCard` number  | `payment-card`| `card-number` |
| `login` + `password`     | `login`       | `password`    |
| anything else storable   | `secure-note` | `content`     |

For `secure-note` the primary is taken from the password, else the first
secret field, else the notes. These are ordinary built-in types — usable from
`xv set --type` independently of any import.

### The only records that are refused

A record is refused only when it is genuinely empty. In practice that means
its content was a Keeper **file attachment**, which Keeper's JSON export does
not include:

```
[error] Skipping record 'salesforce-private-key': has no password, notes, or
        custom fields, so there is nothing to store (a Keeper record whose
        content is a file attachment exports empty — download the attachment
        from Keeper and add it with 'xv attach')
```

Download those from Keeper and attach them with `xv attach`.

## What does not carry over

**Shared-folder permissions.** Keeper's `shared_folders` grant access per user
and per team (`manage_users`, `manage_records`, `can_edit`, `can_share`). `xv`
has no per-folder ACL — it shares whole vaults through backend RBAC — so a
shared folder is imported as an ordinary folder and its grants are reported:

```
[warn] 1 shared folder(s) imported as plain folders; xv has no per-folder ACL,
       so their permissions were NOT applied.
[warn] Keeper granted access to myusername@company.com, team kVM96KGEoGxhskZoSTd_jw.
       Grant equivalent vault access with 'xv share grant'.
```

Rebuild the equivalent access with [`xv share grant`](../README.md). The
principals are always named so you can reconstruct them.

**Multiple folders per record.** A Keeper record can live in several folders at
once; an `xv` secret has one. The first is used and the rest are reported.

**Duplicate titles are renamed, not refused.** Legal in Keeper, where folders
disambiguate. xv has one flat namespace per vault, so the second and later
records are qualified by their folder where that distinguishes them, and
otherwise get a numeric suffix. The original title is always preserved in the
`original_name` tag.

```
Finance/American Express -> Finance American Express
same folder, 2x github   -> github.com, github.com 2
```

## Oversized values

Azure caps a tag value at 256 characters. A `note` or URL over that limit is
stored as an **encrypted envelope field** instead of a tag, rather than failing
the record:

```
[warn] Sendgrid BQM Data Jobs: note is 1495 characters, over the backend's
       256-character tag limit; stored as an encrypted 'note' field instead
       of listable metadata.
```

Nothing is lost or truncated; the value simply stops being listable in
`xv ls`. This only happens where the backend actually requires it — the local
backend has no tag cap, so the same file keeps its notes listable there.

A record whose tag *count* would exceed the backend's limit (15 on Azure) is
still refused, and that check runs **before** any write, so it fails cleanly
rather than part-way through.

## Export notes

`--fmt keeper` requires `--include-values`. A Keeper file whose records have no
`password` imports as a set of empty records, which looks like a successful
migration; `xv` refuses to produce one:

```
$ xv vault export myvault --fmt keeper
error: --fmt keeper requires --include-values (a Keeper import file without
       passwords cannot be imported)
```

Typed `login` records export with full field fidelity. Any other secret exports
as a title plus a `password` holding its value, so nothing is omitted from the
file. Non-primary envelope fields stay in `custom_fields` — note that this
writes secret material into the exported file in plaintext, which is inherent
to the Keeper format.

Exported files are written with owner-only permissions when you pass `-o`.
They contain every password in plaintext: treat them as you would any secret
material, and delete them once imported.

## Record types

Keeper import builds on `xv`'s record types, so an imported login behaves like
any other typed record:

```bash
xv get "Dev Server 1"                  # the password (primary field)
xv get "Dev Server 1" --record         # every field as JSON
xv get "Dev Server 1" --field username
xv ls --format json                    # username/url visible as metadata
```
