# TODO — Engineering documentation refresh

## Goal

Keep developer-facing technical documentation aligned with recently changed
subsystems, without adding redundant pages or documenting unverified behavior.

## Plan

- [x] Review recent commits and identify changed subsystems with weak docs.
- [x] Verify behavior against source for:
  - local backend/file-storage path and permission hardening,
  - `.xv.toml` env/backend resolution and `xv config show --resolved`,
  - backend-prefixed `xv://` and `xv migrate` addressing,
  - AWS credential-remediation messaging.
- [x] Update existing docs in place.
- [ ] Run docs/source verification checks and targeted tests.
- [ ] Commit and push the documentation-only change.

## Verification plan

- Run targeted Rust tests covering documented contracts:
  - backend address parsing,
  - project env/backend resolution,
  - file download traversal guards,
  - AWS credential error classification.
- Run a narrow markdown/source consistency check for updated examples and
  stale phrases.
