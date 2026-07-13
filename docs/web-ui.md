# Web UI (`xv ui`)

Build with `cargo build --features ui`. Run `xv ui` — it binds an ephemeral
port on 127.0.0.1, prints a tokenized URL, and opens your browser
(`--no-open` to skip, `--port N` to pin the port). Ctrl-C stops it.

Everything the UI does goes through the same backend layer as the CLI, so
all backends (Azure, AWS, local) work, including offline local vaults.

Typed records are supported: create one via the type picker on the "New
secret" drawer or open an existing one to edit it field-by-field, with
secret-kind fields masked and individually revealable/copyable.

Entries in both tables are grouped by folder into collapsible sections
(collapsed by default), with file sizes shown in human-readable units.

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

Design: `docs/superpowers/specs/2026-07-08-web-ui-design.md`.
