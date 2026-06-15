# `xv tui` — Read-Only Terminal UI

Three-pane terminal browser for vaults and secrets. The TUI is **read-only**:
create/edit/delete flows are reserved for a future write mode.

Release binaries include the TUI. When building from source, enable the `tui`
feature flag:

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

### Reserved for write mode

`c` (create), `d` (delete), `r` (rename / rotate). Pressing one shows a
reserved-for-write-mode toast; the TUI does not mutate vault or secret state.

## On-demand value fetch

The TUI lists secrets cheaply (names + metadata). The **value** for the highlighted secret loads on demand: settle the cursor for ~200ms and the value fetches in the background, lands in an in-memory cache (cleared on quit), and the detail pane shows `●●●●●●` until you press `Space` to reveal.

Values are wrapped in `Zeroizing<String>` end-to-end.

## Audit overlay limitation

The audit overlay (`a`) is still a **placeholder** inside the TUI. Use
`xv audit` from the CLI for Azure Activity Log or AWS CloudTrail-backed audit
history.

## Performance

Vault list: one fetch on launch. Secrets list: one fetch per vault, cached per session. Value fetch: bounded concurrency 1; 200ms debounce. `R` invalidates and refetches.
