# Record Types Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Typed secret records (built-in + custom types) with per-field metadata/secret storage, per the approved spec `docs/superpowers/specs/2026-07-03-record-types-design.md`.

**Architecture:** A new `src/records/` module owns type definitions, resolution (built-in → global config → project config), and the JSON envelope codec. Secret fields ride the existing secret value as a JSON document marked by `content_type = application/vnd.xv.record`; metadata fields ride tags prefixed `f.`; the type name rides the reserved tag `xv-type`. CLI verbs (`set`, `get`, `update`, `ls`, `type`) consume the module; no backend trait changes except two new capability fields for tag limits.

**Tech Stack:** Rust, serde_json (already a dependency), clap derive, existing e2e harnesses (`tests/e2e_local_backend.rs::TestEnv`, `tests/common/mod.rs`).

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-03-record-types-design.md` — read it before starting any task.
- Untyped secrets must be byte-identical in behavior on every code path (`get`/`run`/`inject`/`ls`).
- The content-type marker `application/vnd.xv.record` — never JSON sniffing — decides record-ness.
- Reserved tag `xv-type`; metadata-field tag prefix `f.`.
- Exactly one `primary` field per type; `primary` implies `kind = secret` and `required = true`.
- Fail before write: every validation error (required field, tag cap, tag value length) must abort before any backend call.
- All e2e tests hermetic (isolated config/env — reuse `TestEnv` / `xv_isolated_local*`; NEVER depend on host config; simulate CI with a scrubbed env before declaring done).
- NEVER use `git stash` (shared refs/stash across concurrent worktrees); use `cp` backups + `git checkout HEAD -- <file>` for fail-first proofs.
- Verification gate per task: `cargo fmt`, `cargo clippy --all-targets --features aws` (0 warnings), `cargo test --features aws` (the pre-existing `test_detached_clear_process_clears_clipboard` timing flake is acceptable iff it passes in isolation).
- Commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

## Phase → PR mapping

- **Phase A (Tasks 1–7):** core model + `set`/`get`/`type` — PR "feat: record types core (#<issue>)".
- **Phase B (Tasks 8–11):** `update`/convert/`ls`/TUI — PR "feat: record editing, conversion, and listing".
- **Phase C (Tasks 12–13):** `inject` field syntax + docs — PR "feat: inject field access for records + docs".

Each phase lands via the session-standard flow: executor implements on a branch from origin/main → independent code-review pass → push → PR → Bugbot/CI watch.

---

### Task 1: Records module — types, built-ins, validation

**Files:**
- Create: `src/records/mod.rs` (`pub mod types; pub mod envelope;` + re-exports)
- Create: `src/records/types.rs`
- Modify: `src/lib.rs` (add `pub mod records;`)
- Test: unit tests inside `src/records/types.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum FieldKind { Metadata, Secret }
  pub struct FieldDef { pub name: String, pub kind: FieldKind, pub required: bool, pub primary: bool }
  pub enum TypeSource { Builtin, Global, Project }
  pub struct RecordType { pub name: String, pub fields: Vec<FieldDef>, pub source: TypeSource }
  impl RecordType {
      pub fn primary(&self) -> &FieldDef;                    // the single primary field
      pub fn field(&self, name: &str) -> Option<&FieldDef>;
      pub fn validate(&self) -> Result<()>;                  // one primary; primary is secret+required; kebab-case names
  }
  pub fn builtin_types() -> Vec<RecordType>;                 // login, api-key, database per spec table
  ```
- Consumes: `CrosstacheError::config` for validation errors; kebab-case check mirrors `validate_folder_path`'s charset idiom (`src/utils/helpers.rs`).

- [ ] **Step 1: Write failing unit tests** — `builtin_types_are_valid` (every built-in passes `validate()`, `login` primary is `password`, `database` has optional secret `connection-string`), `validate_rejects_zero_primaries`, `validate_rejects_two_primaries`, `validate_rejects_non_secret_primary`, `validate_rejects_bad_field_name` (`"Bad Name"` fails, `"totp-seed"` passes).
- [ ] **Step 2: Run** `cargo test --lib records::types` — expect compile failure / test failures.
- [ ] **Step 3: Implement** the structs, `validate()`, and `builtin_types()` exactly per the spec's built-in table (login: username required metadata, url metadata, password primary; api-key: url + account metadata, key primary; database: host/port/database/username metadata, password primary, connection-string optional secret).
- [ ] **Step 4: Run** `cargo test --lib records::types` — expect PASS.
- [ ] **Step 5: Commit** `feat: records module with built-in types and validation`

### Task 2: Envelope codec + reserved-tag constants

**Files:**
- Create: `src/records/envelope.rs`
- Test: unit tests inside `src/records/envelope.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const RECORD_CONTENT_TYPE: &str = "application/vnd.xv.record";
  pub const TYPE_TAG: &str = "xv-type";
  pub const FIELD_TAG_PREFIX: &str = "f.";
  pub fn encode_envelope(fields: &BTreeMap<String, String>) -> Result<String>;   // deterministic key order
  pub fn parse_envelope(value: &str) -> Result<BTreeMap<String, String>>;        // strict: JSON object of strings only
  pub fn is_record(content_type: &str) -> bool;                                  // exact match on RECORD_CONTENT_TYPE
  ```

- [ ] **Step 1: Failing tests** — `roundtrip_preserves_fields`, `parse_rejects_non_object` (`"[1,2]"`, `"\"str\""`), `parse_rejects_non_string_values` (`{"a":1}`), `is_record_matches_exactly` (`"application/vnd.xv.record"` true; `"application/json"`, `""`, `"text/plain"` false), `encode_is_deterministic` (two BTreeMaps with same entries produce identical strings).
- [ ] **Step 2: Run** `cargo test --lib records::envelope` — FAIL.
- [ ] **Step 3: Implement** with `serde_json::Map` → `BTreeMap<String,String>`; parse errors wrap `CrosstacheError::config` naming the failure ("record envelope is not a JSON object of strings").
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat: record envelope codec and reserved tag constants`

### Task 3: Type config parsing + resolution/shadowing

**Files:**
- Modify: `src/config/settings.rs` (global `[types.<name>]` blocks in `xv.conf`; add `pub types: HashMap<String, RecordTypeConfig>` to `Config`, parsed in `load_from_file`)
- Modify: `src/config/project.rs` (same block in `.xv.toml`'s `ProjectConfig`)
- Modify: `src/records/types.rs` (resolution)
- Test: unit tests in `src/config/project.rs` + `src/records/types.rs`

**Interfaces:**
- Produces:
  ```rust
  // serde shape shared by both config layers (define once in records::types, re-use via serde):
  #[derive(Deserialize)] pub struct RecordTypeConfig { pub fields: Vec<FieldDefConfig> }
  #[derive(Deserialize)] pub struct FieldDefConfig {
      pub name: String,
      #[serde(default)] pub kind: Option<String>,      // "metadata" (default) | "secret"
      #[serde(default)] pub required: bool,
      #[serde(default)] pub primary: bool,
  }
  pub fn resolve_types(
      global: &HashMap<String, RecordTypeConfig>,
      project: &HashMap<String, RecordTypeConfig>,
  ) -> Result<Vec<RecordType>>;
  // precedence: project > global > builtin, matched by name; shadowing a builtin emits
  // output::warn("type '<name>' shadows a built-in type"); every resolved type passes validate().
  pub fn find_type<'a>(types: &'a [RecordType], name: &str) -> Option<&'a RecordType>;
  ```
- Consumes: Task 1's `RecordType`/`validate()`; existing TOML parsing paths in both config files (mirror how `[scan]`/`[local]` blocks are declared).

- [ ] **Step 1: Failing tests** — `parse_types_block_from_project_toml` (the spec's `[types.smtp]` example parses; `kind` omitted → Metadata), `resolve_project_shadows_global`, `resolve_custom_shadows_builtin_with_warning` (assert resolution result; warning is stderr, assert via the existing capture idiom if one exists, else assert resolution only and note it), `resolve_rejects_invalid_custom_type` (two primaries → Err naming the type).
- [ ] **Step 2: Run** targeted tests — FAIL.
- [ ] **Step 3: Implement** parsing in both config layers + `resolve_types`/`find_type`.
- [ ] **Step 4: Run** — PASS. Also `cargo test --lib config` to catch regressions in existing config tests.
- [ ] **Step 5: Commit** `feat: parse [types.*] config blocks and resolve with shadowing`

### Task 4: `xv type list` / `xv type show`

**Files:**
- Modify: `src/cli/commands.rs` (new `Type { List, Show { name } }` subcommand with `ls` alias on `list`, matching every other subcommand's alias convention from PR #299)
- Create: `src/cli/type_ops.rs` (execution; register in `src/cli/mod.rs`)
- Modify: `src/main.rs` (`needs_backend` excludes `Type` — it is config-only)
- Test: `tests/e2e_record_types.rs` (new file, harness: `tests/common/mod.rs::xv_isolated_local_with_profile` for the project-type case, `xv_isolated_local` otherwise)

**Interfaces:**
- Consumes: `resolve_types`, `Config::types`, project config discovery (same walk as `resolve_group` in `src/config/settings.rs`).
- Produces: `pub async fn execute_type_list(config: Config, format: OutputFormat) -> Result<()>`, `pub async fn execute_type_show(config: Config, name: String, format: OutputFormat) -> Result<()>`. Table output: `NAME  SOURCE  FIELDS` (fields rendered `username*` for required, `[password]` for secret, `[password]•` for primary); json output serializes the resolved `RecordType`.

- [ ] **Step 1: Failing e2e tests** — `type_list_shows_builtins` (`xv type list` contains `login`, `api-key`, `database`, source `built-in`), `type_show_login_fields`, `type_list_includes_project_custom_type` (`.xv.toml` with `[types.smtp]` → listed with source `project`), `type_show_unknown_errors` (exit 3, message lists known type names).
- [ ] **Step 2: Run** `cargo test --features aws --test e2e_record_types` — FAIL (unknown subcommand).
- [ ] **Step 3: Implement** subcommand + ops file.
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat: xv type list/show`

### Task 5: Tag-limit capabilities + pre-write budget check

**Files:**
- Modify: `src/backend/mod.rs` (`BackendCapabilities`: add `pub max_tags: Option<usize>` and `pub max_tag_value_len: Option<usize>`; `Default` → both `None`)
- Modify: `src/backend/azure/mod.rs` (`max_tags: Some(15)`, `max_tag_value_len: Some(256)`), `src/backend/aws/mod.rs` (`max_tags: Some(50)`, `max_tag_value_len: Some(256)`), `src/backend/local/mod.rs` (both `None`)
- Create: tag-budget helper in `src/records/mod.rs`
- Test: unit tests in `src/records/mod.rs`

**Interfaces:**
- Produces:
  ```rust
  /// Count = reserved tags actually present (xv-type, groups, note, folder,
  /// original_name, created_by) + f.* field tags + user tags.
  pub fn check_tag_budget(
      caps: &BackendCapabilities,
      reserved_count: usize,
      field_tags: &BTreeMap<String, String>,
      user_tag_count: usize,
  ) -> Result<()>;   // Err(config) with per-category breakdown when over max_tags;
                     // Err(config) naming the field + suggesting kind="secret" when a value exceeds max_tag_value_len
  ```

- [ ] **Step 1: Failing tests** — `budget_ok_under_cap`, `budget_errors_over_cap_with_breakdown` (msg contains "reserved", "fields", "user"), `budget_errors_on_long_tag_value` (msg contains the field name and `kind = "secret"`), `no_caps_never_errors`.
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement**; update the three backends' capability constructors.
- [ ] **Step 4: Run** unit tests + `cargo test --features aws --lib backend` — PASS.
- [ ] **Step 5: Commit** `feat: tag-limit capabilities and record tag-budget check`

### Task 6: `xv set --type` (non-interactive + interactive)

**Files:**
- Modify: `src/cli/commands.rs` (`SecretWriteArgs` unchanged; `Set` gains `#[arg(long)] r#type: Option<String>`, `#[arg(long = "field", value_parser = parse_key_val)] fields: Vec<(String, String)>`, `#[arg(long = "field-secret")] secret_fields: Vec<(String, String)>` — reuse/add the `key=value` parser near the existing tag parser)
- Modify: `src/cli/secret_ops.rs` (`execute_secret_set_direct`: record path builds envelope value + `f.*`/`xv-type` tags + content type before the existing request construction; primary value comes from `--value`/`--stdin`/prompt exactly like today)
- Modify: `src/utils/interactive.rs` consumers only (reuse `input_text` for metadata fields, the existing masked secret prompt for secret fields — find it via the current `set` prompt path)
- Test: `tests/e2e_record_types.rs`

**Interfaces:**
- Consumes: Tasks 1–3 (find_type, FieldKind), Task 2 (encode_envelope, constants), Task 5 (check_tag_budget), existing `SecretRequest`.
- Produces: record write behavior other tasks rely on — a `login` record set as
  `xv set cred --type login --field username=bob --field url=https://ex.com --value hunter2`
  stores value `{"password":"hunter2"}`, content type `application/vnd.xv.record`, tags `xv-type=login`, `f.username=bob`, `f.url=https://ex.com`.

Rules (all fail-before-write):
- unknown type → error listing known types;
- required field missing (non-interactive) → error listing every absent required field;
- `--field` names not in the type are accepted as ad-hoc metadata; `--field-secret` ad-hoc goes into the envelope;
- interactive mode (no `--field`s, no `--value`/`--stdin`, TTY): prompt per declared field in order, metadata via `input_text` (empty allowed unless required), secret masked, primary last;
- tag budget checked via Task 5 before the backend call.

- [ ] **Step 1: Failing e2e tests** — `set_typed_record_stores_envelope_and_tags` (create, then `get --format json` on the raw secret via `ls --format json` asserts `xv-type` and `f.username` present; value assertions come in Task 7), `set_typed_missing_required_field_fails_before_write` (exit 3, names `username`, and `ls` shows no secret created), `set_unknown_type_errors_listing_types`, `set_adhoc_field_allowed`, `set_field_secret_goes_to_envelope` (assert NOT present as a tag).
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement** non-interactive path first, then the interactive prompt loop.
- [ ] **Step 4: Run** e2e + full `cargo test --features aws` — PASS (interactive loop is not e2e-testable without a TTY; unit-test the field-ordering plan function you extract for it: `fn prompt_plan(t: &RecordType, provided: &BTreeMap<String,String>) -> Vec<&FieldDef>`).
- [ ] **Step 5: Commit** `feat: xv set --type creates typed records`

### Task 7: `xv get` — primary, `--field`, `--record`, failure modes

**Files:**
- Modify: `src/cli/commands.rs` (`Get` gains `#[arg(long)] field: Option<String>`, `#[arg(long)] record: bool`, mutually exclusive)
- Modify: `src/cli/secret_ops.rs` (`execute_secret_get_direct`: when `is_record(content_type)` → parse envelope; plain → primary field's value through the existing output path (clipboard/raw semantics unchanged); `--field` → envelope value or `f.*` tag value; `--record` → merged view of all fields in the requested format)
- Test: `tests/e2e_record_types.rs`

**Interfaces:**
- Consumes: Task 6's stored shape; `parse_envelope`, `find_type`.
- Produces: the compatibility contract — plain `get` on a typed record returns the primary field's bare value with today's exit codes/output paths.

Failure modes (per spec §6): corrupt envelope → exit 3 naming secret + content type, never printing raw JSON as the value; `--field` unknown → exit 3 listing the record's actual field names (envelope keys ∪ `f.*` tags); record whose `xv-type` resolves to no known type → plain `get` exits 3 telling the user to use `--field`/`--record` (primary unknowable), `--field`/`--record` still work; `--field`/`--record` on an untyped secret → exit 3 ("not a record").

- [ ] **Step 1: Failing e2e tests** — `get_typed_record_returns_primary_bare`, `get_field_metadata_and_secret`, `get_record_json_includes_all_fields`, `get_unknown_field_lists_fields`, `get_corrupt_envelope_fails_loud` (write a record, then overwrite its value with `not-json` via a plain `xv set` keeping the content type — if the CLI can't produce that state, drive the local store file directly and document why), `get_unknown_type_degrades` (record with `xv-type=nosuch` → plain get exit 3, `--field` works), `get_field_on_untyped_errors`.
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** e2e + full suite; also run `tests/e2e_local_backend.rs` untouched-behavior spot checks (`get` on plain secrets unchanged).
- [ ] **Step 5: Commit** `feat: xv get primary/--field/--record for typed records`
- [ ] **Step 6: Phase A gate** — full verification (Global Constraints), independent code-review pass, push branch, PR referencing the tracking issue, Bugbot/CI watch.

---

### Task 8: `xv update --field`

**Files:**
- Modify: `src/cli/commands.rs` (`Update` gains the same `--field` args as `Set`)
- Modify: `src/cli/secret_ops.rs` (`execute_secret_update_direct`: metadata-field change → tag-only update path; secret-field change → fetch envelope, merge, re-encode, new version via the existing value-update path; both re-run `check_tag_budget`)
- Test: `tests/e2e_record_types.rs`

**Interfaces:**
- Consumes: Tasks 2/5/6/7 shapes.
- Produces: `xv update cred --field username=alice` (tag-only), `xv update cred --field-secret totp-seed=XYZ` (new version, envelope gains key).

- [ ] **Step 1: Failing e2e tests** — `update_metadata_field_is_tag_only` (version count unchanged if backend exposes it via `ls --format json`; else assert value unchanged + tag changed), `update_secret_field_writes_new_envelope`, `update_field_on_untyped_errors`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat: xv update --field edits record fields`

### Task 9: Conversion — `--type` and `--untype`

**Files:**
- Modify: `src/cli/commands.rs` (`Update` gains `r#type: Option<String>` and `untype: bool`, mutually exclusive with each other and with `--field`)
- Modify: `src/cli/secret_ops.rs`
- Test: `tests/e2e_record_types.rs`

**Interfaces:**
- Produces: `xv update name --type login` — bare secret's value becomes `{"password":"<old value>"}` + content type + `xv-type` tag (existing tags/groups/note/folder untouched; error if already a record); `xv update name --untype` — value becomes the primary field's bare value, content type cleared, `xv-type` and all `f.*` tags removed; if non-primary **secret** fields exist, interactive confirm listing what will be dropped (`--yes` bypasses; non-TTY without `--yes` → exit 3, mirroring `mv`'s bulk-confirm convention).

- [ ] **Step 1: Failing e2e tests** — `type_conversion_roundtrip` (bare → `--type login` → `get` still returns old value → `--untype --yes` → bare again, tags gone), `untype_with_extra_secret_fields_requires_yes` (non-TTY: exit 3 without `--yes`, succeeds with it, dropped field named in output), `type_on_existing_record_errors`.
- [ ] **Step 2–4:** standard TDD cycle, full-suite run.
- [ ] **Step 5: Commit** `feat: explicit record conversion with --type/--untype`

### Task 10: `ls` — type column, `f.*` fields in JSON, `--type` filter

**Files:**
- Modify: `src/cli/commands.rs` (`List` gains `#[arg(long = "type")] type_filter: Option<String>`)
- Modify: `src/cli/secret_ops.rs` (list path: filter on `xv-type` tag; table gains `TYPE` column only when at least one listed secret is typed — avoids churn for untyped-only vaults; json output already carries tags, additionally lift `f.*` into a `fields` map and `xv-type` into `record_type`)
- Modify: `src/secret/models.rs` if the list summary struct needs the two new serialized fields
- Test: `tests/e2e_record_types.rs`

- [ ] **Step 1: Failing e2e tests** — `ls_shows_type_column_when_typed_present`, `ls_json_lifts_fields_and_type`, `ls_type_filter`, `ls_untyped_only_output_unchanged` (byte-compare table header against a pre-recorded expected string).
- [ ] **Step 2–4:** TDD cycle + full suite.
- [ ] **Step 5: Commit** `feat: ls type column, field lifting, --type filter`

### Task 11: TUI record detail

**Files:**
- Modify: `src/tui/view.rs` (detail pane: when the selected secret has `record_type`, render a `Fields` section — metadata fields plain, secret field names shown with masked values, fetched only on the existing detail-fetch path)
- Test: existing TUI test approach (`rg "cfg(test)" src/tui/` — follow it; if the TUI has no render tests, add a pure-function test for the field-section formatter you extract: `fn record_field_lines(record_type: &str, fields: &BTreeMap<String,String>, secret_names: &[String]) -> Vec<String>`)

- [ ] **Step 1–4:** TDD on the extracted formatter; manual TUI smoke via `cargo run -- tui` against a local-backend store with one typed record (document the smoke result in the commit body).
- [ ] **Step 5: Commit** `feat: TUI shows record fields`
- [ ] **Step 6: Phase B gate** — verification, review pass, push, PR, watch.

---

### Task 12: `inject` field syntax

**Files:**
- Modify: `src/cli/secret_ops.rs` (`execute_secret_inject`: template grammar `{{ secret:name.field }}` — split on the LAST `.` only when the base name resolves to a record and the suffix matches a field, so existing secrets with dots in names keep working: try exact-name match FIRST, fall back to name.field split; URI grammar `xv://vault/name#field` — fragment split is unambiguous since `#` is invalid in names on every backend)
- Modify: `src/cli/secret_ops.rs` run path: NO changes (spec: primary only) — add a guard test proving `xv run` on a typed record injects the primary.
- Test: `tests/e2e_record_types.rs`

**Interfaces:**
- Consumes: Task 7's field-read logic (extract a shared `fn record_field_value(props: &SecretProperties, field: &str, types: &[RecordType]) -> Result<String>` if not already shared).
- Failure semantics inherit #313/#319: unresolved field references collect into `fetch_failures` and abort before write unless `--best-effort`.

- [ ] **Step 1: Failing e2e tests** — `inject_field_syntax_renders`, `inject_bare_name_renders_primary`, `inject_dot_name_exact_match_wins` (create untyped secret literally named `a.b`; `{{ secret:a.b }}` resolves it, not field `b` of record `a`), `inject_unknown_field_aborts` (exit 3, no output file), `inject_uri_fragment_field`, `run_typed_record_injects_primary`.
- [ ] **Step 2–4:** TDD cycle + full suite.
- [ ] **Step 5: Commit** `feat: inject {{ secret:name.field }} and xv://vault/name#field`

### Task 13: Docs + CHANGELOG

**Files:**
- Modify: `README.md` (record types section: built-ins table, custom `[types.*]` example, per-field sensitivity note, external-consumer JSON note + explicit-conversion rule), `docs/FEATURES.md` (same, shorter), `docs/env-profiles.md` (no change unless types interact — they don't; verify), `CHANGELOG.md` (Unreleased/Added entry; NOT breaking — say so explicitly and why: only explicitly-created/converted records change shape)
- Test: none (docs) — but re-run the full suite as the phase gate.

- [ ] **Step 1: Write docs** matching implemented behavior exactly (copy flag names from `--help` output, not from memory).
- [ ] **Step 2:** `cargo test --features aws` full suite green.
- [ ] **Step 3: Commit** `docs: record types`
- [ ] **Step 4: Phase C gate** — verification, review pass, push, PR, watch.

---

## Self-review (done at write time)

- **Spec coverage:** decisions 1–5 → Tasks 1–7; spec §4 CLI → Tasks 4/6/7/8/9/10; §5 integrations → Tasks 11/12 (run: guard test only, per spec); §6 limits/errors → Tasks 5/7; §7 compat → untouched-behavior tests in Tasks 7/10 + non-breaking CHANGELOG in Task 13; §8 testing → per-task tests + hermetic constraint; §9 out-of-scope respected (no run expansion, no vault registry, no value validation).
- **Placeholders:** none — every step names its tests and exact behavior; code blocks give exact signatures/constants; two deliberate implementation-freedom points (TUI test idiom discovery, corrupt-envelope state setup) instruct discovery rather than leaving gaps.
- **Type consistency:** `RecordType`/`FieldDef`/`find_type`/`check_tag_budget`/`encode_envelope`/`parse_envelope`/`RECORD_CONTENT_TYPE`/`TYPE_TAG`/`FIELD_TAG_PREFIX` used identically across Tasks 1–12.
