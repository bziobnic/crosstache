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
