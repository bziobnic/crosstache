# Web UI (`xv ui`)

Build with `cargo build --features ui`. Run `xv ui` — it binds an ephemeral
port on 127.0.0.1, prints a tokenized URL, and opens your browser
(`--no-open` to skip, `--port N` to pin the port). Ctrl-C stops it.

Everything the UI does goes through the same backend layer as the CLI, so
all backends (Azure, AWS, local) work, including offline local vaults.

## Secrets and attachments

Typed records are supported: create one via the type picker on the "New
secret" drawer or open an existing one to edit it field-by-field, with
secret-kind fields masked and individually revealable/copyable.

Opening a secret lists its file attachments (if any) as download links in the
detail drawer (`GET /api/secrets/{name}/attachments`). Downloads decrypt
age-encrypted blobs the same way `xv file download` does. Renaming a secret
that still has attachments is refused (`xv-attachments-block-rename`) —
detach first, or keep the current name. See
[`docs/attachments.md`](attachments.md).

The secret drawer can be dismissed with the close control (top of the drawer),
**Cancel**, or Escape. Unsaved edits follow the same discard confirmation as
other sheets.

## Tree grid and selection

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

### Bulk file download (ZIP)

On the Files tab, selection mode exposes **Download**. It posts the selected
logical names to `POST /api/files/archive` (same vault query scope as other
file routes) and saves `crosstache-files.zip`. Plain files pass through;
objects marked `xv_encrypted=age` are decrypted with the vault attachment key
before they enter the archive (same helper as single-file download).

Constraints enforced by the server:

| Limit | Value |
|-------|-------|
| Files per archive | 1–1000 |
| JSON body | ≤ 512 KiB |
| Name length | ≤ 1024 UTF-8 bytes |
| Per-file size | ≤ 100 MiB |
| Total archive payload | ≤ 512 MiB |
| Concurrent archive jobs | 2 |

Names must be unique, relative, forward-slash paths with no empty / `.` / `..`
components, backslashes, NULs, or Windows drive prefixes. Folder paths are
preserved as ZIP entry paths. Failures discard the temporary archive — no
partial ZIP is downloaded. Selection stays intact so you can retry. Backends
without file storage return “not implemented” for this endpoint (the Files
surface is unavailable there anyway).

## Settings: theme, density, timeout

**Settings** (context rail) persists UI preferences beside the global config as
`ui.json` (same directory as `xv.conf`). Schema version is currently `2`.

| Preference | Values / notes |
|------------|----------------|
| Display mode (`theme`) | `system`, `light`, `dark` — independent of palette |
| Palette | `forest` (default), `nord`, `solarized`, `high-contrast`, `custom` |
| Custom theme | When palette is `custom`: light and dark each need `canvas`, `surface`, `text`, `accent`, `danger` as `#RRGGBB`. Server rejects unknown keys and pairs below 4.5:1 contrast. |
| Density | `comfortable` or `compact` |
| Protected-value timeout | Seconds; clamped by config `clipboard_timeout` when that value is non-zero (`0` disables the clamp) |

Legacy preference files without palette/custom-theme fields load with defaults
rather than failing. Preferences must not contain vault-data keys (names,
secret values, etc.) — the API rejects them.

## Keyboard

Arrow up/down move between rows, arrow right expands a folder then steps into
it, arrow left collapses or moves to the parent, Home/End jump to the ends,
Space toggles selection, and Enter opens a secret, downloads a file, or
toggles a folder. Escape closes the topmost sheet or dialog (drawer, Settings,
Help, command palette) before leaving selection mode.

## Session and security

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

If `xv ui` cannot start because `xv.conf` is invalid, run
[`xv doctor`](doctor.md) — it repairs the global config without needing a
healthy normal load path.

## Related designs

- [`superpowers/specs/2026-07-08-web-ui-design.md`](superpowers/specs/2026-07-08-web-ui-design.md)
- [`superpowers/specs/2026-07-14-web-ui-selection-design.md`](superpowers/specs/2026-07-14-web-ui-selection-design.md)
- [`superpowers/specs/2026-07-31-files-bulk-download-design.md`](superpowers/specs/2026-07-31-files-bulk-download-design.md)
