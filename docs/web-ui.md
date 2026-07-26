# Web UI (`xv ui`)

Build with `cargo build --features ui`. Run `xv ui` — it binds an ephemeral
port on 127.0.0.1, prints a tokenized URL, and opens your browser
(`--no-open` to skip, `--port N` to pin the port). Ctrl-C stops it.

Everything the UI does goes through the same backend layer as the CLI, so
all backends (Azure, AWS, local) work, including offline local vaults.

Typed records are supported: create one via the type picker on the "New
secret" drawer or open an existing one to edit it field-by-field, with
secret-kind fields masked and individually revealable/copyable.

Both surfaces render a single hierarchical tree grid: folders and their
contents live in one table, each row indented by depth, with a disclosure
chevron on folder rows. Vaults with 50 or fewer items open fully expanded;
larger ones start collapsed. **Expand all** / **Collapse all** sit in the
toolbar, expansion is remembered per backend/vault/surface, and searching or
filtering temporarily reveals matches inside collapsed folders without
changing what you had open. Each surface keeps its own columns (secrets show
folder, groups, note and updated; files show size, type and modified), and
file sizes use human-readable units.

Use **Select** to reveal per-row checkboxes. Folder rows are containers rather
than selectable entities: checking a folder selects every item beneath it,
partial selection shows the indeterminate state, and unchecking clears the
branch. Bulk actions therefore always operate on items — a checked folder puts
its descendants in scope. The header checkbox selects every item currently
listed (honouring the active search and filters). Both tables support bulk
deletion; selected secrets can also be moved to another folder. Bulk file moves
are not available because file backends do not expose a portable move
operation.

Keyboard: arrow up/down move between rows, arrow right expands a folder then
steps into it, arrow left collapses or moves to the parent, Home/End jump to
the ends, Space toggles selection, and Enter opens a secret, downloads a file,
or toggles a folder.

The URL token is copied into per-tab `sessionStorage`, so reloads in that tab
remain authenticated while the server is running. Closing the tab discards the
app's session access. Opening the scrubbed URL in a new tab requires the
original tokenized URL printed in the terminal.

Scope note: the UI operates on the **active backend** — the vault switcher
lists that backend's vaults and every operation targets it. Multi-backend
workspaces (`xv cx` attached vaults and aliases) are not resolved here yet;
like `xv gen` or `xv find --all-vaults`, the UI uses the context/config
default vault, not the workspace seam. Workspace-aware switching is tracked
as a follow-up.

Security model: loopback bind only; per-session bearer token (the `?token=`
in the URL, held in per-tab session storage); Host/Origin validation; secret
values only in POST bodies; `Cache-Control: no-store`. There is no TLS and no
login — if you need network access to your vaults from another device, this is
deliberately not the tool.

Designs: `docs/superpowers/specs/2026-07-08-web-ui-design.md` and
`docs/superpowers/specs/2026-07-14-web-ui-selection-design.md`.
