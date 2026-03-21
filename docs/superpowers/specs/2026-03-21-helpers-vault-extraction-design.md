# Helpers + Vault Commands Extraction Design

> Date: 2026-03-21

## Goal

Continue the `commands.rs` decomposition by extracting shared helper functions into `cli/helpers.rs` and vault command execution logic into `cli/vault_ops.rs`. Two PRs, each independently compilable, following the pattern established by the file_ops extraction.

## Current State

- `commands.rs`: 7,596 lines (down from ~9,656 after file_ops extraction)
- `helpers.rs`: 17 lines (only `parse_key_val`)
- File ops extraction complete and merged

## PR 1: Shared Helpers Extraction

### Functions moving to `cli/helpers.rs`

| Function | Lines | Used by |
|----------|-------|---------|
| `copy_to_clipboard()` | ~20 | secret get, find, gen |
| `linux_clipboard_copy()` | ~60 | copy_to_clipboard |
| `schedule_clipboard_clear()` | ~50 | secret get, find, gen |
| `mask_secrets()` | ~12 | secret run |
| `execute_custom_generator()` | ~75 | generate_random_value (sole caller) |
| `generate_random_value()` | ~37 | secret rotate, gen |
| `extract_claims_from_token()` | ~110 | whoami |
| `format_cache_size()` | ~16 | cache status |

**Estimated total:** ~380 lines moved.

### Functions staying in `commands.rs`

- `get_version()`, `get_help_template()`, `should_hide_options()`, `get_build_info()` — clap/CLI-specific, coupled to Cli struct definition
- `display_cached_secret_list()` — tightly coupled to secret list rendering; moves with secrets in a future extraction

### Visibility

All moved functions: `pub(crate)`.

### Module wiring

`cli/mod.rs` already exports `helpers`. No changes needed to mod.rs. `commands.rs` will need new `use crate::cli::helpers::{...}` imports for the moved functions.

### Exit criteria

- `helpers.rs` contains all listed functions
- `commands.rs` imports from `crate::cli::helpers::*` for these functions
- No logic changes — pure mechanical move
- `cargo check` and `cargo clippy` pass

## PR 2: Vault Commands Extraction

### New file: `cli/vault_ops.rs`

### Functions moving to `vault_ops.rs`

| Function | Approx. lines |
|----------|---------------|
| `execute_vault_command()` | ~140 |
| `execute_vault_create()` | ~50 |
| `execute_vault_list()` | ~40 |
| `execute_vault_delete()` | ~15 |
| `execute_vault_info()` | ~25 |
| `execute_vault_restore()` | ~10 |
| `execute_vault_purge()` | ~12 |
| `execute_vault_export()` | ~210 |
| `execute_vault_import()` | ~195 |
| `execute_vault_update()` | ~40 |
| `execute_vault_share()` | ~105 |
| `execute_vault_info_from_root()` | ~27 |

**Estimated total:** ~870 lines moved (includes vault share sub-handlers).

### What stays in `commands.rs`

- `VaultCommands`, `VaultShareCommands` enum definitions — remain with clap definitions
- Single dispatch line in `Cli::execute()`: `vault_ops::execute_vault_command()`

### Dependencies

- `VaultManager`, `Config`, auth provider — from existing crate modules
- `VaultCommands`, `VaultShareCommands` — from `commands.rs` clap definitions
- No dependency on `cli/helpers.rs` (vault ops don't use clipboard/masking)
- No cross-domain calls into secret or file logic

### Module wiring

Add to `cli/mod.rs`:
```rust
pub mod vault_ops;
```

### Exit criteria

- `commands.rs` contains no vault operation implementations (only clap definitions + dispatch)
- `vault_ops.rs` contains all `execute_vault_*` functions including `execute_vault_info_from_root`
- Verify: `rg "execute_vault_" src/cli/commands.rs` matches only imports and dispatch lines
- One-way dependency: `commands.rs` → `vault_ops.rs`
- No circular imports
- `cargo check` and `cargo clippy` pass
- CLI behavior unchanged

## Combined impact

After both PRs:
- `commands.rs`: ~6,350 lines (down from 7,596)
- `helpers.rs`: ~400 lines
- `vault_ops.rs`: ~890 lines

## Risks and mitigations

- **Risk:** Shared helpers have platform-specific code (clipboard). **Mitigation:** Move entire platform blocks together; `#[cfg]` attributes travel with the functions.
- **Risk:** `VaultCommands` enum stays in `commands.rs` while execution moves to `vault_ops.rs`. **Mitigation:** Import via `use super::*` or `crate::cli::commands::VaultCommands`. Clap definitions extraction is a separate future step.
- **Risk:** Merge conflicts with parallel work. **Mitigation:** One PR at a time, each small and fast to review.

## Non-goals

- Moving clap enum definitions out of `commands.rs` (future work)
- Pushing domain logic down into `vault/`, `secret/` modules (second pass)
- Extracting secret, config, or env commands (subsequent PRs)
- Moving whoami-only helpers (`get_tenant_name`, `get_current_subscription_details`) to `auth/` — reasonable future candidate but single-caller today
