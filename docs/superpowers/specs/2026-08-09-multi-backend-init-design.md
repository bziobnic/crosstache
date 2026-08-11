# Multi-Backend `xv init`: Select Several Backends in One Setup

**Date:** 2026-08-09
**Status:** Approved design, not yet implemented
**Depends on:** multi-vault workspaces (`src/workspace/`, Phase A), the shared
setup service (`src/config/setup.rs`, `SetupRequest` / `build_setup_config`),
named backends (`Config.named_backends`, v0.10+)

## Motivation

`xv init` configures exactly one backend. `run_interactive_setup` asks a
single-select "which secrets backend?", then early-returns into
`run_local_setup` / `run_aws_setup`, each of which builds a config from
`Config::default()` and saves it. A user who wants a local store *and* a work
Key Vault runs init, gets one, and then hand-edits `xv.conf` for the other.

This design lets init configure **several backends in one run** and leaves the
user with a workspace already attached, so `xv ls` shows the union and
`alias:secret` addressing works immediately.

```
$ xv init
Which backends would you like to configure?  (space to toggle)
 [x] Local (age-encrypted files, offline, no cloud account needed)
 [ ] Azure Key Vault
 [x] AWS Secrets Manager
...
Which should be the default write target? › local

Configured 2 backends:
  local   store ~/.xv/store        vault default   alias default-local  (default)
  aws     region us-east-1         vault default   alias default-aws
```

## Decisions (settled during brainstorm)

1. **All three backends in scope**, with `local` pre-selected in the
   multi-select (it needs no cloud account).
2. **Init auto-attaches a workspace** — not just config blocks. One entry per
   configured backend.
3. **Per-backend validation gate.** A backend is persisted only if its
   configuration validates *and* the backend actually initializes. Otherwise it
   is dropped, and the ones that worked are still saved.
4. **The default write target is prompted for**, defaulting to `local`.
5. **Azure gets a real `[azure]` config block**, so all three backends are
   symmetric.
6. **Partial init exits 53** (`xv-init-partial`).

## Architecture

### The `build_setup_config` problem

`build_setup_config(request, base) -> Config` looks like it folds a request
into an existing config, but every arm is deliberately **mutually exclusive**:

| Arm     | Clears                                                                   |
|---------|--------------------------------------------------------------------------|
| `Local` | `aws`, `subscription_id`, `tenant_id`, `resource_group`, `location`, `blob_config` |
| `Azure` | `local`, `aws`                                                            |
| `Aws`   | `local`, `subscription_id`, `tenant_id`, `resource_group`, `location`, `blob_config` |

Folding three requests through it therefore leaves only the last one. The
function is split rather than reused:

- **`apply_backend(request, &mut Config)`** — validates and writes *only* that
  backend's own block. Never clears a sibling.
- **`build_setup_config(request, base)`** — unchanged signature and unchanged
  behavior, now expressed as *clear every backend block, then `apply_backend`*.

Existing callers (the desktop setup service at `setup.rs:123`, single-backend
init, and the ~15 tests) keep exclusive semantics untouched. Multi-backend init
calls `apply_backend` in a loop.

### Phases

**Collect.** Multi-select the backends, then run each backend's existing prompt
flow to produce a `SetupRequest`. Pure prompting, no side effects.

**Validate & apply**, per backend, into a *clone* of the accumulator:

1. Shape validation — the existing `required(...)`, `persisted_path(...)`,
   `ResolvedLocalConfig::validate()` checks inside `apply_backend`.
2. Live initialization — local writes the age key and store dirs; Azure probes
   CLI/subscription; AWS resolves region and credentials.

The clone merges back into the accumulator only when both pass. This is why no
"unapply" is needed. On failure the clone is discarded and
`(backend, reason, remediation)` is recorded.

Live init runs immediately after that backend's prompts, not in a batch at the
end, so a bad value is reported near where it was typed.

**Finish.** Prompt for the default write target among the survivors, derive
aliases, build and validate the workspace, then write once.

Side effects already committed by a *successful* backend are kept — a written
age key belongs to a config that is about to be saved.

If **every** backend fails, nothing is written and the command takes the normal
error path.

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

Azure previously had no block of its own: its settings *are* the top-level
`subscription_id` / `tenant_id` / `default_vault` / `default_resource_group` /
`default_location` fields, and `NamedBackendEntry` omits Azure. Everything
coexists except `default_vault`, which is global and single-valued — `local` and
`aws` keep their own vault in `local.default_vault` / `aws.default_vault`, but
Azure's had nowhere else to live.

- **Read precedence:** `config.azure` when present, else the top-level fields.
  This fallback is the back-compat guarantee — a config written before this
  change has no `[azure]` block and must behave identically.
- **Write:** init writes the `[azure]` block always, and the top-level mirror
  only when Azure is the active backend.

`NamedBackendEntry` is unchanged. Multi-tenant Azure becomes possible later but
is out of scope.

`Config::validate()` needs no change: it only validates the *active* backend, so
several populated blocks coexist. That is also why the per-backend gate above
must validate during init — an inactive backend is never validated on load.

### Alias derivation

`Workspace::validate` rejects an alias that collides with a registry backend
name, to keep `xv://alias/...` unambiguous. **The obvious aliases `local`,
`azure`, and `aws` are therefore all illegal.**
`WorkspaceEntryConfig::resolved_alias()` already defaults to the vault name, so
init derives from that:

1. Start with the backend's vault name.
2. If it collides with a backend name or an already-taken alias, use
   `<vault>-<backend>` (`default-local`, `default-aws`).
3. If still taken, append `-2`, `-3`, …

Derivation is silent; the final summary prints every alias. Renaming is existing
`xv cx` territory rather than another init prompt.

### Persistence

One `Config` write via `atomic_save_config`, plus one `WorkspaceState` write to
the context store. The workspace has one entry per surviving backend with
`default: true` on the chosen target, and is run through `Workspace::validate`
**before either write** — an alias bug fails the finish phase rather than
persisting a workspace later commands reject.

## CLI surface

`xv init` gains the multi-select. No new flags are needed for the headline
feature.

`xv init --add <backend>` **is in scope for this change**, not a follow-up — the
failure path prints it as remediation, so it has to exist. It falls out of the same machinery: load the existing
config as the accumulator instead of `Config::default()`, run one backend's
collect/validate/apply, append a workspace entry, save. It is also the
remediation printed for any backend that fails, so a failed AWS step ends with a
copy-pasteable next step instead of "run init again and redo Azure".

Non-TTY behavior is unchanged: `xv init` stays interactive and errors without a
TTY. The non-interactive path remains `SetupRequest` plus the setup service,
which is already multi-call capable.

## Error handling

New `CrosstacheError` variant, code `xv-init-partial`, exit **53**:

| Outcome                  | Exit | Written                        |
|--------------------------|------|--------------------------------|
| All selected succeeded   | `0`  | config + workspace             |
| Some succeeded           | `53` | config + workspace (survivors) |
| None succeeded           | the underlying error's own code | nothing |

"None succeeded" deliberately does *not* get its own code: with nothing saved
there is no partial state to signal, and the single backend's real failure
(e.g. `20` authentication, `3` config) is more useful than a generic one.

53 is free (50 scan, 51 rotation, 52 audit). `docs/exit-codes.md` calls these
codes "part of the scripting contract", so the change requires a registry row
and a case in `test_exit_code_families`.

Each failure reports the backend, the reason, and the `xv init --add <backend>`
remediation.

## Testing

The risk surface is "which backends ended up in the config, and which didn't",
which requires driving the prompts. `InteractivePrompt` is a concrete struct
with no seam, and `init.rs` has 4 tests across 1,221 lines — the collect phase
is currently untestable.

**A `Prompter` trait** is introduced over the methods init uses (`confirm`,
`input_text`, `input_text_validated`, `select`, and a new `multi_select`).
`InteractivePrompt` implements it unchanged; tests use a scripted fake that
returns queued answers and records the prompts it was asked. This mirrors
`src/schedule/`, which is tested against a fake `CommandRunner` so no test
registers a real job — an existing pattern, not a new one.

Coverage, in priority order:

1. **Additive semantics** — apply local, then aws, then azure; assert all three
   blocks survive. This is the exact regression the decomposition prevents.
2. **`build_setup_config` unchanged** — the existing tests pass *unmodified*.
   This is the back-compat proof for the desktop setup service, not a new test.
3. **`[azure]` back-compat** — a TOML with only top-level fields resolves Azure
   identically to one with an `[azure]` block, and round-trips without gaining
   or losing fields.
4. **The failure gate** — a backend whose live init fails is absent from *both*
   the saved config and the workspace, while survivors are present; all-fail
   writes nothing.
5. **Alias derivation** — collision with a backend name, and two backends
   sharing a vault name; assert the derived set passes `Workspace::validate`.
6. **Exit code** — partial init yields 53 / `xv-init-partial`.

Live-init tests use the local backend against a `tempdir` (real age keys, real
files). Azure and AWS live init sit behind a stub initializer that can be told to
fail; no test touches a cloud account.

## Out of scope

- Multi-tenant Azure via `NamedBackendEntry::Azure`.
- Non-interactive multi-backend flags (`--backend local --backend aws`).
- Reconfiguring or removing an already-configured backend (`xv init --add` only
  adds).
- Changing how `xv cx` manages workspaces after init.
