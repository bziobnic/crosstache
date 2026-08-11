# `xv backend` — configured-backend lifecycle

`xv` supports three backend types — `local`, `azure`, `aws` — and can have more
than one **configured** at once, even though only one is **active** (the one
commands actually use) at a time. `xv backend add/rm/ls` manage that set
without disturbing which backend is active, so a local store and a cloud vault
can coexist instead of one replacing the other.

- [Backend types](#backend-types)
- [`xv init` vs `xv backend add`](#xv-init-vs-xv-backend-add)
- [`xv backend ls`](#xv-backend-ls)
- [`xv backend add`](#xv-backend-add)
- [`xv backend rm`](#xv-backend-rm)
- [The `[azure]` config block](#the-azure-config-block)
- [One instance per type — `named_backends` for more](#one-instance-per-type--named_backends-for-more)

---

## Backend types

| Type | Storage | Notes |
|------|---------|-------|
| `local` | Age-encrypted files on disk | `[local]` block: `store_path`, `key_file`, `default_vault`, plus optional hardening (`encrypt_metadata`, `opaque_filenames`, `audit`, `git`) |
| `azure` | Azure Key Vault | `[azure]` block (see [below](#the-azure-config-block)) |
| `aws` | AWS Secrets Manager | `[aws]` block: `region`, `profile`, `vault_prefix` |

## `xv init` vs `xv backend add`

- **`xv init`** is the interactive bootstrap. It still walks you through a
  single backend and switches to it — that has not changed. What *has*
  changed: it now **preserves** any other backends already configured instead
  of discarding them. Re-running `xv init` to add a second backend, or to
  reconfigure the one you have, no longer erases the others.
- **`xv backend add <type>`** configures a backend **without** changing which
  one is active. Use it to add a second (or third) backend alongside your
  current one.

```bash
xv backend add local            # interactive prompts for store_path/key_file/vault
xv backend add local --yes      # skip the reconfigure confirmation if already configured
```

`xv backend add` never moves the write target — after it finishes, the newly
configured backend is not active. The command tells you how to switch:

```
Configured backend 'local'
It is not the active backend. Switch with `xv config set backend local`,
or attach one of its vaults with `xv cx add <vault> --backend local`.
```

Re-adding an already-configured backend prompts first (its existing settings
will be replaced) unless `--yes` is passed. In a non-interactive shell without
`--yes`, the confirmation refuses rather than silently overwriting your
settings.

Adding or reconfiguring an `azure` backend always prints a reminder that blob
storage (used for `xv file` operations) was not set up by this command —
`xv init` sets it up interactively, `xv backend add` does not — so add it
later if you need file storage on that vault.

## `xv backend ls`

Read-only. Lists every configured backend, its storage location, and marks
the active one:

```
$ xv backend ls
azure   my-vault                                  (active)
local   /Users/me/.xv/store
```

With nothing configured, it points you at `xv init` instead of printing an
empty table.

## `xv backend rm`

```bash
xv backend rm local              # config only — the store stays on disk
xv backend rm local --purge      # config-only isn't enough — see below
xv backend rm aws --yes          # skip the confirmation prompt
```

By default `rm` only edits the config file: it drops the backend's block
(`[local]`/`[azure]`/`[aws]`) and any multi-vault workspace entries pointing at
it. **Nothing on the backend itself is touched** — the local store stays on
disk, the Azure vault and its secrets are untouched, the AWS secrets are
untouched. Re-adding the backend later (`xv backend add`) picks up wherever
its actual data still lives.

`rm` refuses in four situations:

| Situation | Refusal | What to do |
|---|---|---|
| Backend not configured | Errors, naming what *is* configured | Check `xv backend ls` |
| `--purge` on a non-local backend | Rejected — cloud secrets are never deleted by `xv backend rm` | Use `xv vault delete` for the remote data; `xv backend rm <type>` (no `--purge`) for the config |
| Removing the **active** backend while others remain configured | Refused, pointing at `xv config set backend <other>` | Switch active backend first, then remove |
| Removal would strand the workspace's default vault while other workspace entries remain | Refused, pointing at `xv cx default <alias>` | Pick a new default first |

Removing the active backend **is** allowed when it is the *only* one
configured — that leaves the config backend-less (the next command that needs
a backend will prompt you, same as a fresh install).

### Project vault overlays block `rm` entirely

`xv backend rm` also refuses outright inside any directory governed by an
active `.xv.toml` `[env.X].vaults` overlay — even when, in that specific case,
no workspace entry would actually have been pruned. This is fail-closed by
design: a project overlay *replaces* the context workspace for that directory,
so `rm` cannot safely reason about "what workspace state exists here" at all.
It is the same guard `xv cx rm` already applies, in the same place in the
sequence. There is no override flag. If you hit this, either run the command
from a directory without an active `[env.X].vaults` overlay, or edit
`.xv.toml` directly.

### `--purge` is unrecoverable — read this before using it

**`--purge` deletes the local store, the age identity (`key_file`), and the
recipients file. Once the age identity is gone, every secret it protected is
unrecoverable — by anyone. There is no undo, no trash, and no backup.** Treat
`--purge` the way you would treat `rm -rf` on the only copy of your secrets,
because that is exactly what it is.

Guards that limit the blast radius:

- **Local only.** `--purge` on `azure` or `aws` is rejected — see the refusal
  table above.
- **Refuses a `store_path` that doesn't look like an xv store.** It must
  contain a `vaults/` directory (or be empty/missing). This catches a
  misconfigured `store_path` pointing at `$HOME` or a documents folder before
  it deletes anything.
- **Refuses a `key_file` that isn't a real age identity.** A stale or
  misconfigured path — an SSH key, a random document — is refused outright
  rather than silently skipped, so you never come away believing "the key" was
  deleted when it wasn't.
- **Refuses in a non-interactive shell without `--yes`.** The confirmation
  prompt spells out exactly what is being destroyed and that it cannot be
  undone.

Ordering caveat, stated honestly: the deletion happens **before** the config
file is saved. If the config save then fails for some unrelated reason (disk
full, permissions), the data is already gone while the config still lists the
backend — the next use of that backend would create a fresh, empty store at
the same path. This is a narrow window between two operations, not a
transaction; there is no rollback of the delete.

## The `[azure]` config block

`xv`-written configs carry an `[azure]` block:

```toml
[azure]
subscription_id  = "00000000-0000-0000-0000-000000000000"
tenant_id        = "11111111-1111-1111-1111-111111111111"
default_vault    = "my-vault"
resource_group   = "Vaults"
location         = "eastus"
```

Configs from before this block existed keep working unchanged: the top-level
`subscription_id` / `tenant_id` / `default_vault` / `default_resource_group` /
`default_location` fields remain the legacy fallback, and `Config::azure_settings()`
reads the `[azure]` block when present, falling back to those top-level fields
otherwise.

Whenever `xv` itself writes an `[azure]` block (via `xv init` or
`xv backend add azure`), it also mirrors `subscription_id` and `tenant_id` to
the top level, so an `xv`-generated config is always valid both ways.

**This is not a hand-authoring recommendation.** The block is read correctly
either way — `xv backend ls` and other config/lifecycle commands resolve the
vault out of an `[azure]`-only config just fine, because they go through
`azure_settings()`. But `Config::validate()` reads the top-level
`subscription_id`/`tenant_id` directly and never consults `azure_settings()`,
and validation runs for any command that actually needs a backend. So if you
hand-write a config with *only* an `[azure]` block and leave the top-level
`subscription_id`/`tenant_id` empty, `xv backend ls` still works, but
`xv list` (or any other command that talks to the backend) fails with:

```
error[xv-config-invalid]: Configuration error: Subscription ID is required
```

This is a known gap, not a supported form. Keep the `[azure]` block as what
`xv` itself writes and reads, and keep the top-level `subscription_id`/
`tenant_id` present in any config you edit by hand.

## One instance per type — `named_backends` for more

`xv backend` manages exactly one instance each of `local`, `azure`, and `aws`
— the `[local]`, `[azure]`, and `[aws]` blocks in `xv.conf`. If you need more
than one Azure vault or AWS account configured as distinct, independently
switchable backends (e.g. two subscriptions, or prod/staging AWS accounts),
that is what `named_backends` is for — an existing, more advanced
multi-instance mechanism that `xv backend` does not touch, read, or replace:

```toml
backend = "aws-east"
[named_backends.aws-east]
type = "aws"
region = "us-east-1"
[named_backends.aws-west]
type = "aws"
region = "us-west-2"
```

Switch between named backends by setting `backend` to the block's key. See
the "AWS Secrets Manager backend" section of the main README for the full
example.
