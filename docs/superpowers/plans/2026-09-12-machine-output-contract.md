# Machine-Output Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In explicit `--format json|yaml|csv` mode, stdout carries exactly one document, on success, failure, and partial failure; all human status leaves stdout.

**Architecture:** `src/utils/machine.rs` holds a process-wide single-slot report and the `is_machine_mode` predicate. Commands that can partially fail build an `ItemReport` (or their existing report type), hand it to `machine::report`, and return `Ok` or `Err` as today. `main` prints the pending report on `Ok`, or the error envelope with `report` attached on `Err` (JSON/YAML), or stderr text (CSV/plain). Human lines move to `output::*` (stderr).

**Tech Stack:** Rust 2021, `serde_json`, `serde_yaml`, existing `OutputFormat`, `tests/common` and `WorkspaceEnv`-style harnesses.

**Spec:** `docs/superpowers/specs/2026-09-12-machine-output-contract-design.md`

## Global Constraints

- Never run `cargo` with `run_in_background`; foreground only, timeout up to 600000 ms, `| tail -40`.
- Never `git stash`. Never push.
- Machine mode = `config.format_explicit && matches!(config.runtime_output_format, Json | Yaml | Csv)`. `--format auto` behavior is unchanged. Raw prints, `--names-only`, `schedule install --print`, `completion`, `run` passthrough, `xv doctor` are untouched.
- Successful machine documents of list-style commands keep their shape. The error envelope gains only the optional `report` key. Exit codes unchanged.
- Never put a secret value in a report; names only. `detail`/`error` strings go through `crate::utils::format::sanitize_control_chars`.
- Every `println!` moved to stderr becomes an `output::*` call (which already writes to stderr), not a bare `eprintln!`, unless the line is a scripting output kept on stdout.
- Gates per task: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, named tests. Task 7 runs the full workspace.

---

### Task 1: `machine` module and `main` integration

**Files:**
- Create: `src/utils/machine.rs`
- Modify: `src/utils/mod.rs` (add `pub mod machine;`), `src/main.rs:60-76` and `:471-514`

**Interfaces (produced):**

```rust
// src/utils/machine.rs
pub fn is_machine_mode(config: &Config) -> bool;
pub fn report<T: Serialize>(config: &Config, report: &T);            // no-op outside machine mode
pub fn take_pending() -> Option<serde_json::Value>;                    // used by main only
pub fn render_success(format: OutputFormat, report: &serde_json::Value) -> String; // json pretty / yaml / csv
pub fn render_failure(format: OutputFormat, envelope: serde_json::Value, report: Option<serde_json::Value>) -> String;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)] pub struct ItemReport { pub summary: ItemSummary, pub items: Vec<ItemOutcome> }
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)] pub struct ItemSummary { pub total: usize, pub succeeded: usize, pub skipped: usize, pub failed: usize }
#[derive(Debug, Clone, Serialize, PartialEq, Eq)] pub struct ItemOutcome { pub name: String, pub status: ItemStatus, #[serde(skip_serializing_if = "Option::is_none")] pub detail: Option<String>, #[serde(skip_serializing_if = "Option::is_none")] pub error: Option<String> }
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)] #[serde(rename_all = "kebab-case")] pub enum ItemStatus { Ok, Skipped, Failed }
impl ItemReport { pub fn new() -> Self; pub fn ok(&mut self, name: &str, detail: Option<&str>); pub fn skipped(&mut self, name: &str, detail: Option<&str>); pub fn failed(&mut self, name: &str, error: &str); pub fn has_failures(&self) -> bool; }
```

Semantics: `report` serializes to `serde_json::Value` and stores it in a `static PENDING: Mutex<Option<Value>>`; a second call while one is pending is `debug_assert!`-fatal and, in release, replaces with a `tracing::warn!`. `ItemReport::failed` sanitizes `error` with `sanitize_control_chars` and truncates to 1024 scalars. CSV rendering of an `ItemReport` writes a header `name,status,detail,error` and one row per item; CSV rendering of any other report writes the JSON-array-of-objects as rows when it is an array of flat objects, else a single `report` column with the JSON string.

- [ ] **Step 1: Write failing unit tests** in `machine.rs`: `is_machine_mode` truth table over `(format_explicit, runtime_output_format)`; `ItemReport` counters and sanitization (`\u{1b}[31m` stripped, 2000-char error truncated to 1024); `render_failure(Json, envelope, Some(report))` yields one object with `error` and `report` keys; `render_success(Csv, item_report)` yields header + rows; `report()` outside machine mode leaves `take_pending()` `None`.
- [ ] **Step 2: Run** `cargo test --lib utils::machine 2>&1 | tail -10` → compile errors.
- [ ] **Step 3: Implement** `machine.rs` as specified. In `src/main.rs`:
  - In `main()` (line ~71): after `run(cli).await` returns `Ok(())`, `if let Some(report) = machine::take_pending() { println!("{}", machine::render_success(format.resolve_for_stdout(), &report)); }`.
  - In `print_user_friendly_error`: replace the `Json | Yaml` branch so it builds `envelope` as today, then `let report = machine::take_pending();` and prints `machine::render_failure(format, envelope, report)`. Add `OutputFormat::Csv` handling: if a report is pending, print `render_success(Csv, &report)` to stdout (rows only), then fall through to the stderr plain-text path. Keep the `Auto` exclusion comment.
- [ ] **Step 4: Run** `cargo test --lib utils::machine 2>&1 | tail -10` and `cargo test --test error_codes_tests 2>&1 | tail -5` → pass (envelope shape unchanged when no report).
- [ ] **Step 5: Commit** `output: add the machine-mode single-document sink and envelope report`.

---

### Task 2: Fix the double-document commands: `scan` and `rotate --check`

**Files:** `src/cli/scan_ops.rs:342-385` (`render_findings`), `src/cli/secret_ops.rs:3348-3440` (`execute_rotate_check`), tests in `tests/scan_tests.rs` and `tests/rotation_policy_tests.rs` (or the nearest existing file).

- [ ] **Step 1: Failing tests.** `tests/scan_tests.rs`: run `xv scan --format json` over a file containing a vault value (existing harness), assert `serde_json::Deserializer::from_str(&stdout).into_iter::<Value>().count() == 1`, that the single object has `error.code == "xv-scan-leak-detected"` and `report` is an array with one finding, exit 50. Same for `--format yaml` (parse with `serde_yaml::from_str::<Value>` on the whole stdout). `rotate --check --format json` with one due secret (set `--rotate-every 1d` and backdate `xv:rotated_at` via `xv update` if the flag exists; otherwise use the existing rotation test fixture): one object, `error.code == "xv-rotation-due"`, `report` array has the due row, exit 51.
- [ ] **Step 2: Run** → fail (two documents today).
- [ ] **Step 3: Implement.** `render_findings`: in machine mode call `machine::report(config, findings)` instead of `println!`; keep the stderr human listing for non-machine; keep the `Err` return. It needs `&Config` — thread it from the caller (`execute_scan…`). `execute_rotate_check`: when `is_machine_mode`, call `machine::report(config, &rows)` instead of `println!("{}", formatter.format_table(&rows)?)` (both the empty and non-empty branches), keep the human path as is; the `xv-rotation-due` error return stays.
- [ ] **Step 4: Run** the named tests plus `cargo test --test scan_tests 2>&1 | tail -5`.
- [ ] **Step 5: Commit** `scan,rotate: emit one document when findings or due secrets make the command fail`.

---

### Task 3: `migrate`

**Files:** `src/cli/migrate_ops.rs` (banner `:123-135`, dry-run preview loop `:590`, per-item lines `:706-721`, summary `:726-737`), `tests/e2e_workspaces.rs` (existing `migrate_*` tests) or a new `tests/e2e_machine_output.rs` started here.

- [ ] **Step 1: Failing tests.** Local→local migrate with two secrets and `--format json`: stdout is one object `{summary:{total:2,succeeded:2,skipped:0,failed:0}, items:[…]}`, exit 0, stderr holds the banner. With a conflict and `--on-conflict fail`: one envelope with `error.code` and no report (the refusal happens before writes) — assert exactly one document. Dry run with `--format json`: one object (the plan: `{source, target, to_migrate, to_skip, conflicts, on_conflict, dry_run, attachment_previews:[…]}`), exit 0.
- [ ] **Step 2: Run** → fail.
- [ ] **Step 3: Implement.** Banner `println!`s → one `output::info` block (stderr) in human mode; in machine mode collected into a `MigratePlan` struct (`Serialize`) that is reported on dry run. The attachment preview loop pushes into `MigratePlan.attachment_previews` instead of printing. Per-item `[ok]/[skip]/[error]` → `output::success/info/warn` in human mode; always recorded into an `ItemReport`. After the summary, `machine::report(&config, &item_report)` before the `if !errors.is_empty()` split, so the partial-failure `Err` carries it.
- [ ] **Step 4: Run** the tests plus `cargo test --test e2e_workspaces migrate_ 2>&1 | tail -5` and `cargo test --test cli_integration_tests 2>&1 | tail -5` (the permissive `Source:` assertions must be tightened to stderr in this task).
- [ ] **Step 5: Commit** `migrate: report items as one machine document and keep narration on stderr`.

---

### Task 4: Bulk and narrated commands

**Files:** `src/cli/secret_ops.rs` (bulk `set` `:593-677`, single `set` `:534-535`/`:576-577`, `copy` `:7303`/`:7451-7458`, `move` `:7481`/`:7523`/`:7548`, `rotate NAME` `:5874-6009`, `rotate --native` `:3694`/`:3703`, `rotate --due` observer `finish` `:3593-3620`, `inject` `:6905`/`:6920`/`:6936`/`:6943`), `src/cli/mv_ops.rs` (dry-run previews `:623`/`:804`/`:934`/`:1138`, partial failure `:984-994`/`:1193-1203`), `src/cli/vault_ops.rs` (export `:1068`, import dry-run `:1284`, `:1425`/`:1483`/`:1521`), `src/cli/system_ops.rs` (`version` `:438-443`).

Rules: narration → `output::*`; bulk `set`, `mv` bulk, `vault import`, `rotate --due` build an `ItemReport` and call `machine::report` before returning (success or partial-failure `Err`); `mv` dry-run previews → stderr in human mode, a report `{planned:[{from,to}]}` in machine mode; `vault export --output` confirmation → stderr; `version` → a `{version, …}` report in machine mode, stderr text otherwise? No: `version` stays stdout text in human mode (it is data-like); in machine mode emit `{ "version": … }`.

- [ ] **Step 1: Failing tests** in `tests/e2e_machine_output.rs`: bulk `set A=1 B=2 --format json` → one `ItemReport`, exit 0; bulk set with an invalid name → one object `{error, report}` with `summary.failed == 1`, exit non-zero; `mv --filter` with one collision → same shape; `vault import --dry-run --format json` with one rejected record → `{error, report}`; `copy`/`move --format json` → exactly one document and no `Copying`/`Moving` text on stdout (the document is the returned `SecretMetadata` for the destination, or `{"moved": name}`; pick the metadata).
- [ ] **Step 2: Run** → fail.
- [ ] **Step 3: Implement** per the rules.
- [ ] **Step 4: Run** the new tests plus `cargo test --test e2e_local_backend 2>&1 | tail -5`, `cargo test --test e2e_mv_filter 2>&1 | tail -5`, `cargo test --test e2e_record_types 2>&1 | tail -5`.
- [ ] **Step 5: Commit** `cli: move narration to stderr and report bulk outcomes as one document`.

---

### Task 5: File batches, `file sync`, and `transfer` format

**Files:** `src/cli/file_ops.rs` (single upload detail `:585-588`, recursive batch `:1200-1307`, multi-file `:1322-1374`, download batch `:1421-1701`, delete batch `:1724-1777`, sync `:1976-2500`), `src/cli/transfer_ops.rs:131,142`.

- [ ] **Step 1: Failing tests** (local backend supports file ops): `file upload` of two files with one missing path, `--format json` → one `{error, report}` with an `ItemReport`; `file sync --dry-run --format yaml` → one YAML document and no `upload (dry-run):` lines on stdout; `transfer … --format yaml` preview → YAML, not JSON.
- [ ] **Step 2: Run** → fail.
- [ ] **Step 3: Implement.** All batch narration and `format_line`-on-stdout summaries → `output::*`; batches build `ItemReport`s and call `machine::report`; `file sync` replaces every `config.output_json` check with `machine::is_machine_mode(config)` and reports `summary` via `machine::report`; `transfer` renders preview/report with `machine::report` (main prints in the resolved format) and, in human mode, keeps pretty JSON on stdout as today.
- [ ] **Step 4: Run** the tests plus `cargo test --test e2e_local_file_ops 2>&1 | tail -5`, `cargo test --test file_commands_tests 2>&1 | tail -5`, `cargo test --test e2e_transfer 2>&1 | tail -5`.
- [ ] **Step 5: Commit** `file,transfer: one machine document for batches and format-aware transfer output`.

---

### Task 6: Contract tests and docs

**Files:** `tests/e2e_machine_output.rs` (consolidate), `tests/cli_integration_tests.rs:387,464-473` (tighten), `docs/exit-codes.md`, `docs/FEATURES.md`, `CHANGELOG.md`, `CLAUDE.md`.

- [ ] **Step 1:** A table-driven test `every_machine_mode_run_writes_exactly_one_document` over the command list in the spec's Verification section × `{json, yaml, csv}`: one document (JSON via `Deserializer::into_iter` count; YAML via `serde_yaml::from_str`; CSV via `csv::Reader` header parse or empty), `exit_code` agreement, no line on stderr starting with `{` or `[`, canary absent from both streams.
- [ ] **Step 2:** Docs: `docs/exit-codes.md` (envelope `report` key with an example; CSV rule; "exactly one document" statement), `docs/FEATURES.md` (§ Output Formats: stream rules, the `ItemReport` shape), `CHANGELOG.md` Unreleased → Changed (list every command whose stdout narration moved to stderr, the `report` key, CSV behaviour), `CLAUDE.md` (one paragraph: new commands that can partially fail call `machine::report`; never print human text to stdout).
- [ ] **Step 3: Commit** `docs,tests: pin the one-document machine-output contract`.

---

### Task 7: Full gates

`cargo fmt --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo check --no-default-features`; `cargo test --all-features --workspace`; `cargo test --doc`; `grep -rn "println!" src/cli/migrate_ops.rs src/cli/file_ops.rs | grep -v "names_only\|format_table\|render\|// "` reviewed line by line — each remaining `println!` must be a data document.
