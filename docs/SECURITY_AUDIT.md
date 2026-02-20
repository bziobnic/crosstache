# Security Audit — crosstache

> Reviewed: 2026-02-19 | Reviewer: Jackson (AI security review)

---

## 🔴 Critical Issues

### 1. Secret Values Not Zeroed from Memory
**Location:** `src/secret/manager.rs`, `src/cli/commands.rs` throughout  
**Risk:** High  
**Issue:** Secret values are stored as plain `String` types. When these go out of scope, Rust deallocates the memory but does NOT zero it. The secret data remains in the process's memory pages until overwritten by something else. A memory dump, core dump, or swap file could expose secrets.

**The `zeroize` crate is in Cargo.toml but never actually used anywhere.**

**Fix:**
- Replace `String` with `zeroize::Zeroizing<String>` for all secret value fields (`SecretProperties.value`, `SetSecretRequest.value`, `UpdateSecretRequest.value`)
- Use `Zeroizing<String>` for any local variable holding a secret value
- Ensure clipboard contents, env vars in `xv run`, and template injection outputs all use zeroized types where possible

### 2. Secrets Printed to stderr on Clipboard Failure
**Location:** `src/cli/commands.rs:3834, 3839`  
**Risk:** High  
**Issue:** When clipboard copy fails, the code falls back to printing the raw secret value to stderr:
```rust
eprintln!("Secret value: {value}");
```
This could end up in terminal scrollback, log files, or screen recordings.

**Fix:** Remove these fallback prints. On clipboard failure, tell the user to use `--raw` flag instead. Never print secrets as a fallback.

### 3. Config File Written Without Restricted Permissions
**Location:** `src/config/init.rs:737`, `src/config/settings.rs`  
**Risk:** High  
**Issue:** Config files (which may contain subscription IDs, tenant IDs, storage account names) are written with default file permissions (typically 0644 — world-readable). Should be 0600 (owner-only).

**Fix:**
```rust
use std::os::unix::fs::PermissionsExt;
let perms = std::fs::Permissions::from_mode(0o600);
std::fs::set_permissions(&config_file, perms)?;
```

---

## 🟡 Medium Issues

### 4. Custom Generator Scripts — Command Injection Risk
**Location:** `src/cli/commands.rs:4004`  
**Risk:** Medium  
**Issue:** `xv rotate --generator <script>` executes an arbitrary file path as a subprocess. While this is intentional functionality, there's no validation that the script is owned by the current user, no check for world-writable scripts, and no sandboxing.

**Fix:**
- Validate script is owned by current user
- Reject world-writable scripts (anyone could have modified it)
- Document the security implications clearly in `--help`

### 5. Secrets Leaked into Process Environment
**Location:** `src/cli/commands.rs:4304` (`xv run`)  
**Risk:** Medium  
**Issue:** `xv run` injects secrets as environment variables into the child process. These are visible via `/proc/<pid>/environ` on Linux (readable by same user), and persist in the child's memory. Any child process crash dump will contain them.

**Fix:** This is inherent to the env-var injection pattern (1Password, Doppler, etc. all have this). Document the risk. Consider adding a `--no-env` mode that uses a Unix domain socket or named pipe instead. At minimum, ensure parent process env vars with secrets are cleaned up after the child exits.

### 6. Clipboard Not Cleared After Timeout
**Location:** `src/cli/commands.rs:3830`  
**Risk:** Medium  
**Issue:** After copying a secret to clipboard, it stays there indefinitely. Any application can read it. Other tools (1Password CLI, Bitwarden) auto-clear clipboard after 30 seconds.

**Fix:** Spawn a background thread that clears the clipboard after a configurable timeout (default 30s):
```rust
std::thread::spawn(move || {
    std::thread::sleep(Duration::from_secs(30));
    if let Ok(mut ctx) = ClipboardContext::new() {
        let _ = ctx.set_contents(String::new());
    }
});
```

### 7. Export Files Written Without Restricted Permissions
**Location:** `src/cli/commands.rs:2584, 5505, 6117`  
**Risk:** Medium  
**Issue:** `xv vault export`, `xv env pull`, and `xv inject --out` write files containing secret values with default permissions (world-readable).

**Fix:** Set 0600 permissions on any file that contains secret values.

### 8. Template Output May Contain Secrets in Plain Text
**Location:** `src/cli/commands.rs:4455, 4608` (`xv inject`)  
**Risk:** Medium  
**Issue:** `xv inject` writes resolved templates (with real secret values) to files. These files should be treated as sensitive, but there's no warning, no restricted permissions, and no cleanup mechanism.

**Fix:** 
- Set 0600 on output files
- Print a warning: "⚠️  Output file contains resolved secrets — treat as sensitive"
- Consider a `--cleanup-after <duration>` flag

### 9. Token/JWT Parsed Without Verification
**Location:** `src/auth/provider.rs` (JWT parsing in whoami)  
**Risk:** Low-Medium  
**Issue:** JWT tokens are decoded (base64) to extract claims like tenant ID, but signature verification is not performed. This is fine for display purposes but should never be used for authorization decisions.

**Fix:** Add a comment documenting this is display-only. Not a functional issue currently.

---

## 🟢 Low / Informational

### 10. 105 `unwrap()` Calls in Non-Test Code
**Risk:** Low (availability, not confidentiality)  
**Issue:** Panics in production are bad UX and could leave secrets in an inconsistent state.

**Fix:** Gradually replace with proper error handling. Not a security-critical issue but improves robustness.

### 11. No Rate Limiting on Authentication Retries
**Location:** `src/utils/retry.rs`  
**Risk:** Low  
**Issue:** Retry logic uses exponential backoff but doesn't cap total retries for auth failures. Unlikely to be exploitable since Azure handles auth server-side.

### 12. Secret Names Visible in Process Arguments
**Risk:** Low  
**Issue:** Running `xv get my-database-password` exposes the secret *name* in `ps` output. Not the value, but the name itself may be sensitive.

**Fix:** Document this. No easy fix without changing the CLI interface.

### 13. No Audit Logging of Local Operations
**Risk:** Informational  
**Issue:** There's no local log of which secrets were accessed, when, and by whom. Azure has server-side audit logs, but local access (clipboard copies, env injection) is untracked.

**Fix:** Optional local audit log file (append-only, restricted permissions).

---

## Priority Implementation Order

| # | Issue | Effort | Impact |
|---|-------|--------|--------|
| 1 | Zeroize secret values in memory | Medium | 🔴 Critical |
| 2 | Remove secret fallback printing | Low | 🔴 Critical |
| 3 | Restrict config file permissions | Low | 🔴 Critical |
| 6 | Auto-clear clipboard | Low | 🟡 Medium |
| 7 | Restrict export file permissions | Low | 🟡 Medium |
| 4 | Validate generator scripts | Low | 🟡 Medium |
| 8 | Restrict template output permissions | Low | 🟡 Medium |
| 5 | Document env var exposure risk | Low | 🟡 Medium |

Issues 2, 3, 6, and 7 are quick wins — could be done in an afternoon.
Issue 1 (zeroize) is the most impactful but requires touching many files.
