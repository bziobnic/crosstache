# Documentation maintenance

[`ROADMAP.md`](./ROADMAP.md) is the canonical list of open product and engineering
work. This file is only the recurring checklist for keeping public documentation
truthful; it does not track feature implementation.

Before merging documentation for a release or feature:

- [ ] Verify commands, flags, defaults, metadata keys, limits, and failure modes
      against the current source and tests.
- [ ] Update the relevant operator guide plus `README.md` and
      `docs/FEATURES.md` where the public surface changes.
- [ ] Add the release entry to `CHANGELOG.md`; remove completed work from
      `ROADMAP.md` rather than leaving checked-off history there.
- [ ] Mark retained specs/plans with an accurate shipped, partial, superseded, or
      unshipped status and version.
- [ ] Re-check backend and UI parity claims (Azure/AWS/local, CLI/TUI/web/desktop)
      and document unsupported combinations explicitly.
- [ ] Validate links and copy-paste examples, and search for stale version numbers,
      renamed commands, metadata keys, and obsolete limitations.

## 2026-09-07 docs run

Verified against `src/cli/transfer_ops.rs`, `transfer_support.rs`,
`attachment_key_ops.rs`, `migrate_ops.rs`, `src/secret/attachment_transfer_execution.rs`,
and provider `supports_atomic_create` / `supports_conditional_delete`.

- [x] Truth-up README transfer/copy/move (execution is not preview-only)
- [x] Truth-up `docs/migration.md` (`--with-attachments` is shipped)
- [x] Add FEATURES command rows for `xv transfer` and `attachment-key`
- [x] Record transfers in CHANGELOG v0.39.0; drop shipped P0 from ROADMAP
- [x] Mark attachment specs/plans shipped; note remaining cloud-route limits
- [x] Cache pitfall: vault delete does not yet call `on_vault_removed`
