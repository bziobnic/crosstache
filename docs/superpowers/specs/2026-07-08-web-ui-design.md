# Embedded Web UI: `xv ui`

**Date:** 2026-07-08
**Status:** Approved design, not yet implemented

## Motivation

The CLI is fine for scripted and quick operations, but three workflows are
clunky in a terminal: full secret CRUD (multiline values, tags, folders,
expiry in one form), file management (drag-and-drop beats `xv file put`),
and occasional-use vault/admin operations where nobody remembers the flags.

Constraints that shape the design:

- **Offline vault access must keep working** — a hosted interface is out.
  The local backend works offline; the UI must too.
- **No logic reimplementation** — the multi-backend architecture
  (`Backend`, `SecretBackend`, `VaultBackend`, `FileBackend` traits in
  `src/backend/`) means any second implementation of business logic is a
  bug farm. The web layer is a *frontend* to those traits, exactly like
  `cli/` and `tui/` are.
- Single user, single machine. Not a team product.

## Architecture

New module `src/web/` behind a `ui` cargo feature (mirrors the existing
`tui` feature gate). New subcommand `xv ui [--port N] [--no-open]`.

Startup sequence:

1. Resolve config and construct backend/manager handles exactly like every
   other subcommand.
2. Build an axum `Router` with those handles in shared state (`Arc`; the
   trait objects are already `Send + Sync`).
3. Generate a random 256-bit session token.
4. Bind `127.0.0.1:0` (ephemeral port) unless `--port` is given.
5. Print `http://127.0.0.1:PORT/?token=...` and open it via the `opener`
   crate (already a dependency) unless `--no-open`.
6. Run in the foreground until Ctrl-C. No daemon, no pidfile, no
   background service — the server's lifetime equals the user's intent to
   use it.

Files (approximate):

- `src/web/mod.rs` — router construction, startup, shutdown.
- `src/web/api.rs` — JSON handlers.
- `src/web/assets/` — `index.html`, `app.js`, `style.css`, embedded with
  `include_str!`.

**One new dependency: `axum`.** It is built on hyper/tower, which
`reqwest` already pulls into the tree, so the real dependency cost is
marginal. No other new crates.

### Rejected alternatives

- **Tauri desktop app** — second binary, heavy dependency tree, platform
  packaging; loses "it's just the CLI you already installed."
- **Separate `xv-web` workspace binary** — two binaries to version and
  release, links all the same code anyway. The feature flag provides the
  same opt-out inside one binary.
- **Node/Vite frontend toolchain** — adds a second build system for a UI
  that is a sidebar, a table, a form, and a file panel. Vanilla JS
  suffices; revisit only if the frontend grows real complexity.

## Security model

The threat on localhost is not the network — it is **other web pages in
the user's browser** making requests to `http://127.0.0.1:PORT` (CSRF /
DNS rebinding). The design counters that specifically:

- **Bind 127.0.0.1 only.** Never 0.0.0.0, not even as an option. No TLS —
  traffic never leaves loopback.
- **Per-session bearer token** (Jupyter model). Generated at startup,
  delivered once in the opened URL; the page keeps it in memory (not
  localStorage) and sends `Authorization: Bearer <token>` on every API
  call. Every API handler rejects requests without a valid token using a
  constant-time comparison.
- **Host/Origin validation.** Reject requests whose `Host` is not
  `127.0.0.1:PORT`/`localhost:PORT`, and whose `Origin` (when present) is
  not the served origin. Nearly free second layer against DNS rebinding.
- **No secret values in URLs.** Values travel only in JSON bodies; reveal
  is an explicit POST. List endpoints return metadata only (same
  reveal-on-demand behavior as the TUI).
- **`Cache-Control: no-store`** on all API responses.

Deliberately skipped (meaningless on loopback with a bearer token, each a
real complexity cost): TLS, login pages, sessions, rate limiting. If LAN
access is ever wanted, that is the moment to add TLS + real auth.

## API

Thin JSON layer; one handler per operation; each handler parses,
delegates to the existing manager/backend method, and serializes. No
business logic in handlers.

```
GET    /api/context                     current vault/backend/config for header bar
GET    /api/vaults                      list vaults
GET    /api/secrets?vault=..&folder=..  list (metadata only; follows existing pagination)
GET    /api/secrets/:name               metadata for one secret
POST   /api/secrets/:name/value         reveal value (POST so value request isn't a GET URL)
PUT    /api/secrets/:name               create/update (value, groups, note, folder, expires…)
DELETE /api/secrets/:name
POST   /api/secrets/:name/move          mv (rename / re-folder)
GET    /api/files?prefix=..             file list
POST   /api/files                       upload (multipart)
GET    /api/files/:name                 download
DELETE /api/files/:name
```

- **Errors:** one `impl IntoResponse` mapping `CrosstacheError` variants
  to status codes + `{"error": "<display message>"}`. The user-friendly
  messages already exist in the error type.
- **Models:** reuse existing serde types (`SecretInfo` etc. already derive
  `Serialize`). No parallel DTO layer unless a type leaks something it
  shouldn't.
- **Scope deferral:** sharing/RBAC and config editing are v2. The v1
  header bar shows current vault/backend and switches vaults, which
  covers the day-to-day admin need. Endpoints are trivial to add once the
  pattern exists.

## Frontend

No build step. `index.html` + `app.js` + `style.css`, vanilla JS with
`fetch`, embedded in the binary.

Layout:

- **Left sidebar:** folder/group tree + vault switcher.
- **Main panel:** secret table with search box. Client-side filtering over
  the loaded page is a `String.includes`; the list endpoint already
  filters server-side.
- **Detail drawer:** view/edit form — value textarea, tags, note, expiry;
  reveal + copy buttons. Copy uses `navigator.clipboard.writeText`
  (localhost is a secure context, so it works without hacks).
- **Files tab:** list + drag-and-drop upload via native `dragover`/`drop`
  events, download, delete.

## Testing

- Handler tests via axum's `tower::ServiceExt::oneshot` against mock
  backends (`mockall` is already a dev-dependency; the traits are
  mockable). Cover: request without token rejected, bad Host/Origin
  rejected, one happy path per endpoint, error mapping.
- Smoke test that the router serves the embedded assets.
- No browser/E2E automation in v1; the JS is thin enough that API tests
  plus manual use cover it. Add Playwright only if frontend logic grows.

## Size estimate

~600–900 lines of Rust, ~500–800 lines of HTML/JS/CSS, one new crate
(`axum`).
