# `xv tui` — Read-Only Terminal UI

Three-pane terminal browser for vaults and secrets. **Read-only** in v0.7 — write mode (create/edit/delete) is reserved for v0.8.

Build with the `tui` feature flag:

```bash
cargo install crosstache --features tui
```

## Layout

```
┌──────────────┬────────────────────────────┬──────────────────┐
│ Vaults       │ Secrets (filter: /db_)     │ Detail           │
│ > dev-kv     │ > DB_PASSWORD              │ name: DB_PASSWORD│
│   stage-kv   │   DB_HOST                  │ value: ●●●●●●    │
│   prod-kv    │   DB_PORT                  │ groups: backend  │
└──────────────┴────────────────────────────┴──────────────────┘
status: dev-kv · 24 secrets                                ?:help
⚠ [xv-network-dns] DNS resolution failed       (e: details, Esc: dismiss)
```

## Keymap

| Key | Action |
|-----|--------|
| `h j k l` / arrows | move within / between panes |
| `Tab` / `Shift-Tab` | cycle panes |
| `/` | live fuzzy filter (secrets pane); Esc cancels, Enter commits |
| `Space` | toggle value reveal |
| `y` | copy value (with countdown using `clipboard_timeout`) |
| `Y` | copy secret name |
| `R` | refresh — invalidate cache and reload current scope |
| `H` | history (versions) overlay |
| `a` | audit log overlay |
| `?` | help overlay |
| `e` | expand error toast |
| `q` / `Esc` | quit (or close current overlay) |

### Reserved for v0.8 (write mode)

`c` (create), `d` (delete), `r` (rename / rotate). Pressing one in v0.7 shows a "reserved for v0.8 write mode" toast.

## On-demand value fetch

The TUI lists secrets cheaply (names + metadata). The **value** for the highlighted secret loads on demand: settle the cursor for ~200ms and the value fetches in the background, lands in an in-memory cache (cleared on quit), and the detail pane shows `●●●●●●` until you press `Space` to reveal.

Values are wrapped in `Zeroizing<String>` end-to-end.

## Audit overlay limitation

The audit overlay (`a`) ships as a **placeholder** in v0.7. Real Azure Activity Log integration needs a separate API surface and elevated RBAC; it lands in v0.7.1.

## Performance

Vault list: one fetch on launch. Secrets list: one fetch per vault, cached per session. Value fetch: bounded concurrency 1; 200ms debounce. `R` invalidates and refetches.
