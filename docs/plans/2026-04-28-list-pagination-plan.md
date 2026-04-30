# List Commands Pagination — Implementation Plan

**Goal:** Add user-visible pagination to `xv` list-style commands without changing default output or breaking machine-readable consumers.

**Non-goals:**

- Do not change Azure API traversal semantics. Secrets and vaults already follow Azure `nextLink`; blobs already stream SDK pages. This plan adds CLI result pagination after filtering/sorting.
- Do not implement cursor/token pagination in the first pass.
- Do not wrap existing JSON/YAML/CSV array outputs with metadata, because that would break scripts.

**Assumptions from repo inspection:**

- Top-level `xv list` is the secret list command in `src/cli/commands.rs` and dispatches to `execute_secret_list_direct()` in `src/cli/secret_ops.rs`.
- `xv vault list` is in `VaultCommands::List` and dispatches through `src/cli/vault_ops.rs` to `VaultManager::list_vaults_formatted()`.
- `xv file list` already has `--limit`, but truncation currently happens in `BlobManager::list_files_hierarchical()` via `FileListRequest.limit`; pagination should move to the CLI/display layer so page 2+ can work.
- Access list commands also collect full result vectors before formatting: `xv share list <secret>` and `xv vault share list <vault>`.
- Cache entries should continue storing full unpaginated list results, not page slices.

---

## Proposed CLI UX

Add common flags to list commands:

```text
--page <N>          1-based page number; default 1 when --page-size/--limit is used
--page-size <N>     number of rows per page
```

Behavior:

- With no pagination flags: unchanged output.
- With `--page-size N`: show only rows for page 1.
- With `--page P --page-size N`: show rows for that page.
- `--page` without `--page-size` should be rejected with a clear clap/config error.
- Values must be positive integers.
- Human formats (`table`, `plain`, `raw`) get a footer like:

```text
Showing 51-100 of 137 item(s) — page 2 of 3
Next page: xv list --page 3 --page-size 50
```

- Machine formats (`json`, `yaml`, `csv`, `template`) output only the paged rows and no footer.
- Existing `xv file list --limit N` remains supported as a backward-compatible alias for `--page-size N` on page 1. If both `--limit` and `--page-size` are provided, error.

---

## Task 1 — Add reusable pagination primitives

**Files:**

- Create `src/utils/pagination.rs`
- Modify `src/utils/mod.rs`
- Modify `src/cli/commands.rs` or a small `src/cli/pagination.rs` helper if keeping CLI args separate is cleaner

Implementation shape:

- Add a `PaginationArgs` clap struct with optional `page: Option<usize>` and `page_size: Option<usize>`.
- Add a runtime `Pagination` struct with `page: usize` and `page_size: Option<usize>`.
- Add `Pagination::from_args(page, page_size)` validation:
  - `page == Some(0)` or `page_size == Some(0)` => error
  - `page.is_some() && page_size.is_none()` => error
  - no flags => disabled pagination
- Add `paginate_slice<T: Clone>(items: &[T], pagination: Pagination) -> Page<T>`.
- `Page<T>` should include:
  - `items: Vec<T>`
  - `total_items: usize`
  - `page: usize`
  - `page_size: Option<usize>`
  - `total_pages: Option<usize>`
  - `start_index_1_based: Option<usize>`
  - `end_index_1_based: Option<usize>`

Design note: keep this generic and output-format agnostic. Formatting footer text should live near CLI display code, not in the helper.

Validation gate:

- Unit tests for disabled pagination, first page, middle page, last partial page, page past end, zero values, and `--page` without `--page-size`.

---

## Task 2 — Wire pagination into secret list (`xv list`)

**Files:**

- `src/cli/commands.rs`
- `src/cli/secret_ops.rs`

Steps:

1. Add `--page` and `--page-size` to `Commands::List`.
2. Pass pagination args through `Cli::execute()` into `execute_secret_list_direct()` and then `execute_secret_list()`.
3. In `execute_secret_list()`:
   - keep fetching `all_secrets` exactly as today for cache correctness;
   - apply existing `--all`, `--group`, `--expired`, and `--expiring` filters;
   - paginate the filtered `secrets` vector immediately before formatting;
   - format only `page.items`;
   - return the original `all_secrets` for cache writes.
4. In `display_cached_secret_list()`, apply the same filtering and pagination before formatting cached data.
5. Add a human-only footer after the existing count line. For machine formats, print no extra metadata.

Important compatibility point: the existing count line says e.g. `12 secret(s) in vault 'x'`. For paginated output, make it clear this is page count versus total, e.g. `Showing 10 of 137 secret(s) in vault 'x'` or use a separate footer.

Validation gate:

- Clap parse test for `xv list --page-size 25 --page 2`.
- Unit/helper test that filtering happens before pagination.
- Existing cache behavior remains: cache key is still `SecretsList { vault_name }`, and cache stores all secrets.

---

## Task 3 — Wire pagination into vault list (`xv vault list`)

**Files:**

- `src/cli/commands.rs`
- `src/cli/vault_ops.rs`
- Optionally `src/vault/manager.rs` if display responsibility stays there

Recommended small refactor:

- Keep `VaultManager::list_vaults_formatted()` available if other callers need it, but for `execute_vault_list()` prefer fetching raw vaults then formatting in CLI code so caching and pagination stay together.
- Alternatively add a `pagination` parameter to `list_vaults_formatted()` and ensure it returns the full unpaginated `Vec<VaultSummary>`.

Steps:

1. Add `--page` and `--page-size` to `VaultCommands::List`.
2. On cache hit, paginate cached `Vec<VaultSummary>` before formatting.
3. On cache miss, fetch full vault list, write full list to cache, paginate only for display.
4. Preserve `--format` behavior and current `auto` resolution.

Validation gate:

- Clap/help tests for `xv vault list --page-size 50 --page 2`.
- Cache test or focused unit test proving cached data is not page-sliced.

---

## Task 4 — Convert file list `--limit` into page-size-compatible pagination

**Files:**

- `src/cli/file.rs`
- `src/cli/file_ops.rs`
- `src/blob/models.rs`
- `src/blob/manager.rs`
- `tests/file_commands_tests.rs`

Steps:

1. Add `--page` and `--page-size` to `FileCommands::List`.
2. Keep existing `--limit` as compatibility syntax.
3. Resolve file pagination rules in `execute_file_list()`:
   - no pagination and no limit => unchanged;
   - `--limit N` => `page_size=N`, `page=1`;
   - `--page-size N` => `page_size=N`, `page=page.unwrap_or(1)`;
   - both `--limit` and `--page-size` => error.
4. Stop passing user pagination as `FileListRequest.limit` for normal display; fetch the full filtered/sorted list, then paginate in `file_ops`.
5. Either remove `FileListRequest.limit` in a later cleanup or leave it unused for now with a comment; do not let it pre-truncate data needed for page 2+.
6. Adjust cache logic:
   - pagination-only flags should still allow cache use because the cached value is full list data;
   - prefix/group/recursive still define the underlying result set.
7. Apply pagination before `display_file_list_items()`.
8. Add human-only footer after the existing file/directory count.

Validation gate:

- Update existing `tests/file_commands_tests.rs` to verify `limit`, `page`, and `page_size` parsing.
- Add helper tests that page 2 for a synthetic list returns the expected files.
- Verify `--limit` behavior remains page 1 truncation.

---

## Task 5 — Add pagination to access list commands

**Files:**

- `src/cli/commands.rs`
- `src/cli/secret_ops.rs`
- `src/cli/vault_ops.rs`

Commands:

- `xv share list <secret-name>`
- `xv vault share list <vault-name>`

Steps:

1. Add `--page` and `--page-size` to `ShareCommands::List` and `VaultShareCommands::List`.
2. After role resolution/filtering (`resolve_and_filter_roles()`), paginate roles before formatting.
3. Use the common human-only footer.
4. Keep service account filtering (`--all`) before pagination.

Validation gate:

- Clap parse/help tests for both commands.
- Unit/helper tests over a synthetic `Vec<VaultRole>` if practical.

---

## Task 6 — Documentation and examples

**Files:**

- `README.md`
- Possibly `docs/FEATURES.md`

Add concise examples:

```bash
xv list --page-size 50
xv list --page 2 --page-size 50
xv vault list --page-size 25
xv file list --page-size 100 --page 3
xv file list --limit 100          # compatibility alias for first page
```

Document that pagination is applied after filters:

```bash
xv list --group prod --page-size 20 --page 2
```

Mention that machine formats return only selected rows and no footer.

---

## Task 7 — Tests and quality gates

Preferred gates:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Focused tests if full suite is slow or Azure-dependent:

```bash
cargo test pagination
cargo test --test cli_integration_tests -- --nocapture
cargo test --test file_commands_tests -- --nocapture
```

Current local caveat from this planning session: `cargo` was not installed/available in PATH on the host, so implementation verification needs either Rust tooling installed or a dev environment/container with cargo.

---

## Risks and mitigations

- **Breaking JSON/YAML/CSV consumers:** Do not add metadata wrappers by default. Keep machine output as arrays/rows only.
- **Cache stores page slices by accident:** Always cache pre-pagination vectors.
- **File page 2 impossible if blob manager truncates early:** Move limit/pagination out of `BlobManager` into `file_ops` display flow.
- **Ambiguous `--limit` vs `--page-size`:** Keep `--limit` only for file list compatibility and reject combinations with `--page-size`.
- **Large result sets still fetched fully:** This matches existing behavior. A later cursor/server-side pagination feature can optimize network/memory separately.

---

## Smallest useful first milestone

Implement common pagination helper plus `xv list` secret pagination only. That exercises the cache, filtering-before-pagination, table footer, and machine-output compatibility patterns before applying the same structure to vault/file/access list commands.
