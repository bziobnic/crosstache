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
| `notes`                      | `note` tag                            |
| `folders[].folder`           | `folder` tag, `\` rewritten to `/`    |
| `custom_fields`              | ordinary tags                         |
| `custom_fields.$oneTimeCode` | record envelope, `one-time-code` field |

A TOTP seed is a second authentication factor, so `$oneTimeCode` is stored as
encrypted secret material rather than as a tag. It never appears in `xv ls`:

```bash
xv get Facebook --field one-time-code
```

Titles pass through `xv`'s usual name sanitization — `Dev Server 1` is stored
under the secret name `Dev-Server-1` with the original title preserved in the
`original_name` tag, so it still displays and round-trips as `Dev Server 1`.

### Records without a login or password

Not every Keeper record is a login, and none of these are dropped silently:

| Keeper record has            | Result                                              |
|------------------------------|-----------------------------------------------------|
| `login` + `password`         | typed `login` record                                |
| `password`, no `login`       | plain secret; the password is the value             |
| `notes` only (a secure note) | plain secret; the notes are the value, with a warning |
| neither                      | refused, and reported as a failure                  |

A record with no `login` cannot become a typed `login` record, because that
type requires a username. It degrades to a plain secret instead — and if it
also carries a `$oneTimeCode`, it is refused rather than written, since a plain
secret has one value slot (already holding the password) and the seed would
otherwise have to go into a plaintext tag.

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

**Duplicate titles.** Legal in Keeper, where folders disambiguate. Two titles
that resolve to the same secret name are refused rather than silently
overwriting each other — rename one in Keeper and re-import.

## Records that get refused

Refusals are per-record: everything else in the file still imports, the reason
is printed, and the command exits non-zero so a scripted migration cannot
silently lose secrets.

- **Nothing storable** — no password and no notes.
- **A name collision** with an earlier record in the same file.
- **An unusable folder path** — `xv` caps folder names at 50 characters and
  nesting at 10 levels.
- **Too many tags for the backend.** Azure Key Vault allows 15 tags per secret,
  and `xv`'s own bookkeeping uses some of them, so a record with many
  `custom_fields` can exceed the cap. This is checked *before* any write, so it
  fails cleanly instead of half-writing. The same file imports without
  complaint into the local backend, which has no tag limit.

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
