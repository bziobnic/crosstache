# Documentation automation plan

## Goal

Keep engineering and user-facing documentation aligned with recently shipped
changes without adding redundant pages.

## Steps

- [x] Update the README local-backend section for opaque filenames, migration
      commands, and current local hardening constraints.
- [x] Refresh README command examples for `xv set` / `xv gen --save` write-time
      metadata parity, `xv config edit`, bounded `xv run` masking, and private
      context-file writes.
- [x] Fix stale group and feature references that still describe groups as
      update-only.
- [x] Move shipped security-hardening items out of ROADMAP open work and into
      shipped history.
- [x] Update CLAUDE.md current-status notes so future agents see the v0.14+
      implementation state.
- [x] Verify documentation diffs for accuracy against source and run an
      appropriate docs-only validation command.

## Audit Backend Routing Fix Plan

### Audit Routing Goal

Ensure `xv audit --resource-group ...` only uses the legacy Azure Activity Log
fallback for Azure, and keeps auditor-backed non-Azure backends on the generic
`AuditBackend` path.

### Audit Routing Steps

- [x] Confirm the baseline routing bug: the old condition skipped generic audit
      dispatch for any backend when `--resource-group` was supplied.
- [x] Add a regression test with an auditor-backed non-Azure backend and a
      resource-group override.
- [x] Verify the routing fix with targeted tests and lint diagnostics.

## v0.16 Public Workflow Documentation Refresh

### Documentation Goal

Refresh existing engineering/user docs for v0.16 public workflow changes that
are already implemented in source: backend-trait routing for advanced commands,
new secret write/run/pager flags, scan `--all`, and stdout/stderr separation.

### Documentation Steps

- [x] Confirm branch cleanliness and recent v0.16 changes against `origin/main`.
- [x] Verify the exact CLI behavior in `src/cli/commands.rs`,
      `src/cli/secret_ops.rs`, `src/cli/scan_ops.rs`, `src/scan/staged.rs`,
      `src/utils/output.rs`, and `src/utils/pager.rs`.
- [x] Update `docs/FEATURES.md` with concise examples and constraints for
      `set --value`, write-time `--tag`, `run --include/--exclude`, advanced
      command backend support, `--pager`, and clean stdout piping.
- [x] Update `docs/scan.md` with the `--all` HEAD-tree mode, staged/all
      conflict, backend support, exclude behavior, and hook/CI pitfalls.
- [ ] Verify documentation diffs and run a docs-only validation command.
