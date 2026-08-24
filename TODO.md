# Documentation automation — v0.36–v0.38 operator gaps

## Goal

Land public operator docs for subsystems that shipped with design specs or
CHANGELOG entries but weak `docs/` / FEATURES coverage. Prefer updating
existing pages; do not invent behavior.

## Plan

- [x] Inventory CHANGELOG vs public docs; confirm `xv doctor` (v0.36) still
      missing from `docs/` (prior draft PR #408 never merged), plus web UI
      themes/ZIP, built-in types `ssh-key`/`payment-card`/`secure-note`, and
      stale TOTP design status.
- [x] Add `docs/doctor.md` verified against `src/config/doctor.rs`,
      `src/cli/doctor_ops.rs`, and `src/main.rs` early dispatch.
- [x] Cross-link doctor from README, FEATURES, exit-codes, CLAUDE.md; mark
      the doctor design shipped.
- [x] Refresh `docs/web-ui.md` for themes, ZIP archive limits, drawer close,
      keeping the v0.38 `/api/health` connection section.
- [x] Update FEATURES built-ins and configuration command table (`xv doctor`,
      `xv backend`).
- [x] Mark TOTP design shipped; add README TOTP pitfalls from `src/totp.rs`.
- [x] Document Windows test-suite / clipboard lock in `docs/testing.md`.
- [ ] Commit, push, open PR.
