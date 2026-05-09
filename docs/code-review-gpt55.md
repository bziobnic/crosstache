# Crosstache Code Review (GPT-5.5)

## P1 — Critical
- [src/secret/manager.rs:273] Azure vault names are interpolated directly into Key Vault URLs (`https://{vault_name}.vault.azure.net/`) without validating that `vault_name` is an Azure Key Vault DNS label. The REST paths repeat the same pattern at [src/secret/manager.rs:383] and [src/secret/manager.rs:476] while sending an Authorization header at [src/secret/manager.rs:430]. A malicious vault name containing URL authority/path delimiters can redirect a vault-scoped bearer token to an attacker-controlled host if it reaches `reqwest` as a valid URL. Remediation: introduce a single `ValidatedVaultName` type for Azure (`^[a-zA-Z][a-zA-Z0-9-]{1,22}[a-zA-Z0-9]$`, plus Azure's no-consecutive-hyphen constraints if required), reject anything else before token acquisition, and build URLs with `url::Url` rather than string concatenation.

- [src/backend/local/vaults.rs:49] Local vault names are used as raw filesystem path components (`vaults_dir().join(name)`) and the same pattern is used by local secrets at [src/backend/local/secrets.rs:77], trash paths at [src/backend/local/secrets.rs:96], and local files at [src/backend/local/files.rs:32]. A configured or CLI-supplied vault name like `../../outside` can create, read, rename, or delete content outside the store, including via `delete_vault` at [src/backend/local/vaults.rs:204] and `purge_secret` at [src/backend/local/secrets.rs:792]. Remediation: validate local vault names with a conservative component-only policy or encode them like secret/file names, then canonicalize and assert every resolved path remains under the store root.

- [src/cache/models.rs:43] Cache keys also use raw `vault_name` path components. This lets malformed cache keys such as `secrets:../../target` resolve outside the cache directory; writes happen at [src/cache/manager.rs:171], renames at [src/cache/manager.rs:175], per-vault deletion at [src/cache/manager.rs:195], and lock creation at [src/cache/refresh.rs:90]. Remediation: encode or hash vault names for cache paths, reject separators and parent components in `CacheKey::from_str`, and add canonical containment checks before every cache write/remove.

- [src/backend/local/secrets.rs:300] Updating an existing local secret archives the current `.age` and `.meta.json` before writing the replacement. If encryption or metadata writing fails at [src/backend/local/secrets.rs:334] or [src/backend/local/secrets.rs:335], the active secret disappears or is left inconsistent. The same data-loss window exists in metadata updates with value changes at [src/backend/local/secrets.rs:571]. Remediation: write the new ciphertext and metadata to unique temp files, fsync, then atomically rename into place; only archive the previous version after the replacement is durable, or use a journaled transaction/rollback path.

- [src/utils/helpers.rs:21] Sensitive file writes use `OpenOptions::open` with `create/truncate`, which follows symlinks. `encrypt_to_file` has the same issue at [src/backend/local/crypto.rs:38]. If an attacker can place symlinks in a configured store/key/cache path, secret writes can clobber arbitrary files owned by the user. Remediation: on Unix use `custom_flags(O_NOFOLLOW | O_CLOEXEC)` plus `create_new` for temp files, verify `symlink_metadata` before writes, write through temp files in trusted directories, and use `rename` for the final swap.

## P2 — High
- [src/backend/local/files.rs:57] Local file metadata is written with `fs::write`, not the private writer used for secret metadata. File names, groups, metadata, tags, and possibly sensitive notes may be world-readable depending on umask. Remediation: use `write_private` for all local backend metadata and add permission assertions in tests.

- [src/cli/file_ops.rs:428] Single-file download defaults the output path to the remote blob name and writes directly at [src/cli/file_ops.rs:463]. Recursive download has explicit traversal protection at [src/cli/file_ops.rs:1294], but the single-file path does not; a blob named `../../.ssh/config` or an accidental malicious input can write outside the intended directory. Remediation: share the recursive/sync containment helper with single and multiple downloads, and treat `--output` as a directory unless `--rename` is explicitly used.

- [src/cli/file_ops.rs:1162] Multiple downloads pass the same `output.clone()` to every file. If `--output` names a file rather than a directory, each download overwrites the previous one, especially with `--force`. Remediation: require `--output` to be an existing or creatable directory for multi-downloads, join each remote basename under it, and test multi-download collision behavior.

- [src/secret/manager.rs:1959] Secret rename is implemented as create-new then delete-old. If creation succeeds and deletion fails, both names remain; if creation partially updates tags or metadata, the operation is not reversible. Remediation: expose a backend-level rename transaction where possible, or at least verify the new secret, only delete after explicit success, and surface a recovery plan when cleanup fails.

- [src/vault/operations.rs:133] Azure ARM resource IDs and URLs are built by string formatting unescaped `subscription_id`, `resource_group`, `vault_name`, and sometimes `secret_name`. This is less likely to leak tokens because the host is fixed, but it can address the wrong ARM path or break RBAC operations. Remediation: validate Azure resource names and URL-encode every path segment through a helper like `arm_resource_id_segment`.

- [src/scan/engine.rs:17] Scanner secret values are stored as ordinary `String`s, cloned again from `Zeroizing` at [src/scan/orchestrator.rs:61], and held in spawned tasks and the Aho-Corasick builder. This defeats the project's zeroization posture for a feature that loads every secret value. Remediation: store fetched values in `Zeroizing<String>` as long as possible, document unavoidable copies made by third-party automata, drop the engine promptly, and add tests that findings never serialize values.

- [src/blob/manager.rs:393] Blob downloads buffer the entire blob in memory; the nominal streaming API also calls `get_content()` at [src/blob/manager.rs:568]. Large-file upload stores every spawned chunk task in `upload_tasks` at [src/blob/manager.rs:654] and only awaits them after all chunks have been read. These paths can exhaust memory on large files. Remediation: stream downloads to the destination writer, await/upload chunks with bounded buffering, and enforce configurable max file sizes.

- [src/backend/local/files.rs:100] `LocalFileBackend` captures one `vault` at construction and its trait methods do not accept a vault argument, unlike `SecretBackend`. Changing context/default vault after startup will not affect local file operations. Remediation: include vault in file backend method signatures or create per-operation file backends from the resolved vault.

- [src/backend/azure/secrets.rs:204] Azure backend advertises optional operations through the trait, but `list_deleted_secrets`, `backup_secret`, and `restore_secret_from_backup` still delegate to placeholder implementations that return "not yet fully implemented". Remediation: align `BackendCapabilities`/CLI capability checks with implemented behavior or complete the REST implementations before exposing commands.

- [src/backend/local/secrets.rs:514] Local soft-delete moves active files into `.trash/{encoded_name}` without checking for an existing trash entry. Deleting the same secret name after recreate can overwrite or merge with prior deleted material. Remediation: make trash entries unique by deletion timestamp/version, fail on collision, and test delete-recreate-delete-restore sequences.

## P3 — Medium
- [src/cli/secret_ops.rs:51] `--stdin` secret input is trimmed before storage, and update stdin uses the same behavior around [src/cli/secret_ops.rs:724]. This corrupts secrets where leading/trailing whitespace or final newlines are significant. Remediation: preserve stdin bytes exactly by default; add an explicit `--trim` option if desired.

- [src/cli/secret_ops.rs:723] The direct trait update path cannot distinguish "leave expiry unchanged" from "clear expiry": `clear_expires` becomes `None`, which is also the no-op value. The local backend only updates expiry when `request.expires_on.is_some()` at [src/backend/local/secrets.rs:611]. Remediation: model optional updates as tri-state (`Unchanged`, `Set(T)`, `Clear`) for expiry, not-before, note, and folder.

- [src/config/context.rs:193] Context files are saved with `tokio::fs::write`, and local context initialization does the same at [src/config/context.rs:348]. They mostly contain vault metadata, but may include subscription/resource identifiers and should be treated as user-private configuration. Remediation: use the same sensitive-file helper as global config and set `.xv` directory permissions to owner-only where possible.

- [src/backend/azure/auth.rs:155] Azure CLI subprocess calls use `.output()` without a timeout, and this pattern repeats at [src/backend/azure/auth.rs:366] and throughout Azure environment detection. A hung `az` process can hang the CLI. Remediation: centralize `az` execution with a timeout, stderr size cap, and clear fallback behavior.

- [src/backend/azure/auth.rs:300] JWT payloads are decoded and trusted for tenant discovery without signature validation. This may be acceptable as a fallback for an SDK-returned token, but the security boundary is not documented and the helper is easy to reuse unsafely. Remediation: prefer authoritative SDK/account metadata, validate claim formats, and document that decoded claims are identity hints rather than proof.

- [src/backend/local/crypto.rs:138] Age identity files are loaded into an ordinary `String` and not zeroized. Remediation: read into `Zeroizing<String>` or a zeroizing byte buffer and avoid formatting private key material into normal strings beyond the age crate boundary.

- [src/backend/local/crypto.rs:139] Key file loading checks size with `metadata` and then reads the path, which is a TOCTOU window and follows symlinks. Remediation: open the file once with no-follow where supported, inspect metadata from the file handle, then read from that handle.

- [src/cache/refresh.rs:77] Cache lock acquisition is check-then-create and uses `File::create`, so two processes can both observe no lock and both create/truncate it. Remediation: use `OpenOptions::create_new(true)` and include process ID/timestamp metadata for stale-lock diagnostics.

- [src/scan/orchestrator.rs:19] Scanner file reads use `read_to_string` with no size cap and silently skip unreadable files. This can miss leaks in large but valid text files and hides permission problems. Remediation: stream scan with a max file size policy, report skipped files in human and machine output, and make unreadable files fail in hook/CI mode.

- [src/secret/manager.rs:802] `list_secrets` performs an extra `get_secret` call per listed secret to recover tags and metadata. This creates N+1 network behavior and can make large vaults slow or rate-limit-prone. Remediation: use list response metadata when sufficient, batch where APIs allow, and apply concurrency with retry/backoff.

- [src/cli/secret_ops.rs:2220] `stream_and_mask` reads child output until newline. A child that writes a very large line can grow memory substantially, and masking cannot catch secrets split across chunk boundaries if this is later changed to chunking. Remediation: implement bounded chunked masking with overlap equal to the longest secret length.

- [src/cli/vault_ops.rs:644] Env export writes raw `KEY=value` lines without shell quoting or escaping, so values with newlines, spaces, `#`, `$`, or quotes produce broken or dangerous env files. Remediation: implement POSIX-safe single-quote escaping or emit dotenv-compatible quoted values with tests.

- [src/utils/format.rs:174] CSV output is manually assembled in the formatter rather than using a CSV writer. This risks malformed output for commas, quotes, and newlines in secret names, notes, tags, or file metadata. Remediation: use the `csv` crate or a well-tested escaping helper.

- [src/backend/local/secrets.rs:146] Local metadata intentionally stores names, groups, folders, notes, tags, and content type in plaintext. That may be an acceptable design, but for a secrets manager it should be called out as a confidentiality limitation and users should be able to opt into encrypted metadata. Remediation: document the leakage clearly and consider encrypting metadata or separating public indexes from private annotations.

- [src/error.rs:637] There is a good test preventing error variants from carrying secret values, but similar guards are missing for cache entries, scan findings, structured output, logs, and tracing fields. Remediation: add snapshot/serialization tests that no value-like fields are emitted for findings, errors, list caches, and debug logs.

## P4 — Low
- [src/secret/manager.rs:493] Azure secret response parsing is duplicated across get, get-version, restore, and list-version paths. Remediation: factor a shared `SecretProperties::from_keyvault_json` parser to keep timestamp, tag, and recovery-level behavior consistent.

- [src/blob/manager.rs:6] Several comments say the blob implementation is a placeholder, but the module is active production code. Remediation: update comments so future maintainers do not under-review this path.

- [src/secret/manager.rs:382] Comments still mention Azure SDK v0.20 while `Cargo.toml` uses Azure SDK v0.21. Remediation: refresh stale comments during the next maintenance pass.

- [src/cli/file_ops.rs:814] `path_to_blob_name` silently drops root, prefix, current-dir, and parent-dir components. That is useful for uploads, but surprising for malformed input. Remediation: return a `Result` and reject paths that contain parent components instead of silently normalizing them away.

- [src/secret/manager.rs:418] Production code uses `expect("json!({}) always produces an Object")`. This is safe in practice, but unnecessary. Remediation: use `attributes.as_object().is_some_and(|o| !o.is_empty())` or construct a `serde_json::Map` directly.

- [src/secret/manager.rs:2020] `xv run` scans all inherited environment variables for `xv://` references even when `inherit_env` is false. Remediation: skip cross-vault URI resolution when the parent environment will be cleared, unless explicitly requested.

- [src/tui/update.rs:142] TUI copy-to-clipboard commands move secret values through ordinary `String`s. Remediation: keep TUI value state zeroizing where practical and clear it after the clipboard timeout.

- [src/main.rs:170] The only `unsafe` block resets SIGPIPE through libc. This is small and defensible, but it should have a short safety comment explaining why the signal call is process-global and performed before async work starts.

- [src/backend/local/secrets.rs:651] Version listing silently skips corrupted archived metadata. Remediation: include a warning/count in the result or expose a repair/verify command for local stores.

- [src/backend/local/secrets.rs:861] Local backend tests cover happy paths, but not path traversal, symlink writes, failed encryption rollback, duplicate trash entries, or permission modes. Remediation: add focused regression tests for each P1/P2 local-store issue.

- [src/cli/file_ops.rs:1203] Recursive download has traversal tests implied by comments but the single-file and sync helper behavior should be covered with malicious blob names and symlinked destination directories. Remediation: add filesystem tests around `../`, absolute paths, Windows prefixes, and symlinked parents.

- [src/scan/patterns.rs:62] The high-entropy fallback is regex-only and will flag many long identifiers while missing lower-entropy secrets. Remediation: either compute actual entropy with allowlists or present this as a low-confidence heuristic in output.

## Summary
Crosstache has a generally memory-safe Rust foundation and uses strong primitives for local secret encryption (`age`) and value zeroization in several user-facing paths. The main risks are not Rust memory unsafety; they are boundary validation, filesystem safety, durability, and whole-object buffering. The highest-priority fixes are to validate/encode every resource name before using it in URLs or filesystem paths, make local secret writes transactional and symlink-safe, harden cache path handling, and stream large blobs/files instead of buffering them. Test coverage is solid for basic behavior but needs adversarial security tests around traversal, symlinks, rollback failure, malformed cache keys, large files, and secret-value non-disclosure in logs/cache/output.
