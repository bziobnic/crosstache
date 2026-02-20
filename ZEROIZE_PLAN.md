# Zeroize Implementation Plan

> Goal: Ensure all secret values are zeroed from memory when no longer needed.

## Background

The `zeroize` crate is already in `Cargo.toml` but unused. Secret values currently live as plain `String`s — when dropped, Rust frees the memory but doesn't zero it. The data persists until the OS reuses that page.

The `zeroize` crate provides `Zeroizing<T>`, a wrapper that zeros the inner value on drop. `Zeroizing<String>` behaves like a `String` but wipes itself when it goes out of scope.

---

## Phase 1: Core Data Types (manager structs)

**Files:** `src/secret/manager.rs`

Change the `value` field on all secret-related structs:

```rust
use zeroize::Zeroizing;

// SecretProperties — the main struct returned from API calls
pub struct SecretProperties {
    pub value: Option<Zeroizing<String>>,  // was Option<String>
    // ... rest unchanged
}

// SecretRequest — used for set/create operations
pub struct SecretRequest {
    pub value: Zeroizing<String>,  // was String
    // ... rest unchanged
}

// SecretUpdateRequest — used for update operations  
pub struct SecretUpdateRequest {
    pub value: Option<Zeroizing<String>>,  // was Option<String>
    // ... rest unchanged
}
```

**Impact:** `Zeroizing<String>` implements `Deref<Target=String>`, so most read-only usages (`.as_str()`, format strings, comparisons) work without changes. But:
- `Zeroizing<String>` does NOT implement `Serialize`/`Deserialize` by default — need `zeroize` feature `serde` (already enabled in Cargo.toml ✅)
- `.clone()` on `Zeroizing<String>` returns a new `Zeroizing<String>` — clones are also zeroized on drop ✅
- `Tabled` derive may need `#[tabled(skip)]` or a display adapter (value field is already skipped ✅)

**Complications:**
- The `Serialize` impl for `Zeroizing<String>` requires the inner type to impl `Serialize`. This works for `String` ✅.
- `serde_json::json!({ "value": request.value })` — need to verify `Zeroizing` serializes transparently as the inner value. It does with the `serde` feature ✅.

---

## Phase 2: API Boundary (JSON serialization/deserialization)

**Files:** `src/secret/manager.rs` (API call functions)

The Azure Key Vault REST API returns JSON like:
```json
{ "value": "my-secret-value", "id": "...", ... }
```

When deserializing, `serde` will construct the `Zeroizing<String>` directly. When serializing for PUT/PATCH, it unwraps transparently. No changes needed to the HTTP layer if `serde` feature is enabled.

**Verify:** The `json!()` macro calls in `set_secret`, `update_secret`, etc. need to accept `Zeroizing<String>`. Since `Zeroizing<String>` derefs to `String` and implements `Serialize`, this should work. Test it.

---

## Phase 3: CLI Command Handlers (the messy part)

**File:** `src/cli/commands.rs` (~30+ locations)

### 3a. Secret Input (set/update/rotate)

| Location | Current | Change |
|----------|---------|--------|
| `rpassword::prompt_password()` | Returns `String` | Wrap in `Zeroizing::new()` |
| `stdin` read for values | Returns `String` | Wrap in `Zeroizing::new()` |
| `generate_random_value()` | Returns `String` | Return `Zeroizing<String>` |
| Bulk set `KEY=value` parsing | Splits into `String` | Wrap value half in `Zeroizing::new()` |
| `@file` value loading | `fs::read_to_string()` | Wrap result in `Zeroizing::new()` |

### 3b. Secret Output (get/export/inject/run)

| Location | Current | Change |
|----------|---------|--------|
| `xv get` — clipboard copy | `value.clone()` → clipboard | Use `Zeroizing<String>`, clipboard gets a ref |
| `xv get --raw` — stdout | `print!("{value}")` | Works via `Deref`, no change needed |
| `xv run` — `env_vars` HashMap | `HashMap<String, String>` | `HashMap<String, Zeroizing<String>>` |
| `xv run` — `secret_values` Vec | `Vec<String>` | `Vec<Zeroizing<String>>` |
| `xv run` — `uri_values` HashMap | `HashMap<String, String>` | `HashMap<String, Zeroizing<String>>` |
| `xv inject` — `secret_values` HashMap | `HashMap<String, String>` | `HashMap<String, Zeroizing<String>>` |
| `xv inject` — template result | `String` with resolved secrets | `Zeroizing<String>` |
| `xv vault export --include-values` | Writes values to file | Values pass through `Zeroizing` then drop |
| `xv env pull` | Writes values to dotenv format | Same |
| `xv copy/move` | Reads value, creates in target vault | `Zeroizing<String>` flows through |

### 3c. Masking function

| Location | Current | Change |
|----------|---------|--------|
| `mask_secrets()` | Takes `&[String]` | Takes `&[Zeroizing<String>]` — works via `Deref` |

---

## Phase 4: Environment Variable Injection (`xv run`)

**Special case:** `std::process::Command::envs()` requires values that implement `AsRef<OsStr>`. `Zeroizing<String>` derefs to `String` which implements `AsRef<OsStr>`, so this should work. BUT — once the env var is passed to the child process, we can't zeroize the child's memory.

**What we CAN do:**
- Zeroize `env_vars`, `secret_values`, and `uri_values` in the parent process after the child spawns
- These will be auto-zeroized on drop anyway, but we can explicitly drop them early:
```rust
drop(env_vars);    // Triggers zeroize
drop(secret_values);
drop(uri_values);
```

---

## Phase 5: Clipboard

**File:** `src/cli/commands.rs` (get command)

After copying to clipboard, the `Zeroizing<String>` holding the value will be dropped and zeroed when the function returns. The clipboard itself retains a copy we can't zeroize — that's addressed by the separate "auto-clear clipboard" fix.

---

## Implementation Order

```
Step 1: Add `use zeroize::Zeroizing;` to manager.rs and commands.rs
Step 2: Change struct fields in manager.rs (Phase 1)
Step 3: Verify serde round-trip works (Phase 2) — cargo test
Step 4: Fix all compile errors in commands.rs (Phase 3)
         - This is the bulk of the work, ~30 locations
         - Most are just wrapping with Zeroizing::new() or adjusting types
Step 5: Add explicit early drops in xv run (Phase 4)
Step 6: cargo clippy + cargo test + cargo fmt
Step 7: Commit
```

## Estimated Scope

- **Files modified:** 2 (`src/secret/manager.rs`, `src/cli/commands.rs`)
- **Lines changed:** ~80-120 (mostly type changes and `Zeroizing::new()` wraps)
- **Risk:** Medium — the `Deref` impl on `Zeroizing` handles most cases transparently, but `.clone()`, `.to_string()`, and format macros may produce un-zeroized copies if not careful
- **Testing:** Existing tests should pass since `Zeroizing<String>` is API-compatible with `String` in most contexts

## Known Limitations

1. **`.to_string()` / `format!()` create un-zeroized copies** — minimize these, use references where possible
2. **Clipboard contents are not zeroized** — separate fix (auto-clear timer)
3. **Child process env vars are not zeroized** — OS limitation, can't control child memory
4. **Log/debug output** — ensure `tracing` macros never log secret values (currently they don't ✅)
5. **Swap/hibernation** — OS may page secret-containing memory to disk. Only `mlock()` prevents this, which is out of scope for now
