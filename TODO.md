# TODO — Engineering documentation refresh

## Goal

Keep user-facing engineering docs aligned with recent v0.11 code changes without
inventing behavior. Prefer focused updates to existing pages over new redundant
documents.

## Plan

- [x] Step 1: Inspect recent commits and identify changed subsystems with weak
      docs.
- [x] Step 2: Verify doc candidates against source code and command help.
- [x] Step 3: Update existing docs for backend diagnostics and `.xv.toml`
      workflows.
- [x] Step 4: Update existing docs for security hardening in local storage, blob
      downloads, scanner memory hygiene, and TUI exit behavior.
- [x] Step 5: Run formatting/verification checks appropriate for docs-only
      changes.
- [x] Step 6: Commit, pull/rebase, push the documentation-only branch, and open a
      PR with a concise summary.

## Verification

- Docs reference only behavior verified from source or generated help.
- Markdown links and examples remain concise and consistent with existing style.
- `cargo test --doc` succeeds or any environment-related limitation is recorded.
