# Documentation automation — listing cache + self-update

## Goal

Land public operator docs for `xv cache` / listing cache and `xv upgrade`,
which shipped years ago with design specs but no `docs/*.md` guide. Prefer
updating existing pages; do not invent behavior.

## Plan

- [x] Inventory CHANGELOG / FEATURES / README vs CLI: `xv cache` and
      `xv upgrade` missing from FEATURES tables; cache only appears as
      `CACHE_TTL` / `XV_CACHE_DIR` env vars.
- [x] Add `docs/cache.md` verified against `src/cache/{mod,models,manager,refresh}.rs`,
      `src/cli/config_ops.rs` (`execute_cache_command`), `src/config/settings.rs`.
- [x] Add `docs/upgrade.md` verified against `src/cli/upgrade_ops.rs` and the
      clap `Upgrade` flags in `src/cli/commands.rs` (including `--check` exit 0).
- [x] Cross-link from README, FEATURES, CLAUDE.md, GROUPS.md; mention Keeper
      on vault import/export in FEATURES.
- [x] Align `xv upgrade --check` clap help with the implementation.
- [x] Commit, push, open PR.
