# Documentation automation — secret file attachments

## Goal

Ship operator-facing docs for secret file attachments (v0.27.0+) — the feature
had design/plan specs but no public workflow guide in `docs/`, `README.md`, or
`docs/FEATURES.md`.

## Plan

- [x] Inventory recent feature work vs public docs; confirm attachments as the
      largest post-ship gap (schedule/rotation/git/CI already documented).
- [x] Add `docs/attachments.md` verified against
      `src/secret/attachments.rs`, `src/cli/attach_ops.rs`, `src/cli/file_ops.rs`,
      `src/cli/secret_ops.rs`, `src/cli/migrate_ops.rs`, and `src/web/api.rs`.
- [x] Update `docs/FEATURES.md` command tables for `attach` / `attachments` /
      `detach` and `xv file upload --encrypt`.
- [x] Update `README.md` TOC, Files section, Web UI note, and troubleshooting.
- [x] Update `docs/web-ui.md` for secret-drawer attachment download links.
- [x] Mark the attachments design spec as shipped; correct the metadata key to
      `xv_encrypted` (Azure-valid underscore form).
- [x] Note migrate limitations for attachment blobs in `docs/migration.md`.
- [x] Commit, push, open PR.

## Validation

- Cross-check constants and commands against source (no fabricated behavior).
- Prefer updating existing public docs over inventing parallel guides.
- Spot-check: `rg` for `xv_encrypted`, `xv attach`, `xv-attachment-key` in the
  updated files; confirm design banner no longer says "not yet implemented".
- Clarified that `download_decrypted` keys off `xv_encrypted=age`, not path
  prefix alone.
