# Security Audit — crosstache

> Last reviewed: 2026-02-20 | Reviewer: Jackson (AI security review)

---

## Status of Previously Identified Issues

| # | Issue | Status |
|---|-------|--------|
| 1 | Secret values not zeroed from memory | ✅ **Fixed** (v0.3.0, PR #26 — `Zeroizing<String>` throughout) |
| 2 | Secrets printed to stderr on clipboard failure | ❌ **Still open** |
| 3 | Config file written without restricted permissions | ❌ **Still open** |
| 4 | Custom generator scripts — command injection risk | ❌ **Still open** |
| 5 | Secrets leaked into process environment (`xv run`) | ⚠️ Inherent to design, undocumented |
| 6 | Clipboard not cleared after timeout | ❌ **Still open** |
| 7 | Export files written without restricted permissions | ❌ **Still open** |
| 8 | Template output may contain secrets in plain text | ❌ **Still open** |
| 9 | Token/JWT parsed without verification | ⚠️ Informational, acceptable |
| 10 | 105 `unwrap()` calls in non-test code | ❌ **Still open** (still 105) |
| 11 | No rate limiting on authentication retries | ⚠️ Low risk |
| 12 | Secret names visible in process arguments | ⚠️ Inherent to CLI design |
| 13 | No audit logging of local operations | ⚠️ Informational |

---

## 🔴 Critical — Outstanding

### 2. Secrets Printed to stderr on Clipboard Failure
**Location:** `src/cli/commands.rs:3835, 3840`
**Status:** Still present.
```rust
eprintln!("Secret value: {}", value.as_str());
```
**Risk:** Secret values end up in terminal scrollback, log files, or screen recordings.
**Fix:** Replace with message telling user to use `--raw`. Never print secrets as fallback.
**Effort:** Low (5 min)

### 3. Config & Export Files Written World-Readable
**Locations:**
- `src/config/init.rs:777` — config file
- `src/config/settings.rs:425` — config save
- `src/cli/commands.rs:2585` — `xv env pull` output
- `src/cli/commands.rs:4461, 4614` — `xv inject` output
- `src/cli/commands.rs:5510` — `xv vault export`
- `src/cli/commands.rs:6125, 6137` — other file writes
- `src/cli/commands.rs:2165` — profile files

**Status:** No `set_permissions` or `PermissionsExt` usage anywhere in the codebase.
**Risk:** Any file containing secrets or sensitive config (subscription IDs, tenant IDs) is created with default 0644 permissions — readable by all users on the system.
**Fix:** Create a helper function and use it everywhere:
```rust
#[cfg(unix)]
fn write_sensitive_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, content)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}
```
**Effort:** Low-Medium (create helper, update ~8 call sites)

---

## 🔴 Critical — New Finding

### 14. Path Traversal in Recursive Download
**Location:** `src/cli/commands.rs:6800`
```rust
let local_path = output_path.join(blob_name);
```
**Risk:** If a malicious blob is named `../../etc/cron.d/backdoor` or `../../../.ssh/authorized_keys`, a recursive download would write files outside the intended output directory. An attacker with write access to the blob container could use this for arbitrary file write on the user's machine.
**Fix:** Validate that the resolved path stays within the output directory:
```rust
let local_path = output_path.join(blob_name);
let canonical_output = output_path.canonicalize()?;
let canonical_local = local_path.canonicalize()
    .unwrap_or_else(|_| local_path.clone());
if !canonical_local.starts_with(&canonical_output) {
    return Err(CrosstacheError::config(format!(
        "Path traversal detected in blob name: {blob_name}"
    )));
}
```
**Effort:** Low (10 min)

---

## 🟡 Medium — Outstanding

### 4. Custom Generator Scripts — No Validation
**Location:** `src/cli/commands.rs:4005`
**Status:** Still no ownership or permission checks on the script file.
**Risk:** A world-writable script at the given path could be modified by another user.
**Fix:** Check script is owned by current user, reject world-writable (mode & 0o002).
**Effort:** Low

### 6. Clipboard Not Cleared After Timeout
**Location:** `src/cli/commands.rs:3830`
**Status:** Still no auto-clear.
**Risk:** Secrets persist in clipboard indefinitely. Every other secret manager (1Password, Bitwarden) clears after 30s.
**Fix:** Spawn background thread to clear clipboard after configurable timeout.
**Effort:** Low

### 8. Template/Inject Output Not Marked Sensitive
**Location:** `src/cli/commands.rs:4461, 4614`
**Status:** Files with resolved secrets written without warning or restricted permissions.
**Fix:** Set 0600 + print warning about sensitive content.
**Effort:** Low

---

## 🟡 Medium — New Finding

### 15. Bearer Tokens Not Zeroized
**Location:** `src/secret/manager.rs` — 7+ locations with pattern:
```rust
format!("Bearer {}", token.token.secret())
```
**Risk:** Bearer tokens are stored as plain `String` in the authorization header value. While tokens are short-lived (~1 hour), they remain in process memory until the page is reused. A core dump during an active session could expose tokens.
**Status:** Zeroize was applied to secret values but not to auth tokens.
**Fix:** Use `Zeroizing<String>` for the formatted bearer string. Lower priority than secret values since tokens are ephemeral.
**Effort:** Low-Medium

### 16. `xv run` — Env Vars Not Cleaned After Child Exits
**Location:** `src/cli/commands.rs:4305+`
**Risk:** The `env_vars`, `secret_values`, and `uri_values` collections persist in the parent process memory until the function returns. They should be explicitly dropped immediately after the child process spawns.
**Fix:** Add `drop(env_vars); drop(secret_values); drop(uri_values);` after `cmd.spawn()`.
**Effort:** Low (5 min)

---

## 🟢 Low / Informational

### 10. 105 `unwrap()` Calls in Non-Test Code
**Status:** Still 105. Panics could leave the process in an inconsistent state.
**Effort:** High (gradual cleanup)

### 12. Secret Names Visible in Process Arguments
**Status:** Inherent to CLI design. Document in README security section.

### 13. No Local Audit Logging
**Status:** Optional enhancement. Azure-side logs exist.

---

## Priority Implementation Order

| Priority | Issue | Effort | Impact |
|----------|-------|--------|--------|
| 1 | **#14** Path traversal in recursive download | Low | 🔴 Arbitrary file write |
| 2 | **#2** Remove secret fallback printing | Low | 🔴 Secret exposure |
| 3 | **#3** Restrict file permissions (all write sites) | Low-Med | 🔴 Secret exposure |
| 4 | **#6** Auto-clear clipboard | Low | 🟡 Secret persistence |
| 5 | **#4** Validate generator scripts | Low | 🟡 Code execution |
| 6 | **#16** Drop env vars after child spawn | Low | 🟡 Memory hygiene |
| 7 | **#15** Zeroize bearer tokens | Low-Med | 🟡 Token exposure |
| 8 | **#8** Restrict inject output permissions | Low | 🟡 Secret exposure |

**Quick wins (items 1, 2, 4, 6):** Could be done in under an hour combined.
**File permissions (#3):** Create one helper function, update ~8 call sites — a focused PR.
