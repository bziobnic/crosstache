# `xv doctor` — bootstrap-safe config recovery

Diagnose and repair the global `xv.conf` when ordinary commands (including
`xv ui`) cannot start because configuration loading fails.

Doctor is dispatched **before** normal config deserialization, so it remains
available for exactly the corrupt or outdated files it must fix.

## Quick reference

```bash
xv doctor                 # diagnose / auto-repair global xv.conf
xv config path            # show the file doctor inspects
```

Exit `0` when the file is missing (defaults are usable), already healthy, or
fully repaired. Exit `3` (`xv-config-invalid`) when anything remains that
requires a person.

`--format json` and `--format yaml` are rejected up front so a machine-readable
error envelope stays a single valid document (doctor's human report is never
mixed into that envelope).

## Scope

| In scope | Out of scope |
|----------|--------------|
| Global config from `xv config path` (`xv.conf`) | Project `.xv.toml` / env profiles |
| Missing required scalar fields | Credentials, tokens, network reachability |
| Missing required `[blob_config]` fields when that table exists | Backend service health (Key Vault, S3, …) |
| TOML or legacy JSON parse of the global file | Guessing invalid types / enum values |
| Semantic checks for the selected backend after repair | Rewriting healthy JSON solely to convert to TOML |

## What it repairs automatically

Only deterministic schema omissions from `Config::default()` /
`BlobConfig::default()`:

**Top-level:** `debug`, `subscription_id`, `default_vault`,
`default_resource_group`, `default_location`, `tenant_id`, `output_json`,
`no_color`

**When `[blob_config]` exists:** `storage_account`, `container_name`,
`enable_large_file_support`, `chunk_size_mb`, `max_concurrent_uploads`

Comments, formatting, unknown keys, and existing user values are preserved for
editable TOML. There is currently no legacy-key rename table; type mismatches
and invalid enums are reported, never overwritten.

## Backup and write semantics

Before any change, doctor writes an exact sibling backup of the original bytes:

```text
xv.conf.backup-20260807T153012Z
```

- Create-new only (never overwrites an existing backup name)
- Does not follow a final-component symlink
- Private file permissions
- Atomic replace of `xv.conf`; backup/write failure is fatal (no false success)

Deterministic repairs can persist even when a separate semantic problem remains
(for example, restoring missing defaults while still reporting that an Azure
`subscription_id` is empty). The original is always recoverable from the backup.

## Output shape

Human-readable checks in execution order with labels `ok`, `fixed`, and
`error`:

```text
Configuration: /home/you/.config/xv/xv.conf
fixed: Restored missing configuration field 'debug'.
…
ok: Configuration file '/home/you/.config/xv/xv.conf' is valid.
Backup: /home/you/.config/xv/xv.conf.backup-20260807T153012Z
```

Missing file (no write, exit 0):

```text
Configuration: …/xv.conf
ok: Configuration file '…/xv.conf' does not exist.
ok: Configuration defaults are usable; no file was created.
```

Unresolved syntax/schema errors print an `action:` line pointing at the file.
Diagnostics never echo credential-shaped or invalid field *values*.

## Semantic checks (reported, not guessed)

After deserialize + the same environment overrides used at startup (overrides
are **not** persisted):

| Selected backend | Unresolved when |
|------------------|-----------------|
| Azure | `subscription_id` or `tenant_id` empty → suggests `xv config set …` |
| AWS | missing `[aws]` / named AWS entry, or no region (`[aws].region`, `AWS_REGION`, or `AWS_DEFAULT_REGION`) |
| Unavailable backend | `backend` is not a compiled built-in and not an exact `[named_backends]` key |
| Local | no extra semantic gate beyond a valid document |

## Common pitfalls

| Symptom | Cause / fix |
|---------|-------------|
| Ordinary commands fail; `xv doctor` works | Expected — doctor bypasses normal load. Fix the reported issues, then retry. |
| Exit 3 after `fixed:` lines | Schema repaired, but Azure/AWS semantics still incomplete — follow the printed `xv config set` / region actions. |
| `debug = "false"` (string) unchanged | Type error: doctor will not coerce. Edit the field, then re-run. |
| Syntax error, file untouched | Unparseable TOML/JSON cannot be auto-fixed. Edit the indicated location. |
| Healthy JSON left as JSON | By design. Format conversion alone is not a repair. |
| `--format json doctor` | Rejected; use plain output, or fix config then use JSON on other commands. |
| Expecting `.xv.toml` repair | Out of scope. Edit the project file or see [`env-profiles.md`](env-profiles.md). |

## Related

- Design: [`superpowers/specs/2026-08-07-xv-doctor-design.md`](superpowers/specs/2026-08-07-xv-doctor-design.md)
- Exit codes: [`exit-codes.md`](exit-codes.md) (doctor unresolved → `3`)
- Config hierarchy overview: [`FEATURES.md`](FEATURES.md#configuration)
