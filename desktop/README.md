# xv Desktop macOS Proof of Concept

This package runs the existing `xv ui` Axum server inside a Tauri process and
loads its tokenized loopback URL in a native macOS webview. It intentionally
reuses the current web assets, API, backend traits, and security controls.

## Run from source

```bash
cargo run --manifest-path desktop/src-tauri/Cargo.toml
```

The app uses the global xv configuration and the process environment. To make
project `.xv.toml` discovery deterministic, pass a directory or set the
equivalent environment variable:

```bash
cargo run --manifest-path desktop/src-tauri/Cargo.toml -- --project /path/to/project
XV_DESKTOP_PROJECT=/path/to/project cargo run --manifest-path desktop/src-tauri/Cargo.toml
```

For an isolated local-backend smoke test, set `XV_BACKEND=local` and provide a
temporary `XDG_CONFIG_HOME` as described in the web UI test documentation.

## Bundle a macOS app

Install the Tauri CLI once, then build the application bundle:

```bash
cargo install tauri-cli --version '^2' --locked
cd desktop/src-tauri
cargo tauri build --bundles app --no-sign --ci
```

This proof of concept creates an unsigned `.app`. Public distribution still
requires Developer ID signing and notarization.
