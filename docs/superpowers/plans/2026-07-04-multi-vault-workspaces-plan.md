# Multi-Vault Workspaces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Multiple vaults, potentially on different backends, open simultaneously — per the approved spec `docs/superpowers/specs/2026-07-04-multi-vault-workspaces-design.md`.

**Architecture:** A new `src/workspace/` module owns the workspace model (attached vaults + default), colon-address parsing, and resolution; `BackendRegistry` grows lazy multi-backend construction; commands consult the workspace only when one exists, so the no-workspace path stays byte-identical. State lives in the context store with a `.xv.toml` `vaults = [...]` overlay that replaces it.

**Tech Stack:** Rust, serde (context JSON + TOML), existing `BackendRegistry`/`BackendRef`, hermetic e2e harnesses (`tests/common/mod.rs`, local backend; AWS smithy mocks).

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-04-multi-vault-workspaces-design.md` — read completely before any task.
- **No workspace ⇒ byte-identical behavior on every command** (pinned by tests; the record-types precedent).
- Colon addressing: `alias ":" path`; `/` remains folders-only; exact-name-first before alias interpretation.
- Writes NEVER search: unqualified destructive/write verbs target the default vault only.
- Ambiguous unqualified read → `xv-ambiguous-secret`, exit `13`, message lists qualified forms.
- Union/ambiguity reads fail loud when any attached vault errors — no partial unions, no `--best-effort`.
- Lazy backend construction: a command touching only AWS vaults must never construct/authenticate the Azure backend.
- Azure PUT None-semantics rules apply to any write-back (see `azure-put-none-semantics` patterns in `src/cli/secret_ops.rs`).
- Hermetic e2e (isolated config/env; scrubbed-env run `env -i PATH="/usr/bin:/bin:$(dirname $(which cargo))" HOME=/nonexistent <binary>` before declaring done). NEVER `git stash` — cp-backup + `git checkout HEAD -- <file>` for fail-first proofs.
- Gate per phase: `cargo fmt`; `cargo clippy --all-targets --features aws` AND `--features "aws tui"` (0 warnings); `cargo build --features aws`; `cargo test --features aws` (known clipboard timing flake acceptable iff passing in isolation).
- Commits end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

## Phase → PR mapping

- **Phase A (Tasks 1–6):** workspace core — PR `feat: multi-vault workspaces core (cx add, colon addressing, default-vault writes)`.
- **Phase B (Tasks 7–10):** union views — PR `feat: workspace union ls/find and read resolution`.
- **Phase C (Tasks 11–13):** integration — PR `feat: workspace aliases in URIs/templates, cross-vault mv/copy, TUI`.

Each phase: executor implements on a branch from origin/main → independent review pass → push → PR → Bugbot/CI watch → merge before the next phase starts.

---

### Task 1: Workspace model + persistence

**Files:**
- Create: `src/workspace/mod.rs` (model + resolution; submodules if it grows past ~400 lines)
- Modify: `src/config/context.rs` (context schema gains an optional `workspace` section)
- Modify: `src/config/project.rs` (`EnvProfile` gains `vaults: Vec<WorkspaceEntryConfig>`)
- Modify: `src/lib.rs`, `src/main.rs` (`pub mod workspace;` — both module trees)
- Test: unit tests in `src/workspace/mod.rs` + `src/config/project.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct WorkspaceEntry { pub alias: String, pub backend: String, pub vault: String }
  pub struct Workspace { pub entries: Vec<WorkspaceEntry>, pub default_alias: String,
                         pub source: WorkspaceSource }          // Context | ProjectToml
  impl Workspace {
      pub fn entry(&self, alias: &str) -> Option<&WorkspaceEntry>;
      pub fn default_entry(&self) -> &WorkspaceEntry;
      pub fn validate(&self, backend_names: &[&str]) -> Result<()>;
      // unique aliases; alias charset = vault-name charset; exactly one default;
      // alias must not equal any registry backend name (xv:// grammar collision)
  }
  // resolution: .xv.toml overlay REPLACES context; None = no workspace (degenerate case)
  pub async fn resolve_workspace(config: &Config) -> Result<Option<Workspace>>;
  // serde shape for both stores:
  #[derive(Serialize, Deserialize)] pub struct WorkspaceEntryConfig {
      pub vault: String,
      #[serde(default)] pub backend: Option<String>,
      #[serde(default)] pub alias: Option<String>,
      #[serde(default)] pub default: bool,
  }
  ```
- Consumes: `ContextManager` serde JSON (`src/config/context.rs:96-200`, `#[serde(default)]` for the new field so old context files load); `EnvProfile` (`src/config/project.rs`); `find_project_config`/`resolve_env` (post-#334 `Result<Option<…>>` shape).

- [ ] **Step 1: Failing unit tests** — `workspace_validate_rejects_duplicate_alias`, `workspace_validate_rejects_multi_default` / `_no_default_multi_entry`, `workspace_validate_rejects_alias_matching_backend_name` (`"azure"`), `single_entry_implicit_default`, `entry_config_alias_defaults_to_vault_name`, `project_vaults_block_parses` (TOML from the spec's schema section), `resolve_prefers_project_over_context` (overlay REPLACES — context entries absent from result), `resolve_none_when_no_workspace_anywhere`.
- [ ] **Step 2: Run** `cargo test --lib workspace config::project` — FAIL.
- [ ] **Step 3: Implement** model, validation, context field (`#[serde(default)] pub workspace: Option<WorkspaceState>` on `ContextManager` with entries + default), project parsing, `resolve_workspace` (project overlay → context → None). Fail-closed validation errors follow the `[types.*]` precedent wording.
- [ ] **Step 4: Run** — PASS; also `cargo test --lib config` (no regressions; old context JSON fixtures still load).
- [ ] **Step 5: Commit** `feat: workspace model, context schema, .xv.toml vaults overlay`

### Task 2: Lazy multi-backend registry

**Files:**
- Modify: `src/backend/registry.rs`
- Test: unit tests in `registry.rs` + `tests/aws_backend_tests.rs` (smithy mocks)

**Interfaces:**
- Consumes: existing `BackendRegistry::from_config` (builds ONE backend from `effective_backend_name`, `src/backend/registry.rs:47`), `named_backends` resolution (`from_named_entry`).
- Produces:
  ```rust
  impl BackendRegistry {
      /// Register a backend name for on-demand construction without building it.
      pub fn with_lazy(config: &Config, names: &[String]) -> Result<Self, BackendError>;
      /// Get-or-construct. First use materializes; errors name the backend.
      pub fn materialize(&self, name: &str) -> Result<Arc<dyn Backend>, BackendError>;
  }
  ```
  Implementation: interior `Mutex<HashMap<String, Arc<dyn Backend>>>` cache + a stored config snapshot of the pieces needed per backend kind (mirror what `from_config`/`from_named_entry` read). `active()`/`get()` keep today's behavior for the degenerate case.
- [ ] **Step 1: Failing tests** — `materialize_constructs_once` (two calls, same Arc), `materialize_unknown_name_errors`, `lazy_never_constructs_unreferenced_backend` (register azure+local, materialize local only; assert azure constructor not run — expose a test-only construction counter or use the fact that Azure construction requires auth config and would Err in the test env: registering must NOT error, materializing azure must Err), smithy-mock test `workspace_touching_only_aws_never_builds_azure`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS + full `--lib backend`.
- [ ] **Step 5: Commit** `feat: lazy multi-backend construction in BackendRegistry`

### Task 3: Colon address parsing + exact-name-first resolver

**Files:**
- Create: `src/workspace/address.rs`
- Test: unit tests in `address.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct SecretAddress { pub alias: Option<String>, pub path: String }
  pub fn parse_address(raw: &str) -> SecretAddress;
  // split on FIRST ':'; alias side must satisfy the alias charset or the whole
  // string is path (e.g. "a:b:c" → alias "a", path "b:c" only if "a" is charset-valid)
  /// Resolution order (spec §Addressing): with a workspace, an exact secret
  /// name match in the default vault OR any attached vault wins before alias
  /// interpretation; then alias:path; unknown alias errors listing attached aliases.
  pub enum ResolvedTarget { Entry(WorkspaceEntry, String /*path*/), NoWorkspace(String) }
  ```
  (The exact-match probe itself lives in Task 4 where backend access exists; this task pins the pure parsing.)
- [ ] **Step 1: Failing tests** — `parse_bare_name`, `parse_alias_and_folder_path` (`work:app/db/pass`), `parse_colon_in_path_when_prefix_not_charset_valid`, `parse_preserves_multi_colon_path`, `parse_empty_alias_is_path`.
- [ ] **Step 2–4:** TDD cycle. **Step 5: Commit** `feat: workspace colon-address parsing`

### Task 4: `get`/`set` workspace semantics

**Files:**
- Modify: `src/cli/secret_ops.rs` (`execute_secret_get_direct`, `execute_secret_set_direct`: workspace-aware target resolution before existing logic)
- Modify: `src/error.rs` (`AmbiguousSecret { name, candidates }`, code `xv-ambiguous-secret`, exit `13`; add the row to `docs/exit-codes.md`)
- Test: `tests/e2e_workspaces.rs` (new file; harness note below)

**Interfaces:**
- Consumes: Tasks 1–3; `resolve_workspace`; `materialize`.
- Produces: the resolution helper reused by every later verb:
  ```rust
  /// Resolve a raw CLI secret argument against the optional workspace.
  /// mode Read: unqualified searches all entries (bounded-concurrent exists-
  /// probes), 1 match → that entry, ≥2 → AmbiguousSecret, 0 → not-found with
  /// cross-workspace suggestions. mode Write: unqualified → default entry, no search.
  pub async fn resolve_secret_target(
      raw: &str, ws: Option<&Workspace>, registry: &BackendRegistry, mode: TargetMode,
  ) -> Result<(TargetVault, String /*path*/)>;
  pub enum TargetMode { Read, Write }
  ```
  Exact-name-first lives here: in Read mode, before alias interpretation, probe the parsed FULL raw string as a name in scope; any hit short-circuits (mirrors inject's dot rule; comment referencing it).
- Harness: `tests/e2e_workspaces.rs` builds TWO local stores via `[named_backends.local-a]`/`[named_backends.local-b]` in the isolated `xv.conf` — real multi-backend workspaces with zero cloud (spec §Testing).
- [ ] **Step 1: Failing e2e tests** — `cx_workspace_get_unqualified_unique_match`, `get_ambiguous_errors_exit_13_lists_qualified_forms`, `get_qualified_reads_named_vault`, `get_unknown_alias_lists_attached`, `set_unqualified_writes_default_only` (secret lands in default, NOT in the other vault even though absent there), `set_qualified_writes_named_vault`, `exact_name_with_colon_wins_over_alias` (local secret literally named `work:x`), `no_workspace_byte_identical` (fixture without workspace: `get`/`set` output byte-compared to a pre-recorded run).
- [ ] **Step 2: Run** — FAIL (workspace state can't even be created yet — these tests seed context JSON directly until Task 5 lands the CLI; document that in the file header).
- [ ] **Step 3: Implement** resolver + wire `get`/`set`. **Step 4: Run** — PASS + full suite.
- [ ] **Step 5: Commit** `feat: workspace-aware get/set with ambiguity and default-vault writes`

### Task 5: `xv cx` command surface

**Files:**
- Modify: `src/cli/commands.rs` (`Context` gains `visible_alias = "cx"`; `ContextCommands` gains `Add { vault, backend, r#as, default, local, force }`, `Rm { alias, local }`, `Default { alias, local }`, and `Ls` listing the workspace — check the existing `ContextCommands` variants at src/cli/commands.rs:1432 first and follow their arg style)
- Modify: `src/cli/system_ops.rs` or the existing context ops file (wherever `Commands::Context` dispatches — src/cli/commands.rs:2007) — new executors
- Modify: `src/cli/config_ops.rs`/context display paths: `context use` with a workspace present → error pointing at `cx default`
- Test: `tests/e2e_workspaces.rs`

**Behavior (spec §Workspace management):** alias defaults to vault name; backend defaults to active; first entry becomes default; `add` probes the vault exists (list call) unless `--force`; `rm` of default errors unless another remains/named; removing last entry deletes the workspace; duplicate alias / backend-name collision errors at add time.
- [ ] **Step 1: Failing e2e tests** — `cx_add_ls_rm_roundtrip`, `cx_first_add_becomes_default`, `cx_add_duplicate_alias_errors`, `cx_add_alias_colliding_with_backend_name_errors`, `cx_rm_default_requires_replacement`, `cx_rm_last_entry_restores_single_vault_behavior`, `context_use_with_workspace_errors_pointing_at_cx_default`, `cx_ls_shows_source_context_vs_project` (with a `.xv.toml` `vaults` overlay), `cx_add_probes_vault_exists_unless_force`.
- [ ] **Step 2–4:** TDD cycle; rewrite Task 4's context-JSON seeding to go through `cx add` where it makes the tests clearer.
- [ ] **Step 5: Commit** `feat: xv cx add/rm/default/ls workspace management`

### Task 6: Phase A gate

- [ ] Full verification gate (Global Constraints), scrubbed-env run of `e2e_workspaces`.
- [ ] Docs stub: one README paragraph under a new "Multi-vault workspaces (preview)" heading covering `cx add` + colon addressing + default writes, marked as Phase A of the spec; CHANGELOG `## Unreleased` → `### Added` entry.
- [ ] Independent review pass → fixes → push → PR → Bugbot/CI → merge.

---

### Task 7: Union `ls`

**Files:** Modify `src/cli/secret_ops.rs` (list path: per-entry listing via `materialize`, merge, VAULT column only when ≥2 entries), `src/cache/models.rs` (key gains backend+vault; bump `secrets-list-v2.json` → `-v3`); Test `tests/e2e_workspaces.rs`.
**Behavior:** `--filter`/`--type`/folder/`--group` apply per vault then merge; stable sort alias-then-name; pagination over the merged set; single-vault/no-workspace output byte-pinned; any vault erroring → whole command fails naming it.
- [ ] Failing tests: `ls_union_shows_vault_column_when_multi`, `ls_single_vault_output_unchanged` (byte-pin), `ls_union_composes_with_filter_and_type`, `ls_union_pagination_over_merged_set`, `ls_union_fails_loud_when_vault_unreachable` (unreachable = entry pointing at a named backend whose store path is a nonexistent dir with creation disabled — verify the local backend errors there; if it auto-creates, use an entry whose backend name isn't in named_backends), cache-schema test per #323 precedent.
- [ ] TDD cycle → **Commit** `feat: workspace union ls with per-vault filters and v3 cache keys`

### Task 8: Union `find`

**Files:** Modify `src/cli/secret_ops.rs` (`execute_secret_find_direct`: candidate set = union of attached vaults, rows prefixed `alias/` mirroring today's `--all-vaults` vault-prefix style); Test `tests/e2e_workspaces.rs`.
**Behavior:** `--all-vaults` unchanged (superset, documented distinction); `--filter` pre-filter per vault; scoring over merged candidates.
- [ ] Failing tests: `find_unions_workspace`, `find_all_vaults_still_superset`, `find_filter_composes_in_union`.
- [ ] TDD cycle → **Commit** `feat: workspace union find`

### Task 9: Read-verb resolution + capability gating

**Files:** Modify `src/cli/secret_ops.rs` (`history`, `rollback` version listing, and every read embedded in a read verb route through `resolve_secret_target(Read)`; destructive verbs — `delete`/`restore`/`purge`/`rotate`/`update` — route through `TargetMode::Write` explicitly with a test proving no search); capability gating for union views (`ls --deleted` skips non-soft-delete vaults with a stderr note naming vault+backend).
- [ ] Failing tests: `history_ambiguous_errors`, `delete_unqualified_targets_default_never_searches` (secret exists ONLY in non-default vault; unqualified delete errors not-found rather than deleting it), `ls_deleted_skips_incapable_vault_with_note`.
- [ ] TDD cycle → **Commit** `feat: workspace read resolution for history/rollback; capability-gated unions`

### Task 10: Phase B gate — verification, review, PR, merge (as Task 6).

---

### Task 11: Aliases in URIs and templates

**Files:** Modify `src/cli/secret_ops.rs` (inject/run reference resolution: vault slot of `{{ secret:vault/name }}` and `xv://vault/name` checks workspace aliases FIRST, then raw vault names; `xv://backend:vault/name` bypasses aliases — extend the resolution beside the existing `BackendRef` handling, `src/backend/addressing.rs` untouched unless a helper fits there); Test `tests/e2e_workspaces.rs`.
**Behavior:** `#field` fragments compose; failures collect per the #319 fail-fast contract.
- [ ] Failing tests: `inject_alias_uri_resolves`, `inject_raw_vault_name_still_works_when_no_alias_matches`, `inject_backend_qualified_uri_bypasses_alias`, `run_env_alias_uri_resolves`, `inject_alias_with_field_fragment`.
- [ ] TDD cycle → **Commit** `feat: workspace aliases in xv:// URIs and inject templates`

### Task 12: Cross-vault `mv`/`copy` via aliases

**Files:** Modify `src/cli/mv_ops.rs` (alias-qualified source/dest: `mv work:secret stage:/` → route to the existing cross-vault `move` machinery with resolved (backend, vault) pairs; `--force` semantics and metadata preservation ride the #315 path; destination tag-budget checked at the destination backend's caps before any write), `src/cli/secret_ops.rs` (`copy` alias support on `--from`/`--to`); Test `tests/e2e_workspaces.rs`.
- [ ] Failing tests: `mv_alias_to_alias_moves_across_stores`, `mv_alias_preserves_record_envelope_and_metadata` (typed record moves intact), `copy_accepts_aliases_in_from_to`, `mv_alias_dest_tag_budget_checked_at_destination` (unit-level with Azure caps).
- [ ] TDD cycle → **Commit** `feat: cross-vault mv/copy via workspace aliases`

### Task 13: TUI + docs + Phase C gate

**Files:** Modify `src/tui/data.rs`/`view.rs` (vault pane lists workspace entries as `alias (backend)` when a workspace exists; selection scopes the secrets pane — extract any new pure formatter with unit tests, per the Task-11 record-types precedent); README (promote the workspace section out of preview: cx verbs, addressing, union views, ambiguity, capability notes, `.xv.toml` `vaults` schema), docs/env-profiles.md (`vaults` overlay), docs/exit-codes.md (13 row — added in Task 4, verify), CHANGELOG.
- [ ] TUI formatter unit tests + tmux smoke against a two-store workspace (documented in commit body); every README example executed against a scratch multi-store setup (the #333 discipline — README examples are run, not proofread).
- [ ] Full gate + review + PR + merge.

---

## Self-review (done at write time)

- **Spec coverage:** decisions 1–4 → Tasks 3/4/5 (model/addressing/ambiguity) and 1 (overlay); Architecture → Tasks 1–2; cx surface → Task 5; Addressing → Tasks 3/4/11; Reads → Tasks 4/7/8/9; Writes → Tasks 4/9/12; Capabilities → Task 9; Caching → Task 7; Error table → Tasks 1/4/5/7; Backward compat → byte-pin tests in Tasks 4/7 + degenerate-case constraint; Phasing → PR mapping; Testing → harness notes in Tasks 4/7 + smithy mocks in Task 2; Out of scope respected (no merge semantics, no partial unions, no workspace rotate, files single-vault).
- **Placeholders:** none — every task names its tests and behavior; two deliberate discovery points (local-backend unreachable-store construction in Task 7, existing ContextCommands arg style in Task 5) instruct verification rather than assuming.
- **Type consistency:** `Workspace`/`WorkspaceEntry`/`WorkspaceEntryConfig`/`resolve_workspace`/`materialize`/`SecretAddress`/`parse_address`/`resolve_secret_target`/`TargetMode` used identically across Tasks 1–12; exit 13 and `xv-ambiguous-secret` consistent with the spec.
