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
