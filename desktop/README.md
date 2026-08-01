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

With no configuration, the source app shows the interactive Setup Required
screen. Use the package smoke below for the isolated, automated Local setup
verification.

## Test the desktop startup flow

```bash
cargo test -p xv-desktop
bash tests/desktop/package-smoke.sh
```

The package smoke builds an unsigned local `.app`, launches its executable
directly, and uses a temporary HOME/XDG root. Its explicitly opt-in internal
package-smoke hook accepts only that validated root, performs the normal Local
setup/list path, and emits token-free startup state markers. It is not a
desktop user feature and writes no vault secret records.

## Bundle a macOS app

Install the Tauri CLI once, then build the application bundle:

```bash
cargo install tauri-cli --version '^2' --locked
cd desktop/src-tauri
cargo tauri build --bundles app --no-sign --ci
```

This proof of concept creates an unsigned `.app`. Public distribution still
requires Developer ID signing and notarization.

To run the isolated bundle check after a build:

```bash
cd ../..
bash tests/desktop/package-smoke.sh
```
