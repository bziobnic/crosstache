# Security Audit — crosstache

> Last reviewed: 2026-02-20 | Reviewer: Jackson (AI security review)

---

## Status of Previously Identified Issues

| # | Issue | Status |
|---|-------|--------|
| 1 | Secret values not zeroed from memory | ✅ **Fixed** (v0.3.0, PR #26) |
| 2 | Secrets printed to stderr on clipboard failure | ✅ **Fixed** (#28) |
| 3 | Config file written without restricted permissions | ✅ **Fixed** (#28 — all sensitive files now 0600) |
| 4 | Custom generator scripts — command injection risk | ✅ **Fixed** (#29 — ownership + world-writable checks) |
| 5 | Secrets leaked into process environment (`xv run`) | ⚠️ Inherent to design, undocumented |
| 6 | Clipboard not cleared after timeout | ✅ **Fixed** (#29 — auto-clears after 30s) |
| 7 | Export files written without restricted permissions | ✅ **Fixed** (#28 — uses write_sensitive_file) |
| 8 | Template output may contain secrets in plain text | ✅ **Fixed** (#28 — 0600 + warning) |
| 9 | Token/JWT parsed without verification | ⚠️ Informational, acceptable |
| 10 | 105 `unwrap()` calls in non-test code | ❌ **Still open** |
| 11 | No rate limiting on authentication retries | ⚠️ Low risk |
| 12 | Secret names visible in process arguments | ⚠️ Inherent to CLI design |
| 13 | No audit logging of local operations | ⚠️ Informational |
| 14 | Path traversal in recursive download | ✅ **Fixed** (#28) |
| 15 | Bearer tokens not zeroized | ❌ **Still open** (low priority) |
| 16 | Env vars not dropped after child spawn | ✅ **Fixed** (#28) |

---

## Remaining Open Issues

### 🟡 #5 — Secrets in Process Environment (`xv run`)
**Risk:** Medium — inherent to env-var injection pattern (shared by 1Password, Doppler, etc.)
**Status:** By design. Should be documented in README security section.

### 🟡 #15 — Bearer Tokens Not Zeroized
**Location:** `src/secret/manager.rs` — 7+ locations with `format!("Bearer {}", token.token.secret())`
**Risk:** Low-Medium — tokens are short-lived (~1 hour) but remain in process memory as plain `String`.
**Fix:** Use `Zeroizing<String>` for formatted bearer strings.
**Effort:** Low-Medium

### 🟢 #10 — 105 `unwrap()` Calls in Non-Test Code
**Risk:** Low (availability, not confidentiality) — panics could leave inconsistent state.
**Fix:** Gradual replacement with proper error handling.
**Effort:** High

### 🟢 #12 — Secret Names Visible in Process Arguments
**Risk:** Low — inherent to CLI design. Document in README.

### 🟢 #13 — No Local Audit Logging
**Risk:** Informational — Azure has server-side logs. Local logging is optional enhancement.

---

## Remaining Priority

| Priority | Issue | Effort | Impact |
|----------|-------|--------|--------|
| 1 | **#15** Zeroize bearer tokens | Low-Med | 🟡 Token exposure |
| 2 | **#5** Document env var risk | Low | 🟡 User awareness |
| 3 | **#10** unwrap() cleanup | High | 🟢 Robustness |
