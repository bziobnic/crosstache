# Documentation automation plan

## Goal

Keep engineering and user-facing documentation aligned with recently shipped
changes without adding redundant pages.

## Current pass: scanner hook/CI hardening

Recent v0.25.1 security hardening made `xv scan --hook` use a trusted,
fail-closed baseline. The existing scanner guide still described older
repository-controlled `[scan]` behavior for all `--staged` runs, so this pass
updates the existing guide instead of creating a redundant page.

## Steps

- [x] Confirm branch and working-tree state (`cursor/engineering-documentation-updates-a31b`, clean).
- [x] Review recent commits, automation memory, existing docs, and scanner
      source/tests to identify a focused documentation gap.
- [x] Update `docs/scan.md` to distinguish normal scan config from `--hook` /
      CI behavior:
  - [x] Fix scan-mode notes for `--staged` and `--all` when combined with
        `--hook`.
  - [x] Document the trusted hook baseline: all built-in patterns, default
        minimum secret length, built-in excludes only, and no repository
        `[scan]` overrides.
  - [x] Document fail-closed behavior for unreadable/oversized files and
        incomplete vault secret coverage.
- [x] Verify the documentation-only change against source/tests:
      `cargo +stable test --test scan_tests hook_scan_ignores_repository_policy_that_excludes_a_leak`.
- [ ] Commit and push the branch, then open the documentation PR.
