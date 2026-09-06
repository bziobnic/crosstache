# Web UI (`xv ui`)

Build with `cargo build --features ui`. Run `xv ui` — it binds an ephemeral
port on 127.0.0.1, prints a tokenized URL, and opens your browser
(`--no-open` to skip, `--port N` to pin the port). Ctrl-C stops it.

Everything the UI does goes through the same backend layer as the CLI, so
all backends (Azure, AWS, local) work, including offline local vaults.

If `xv ui` cannot start because `xv.conf` is invalid, run
[`xv doctor`](doctor.md) — it repairs the global config without needing a
healthy normal load path.

## Secrets and attachments

Typed records are supported: create one via the type picker on the "New
secret" drawer or open an existing one to edit it field-by-field, with
secret-kind fields masked and individually revealable/copyable.

Opening a secret lists its file attachments (if any) as download links in the
detail drawer (`GET /api/secrets/{name}/attachments`). Downloads decrypt
age-encrypted blobs the same way `xv file download` does. Local attached secrets can
be renamed through a preview and explicit stopped-writers acknowledgement. The
transfer verifies the destination before removing the source and saves a recovery
ID for interrupted operations. Other backends keep the attachment rename guard.
Set `XV_TRANSFER_RECOVERY_DIR` in the server environment to choose a recovery
directory outside Git and the secret store. The browser never supplies local paths.
Ordinary secrets continue to use atomic rename. See
[`docs/attachments.md`](attachments.md).

The secret drawer can be dismissed with the close control (top-right `x`),
**Cancel**, Escape, or the backdrop. Unsaved edits follow the same discard
confirmation as other sheets. Close, Cancel, and the other drawer controls
disable while a save (or a vault switch) is pending.

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
| Custom theme | When palette is `custom`: light and dark each need `canvas`, `surface`, `text`, `accent`, `danger` as `#RRGGBB`. Server rejects unknown keys and pairs below 4.5:1 contrast (`text-canvas`, `text-surface`, `accent-surface`, `danger-surface`). |
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

## Session, connection, and security

The URL token is copied into per-tab `sessionStorage`, so reloads in that tab
remain authenticated while the server is running. Closing the tab discards the
app's session access. Opening the scrubbed URL in a new tab requires the
original tokenized URL printed in the terminal.

Scope note: the UI resolves the effective workspace and lists its entries as
`alias — backend / vault` in the workspace switcher, including entries attached
to different backends. Each request is scoped to the single selected entry,
however: the UI does not perform the CLI's union reads across all attached
entries. The original switching request in #353 is now implemented; re-scope or
close that stale issue before using it to track a future union view.

While a tab is visible it polls `GET /api/health` every 10 seconds to check
that the `xv ui` process it was opened against is still there. Two consecutive
failed probes raise a banner across the top of the page and switch the rail's
connection status to "Disconnected"; a single failed probe gets a fast recheck
before the banner appears, and polling pauses on a hidden tab, probing
immediately when you return. A transient failure can recover on its own, but
restarting `xv ui` mints a new token and session link even on the same port, so
the old tab cannot authenticate to the restarted process. Its next probe
receives `401`, which the banner reports separately and which stops further
probing — reopen the new URL printed in the terminal. The probe touches no
backend, so it does not put a Key Vault call on a timer. While the tab's server
session is healthy, the rail's connection status instead shows backend
reachability sampled from `/api/context` at load and on each workspace switch.

Security model: loopback bind only; per-session bearer token (the `?token=`
in the URL, held in per-tab session storage); Host/Origin validation; secret
values only in POST bodies; `Cache-Control: no-store`. There is no TLS and no
login — if you need network access to your vaults from another device, this is
deliberately not the tool.

## Related designs

- [`superpowers/specs/2026-07-08-web-ui-design.md`](superpowers/specs/2026-07-08-web-ui-design.md)
- [`superpowers/specs/2026-07-14-web-ui-selection-design.md`](superpowers/specs/2026-07-14-web-ui-selection-design.md)
- [`superpowers/specs/2026-07-31-files-bulk-download-design.md`](superpowers/specs/2026-07-31-files-bulk-download-design.md)
