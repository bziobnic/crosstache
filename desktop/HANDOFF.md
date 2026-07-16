# macOS Tauri Proof of Concept Handoff

## Current state

The proof of concept is implemented and verified on macOS. It builds a native
Tauri 2 application named **Crosstache Vault** while preserving the existing
`xv` CLI, TUI, and browser-based `xv ui` behavior.

The unsigned application bundle is produced at:

```text
target/release/bundle/macos/Crosstache Vault.app
```

The verified bundle is approximately 16 MB and includes the Azure, AWS, and
local backends plus file operations and the existing web interface.

## Architecture

This is intentionally a thin native shell, not a second implementation of xv.

1. `xv-desktop` loads the existing crosstache configuration and constructs a
   `BackendRegistry`.
2. `crosstache::web::prepare_web` binds the existing Axum UI to an ephemeral
   loopback port and returns its tokenized URL before serving.
3. Tauri initially displays a bundled loading screen.
4. Once the backend and Axum server are ready, the native WKWebView navigates
   to the tokenized loopback URL.
5. The existing HTML, CSS, JavaScript, API handlers, bearer-token checks,
   Host/Origin checks, and backend traits handle all application behavior.

The new `PreparedWebServer` split keeps `xv ui` unchanged: the CLI still opens
the browser and serves until Ctrl-C, while the desktop process calls the
non-shutdown `serve` method.

## Important files

- `desktop/src-tauri/src/main.rs`: desktop startup, optional project-directory
  selection, configuration/backend initialization, webview navigation.
- `desktop/src-tauri/tauri.conf.json`: product/window/bundle configuration and
  loading-page CSP.
- `desktop/src-tauri/capabilities/default.json`: minimal local-window Tauri
  permissions.
- `desktop/frontend/`: loading and startup-error UI used before Axum is ready.
- `desktop/src-tauri/icons/`: source PNG/SVG and generated macOS `.icns`.
- `src/web/mod.rs`: reusable `prepare_web`/`PreparedWebServer` lifecycle.
- `desktop/README.md`: source-run and bundle commands.

The root manifest is now a workspace with `desktop/src-tauri` as a member and
the original crosstache package as the sole default member. Existing root
`cargo build` and `cargo test` commands therefore retain their prior scope.

## Run and build

Development window:

```bash
cargo run --manifest-path desktop/src-tauri/Cargo.toml
```

Select a project directory for `.xv.toml` discovery:

```bash
cargo run --manifest-path desktop/src-tauri/Cargo.toml -- --project /path/to/project
```

The equivalent environment variable is `XV_DESKTOP_PROJECT`.

Unsigned macOS bundle:

```bash
cd desktop/src-tauri
cargo tauri build --bundles app --no-sign --ci
```

## Verification completed

- `cargo check -p xv-desktop`: passed.
- `cargo build -p xv-desktop`: passed.
- `cargo clippy -p xv-desktop -- -D warnings`: passed.
- `cargo test --features ui web::`: passed, 118 tests across the library and
  binary module copies. This covers authentication, secrets, files, typed
  records, selection/bulk actions, and oversized uploads.
- The full library suite passed with an isolated `HOME`: 817 passed and 1
  ignored. The broader integration run reached the existing clipboard suite;
  8 of 9 clipboard tests passed, while the detached-clear test could not read
  back `pbcopy` contents in the sandbox even with one test thread.
- Native debug window: rendered successfully against an isolated local
  backend.
- Live API smoke test: context, HTML, create, list, reveal, and delete passed
  against the isolated local vault.
- Packaged `.app`: launched successfully and visually rendered the full vault
  UI from its bundled executable.

The smoke test used temporary HOME/XDG directories under `/tmp`; it did not
read or mutate a real vault.

## Known limitations

- This is macOS-only proof-of-concept work. No Windows/Linux desktop bundle or
  desktop release matrix has been added.
- The `.app` is unsigned and not notarized. There is no DMG, updater, or store
  packaging.
- Desktop startup calls `load_config()` directly. It supports project-relative
  vault/type discovery after `--project`, but it does not yet share the CLI's
  full top-level backend-precedence resolution from `src/main.rs`, especially
  project environment profiles that select a backend.
- A Finder-launched app has a different environment and `PATH` than a terminal.
  Azure CLI and AWS SSO/profile authentication still need explicit packaged-app
  testing.
- The desktop app still runs a token-protected loopback HTTP server. Moving to
  direct Tauri IPC would remove that server but requires a transport-neutral UI
  service and a frontend transport adapter.
- There is no in-app project picker, recent-project list, backend switcher, or
  credential recovery workflow.
- The debug build prints the tokenized loopback URL for smoke testing. Release
  builds do not print it.
- The macOS clipboard integration test noted above remains environment-sensitive
  in the sandbox and was not changed as part of this desktop work.

## Recommended next steps

1. Extract the CLI's config/backend/profile precedence into a reusable library
   startup service and use it from both binaries.
2. Add a native project-directory picker and persist recent project choices.
3. Test packaged Azure CLI, Azure environment credentials, AWS profiles, and
   AWS SSO behavior from Finder launch contexts.
4. Decide whether the production desktop app retains the hardened Axum
   loopback transport or migrates to Tauri IPC.
5. Add macOS signing/notarization secrets and a desktop bundle job to the
   release workflow; add a DMG only after that path is stable.
6. Add an automated packaged-app smoke test where platform tooling permits it.
