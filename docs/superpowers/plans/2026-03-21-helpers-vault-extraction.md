# Helpers + Vault Commands Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract shared helper functions into `cli/helpers.rs` and vault command execution logic into `cli/vault_ops.rs`, reducing `commands.rs` by ~1,250 lines.

**Architecture:** Two sequential PRs following the file_ops extraction pattern. PR1 moves cross-cutting helpers; PR2 moves vault execution handlers. Both are pure mechanical moves — no logic changes. One-way dependency from `commands.rs` into the new modules.

**Tech Stack:** Rust, clap derive macros, tokio async, existing `VaultManager`/`SecretManager` APIs, `cargo check`/`cargo clippy` workflow.

**Spec:** `docs/superpowers/specs/2026-03-21-helpers-vault-extraction-design.md`

---

## Task 1: Create branch and extract helper functions to `cli/helpers.rs`

**Files:**
- Modify: `src/cli/helpers.rs`
- Modify: `src/cli/commands.rs`

- [ ] **Step 0: Create feature branch**

```bash
git checkout main && git pull origin main
git checkout -b refactor/cli-helpers-extraction
```

- [ ] **Step 1: Copy helper functions into `helpers.rs`**

Move these functions from `commands.rs` into `helpers.rs`, making each `pub(crate)`. Keep the existing `parse_key_val` function at the top.

Functions to move (in order they appear in `commands.rs`):
1. `format_cache_size` (lines 2294–2302)
2. `TokenClaims` struct (lines 3753–3759) and `extract_claims_from_token` (lines 3762–3802) — the struct must be `pub(crate)` so `commands.rs` callers can access its fields
3. `copy_to_clipboard` (lines 4486–4504)
4. `linux_clipboard_copy` (lines 4509–4570) — include the `#[cfg(target_os = "linux")]` attribute on line 4509 with the function
5. `schedule_clipboard_clear` (lines 4575–4627)
6. `generate_random_value` (lines 4783–4819) — already `pub(crate)`
7. `execute_custom_generator` (lines 4822–4893)
8. `mask_secrets` (lines 5232–5244)

Add required imports at the top of `helpers.rs`:
```rust
use crate::cli::commands::CharsetType;
use crate::error::{CrosstacheError, Result};
use zeroize::Zeroizing;
```

Additional imports needed by specific functions (use inner `use` where already present, or add to top):
- `extract_claims_from_token`: `base64`, `serde::Deserialize`, `serde_json`
- `copy_to_clipboard`/`schedule_clipboard_clear`: `std::process::Command`
- `generate_random_value`: `rand::prelude::*`
- `execute_custom_generator`: `std::process::{Command, Stdio}`, `std::io::Read`
- `mask_secrets`: `zeroize::Zeroizing`

- [ ] **Step 2: Remove moved functions from `commands.rs`**

Delete each function body from `commands.rs` at the line ranges listed above.

- [ ] **Step 3: Add imports in `commands.rs` for the moved functions**

Add to the top of `commands.rs` (or near existing `use crate::cli::helpers::parse_key_val`):
```rust
use crate::cli::helpers::{
    copy_to_clipboard, extract_claims_from_token, format_cache_size,
    generate_random_value, mask_secrets, schedule_clipboard_clear, TokenClaims,
};
```

Note: `execute_custom_generator` and `linux_clipboard_copy` are NOT imported here — their only callers (`generate_random_value` and `copy_to_clipboard` respectively) are also moving to `helpers.rs`, so they remain module-private there.

- [ ] **Step 4: Compile check**

Run: `cargo check`
Expected: Passes with no errors. Warnings about unused imports are acceptable at this stage.

- [ ] **Step 5: Clippy check**

Run: `cargo clippy --all-targets`
Expected: No new warnings beyond pre-existing ones.

- [ ] **Step 6: Commit**

```bash
git add src/cli/helpers.rs src/cli/commands.rs
git commit -m "refactor(cli): extract shared helpers to helpers.rs"
```

---

## Task 2: Push and create PR for helpers extraction

**Files:** None (git operations only)

- [ ] **Step 1: Push and create PR**

```bash
git push -u origin refactor/cli-helpers-extraction
gh pr create --base main --title "refactor: extract shared CLI helpers to helpers.rs" --body "..."
```

Include in PR body:
- List of functions moved
- Line reduction in commands.rs (~380 lines)
- Note: pure mechanical move, no logic changes

- [ ] **Step 2: Wait for merge before proceeding to Task 3**

---

## Task 3: Extract vault command functions to `cli/vault_ops.rs`

**Files:**
- Create: `src/cli/vault_ops.rs`
- Modify: `src/cli/commands.rs`
- Modify: `src/cli/mod.rs`

- [ ] **Step 0: Create feature branch from main (after helpers PR merged)**

```bash
git checkout main && git pull origin main
git checkout -b refactor/cli-vault-ops-extraction
```

- [ ] **Step 1: Create `src/cli/vault_ops.rs` with module header and imports**

```rust
//! Vault command execution handlers.

use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
use crate::cli::commands::{VaultCommands, VaultShareCommands};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use crate::utils::output;
use crate::vault::{VaultCreateRequest, VaultManager};
use std::sync::Arc;
```

Additional imports will be needed — copy them from the inner `use` statements within each function as you move them.

- [ ] **Step 2: Move vault functions into `vault_ops.rs`**

Move these functions, making each `pub(crate)`:

From the first vault block (lines 1128–1404):
1. `execute_vault_command` (lines 1128–1266) — the main router
2. `execute_vault_create` (lines 1268–1319)
3. `execute_vault_list` (lines 1321–1362)
4. `execute_vault_delete` (lines 1364–1379)
5. `execute_vault_info` (lines 1381–1404)

From the info dispatcher section:
6. `execute_vault_info_from_root` (lines 3547–3573)

From the second vault block (lines 6249–6826):
7. `execute_vault_restore` (lines 6249–6257)
8. `execute_vault_purge` (lines 6259–6270)
9. `execute_vault_export` (lines 6272–6481) — note `#[allow(clippy::too_many_arguments)]`
10. `execute_vault_import` (lines 6483–6676) — note `#[allow(clippy::too_many_arguments)]`
11. `execute_vault_update` (lines 6678–6720) — note `#[allow(clippy::too_many_arguments)]`
12. `execute_vault_share` (lines 6722–6826)

Only `execute_vault_command` and `execute_vault_info_from_root` need to be `pub(crate)` — the rest are called only within `vault_ops.rs` and can remain private.

- [ ] **Step 3: Remove moved functions from `commands.rs`**

Delete all vault function bodies at the line ranges above.

- [ ] **Step 4: Update dispatch in `commands.rs`**

In `Cli::execute()`, replace the vault dispatch with:
```rust
Commands::Vault { command } => {
    crate::cli::vault_ops::execute_vault_command(command, config).await?;
}
```

Update the info command dispatch to call:
```rust
crate::cli::vault_ops::execute_vault_info_from_root(...)
```

- [ ] **Step 5: Wire module in `src/cli/mod.rs`**

Add after the `helpers` line:
```rust
pub(crate) mod vault_ops;
```

- [ ] **Step 6: Compile check**

Run: `cargo check`
Expected: Passes. Fix any missing imports in `vault_ops.rs` — common ones are `crate::cache::CacheManager`, `crate::config::ContextManager`, `std::collections::HashMap`.

- [ ] **Step 7: Clippy check**

Run: `cargo clippy --all-targets`
Expected: No new warnings.

- [ ] **Step 8: Verify no vault implementations remain in commands.rs**

Run: `rg "execute_vault_" src/cli/commands.rs`
Expected: Only import lines and dispatch calls (no function definitions).

Run: `rg "VaultManager" src/cli/commands.rs`
Expected: Only in clap definitions or import statements, not in function bodies.

- [ ] **Step 9: Commit**

```bash
git add src/cli/vault_ops.rs src/cli/commands.rs src/cli/mod.rs
git commit -m "refactor(cli): extract vault command handlers to vault_ops.rs"
```

---

## Task 4: Push and create PR for vault extraction

**Files:** None (git operations only)

- [ ] **Step 1: Push and create PR**

```bash
git push -u origin refactor/cli-vault-ops-extraction
gh pr create --base main --title "refactor: extract vault command handlers to vault_ops.rs" --body "..."
```

Include in PR body:
- List of functions moved
- Line reduction in commands.rs (~870 lines)
- Updated module structure diagram
- Note: pure mechanical move, no logic changes

---

## Task 5: Post-extraction verification

- [ ] **Step 1: Verify final line counts**

Run: `wc -l src/cli/*.rs`
Expected approximately:
- `commands.rs`: ~6,350 lines
- `helpers.rs`: ~400 lines
- `vault_ops.rs`: ~890 lines
- `file_ops.rs`: ~2,052 lines (unchanged)

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: All tests pass, no regressions.

- [ ] **Step 3: Verify CLI behavior unchanged**

Run these commands and verify output matches pre-refactor behavior:
```bash
xv vault --help
xv vault list --help
xv --version
```
