# crosstache — Deep-Dive Code Review

**Scope:** Full source tree at HEAD on `main` (~35.5K LOC Rust across 50+ files).
**Method:** Four parallel reviewers across (1) auth + backend abstraction + Azure backend, (2) local backend including `crypto.rs`, (3) managers/utils/cache/config, (4) CLI/TUI/scan. Findings de-duplicated, normalized, and prioritized below.
**Conventions:** P1 = critical / security / data-loss / panic. P2 = high. P3 = medium. P4 = nit. File:line references are HEAD at review time and will drift.

---

## Narrative review

### Architectural posture
The project is in a healthy structural place: a clean `Backend` trait splits Azure from a local age-encrypted backend, hybrid SDK + REST is documented and consistent, and the CLI is broken up into per-domain `*_ops.rs` modules instead of one mega-file (the ~8.3K-line `commands.rs` mentioned in CLAUDE.md has already been carved up — `secret_ops.rs` 3.9K, `file_ops.rs` 2.3K, etc.). Error handling goes through a single `CrosstacheError` enum and `BackendError`. Async is uniform on tokio. There is a real test surface (auth, vault, file, scan, e2e local backend, version history, pagination).

The biggest *category* of weakness is **trust-boundary discipline**. The Azure backend repeatedly interpolates user-controlled or response-derived strings into URLs, OData filters, and Bearer headers without escaping or validation, and the local backend writes secret-bearing files without explicit mode bits and without inter-process locking. None of these are exotic — they are the same shape of bug, repeated in many places, and they are all fixable with helper functions.

The second category is **error fidelity**. Many sites use `.unwrap_or_default()` on response bodies, swallow `Result`s into empty maps, or fabricate placeholder values when a 202 returns no body. The code keeps moving but the operator loses the ability to debug.

The third category is **bounds**: pagination loops with no max page count, JSON decoding with no size cap, downloads with no streamed size limit, regexes scanning unbounded input. None are individually exploitable in a typical workstation install, but they convert "weird upstream behavior" into "your laptop hangs" instead of "you got a clear error."

### What's good and worth keeping
- Single `BackendError` boundary with `From` conversions; the abstraction is real, not cosmetic.
- `Zeroizing` is used in the right places in `local/mod.rs` and `local/secrets.rs`.
- SHA256 verification on `xv upgrade` downloads (the gap is *signature*, not *integrity*).
- Sanitizer preserves the original name in a tag — the design choice is sound; only the truncation length needs a second look.
- Retry helper exists (`utils/retry.rs`) — the issue is that it isn't applied to the secret REST path, not that it doesn't exist.
- Tests for pagination, version history, and the local backend exist; this is unusually good coverage for a CLI of this size.

### Top three things to fix this week
1. **`xv run` does not clear the parent environment** before injecting secrets (`secret_ops.rs:2177`). This is the headline guarantee of the command and it is not delivered. One-line fix: `cmd.env_clear()` before `cmd.envs(&env_vars)` (gated behind a `--keep-env` flag if you need the current behavior for compatibility).
2. **Local secret/metadata files are written with default umask** (`local/crypto.rs:33`, `local/secrets.rs:162`, several more). On a multi-user machine the encrypted age payload is fine, but the *plaintext metadata sidecar* (names, groups, notes, expiry, timestamps) is not, and the age key file at `local/crypto.rs:203` has a write→chmod race window. Centralize on a `write_private(path, bytes)` helper that creates with `0o600` atomically (`OpenOptions::mode(0o600)` on Unix).
3. **TUI has no panic hook to restore the terminal** (`tui/mod.rs:42-44`). A panic in any view code leaves the user staring at a dead terminal. Install `std::panic::set_hook` that calls `teardown_terminal()` before delegating to the default hook. ~10 lines.

### Recurring patterns worth a single fix
- **OData / URL injection.** `vault/operations.rs:765`, `backend/azure/detect.rs:232`, `backend/azure/auth.rs:422`. Add one helper that builds OData `eq` filters by escaping `'` → `''` and percent-encoding the result, and one helper that builds vault/secret REST URLs from a struct (vault, secret_name, version) using `urlencoding::encode` on each path segment. Then forbid `format!("...{...}...")` for URLs in review.
- **`response.text().await.unwrap_or_default()` on error paths.** Seven occurrences in `secret/manager.rs`. Replace with a `read_error_body(resp)` helper that returns `String` and logs the read failure. Restores debuggability with no behavior change on success.
- **Pagination with no upper bound.** `secret/manager.rs:749-836`, `vault/operations.rs:358-389`. Add a `MAX_PAGES` constant (1000 is fine) and a counter; return a structured error if exceeded. Same shape of fix in both files.
- **`.expect("use_trait_path guarantees Some")` repeated 10+ times in `secret_ops.rs`.** This is a sign the function should return the resolved trait object directly instead of returning a bool that the caller then has to recover the value from. Refactor the helper signature; the panic risk goes away by construction.

### Things that look like bugs but are deeper design questions
- **Retry on non-idempotent operations** (`utils/retry.rs:38-60`). `retry_with_backoff` is generic over closures and does not know which Azure operations are safe to retry. `set_secret` is idempotent on Azure Key Vault (PUT with the same name creates a new version), but on the local backend with the current versioning scheme it is **not** idempotent under concurrent writes. The honest fix is two retry helpers (`retry_idempotent`, `retry_at_most_once`) and an audit of every call site, not a parameter on the existing one.
- **Local backend versioning is racy by design** (`local/secrets.rs:189-209, 268-281, 412-437`). `next_version` reads the `.versions/` dir and computes max+1; two concurrent `xv set` calls will both compute the same number and one will silently win. The fix is real cross-process file locking (`fs2::FileExt::try_lock_exclusive` on a `.lock` file in the vault directory) at the `set_secret`/`delete_secret`/`rollback_secret` boundary, not finer-grained checks. Without this, every version bug below is a symptom of the same root cause.
- **`xv upgrade` checksum-over-HTTPS-from-the-same-host is TOFU**, not signature verification (`upgrade_ops.rs:134-172`). To raise the bar you need an embedded public key and signed release manifests (cosign / minisign / age). This is a roadmap-level call, not a one-line patch — but worth noting in the README so users don't assume more than is delivered.
- **`xv scan` patterns** (`scan/patterns.rs:64-68`) — the `[A-Za-z0-9+/_-]{32,}` high-entropy rule will swamp real codebases with false positives (every JWT-shaped string in a test fixture, every base64 image). Consider Shannon-entropy gating on top of the length match, or shipping the rule disabled by default with `--strict`.
- **Sanitizer hash truncation** (`utils/sanitizer.rs:79-86`) keeps 16 bytes (128 bits) of SHA256 — collision-safe in practice but worth either documenting the bound or extending to the full 256 bits, which costs nothing in this context (Azure Key Vault names allow it).

### Things to *not* do
- Do not add a "manager layer," retries framework, or session manager on top of the existing helpers; the CLAUDE.md guidance against this is right and the code is better for it.
- Do not encrypt the local metadata sidecar to "fix" the permission issue — fix the permissions. Encrypting metadata makes listing slow and requires a passphrase prompt for a `xv list`.
- Do not add a config-file migration for the issues below — most are additive (new helpers, stricter perms) and don't change the on-disk format.

---

## Issue list

### P1 — Critical / security / data-loss / panic

- **`src/cli/secret_ops.rs:2177` — `xv run` leaks parent environment to child.** `cmd.envs(&env_vars)` is called without `cmd.env_clear()`. Child processes see both the injected secrets and the operator's full environment (cloud credentials, tokens, history-related env, etc.). `/proc/PID/environ` exposes everything. **Fix:** `cmd.env_clear()` before `cmd.envs(&env_vars)`, optionally gated by `--keep-env` for opt-in compatibility. Document the change.
- **`src/tui/mod.rs:42-44` — no panic hook; terminal not restored on panic.** `setup_terminal()` enables raw mode + alternate screen; any panic in the TUI leaves the terminal unusable. **Fix:** install `std::panic::set_hook` that calls `teardown_terminal()` then delegates to the previous hook.
- **`src/backend/local/crypto.rs:203` — age key written before chmod.** `fs::write(key_path, …)` then later `set_permissions(0o600)`. Default umask makes the key world-readable in the window between calls. **Fix:** create with restrictive mode atomically (`OpenOptions::new().create_new(true).write(true).mode(0o600).open(…)` on Unix) or write to a tempfile in the same dir with mode set, then `rename`.
- **`src/backend/local/crypto.rs:33` — encrypted secret files created without explicit mode.** `File::create(path)` inherits umask. The age payload is encrypted but should still be `0o600` to prevent enumeration and offline attack staging on shared hosts. **Fix:** `OpenOptions` with explicit mode, or `set_permissions` immediately after create.
- **`src/backend/local/secrets.rs:162` — plaintext metadata sidecar world-readable.** `fs::write(path, json)` writes name, groups, tags, notes, expiry, timestamps with default perms. On shared systems this leaks the entire structure of the vault. **Fix:** central `write_private` helper used by every metadata write site (lines 162, 425, 431, 436, 441, vault.json at `local/mod.rs:102-107`).
- **`src/backend/local/secrets.rs:189-209, 268-281, 412-437` — concurrent `xv set/delete/rollback` race.** `next_version()` (read max from `.versions/` then +1) and `archive_current()` (rename) are not synchronized. Two writers compute the same version; one silently wins. Soft-delete leaves split state if interrupted between metadata move and payload move. **Fix:** acquire an exclusive `flock` on a per-vault `.lock` file (e.g., `fs2::FileExt::try_lock_exclusive`) for the duration of any mutating op. Move payload first, metadata last (so a partial failure makes the secret look "still here" rather than "gone").
- **`src/backend/azure/auth.rs:422` — unencoded user input in Graph URL.** `format!("https://graph.microsoft.com/v1.0/users/{user}")`. Slashes or `..` in `user` redirect to other Graph endpoints. **Fix:** `urlencoding::encode(user)` or validate as UPN/objectId before interpolation.
- **`src/vault/operations.rs:765`, `src/backend/azure/detect.rs:232` — OData filter injection.** `$filter=principalId eq '{user_object_id}'` and `$filter=id eq '{tenant_id}'` interpolate raw strings. A `'` in input alters filter semantics. **Fix:** escape `'` → `''` per OData, then percent-encode the whole filter value. Centralize as `odata_eq(field, value)`.
- **`src/cli/secret_ops.rs:2240-2241, 2273` — panics on child process plumbing.** `.expect("stdout was piped")`, `.expect("stderr was piped")`, `.expect("failed to wait on child")` will crash the CLI on any spawn/wait edge case. **Fix:** propagate as `CrosstacheError` and exit with a non-zero code.
- **`src/backend/local/secrets.rs:315, 343, 353` — symlink not validated before decrypt.** An attacker who can write into the vault dir can replace `secret.age` with a symlink to any file the user can read; decryption attempt either errors with the path in the message or, depending on file shape, succeeds for crafted age payloads. **Fix:** `OpenOptions` with `custom_flags(libc::O_NOFOLLOW)` on Unix; explicit `symlink_metadata` check before open elsewhere.
- **`src/cli/upgrade_ops.rs:52` — `xv upgrade --check` exit code inverted.** Returns 1 when an update is available, 0 when up-to-date. Breaks `xv upgrade --check && …`. **Fix:** exit 0 in both cases and surface "update available" via stdout, or use exit code 2 for "update available" (documented).
- **`src/cli/file_ops.rs:1289-1309` — path traversal check is unreliable.** Uses `canonicalize().unwrap_or(path.to_path_buf())` — if the parent doesn't yet exist, comparison is against a non-canonical path. **Fix:** normalize the joined path with a component walker that rejects `..` *before* any FS call, or canonicalize the *parent* and verify the joined child stays within it.

### P2 — High

- **`src/cli/secret_ops.rs:2196-2227` — no signal forwarding from `xv run` parent to child.** Ctrl+C kills `xv` only; child keeps running. **Fix:** put child in a new process group on Unix and forward SIGINT/SIGTERM via a `tokio::signal` handler; on Windows use `CTRL_BREAK_EVENT`.
- **`src/secret/manager.rs:409, 470, 618, 759, 937, 1128` — `response.text().await.unwrap_or_default()` discards Azure error bodies.** Operators see `HTTP 400 - ` with no detail. **Fix:** `read_error_body(resp)` helper; never `unwrap_or_default()` here.
- **`src/secret/manager.rs:749-836`, `src/vault/operations.rs:358-389` — pagination has no max page count.** Bad `nextLink` chain → infinite loop / OOM. **Fix:** `MAX_PAGES = 1000`; structured error if exceeded.
- **`src/secret/manager.rs` (list/set/get) — no retry/backoff on 429.** `vault/operations.rs` uses `execute_with_retry`; secret ops bypass it. **Fix:** route HTTP through a single helper that handles 429 + `Retry-After`.
- **Token leakage risk in error formatting — `src/secret/manager.rs:389,449,599,741,912,1041,1110`, `src/backend/azure/auth.rs:389,426,449`.** `format!("Bearer {token}")` then `.parse().map_err(|e| …format!("…{e}"))` — many `HeaderValue` parse errors include the offending bytes. **Fix:** map parse errors to a static "invalid auth header" message; never include `e` for header construction sites that touch a token.
- **`src/backend/local/secrets.rs:604-616` — `rollback_secret` proceeds even if `archive_current` fails.** Current version is silently lost. **Fix:** propagate the archive error with `?`; refuse rollback if archiving fails.
- **`src/backend/local/secrets.rs:412-437` — soft-delete is non-atomic across two `rename`s.** Partial failure leaves metadata in trash but payload still active (or vice versa). **Fix:** rename payload first; rename metadata last; under the per-vault lock above this becomes safe.
- **`src/utils/retry.rs:38-60` — `retry_with_backoff` is applied to operations that may not be idempotent on every backend.** Local `set_secret` under the current versioning scheme can produce duplicate version numbers under retry-after-timeout. **Fix:** split into `retry_idempotent` and `retry_at_most_once`; audit call sites.
- **`src/cli/upgrade_ops.rs:134-172` — checksum is fetched from the same TLS endpoint as the binary; this is integrity, not signature.** A repo or release-asset compromise replaces both. **Fix:** ship a public key with the binary and verify a detached signature over the release manifest (cosign / minisign / age signing).
- **`src/cli/helpers.rs:154-172, 243` — clipboard clear is not atomic and is best-effort.** Documented as a security feature but cannot be one. **Fix:** rename in help text from "secure clipboard timeout" to "best-effort clipboard clear"; do not weaken the implementation but do not oversell it either.
- **`src/cli/secret_ops.rs:1356-1360` — `xv get --raw` writes to stdout with no shell-history warning.** Users put it in `.bash_history`. **Fix:** `--raw` is fine but the `Hint:` text suggesting raw should mention shell history; consider auto-disable when stdout is a TTY unless `--force`.
- **`src/secret/manager.rs:416 + many` — `.json::<Value>()` on responses with no size cap.** A misbehaving upstream → unbounded allocation. **Fix:** check `Content-Length`, or bound the body via `bytes_with_limit` then `serde_json::from_slice`.
- **`src/blob/sync.rs:98-109` — blob name is split on `/` and joined to `base_path` without rejecting `..` segments.** Attacker-controlled blob names escape the base path. **Fix:** reject any blob name component that is `..`, empty, or absolute.
- **`src/blob/manager.rs:374-380` — string-matching on `"404"` / `"not found"` to detect blob-missing.** Brittle to SDK message changes; will silently mis-classify. **Fix:** match on the SDK's typed error variant.
- **`src/vault/operations.rs:517-540` — restore returns 202 with empty body and the code fabricates `resource_group: String::new()`.** Caller gets back a vault struct that lies. **Fix:** return a typed "Accepted, re-query for state" result (or block on a poll loop), never invent fields.
- **`src/cli/config_ops.rs` (`write_sensitive_file`) — write-then-chmod window for config files containing tenant/subscription IDs.** Same root cause as the local-backend perms issue. **Fix:** central `write_private` helper used here too.
- **`src/backend/local/mod.rs:78`, parent-dir creation in `crypto.rs:187-189` and `secrets.rs:259-261`, `files.rs:100`, `mod.rs:102-107` — directories created with default umask.** Listing leaks names. **Fix:** chmod each created directory to `0o700` after `create_dir_all`, or wrap with a helper.
- **`src/cli/secret_ops.rs:2040, 2324, 2325, 2509-2513` — `Regex::new(...).unwrap()` at runtime.** Constants compile, but using `regex::escape(secret_name)` then `Regex::new(...).unwrap()` panics if the resulting pattern is malformed (very unlikely after escape, still wrong). **Fix:** `lazy_static`/`OnceLock` for fixed patterns; propagate error for dynamic ones.
- **`src/scan/walker.rs:80-89` — no upper file-size cap before `is_binary_file` reads 8KB of every file.** On accidentally-pointed-at large trees this is slow but bounded; on a streaming pseudo-FS it isn't. **Fix:** stat first, skip files over a configurable threshold.
- **`src/backend/local/mod.rs:143` — age key parse error includes parse-error detail.** Fragments of the offending key may surface in log. **Fix:** map to a generic "invalid AGE_KEY" message.

### P3 — Medium

- **`src/cli/helpers.rs:418` — `mask_secrets` does naive `replace`.** Overlapping or substring-of-another-secret values produce confusing chains; values < 4 chars not masked. **Fix:** sort secrets by length descending; mask longest first; document the floor.
- **`src/cli/upgrade_ops.rs:220` — 300s download timeout but no in-flight size cap.** Combine with `MAX_ASSET_SIZE` check happening after checksum; an oversize body can still be buffered. **Fix:** enforce size during streaming, not after.
- **`src/scan/engine.rs` — Aho-Corasick over whole-file reads.** Multi-GB files load into memory. **Fix:** chunked read with overlap buffer, or `memmap2`.
- **`src/cli/secret_ops.rs:2098-2128` — no cap on secret value size before in-memory handling.** Operationally fine; a defensive limit (e.g., 1 MB with override) prevents accidental OOM on a corrupted vault.
- **`src/cli/file_ops.rs:429` — `output` path used directly; symlink in target can redirect overwrite.** Combined with the P1 traversal-check fix, also reject symlinks in the parent chain when `--force`.
- **`src/scan/installer.rs:53` — pre-commit hook written without verifying the executable bit took.** Silent no-op at commit time. **Fix:** stat after write; warn if not exec; document `core.hooksPath`.
- **`src/scan/patterns.rs:64-68` — high-entropy regex too broad.** **Fix:** add Shannon-entropy gate; or move behind `--strict`.
- **`src/utils/sanitizer.rs:79-86` — hash truncated to 128 bits.** Safe in practice; document or extend to full hash.
- **`src/cache/manager.rs:99` — `(Utc::now() - entry.created_at).num_seconds().max(0) as u64`.** Clock-skew tolerant via `max(0)`, but the cast hides intent and a backwards jump permanently looks like a 0-age entry. **Fix:** use `Instant`-based monotonic age; keep `Utc` only for human display.
- **`src/utils/datetime.rs:16-26, 30-74` — calendar math uses 30-day months / 365.25-day years.** "1m" expiration is off by up to a day. **Fix:** use `chronoutil::shift_months` or document the approximation.
- **`src/utils/datetime.rs:115-117` — expiry uses wall clock with no monotonic guard.** Clock rewind reactivates expired secrets. **Fix:** also check on first read after process start; persist a `not_before` to make rewinds detectable.
- **`src/backend/local/secrets.rs:387-394` — corrupted metadata files silently skipped from `list`.** User can't see "this secret exists but is broken." **Fix:** return a `Vec<Result<SecretInfo, SkippedEntry>>` or surface a warning summary.
- **`src/backend/local/secrets.rs:167-185` — `next_version` silently ignores non-numeric entries in `.versions/`.** Compounds the race above. **Fix:** log + reject unknown filenames.
- **`src/backend/azure/detect.rs:232` — `tenant_id` not validated as UUID before injection into filter.** Belt-and-braces with the OData fix above.
- **`src/utils/network.rs:136-161` — vault-name-from-URL extraction has a chain of fallbacks ending in `<unresolved from: …>`.** That string ends up in user-visible errors and breaks log greppability. **Fix:** make the extractor a `Result<VaultName, UrlShape>` and surface the URL shape in a structured error.
- **`src/config/context.rs:82` — `usage_count.saturating_add(1)` is a non-atomic RMW.** Concurrent calls drop increments. **Fix:** if it matters, make it atomic; if not, document that it's best-effort.
- **`src/secret/manager.rs:698` — `get_secret_version` returns the unsanitized requested name in the response struct, while sibling functions return the sanitized form.** API-shape inconsistency. **Fix:** return a struct with both `requested_name` and `stored_name`.
- **`src/blob/manager.rs:853-863` — `chunk_size_mb` and `max_concurrent_uploads` not validated for `0`.** Division by zero / no progress. **Fix:** clamp to a sane minimum at parse time.
- **`src/vault/operations.rs:1000-1025` — `resolve_principal_ids` returns empty map on Graph error with no log.** Role assignments silently un-resolved. **Fix:** log at warn; bubble a soft error so callers can choose to fail.
- **`src/secret/manager.rs:1217` — `rollback_secret` reverts tags silently.** **Fix:** prompt or print a diff under `--show-tag-changes`.
- **`src/blob/sync.rs:46-47` — 2-second mtime epsilon; FS truncation can still mis-classify.** **Fix:** treat equal-by-epsilon as "compare hashes".
- **`src/vault/operations.rs:1081` — `id.split('/').nth(4)` for resource group; index drift if Azure ARM ID shape changes.** **Fix:** parse as a typed ARM-ID with `?` on shape mismatch.

### P4 — Nit / style

- **`src/cli/secret_ops.rs` — repeated `.expect("use_trait_path guarantees Some")` (10+ sites).** Refactor `use_trait_path()` to return `Option<&dyn Backend>` and bind once.
- **`src/tui/mod.rs:60-62` — teardown calls `.ok()` on every cleanup step.** At minimum log the failure once; users currently can't tell why their terminal is wedged.
- **`src/cli/helpers.rs:154-172` — clipboard errors collapsed to a generic string.** Distinguish "tool not found" from "tool failed" so users know whether to install something.
- **`src/cli/file_ops.rs:1216-1220` — recursive download defaults to `.`.** Print the resolved target before any write so the user can abort.
- **`src/backend/local/secrets.rs:572-576` — version numbers are re-indexed during `list_versions` instead of preserved.** Confusing for users who reference v3 and later see it renumbered to v2.
- **`src/backend/local/secrets.rs:441-451` — `.deleted.json` includes `original_name` and `deleted_at` in plaintext.** Same class as P1 metadata perms; once perms are tightened this becomes acceptable, but consider whether trash needs the original name at all.
- **`src/backend/local/crypto.rs:73-75, 106-108` — decryption matches `Recipients` and falls into a silent `_` arm.** Log the unexpected variant.
- **`src/backend/local/crypto.rs:123` — key-file read has no size cap.** `1 KiB` is plenty.
- **`src/backend/local/secrets.rs:66-71` — `decode_name` is `#[allow(dead_code)]`.** Decide: keep + use, or delete.
- **`src/backend/azure/detect.rs:575` — test uses `.unwrap()` on `detect_environment()`.** Skip when Azure CLI is absent.
- **`src/secret/manager.rs:377, 496, 648, 955` — `attributes.enabled` parse pattern is inconsistent.** Always `.and_then(as_bool).unwrap_or(true)`.
- **`src/cli/commands.rs:68-71` — `built_info::GIT_COMMIT_HASH_SHORT.unwrap_or("unknown")` is fine but masks build-script breakage.** Print "unknown" + a one-line warning at first invocation.
- **`src/utils/helpers.rs:56-62` — `parse_connection_string` does no key/value validation.** Constrain keys to `[A-Za-z][A-Za-z0-9_]*`.
- **`src/cli/secret_ops.rs:1629, 1701` — `.expect("single-vault find always resolves a vault name")`.** Replace with structured error.

---

## Suggested execution order

1. The three "this week" P1s in the narrative (env-clear, panic hook, file perms helper).
2. The two refactor-shaped P1/P2 items: `write_private` helper + `odata_eq`/url-builder helper. Each unblocks ~6 individual findings.
3. Per-vault file lock for the local backend. Closes the racy-versioning cluster.
4. Pagination caps + `read_error_body` helper. Closes the unbounded/silent cluster.
5. `xv upgrade` signature verification — design call, schedule separately.

Everything in P3/P4 is fine to batch into a single "hardening" PR after the items above land.
