# Machine-output contract — design

**Date:** 2026-09-12
**Baseline:** `main` at 6ee012a
**Checklist items:** OUT01 through OUT06 (xv Feature Priorities checklist, section 10 "Machine-output contract").
**Compatibility:** no constraint on internal shapes; the two scripting contracts stay: stdout is data, and every existing *successful* machine document keeps its shape. The error envelope gains one optional key.

## Problem (OUT01 inventory, condensed)

`--format json|yaml` errors render as a `{"error": …}` envelope on stdout, but several commands have already written to stdout by then, so stdout is not one document:

- **Two JSON documents:** `xv scan --format json` prints the findings array, then returns the exit-50 error and `main` appends the envelope. `xv rotate --check --format json` prints the due table/JSON, then returns exit 51. `xv migrate --dry-run` with attachments prints one pretty JSON object per preview in a loop.
- **Human text on stdout before an error:** `migrate` (plan banner, per-item `[ok]/[skip]/[error]` lines), `copy`/`move` ("Copying…", "Deleting source…"), `mv` bulk dry runs, `file upload|download|delete` batches and their `[ok]`-formatted summaries, `vault export --output` ("Exported N secrets…"), `vault import --dry-run` per-record list, single `set` ("Vault:/Version:"), `rotate NAME` ("Generator:/New version:"), `inject` stray messages, `version`.
- **No document at all on success:** bulk `set`, `rotate --due`, `schedule run`, `audit --verify`, `mv` bulk, group `delete` write only stderr, so `--format json` succeeds with empty stdout.
- **CSV is uncovered:** `main` renders CSV errors as plain text on stderr while the command may already have written rows.
- **`file sync`** keys its suppression on `config.output_json`, which is true only for JSON, so YAML/CSV still get `upload: …` lines on stdout.
- **Partial results are never modelled:** `MigrateOutcome`, bulk counters, mv failure counts, and import refusals exist only as counters or stdout lines.

Existing tests parse the *whole* stdout buffer as one JSON value (`tests/common::parse_json_envelope`) but no test exercises a partial-failure command, and two `cli_integration_tests` assertions explicitly accept migrate's banner on either stream.

## Decisions

1. **Machine mode is explicit.** A command runs in machine mode when `config.format_explicit` is true and the resolved format is `json`, `yaml`, or `csv`. `--format auto` piped to a non-TTY keeps today's behavior (JSON body, plain-text error on stderr) so `xv get X | cmd` and raw/code-only commands are untouched. Raw and names-only outputs are never machine documents.

2. **Exactly one document on stdout in machine mode.** On success it is the command's data document, shape unchanged. On failure before any data it is the existing error envelope. On failure *after* the command produced a structured result (partial success, item-level errors, scan findings, rotation due) it is one object: the existing envelope plus a `report` key holding the result:

   ```json
   { "error": { "code": "xv-scan-leak-detected", "message": "…", "exit_code": 50 },
     "report": [ …findings… ] }
   ```

   Scripts reading `.error.code` keep working; `.report` is additive. The exit code is unchanged.

3. **CSV is rows-only.** CSV cannot carry an error object, so in CSV mode the error is rendered as plain text on stderr and stdout holds only the rows written (possibly partial, possibly none). No human text ever reaches stdout in CSV mode. Documented as the CSV limitation.

4. **Final emission is centralized (OUT03).** A new `src/utils/machine.rs`:
   - `pub fn is_machine_mode(config: &Config) -> bool`.
   - `pub fn report<T: Serialize>(config: &Config, report: &T)` — in machine mode, stores the document in a process-wide single slot and prints nothing; outside machine mode it is a no-op. Storing twice is a bug and panics in debug builds.
   - `main` owns the end: on `Ok`, if a report is pending it is printed in the resolved format; on `Err`, the envelope is printed with `report` attached when one is pending (JSON/YAML) or stderr text (CSV/plain). Commands that today print bare arrays/tables on success (`ls`, `find`, `history`, …) keep printing directly; they never produce partial results after printing.
   - A debug-build guard in `machine` counts documents written to stdout via the helper; contract tests assert exactly one.

5. **Status and progress leave stdout (OUT04).** Every human line in the inventory moves to stderr through `output::*` (`migrate` banner and per-item lines, `copy`/`move` narration, `mv` dry-run previews, file batch narration and summaries, `vault export --output` confirmation, `vault import --dry-run` list, `set`'s Vault/Version detail, `rotate NAME` detail, `inject` messages, `version`). In machine mode those lines are additionally suppressed in favour of the report. `file sync` keys on `is_machine_mode`, not `output_json`. Indicatif bars already target stderr and are hidden off-TTY.

6. **Partial results get a shared shape.** `machine::ItemReport`:

   ```rust
   #[derive(Serialize)] pub struct ItemReport { pub summary: ItemSummary, pub items: Vec<ItemOutcome> }
   #[derive(Serialize)] pub struct ItemSummary { pub total: usize, pub succeeded: usize, pub skipped: usize, pub failed: usize }
   #[derive(Serialize)] pub struct ItemOutcome { pub name: String, pub status: ItemStatus, #[serde(skip_serializing_if = "Option::is_none")] pub detail: Option<String>, #[serde(skip_serializing_if = "Option::is_none")] pub error: Option<String> }
   #[derive(Serialize)] #[serde(rename_all = "kebab-case")] pub enum ItemStatus { Ok, Skipped, Failed }
   ```

   Used by `migrate`, bulk `set`, `mv` bulk, `vault import`, file upload/download/delete batches, and `rotate --due`. `scan` reports its existing `Vec<Finding>`; `rotate --check` its existing due rows; `transfer` its existing `TransferReport`/preview (rendered in the resolved format, not always pretty JSON). Names only, never values; `detail`/`error` strings pass through the same sanitizer the schedule outcome uses.

7. **Exit codes do not change.** Partial failure keeps each command's current non-zero code. `xv-scan-leak-detected` (50) and `xv-rotation-due` (51) now come with `report` instead of a second document.

8. **Compatibility notes (OUT05)** go in `docs/exit-codes.md` (envelope gains `report`; CSV rule), `docs/FEATURES.md` (stream rules per command family), `CHANGELOG.md` (stdout lines that moved to stderr, listed by command), and `CLAUDE.md` (the `machine::report` rule for new commands).

## Non-goals

- A streaming (NDJSON) format. The checklist allows "an explicitly selected streaming format"; none is added now.
- Changing successful list-command shapes, `--names-only`, raw prints, `schedule install --print`, `completion`, `run` passthrough, or `xv doctor`'s refusal of machine formats.
- Making `--format auto` piped render envelopes.

## Verification (OUT06)

`tests/e2e_machine_output.rs` (local backend, hermetic): for each of `scan` with findings, `rotate --check` with a due secret, `vault import --dry-run` with a rejected record, bulk `set` with one invalid name, `mv --filter` with one collision, `migrate` local→local with a conflict under `--on-conflict fail` and a clean run, `file upload` batch with one missing path, and `copy`/`move` narration: run with `--format json`, `--format yaml`, and `--format csv`, and assert stdout parses as exactly one document (`serde_json::Deserializer::from_str(..).into_iter::<Value>()` yields one item; YAML likewise; CSV has a header row or is empty), the exit code equals `error.exit_code` when present, stderr contains no `{` at line start, and a canary secret value appears nowhere. Success paths assert one document and exit 0. Existing permissive assertions in `tests/cli_integration_tests.rs` are tightened to stderr.
