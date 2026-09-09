# Scheduled rotation target manifest implementation plan

> **For implementers:** Execute these three PRs in order. Rebase each branch on
> the preceding merged `main`. Do not begin the next PR until current-head CI
> and Bugbot are clean and the prior PR is merged.

**Goal:** Make every installed rotation schedule replay one recorded backend and
vault target, refuse drift before mutation, recover safely from reinstall
failures, and report the last and next run.

**Design:** The approved contract is
[`2026-09-09-scheduled-target-manifest-design.md`](../specs/2026-09-09-scheduled-target-manifest-design.md).
Normative output fixtures are in
[`2026-09-09-scheduled-target-manifest-goldens.md`](../specs/2026-09-09-scheduled-target-manifest-goldens.md).
The native scheduler invokes a hidden `xv schedule run --manifest <absolute>`
entry point. A private versioned manifest records the exact resolved target;
the runner reloads recorded inputs and validates them before constructing a
backend or entering the due-rotation service.

**Stack:** Rust, clap, serde/serde_json, sha2, chrono, fs2, tempfile, native
launchd/systemd/Task Scheduler renderers, cargo-nextest-compatible tests.

## Task traceability

| Task ID | Owning PR and plan task | Verification evidence |
|---|---|---|
| A03-01 | PR 1, Tasks 1 and 4 | Manifest round trip plus install-target integration fixtures |
| A03-02 | PR 1, Tasks 2 and 4 | Same-name backend identity and canonical resolver tests |
| A03-03 | PR 1, Tasks 1, 2 and 5 | Permission/no-follow/redaction tests and write-free preview |
| A03-04 | PR 2, Tasks 1 and 2 | All renderer fixtures invoke only the manifest runner |
| A03-05 | PR 2, Task 3 | Drift-table unit tests and decoy-store integration test |
| A03-06 | PR 2, Tasks 4 and 5 | Failure injection, rollback and legacy replacement tests |
| A03-07 | PR 3, Task 1 | Outcome-state, interruption and overlap tests |
| A03-08 | PR 3, Tasks 2 and 3 | Scheduler parser and human status goldens |
| A03-09 | PR 2, Tasks 4 and 5; PR 3, Task 4 | Owned-artifact, upgrade and retained-history tests |
| A03-10 | PR 3, Task 5 | Isolation matrix and three-platform native validation workflow |
| REL02 | Every PR completion gate | Focused tests, full tests, format and clippy |
| REL03 | PR 3, Tasks 3 and 6 | Golden CLI/operator examples and docs |
| REL04 | PR 3, Tasks 5 and 6 | Platform validation plus final roadmap truth-up |
| REL05 | Every PR completion gate | PR description, CI and Bugbot evidence |
| REL07 | Every PR completion gate | Rebase/current-head verification before merge |

## Shared delivery rules

- Keep task tracking out of the repository; this project no longer uses bd.
- Use tests first for every behavior change. Observe each new test fail for the
  intended reason before implementing it.
- Use existing no-follow/private helpers in `src/utils/helpers.rs`. Do not add a
  second atomic-write implementation.
- Never include credentials, tokens, secret names or values in manifest/result
  fixtures, snapshots, diagnostics or logs added by this work.
- Preserve the public behavior of `xv rotate --due`; only the schedule runner
  consumes the new structured service result.
- Run the PR-specific checks below, then `cargo fmt --check`,
  `cargo clippy --all-targets --all-features -- -D warnings`, and
  `cargo test --all-features` before requesting review.
- Before every push, run `git pull --rebase` and then `git push` as required by
  `AGENTS.md`.

---

## PR 1 — Versioned manifest, identity and canonical install resolver

**Task IDs:** A03-01, A03-02, A03-03.

**Result:** `schedule install --print` shows a complete, deterministic manifest
preview and rendered unit inputs without writing. The production install still
uses the old direct `rotate --due` command until PR 2, so documentation must call
this foundation and not claim drift protection is active.

### Task 1: Add schedule state paths and manifest schema

**Files:**

- Create: `src/schedule/manifest.rs`
- Modify: `src/schedule/mod.rs`
- Modify: `src/utils/helpers.rs` only if a reusable bounded no-follow read helper
  is absent; otherwise call the existing helper directly
- Test: unit tests beside `src/schedule/manifest.rs`

**Steps:**

1. Write failing unit tests for `ScheduleStatePaths::resolve` covering
   `XV_STATE_HOME`, `XDG_STATE_HOME`, Unix fallback, Windows local-data input,
   empty overrides, and `rotation-default` filenames. Parameterize environment
   inputs in a pure helper; do not mutate process-global environment in parallel
   tests.
2. Add `ScheduleManifestV1`, `ManifestCadence`, `ManifestExecution` and
   `ManifestTarget` with `Serialize`, `Deserialize`, equality and
   `deny_unknown_fields`. Represent the public enum as a version-dispatching
   loader so an unknown `schema_version` produces a targeted reinstall error.
3. Write failing tests for JSON round trip, unknown/missing fields, null project
   fields, path validation, the 64 KiB read cap, symlink rejection and private
   atomic replacement. Assert `0600`/`0700` on Unix.
4. Implement `load_manifest`, `serialize_manifest_preview`,
   `write_manifest_atomic` and `remove_owned_manifest`. Use
   `create_private_dir` and `atomic_write_file_no_follow(path, bytes, true)`.
5. Run `cargo test schedule::manifest`.

### Task 2: Add a selected-backend identity digest

**Files:**

- Create: `src/schedule/target.rs`
- Modify: `src/schedule/mod.rs`
- Test: unit tests beside `src/schedule/target.rs`

**Steps:**

1. Add table-driven failing tests for built-in Azure/AWS/local and named
   AWS/local instances. Prove same vault names but different tenant,
   subscription, profile, endpoint or local store paths produce different
   digests. Prove unrelated backend entries do not change the selected digest.
2. Implement `SelectedBackendIdentity { name, kind, digest }` and
   `selected_backend_identity(config, registry_name)`. Serialize a private
   fixed-field struct to canonical JSON, prepend
   `xv-schedule-backend-v1\n`, SHA-256 it and return `sha256:<hex>`.
3. Reject an unknown registry name and an AWS selection in a build without the
   `aws` feature. Canonicalize local store paths before hashing.
4. Add a redaction regression test that scans serialized identity/manifest
   fixtures for representative access key, token, password and age-key canaries.
5. Run `cargo test schedule::target` with default and `--all-features`.

### Task 3: Expose exact config/project resolution inputs

**Files:**

- Modify: `src/config/settings.rs`
- Modify: `src/config/project.rs`
- Modify: `src/workspace/mod.rs`
- Test: existing unit-test modules in those files

**Steps:**

1. Add failing tests for `load_config_file_at(&Path)` proving it reads only that
   file and does not apply `XV_BACKEND`, `XV_ENV` or the default config path.
   Add `ContextManager::load_at(&Path)` with the same property for a recorded
   context file.
2. Extract the existing private file parser into
   `pub(crate) async fn load_config_file_at(path: &Path) -> Result<Config>`.
   Keep `load_config_file_only` and normal startup behavior unchanged by routing
   them through the new function.
3. Add a testable project-resolution result that returns the canonical
   `.xv.toml` path, exact bytes/digest and selected environment name together
   with the parsed profile. Reuse the existing traversal and parsing rules.
4. Add or expose a workspace resolver that takes an explicit cwd and context
   snapshot. It must return the selected `WorkspaceEntry` and preserve
   `WorkspaceSource` without re-reading cwd halfway through resolution. Return
   the exact context bytes/path when context contributed the workspace or
   degenerate vault.
5. Run the config, project and workspace unit tests.

### Task 4: Build the install target once

**Files:**

- Modify: `src/schedule/target.rs`
- Modify: `src/cli/schedule_ops.rs`
- Test: `tests/schedule_cli_tests.rs`

**Steps:**

1. Add failing tests for `resolve_install_target` covering an explicit
   workspace alias, implicit workspace default, a degenerate local workspace,
   same-named vaults on two named local backends, an unknown alias, missing cwd,
   project environment selection and paths containing spaces.
2. Implement `ResolvedScheduleTarget` and `resolve_install_target`. It accepts
   explicit config bytes/path/cwd and the optional CLI vault, resolves once,
   creates both digests, and verifies only the selected backend/vault.
3. Change `execute_install` to resolve the target before constructing preview
   output. Do not change non-print scheduler installation in this PR.
4. Update `--vault` help: it selects an attached alias or the sole degenerate
   target and must resolve exactly.
5. Run `cargo test --test schedule_cli_tests`.

### Task 5: Make `--print` the golden manifest preview

**Files:**

- Modify: `src/cli/schedule_ops.rs`
- Modify: `src/schedule/mod.rs`
- Modify: `tests/schedule_cli_tests.rs`
- Modify: `docs/rotation.md`

**Steps:**

1. Replace assertions for `rotate --due --force --vault ...` with the approved
   golden sections: target, exact input paths, manifest preview and future
   `schedule run --manifest` command. The production renderer still receives a
   compatibility schedule in this PR; isolate preview rendering so PR 2 can
   switch it without another output redesign.
2. Add a filesystem snapshot helper and prove `--print` creates no state root,
   lock, result, unit or log and does not call a scheduler command.
3. Render `installed_at` as `<set-at-install>` and sort/pretty-print JSON
   deterministically. Quote all platform command arguments using the existing
   renderer-specific escaping, including paths with spaces.
4. Document preview semantics and the fact that installed schedules remain
   legacy/unpinned until the runner PR ships.
5. Run `cargo test --test schedule_cli_tests` and `cargo test schedule`.

### PR 1 completion

Commit with a message such as `schedule: define pinned target manifests`.
Push a PR whose description states that this is schema/resolver groundwork and
does not yet switch installed jobs. Wait for current-head CI and Bugbot; address
every actionable finding, rerun focused and full checks, then merge.

---

## PR 2 — Manifest runner, drift refusal and transactional reinstall

**Task IDs:** A03-04, A03-05, A03-06, A03-09 for owned artifacts and binary
upgrade behavior.

**Result:** Every newly installed native unit calls the manifest runner.
Scheduled mutation cannot begin after target drift. Reinstall has rollback,
legacy jobs are explicit, and same-path binary upgrades have defined behavior.

### Task 1: Add the private runner command and renderer contract

**Files:**

- Modify: `src/cli/commands.rs`
- Modify: `src/cli/schedule_ops.rs`
- Modify: `src/schedule/mod.rs`
- Test: unit renderer tests in `src/schedule/mod.rs`
- Test: `tests/schedule_cli_tests.rs`

**Steps:**

1. Add failing renderer tests for all three platforms asserting the only
   executable invocation is `<absolute xv> schedule run --manifest <absolute
   manifest>`. Cover spaces and platform-specific escaping.
2. Add hidden `ScheduleCommands::Run { manifest: PathBuf }`. Reject relative
   paths and any path other than the current user's owned manifest location.
3. Replace `RotationSchedule.vault` and `command_args()` with a manifest path.
   Remove target selection variables from rendered units. Keep only environment
   needed by credential/platform libraries.
4. Route `Run` before ordinary ambient registry construction. Ensure a malformed
   manifest cannot cause a backend constructor to run; test with a constructor
   spy or unopened local store.
5. Run renderer unit tests and `cargo test --test schedule_cli_tests`.

### Task 2: Extract a structured due-rotation service

**Files:**

- Modify: `src/cli/secret_ops.rs`
- Create: `src/secret/scheduled_rotation.rs` (or place the service in the
  existing rotation module if that avoids a circular dependency)
- Modify: `src/secret/mod.rs`
- Test: unit tests beside the service plus existing rotation CLI tests

**Steps:**

1. Add failing service tests for nothing due, complete success, invalid policy,
   partial per-secret failure and total discovery failure. The result must expose
   only aggregate counts and typed failure categories to the schedule layer.
2. Extract the discovery/loop portion of `execute_rotate_due` into
   `run_due_rotation(config, registry, backend_name, vault) ->
   Result<DueRotationSummary>`. Keep confirmation and user-facing rendering in
   the CLI adapter.
3. Ensure the runner passes the recorded registry backend name and real vault
   directly. It must not call `resolve_workspace_or_default`,
   `resolve_vault_name` or `effective_backend_name` after validation.
4. Prove public `xv rotate --due` output and exit behavior remain unchanged with
   existing integration tests.
5. Run focused rotation and schedule tests.

### Task 3: Implement deterministic drift validation

**Files:**

- Modify: `src/schedule/target.rs`
- Modify: `src/cli/schedule_ops.rs`
- Test: `src/schedule/target.rs`
- Test: `tests/schedule_cli_tests.rs`

**Steps:**

1. Encode every row of the design drift table as a test before implementing
   `validate_recorded_target`. Assert refusal reasons are ordered by manifest
   field and do not contain config file contents or backend error bodies.
2. Reload config bytes through `load_config_file_at`, reload the exact project
   path/environment, rebuild the selected workspace/backend identity, and
   compare paths, digests and resolved fields. Do not consult ambient context or
   selection environment variables.
3. Check the recorded executable before backend construction. A missing,
   non-regular or non-executable path refuses. A version difference at the same
   canonical path returns `Warning` and permits the run.
4. On `Valid`/allowed `Warning`, construct a lazy registry and materialize only
   the recorded backend. Verify the real vault, then invoke the service. On
   `Refuse`, return the normal config-error exit code without mutation.
5. Add an integration test that changes cwd and supplies conflicting
   `XV_BACKEND`/`XV_ENV`; the recorded local target rotates and a same-named
   alternate target remains unchanged. Add one test per missing/drift input.
6. Run `cargo test schedule` and `cargo test --test schedule_cli_tests`.

### Task 4: Make install/reinstall transactional

**Files:**

- Create: `src/schedule/install.rs`
- Modify: `src/schedule/mod.rs`
- Modify: `src/cli/schedule_ops.rs`
- Test: unit tests in `src/schedule/install.rs`

**Steps:**

1. Define an `OwnedScheduleStore` trait over manifest/unit reads and writes and
   reuse `CommandRunner` for native registration. Add failure-injection tests at
   manifest publish, each native write/register call, scheduler verification
   and each rollback call.
2. Implement the six-stage install transaction from the design. Keep prior
   bytes in memory; write private recovery snapshots only when rollback itself
   fails. Never overwrite a symlinked or unrecognized owned-path file.
3. Extend scheduler verification so it confirms the installed command's binary,
   manifest path, cadence and log path rather than accepting mere presence.
4. Make both first install and reinstall use the transaction. Preserve outcome,
   lock and log files. Emit both primary and rollback errors when recovery is
   incomplete.
5. Run `cargo test schedule::install` and all schedule tests.

### Task 5: Diagnose and replace legacy schedules

**Files:**

- Modify: `src/schedule/mod.rs`
- Modify: `src/cli/schedule_ops.rs`
- Modify: `tests/schedule_cli_tests.rs`
- Modify: `docs/rotation.md`
- Modify: `CHANGELOG.md`

**Steps:**

1. Add fixtures for managed, legacy direct-rotate, orphaned-manifest and foreign
   owned-path states. Scheduler command failure must be distinct from absence.
2. Implement ownership inspection. A recognized old direct-rotate command is
   `legacy-unpinned`; unknown contents at an owned file/task path are `foreign`
   and retained.
3. Make explicit reinstall replace a recognized legacy unit through the normal
   transaction. Do not add silent migration. Make uninstall remove only
   recognized old/new owned artifacts plus `manifest.json`.
4. Document migration (`schedule install ...`), rollback behavior, same-path
   binary upgrade warnings and the need to reinstall after configuration or
   project changes.
5. Add a changelog entry that says new installs are target-pinned and existing
   installs require reinstall; do not claim last-run reporting until PR 3.
6. Run all schedule tests and the documentation link checker if present.

### PR 2 completion

Commit with a message such as `schedule: run from pinned manifests`.
Push, wait for current-head CI and Bugbot, fix actionable findings and rerun the
full required checks before merge. Manually inspect `--print` on the development
host; do not register a real personal job.

---

## PR 3 — Outcomes, explanatory status and native verification

**Task IDs:** A03-07, A03-08, A03-10, A03-09 for historical result retention.

**Result:** Runs are serialized and leave a bounded redacted outcome. Status
explains scheduler/ownership/target/drift/last/next dimensions. Isolation and
all three native formats have explicit automated coverage.

### Task 1: Add the outcome schema and lock

**Files:**

- Create: `src/schedule/outcome.rs`
- Modify: `src/schedule/mod.rs`
- Modify: `src/cli/schedule_ops.rs`
- Test: unit tests in `src/schedule/outcome.rs`

**Steps:**

1. Add failing round-trip tests for all five outcome states, running/terminal
   field invariants, unknown fields/version, 64 KiB cap, private atomic writes,
   symlink rejection and Unicode-safe 1024-scalar diagnostic truncation.
2. Implement `RunOutcomeV1`, `RunSummary`, `RunDiagnostic` and typed state/code
   enums. Validate `schedule_id` and bind outcomes to SHA-256 of the exact
   manifest bytes.
3. Implement `RunGuard::try_acquire` using an owner-private no-follow-created
   `run.lock` and `fs2::FileExt::try_lock_exclusive`. A contending runner logs
   `already_running`, returns success and does not touch `last-run.json`.
4. Write `running` immediately after acquiring the lock; atomically replace it
   on every normal return path with success, partial failure, failure or refused
   drift. Store only aggregate counts and stable sanitized diagnostics.
5. Add a subprocess test proving two runners cannot rotate concurrently and the
   loser does not overwrite the winner's outcome.
6. Run `cargo test schedule::outcome` and schedule integration tests.

### Task 2: Return rich scheduler and ownership status

**Files:**

- Modify: `src/schedule/mod.rs`
- Create: `src/schedule/status.rs`
- Test: unit fixtures in `src/schedule/status.rs`

**Steps:**

1. Replace `ScheduleStatus { installed, detail }` with typed scheduler and
   ownership enums. Add failing fixtures for installed, absent, command error,
   malformed output, managed, legacy, orphaned and foreign states.
2. Implement parser functions for `launchctl print`, `systemctl --user show`
   and `schtasks /Query /V /FO LIST`. Keep raw command output out of persisted
   diagnostics. Locale-dependent/missing next-run data becomes typed `Unknown`.
3. Inspect rendered/registered commands and compare executable, manifest,
   cadence and log path with the manifest. Feed mismatches into status drift;
   do not repair them.
4. Preserve idempotent absence semantics for uninstall while reporting command
   failures honestly.
5. Run `cargo test schedule::status` and lifecycle tests.

### Task 3: Render the complete human status

**Files:**

- Modify: `src/cli/schedule_ops.rs`
- Modify: `tests/schedule_cli_tests.rs`
- Test: add golden text fixtures under `tests/fixtures/schedule/` if inline
  strings become hard to review

**Steps:**

1. Add golden cases for a healthy never-run schedule, healthy successful run,
   partial failure, drift refusal, interrupted running record, retained previous
   install, legacy, orphaned, scheduler unknown and scheduler command error.
2. Render the seven dimensions in the design status contract. Use stable labels
   and deterministic drift order. Do not contact a provider during status.
3. Detect an interrupted run by probing the lock without taking ownership for
   the rest of status. Report exact UTC timestamps from stored/scheduler data;
   never synthesize a next run from cadence.
4. Assert no secret-name/value canaries or backend error bodies appear in
   status, JSON fixtures or stored outcomes.
5. Run `cargo test --test schedule_cli_tests`.

### Task 4: Lock down uninstall/history/upgrade behavior

**Files:**

- Modify: `src/cli/schedule_ops.rs`
- Modify: `src/schedule/install.rs`
- Modify: `tests/schedule_cli_tests.rs`
- Modify: `docs/rotation.md`

**Steps:**

1. Add tests proving uninstall removes recognized native artifacts and manifest
   while retaining `last-run.json`, `run.lock`, the log, foreign files and
   foreign native content.
2. Add tests proving reinstall retains outcome but labels it `previous install`
   when its manifest digest differs; the first completed new run replaces it.
3. Add an in-place version-upgrade fixture: same canonical executable path,
   different reported version, run allowed with warning and status recommends
   reinstall. A changed/missing executable path remains a refusal.
4. Document exact retention and cleanup behavior.
5. Run all schedule tests.

### Task 5: Add target-isolation and native-format gates

**Files:**

- Modify: `tests/schedule_cli_tests.rs`
- Create: `tests/fixtures/schedule/launchd.plist` as needed
- Create: `tests/fixtures/schedule/systemd.service` as needed
- Create: `tests/fixtures/schedule/systemd.timer` as needed
- Create: `.github/workflows/schedule-native.yml`

**Steps:**

1. Add the full isolation matrix from the design: same vault name on two named
   local backends, workspace aliases, two project profiles, path spaces, changed
   cwd, conflicting selection env, and every missing target component. Assert
   both the intended mutation and absence of mutation in the decoy store.
2. Add pure renderer snapshots for launchd, systemd and schtasks on every host.
   Normalize only the temp root and binary version.
3. Add an opt-in native workflow matrix. On Linux run
   `systemd-analyze verify` on the rendered service/timer. On macOS extract and
   run `plutil -lint` on the plist. On Windows create/query/delete a uniquely
   suffixed far-future harmless task; use an `always()` cleanup step and never
   execute rotation.
4. Keep native registration behind an explicit ignored/CI test flag so local
   `cargo test` never touches a developer's scheduler.
5. Run the workflow commands locally where the host supports them and run
   `cargo test --all-features`.

### Task 6: Finish operator documentation and release note

**Files:**

- Modify: `docs/rotation.md`
- Modify: `README.md`
- Modify: `CHANGELOG.md`
- Modify: `ROADMAP.md` only after the whole block ships
- Modify: the external remaining-implementation checklist after merge, not in
  the code PR unless the maintainer requests it

**Steps:**

1. Document the manifest/result locations, target fields, drift refusal,
   reinstall migration, output retention, overlap behavior and status examples.
2. Update README schedule examples to show alias resolution and the manifest
   runner without presenting the hidden command as a user-facing workflow.
3. Consolidate the changelog entry around the end-user behavior: newly
   installed schedules are account/backend/vault pinned; existing schedules
   require reinstall; status exposes drift and last outcome.
4. After all three PRs merge, mark A03-01 through A03-10 complete in the
   out-of-repo checklist and truth up `ROADMAP.md` against merged commits.
5. Verify every documentation command and relative link.

### PR 3 completion

Commit with a message such as `schedule: report pinned run outcomes`.
Push and wait for every matrix job and actual Bugbot content. Fix actionable
findings, rebase if `main` advances, rerun the required checks and merge only
when the review is clean.

## Acceptance checklist for the full block

- An installed job cannot silently switch account, provider, backend registry
  instance, workspace alias or real vault.
- A changed config, project, cwd, backend identity or unit refuses before any
  backend mutation.
- Ambient cwd/context/selection environment cannot redirect a valid job.
- `--print` writes nothing and accurately previews the manifest and native unit.
- Legacy jobs are identified and require explicit reinstall.
- Failed reinstall restores the prior working schedule or leaves explicit
  recovery artifacts and a combined error.
- Concurrent invocations produce at most one rotation sweep.
- Status distinguishes absence, scheduler failure, foreign/legacy/orphaned
  ownership, drift, last outcome and unknown next-run data.
- Uninstall removes only owned active artifacts and retains historical outcomes
  and logs.
- Renderers and quoting pass on launchd, systemd and Task Scheduler validation.
- Formatting, clippy, full tests, current-head CI and Bugbot are clean for each
  PR.
