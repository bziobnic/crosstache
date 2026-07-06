# Documentation automation plan

## Goal

Keep engineering and user-facing documentation aligned with recently shipped
changes without adding redundant pages.

## Steps

- [x] Refresh README multi-vault workspace docs for the v0.22 alias UX:
      `cx add --alias`, `cx alias`, `cx alias --reset`, and long-list backing
      vault suffixes.
- [x] Correct README file-storage notes for v0.21 default-entry routing and
      local backend file/sync support while preserving the AWS sync limitation.
- [x] Update `docs/FEATURES.md` capability and command-reference rows so local
      file operations and workspace context commands match shipped behavior.
- [x] Verify changed docs against source/tests and run an appropriate
      docs-only validation command.
- [x] Prepare the branch handoff after verification.
