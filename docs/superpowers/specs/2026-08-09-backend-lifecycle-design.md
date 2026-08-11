# Backend Lifecycle: `xv init` Plus `xv backend add` / `rm` / `ls`

**Date:** 2026-08-09
**Revised:** 2026-08-10 — scope changed; see "Change of direction" below.
**Status:** Approved design, not yet implemented
**Depends on:** multi-vault workspaces (`src/workspace/`, Phase A), the shared
setup service (`src/config/setup.rs`, `SetupRequest` / `build_setup_config`),
stale-workspace-entry messaging (#404), bootstrap-safe config recovery (v0.36.0)

## Change of direction

The first draft of this spec had `xv init` multi-select several backends in one
run and auto-attach a workspace. That is **withdrawn**. `xv init` keeps its
single-select shape, and backend lifecycle moves to a dedicated `xv backend`
command group.

This removes the riskiest machinery from the design. With one backend per
invocation there is no partial state, so the clone-and-merge accumulator, the
`(backend, reason, remediation)` collection, and exit code **53
(`xv-init-partial`) are all dropped** — a failed command simply fails and writes
nothing. `named_backends` is also untouched, so `NamedBackendEntry` needs no
Azure variant.

What survives is what makes backends *coexist* at all: the `apply_backend`
decomposition, the `[azure]` block, per-backend validation, and the `Prompter`
seam.

## Motivation

`xv init` configures exactly one backend and there is no way to add a second, or
to remove one, short of hand-editing `xv.conf`. A user who starts local and
later adds a work Key Vault has no supported path.

```bash
xv init                       # first-time bootstrap; pick one backend
xv backend add azure          # add a second later
xv backend ls                 # what is configured, and which is active
xv backend rm azure           # drop it (config only)
xv backend rm local --purge   # drop it AND delete the store + keys
```

## Decisions (settled during brainstorm)

1. **`xv init` selects one backend type** and runs that backend's setup, as
   today. It delegates to the same code path as `xv backend add`, then marks the
   chosen backend active.
2. **New top-level `xv backend` group** with `add`, `rm`, `ls`. Top-level rather
   than under `xv config`, and distinct from `xv cx add`, which attaches a
   *vault* to a workspace rather than configuring a backend.
3. **One instance per backend type.** `xv backend add local` writes the
   canonical `[local]` block; re-adding reconfigures after confirmation.
   Multi-instance via `named_backends` is out of scope.
4. **`rm` is config-only by default.** `--purge` opts into deleting local
   on-disk data behind a typed confirmation. Cloud secrets are never deleted.
5. **Removal refuses rather than silently relocating.** Removing the active
   backend, or stranding the workspace's default vault, aborts with instructions.
6. **Azure gets a real `[azure]` config block**, so all three backends coexist.

## Architecture

### The `build_setup_config` problem

`build_setup_config(request, base) -> Config` looks like it folds a request into
an existing config, but every arm is deliberately **mutually exclusive**:

| Arm     | Clears                                                                   |
|---------|--------------------------------------------------------------------------|
| `Local` | `aws`, `subscription_id`, `tenant_id`, `resource_group`, `location`, `blob_config` |
| `Azure` | `local`, `aws`                                                            |
| `Aws`   | `local`, `subscription_id`, `tenant_id`, `resource_group`, `location`, `blob_config` |

`xv backend add` is additive by definition, so reusing this as-is would make
`xv backend add aws` silently wipe an existing local configuration. The function
is split:

- **`apply_backend(request, &mut Config)`** — validates and writes *only* that
  backend's own block. Never clears a sibling.
- **`build_setup_config(request, base)`** — unchanged signature and unchanged
  behavior, now expressed as *clear every backend block, then `apply_backend`*.

Existing callers (the desktop setup service at `setup.rs:123`, `xv init`, and the
~15 tests) keep exclusive semantics untouched. `xv backend add` calls
`apply_backend`.

### Command flow

`xv backend add <type>` and `xv init` share one path:

1. **Collect** — run that backend's existing prompt flow to produce a
   `SetupRequest`. Pure prompting, no side effects.
2. **Validate** — `apply_backend` runs the existing `required(...)`,
   `persisted_path(...)`, `ResolvedLocalConfig::validate()` checks against a
   working copy of the loaded config.
3. **Initialize** — stand the backend up for real: local writes the age key and
   store dirs; Azure probes CLI/subscription; AWS resolves region and
   credentials.
4. **Save** — one `atomic_save_config` write.

Any step failing aborts the command and writes no config. `xv init` additionally
sets `Config.backend` to the chosen type; `xv backend add` leaves the active
backend alone.

`xv init` run against a config that already has backends keeps its current
behavior — it reconfigures the selected type and makes it active, after the same
confirmation `add` uses. It is a bootstrap verb, not a reset: backends other
than the selected one are left untouched, which is a behavior change from today
only because coexistence is new.

The difference from the withdrawn draft: steps 2–4 handle exactly one backend,
so there is no accumulator and no rollback.

## Config model

### New `[azure]` block

`Config.azure: Option<AzureConfig>`:

```rust
pub struct AzureConfig {
    pub subscription_id: Option<String>,
    pub tenant_id: Option<String>,
    pub default_vault: Option<String>,
    pub resource_group: Option<String>,
    pub location: Option<String>,
}
```

Azure has no block of its own today: its settings *are* the top-level
`subscription_id` / `tenant_id` / `default_vault` / `default_resource_group` /
`default_location` fields. Everything coexists except `default_vault`, which is
global and single-valued — `local` and `aws` keep their own vault in
`local.default_vault` / `aws.default_vault`, but Azure's has nowhere else to
live. Adding Azure alongside local would otherwise have to overwrite it.

- **Read precedence:** `config.azure` when present, else the top-level fields.
  This fallback is the back-compat guarantee — a config written before this
  change has no `[azure]` block and must behave identically.
- **Write:** the `[azure]` block always; the top-level mirror only when Azure is
  the active backend.

`Config::validate()` needs no change: it validates only the *active* backend, so
several populated blocks coexist. That is also why validation must happen during
`add` — an inactive backend is never validated on load.

## `xv backend` command group

### `add <local|azure|aws>`

Runs the shared flow above. If the type is already configured, confirms before
reconfiguring (`--yes` to skip). Does not change the active backend, and does not
attach a workspace entry — that stays `xv cx add`.

### `ls`

Lists configured backends: type, whether active, and each one's vault/store
location. Reads config only; never contacts a backend.

### `rm <type>`

Config-only by default: drops the backend's block and any workspace entries
pointing at it, then prints where the data still lives so re-adding with the same
paths is obviously possible.

**Refusals** (all abort before writing anything):

| Condition | Behavior |
|---|---|
| Type not configured | Error naming what *is* configured |
| It is the active backend, others remain | Refuse; name the alternatives and point at `xv config set backend <other>` |
| It is the active backend and the only one | Allowed with confirmation; config is left backend-less, which bootstrap-safe recovery (v0.36.0) and `xv doctor` already handle |
| Removal would strand the workspace default, others remain | Refuse; point at `xv cx default <alias>` |

Refusing rather than auto-promoting is deliberate: silently relocating where
`xv set` writes is the kind of thing users discover later, in the wrong vault.

**`--purge`** additionally deletes the local store and age key. This permanently
destroys every secret in that store — there is no recovery, because the key is
part of what is deleted. Therefore:

- `local` only. `xv backend rm aws --purge` is **rejected**, not ignored: a user
  typing it means "delete my secrets", and silently doing less than asked is
  worse than erroring.
- Requires typing the backend name to confirm; `--yes` is required instead in
  non-TTY.
- Refuses when the resolved store path does not look like an xv store, guarding
  against a misconfigured path aimed at `$HOME`.
- Uses the existing no-follow file helpers, consistent with the rest of the
  local backend's filesystem handling.

## Testing

The risk surface is "which backends are in the config afterwards", which requires
driving the prompts. `InteractivePrompt` is a concrete struct with no seam, and
`init.rs` has 4 tests across 1,221 lines — the collect phase is untestable today.

**A `Prompter` trait** is introduced over the methods used (`confirm`,
`input_text`, `input_text_validated`, `select`). `InteractivePrompt` implements
it unchanged; tests use a scripted fake returning queued answers. This mirrors
`src/schedule/`, which is tested against a fake `CommandRunner` so no test
registers a real job — an existing pattern, not a new one.

Coverage, in priority order:

1. **Additive semantics** — `add` local, then aws, then azure; assert all three
   blocks survive. This is the exact regression the decomposition prevents, and
   the single most valuable test here.
2. **`build_setup_config` unchanged** — the existing tests pass *unmodified*.
   The back-compat proof for the desktop setup service, not a new test.
3. **`[azure]` back-compat** — a TOML with only top-level fields resolves Azure
   identically to one with an `[azure]` block, and round-trips without gaining or
   losing fields.
4. **Failed `add` writes nothing** — a backend whose initialization fails leaves
   the config byte-identical, including when other backends are configured.
5. **Every `rm` refusal** — one test per row of the refusal table, asserting both
   the error and that config and workspace are untouched.
6. **`rm` cleans workspace entries** — entries pointing at the removed backend
   are gone; unrelated entries and the default survive.
7. **`--purge` guards** — rejected for non-local; refuses a store path outside an
   xv store; deletes store and key when confirmed.

Local-backend tests use a `tempdir` with real age keys and real files. Azure and
AWS initialization sits behind a stub that can be told to fail; no test touches a
cloud account.

## Out of scope

- Multiple instances of one backend type (`named_backends`,
  `NamedBackendEntry::Azure`).
- Multi-select during `xv init`, workspace auto-attach, and exit code 53 — all
  withdrawn, see "Change of direction".
- Deleting cloud-side resources (`xv vault delete` territory).
- Changing how `xv cx` manages workspaces.
