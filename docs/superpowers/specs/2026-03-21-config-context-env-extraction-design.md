# Config/Context/Env Commands Extraction Design

> Date: 2026-03-21

## Goal

Extract config, context, cache, and environment profile command execution logic from `commands.rs` into `cli/config_ops.rs`. Single PR, following the established file_ops/vault_ops extraction pattern.

## Current State

- `commands.rs`: 6,377 lines (after helpers + vault extraction)
- Pattern established: `file_ops.rs`, `helpers.rs`, `vault_ops.rs` all successfully extracted

## Functions moving to `cli/config_ops.rs`

### Config commands
| Function | Approx. lines |
|----------|---------------|
| `execute_config_command()` | ~15 |
| `execute_config_show()` | ~148 |
| `execute_config_path()` | ~5 |
| `execute_config_set()` | ~107 |

### Cache commands
| Function | Approx. lines |
|----------|---------------|
| `execute_cache_command()` | ~45 |
| `execute_cache_refresh()` | ~37 |
| `refresh_secrets_list()` | ~27 |
| `refresh_vault_list()` | ~33 |

### Environment profile structs + impl
| Item | Approx. lines |
|------|---------------|
| `EnvironmentProfile` struct + impl | ~30 |
| `EnvironmentProfileManager` struct + impl | ~105 |

### Env commands
| Function | Approx. lines |
|----------|---------------|
| `execute_env_command()` | ~21 |
| `execute_env_list()` | ~40 |
| `execute_env_use()` | ~38 |
| `execute_env_create()` | ~52 |
| `execute_env_delete()` | ~28 |
| `execute_env_show()` | ~33 |
| `execute_env_pull()` | ~136 |
| `execute_env_push()` | ~166 |

### Context commands
| Function | Approx. lines |
|----------|---------------|
| `execute_context_command()` | ~22 |
| `execute_context_show()` | ~33 |
| `execute_context_use()` | ~53 |
| `execute_context_list()` | ~70 |
| `execute_context_clear()` | ~31 |

**Estimated total:** ~1,275 lines moved.

## Cross-domain dependencies

Several functions call into auth/secret/vault domains:
- `execute_env_pull()` and `execute_env_push()` create `DefaultAzureCredentialProvider` and `SecretManager`
- `refresh_secrets_list()` and `refresh_vault_list()` create auth providers and domain managers
- Context commands use `ContextManager` and `VaultContext` from `config/`

These are import dependencies only — they don't create circular references. `config_ops.rs` imports from `auth/`, `secret/`, `vault/`, and `config/` modules, which is the correct dependency direction.

## Visibility

- Entry points called from `Cli::execute()`: `pub(crate)` — `execute_config_command`, `execute_cache_command`, `execute_context_command`, `execute_env_command`
- All other functions: private to the module
- `EnvironmentProfile` and `EnvironmentProfileManager`: `pub(crate)` (may be referenced by other modules)

## What stays in `commands.rs`

- `ConfigCommands`, `CacheCommands`, `ContextCommands`, `EnvCommands` enum definitions — remain with clap definitions
- Dispatch lines in `Cli::execute()`

## Module wiring

Add to `cli/mod.rs`:
```rust
pub(crate) mod config_ops;
```

## Exit criteria

- `commands.rs` contains no config/context/cache/env execution logic (only clap definitions + dispatch)
- Verify: `rg "fn execute_config_|fn execute_context_|fn execute_env_|fn execute_cache_|fn refresh_" src/cli/commands.rs` — no function definitions
- `EnvironmentProfile` and `EnvironmentProfileManager` no longer in `commands.rs`
- One-way dependency: `commands.rs` → `config_ops.rs`
- `cargo check` and `cargo clippy` pass
- CLI behavior unchanged

## Combined impact (after all extractions)

After this PR:
- `commands.rs`: ~5,100 lines (down from original ~9,656)
- `config_ops.rs`: ~1,275 lines
- Remaining in `commands.rs`: secret commands (~2,800), clap definitions (~750), audit/system (~700), tests (~170)

## Non-goals

- Moving clap enum definitions out of `commands.rs`
- Extracting secret commands (next PR)
- Pushing domain logic down into `config/`, `secret/` modules
