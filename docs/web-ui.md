# Web UI (`xv ui`)

Build with `cargo build --features ui`. Run `xv ui` — it binds an ephemeral
port on 127.0.0.1, prints a tokenized URL, and opens your browser
(`--no-open` to skip, `--port N` to pin the port). Ctrl-C stops it.

Everything the UI does goes through the same backend layer as the CLI, so
all backends (Azure, AWS, local) work, including offline local vaults.

Security model: loopback bind only; per-session bearer token (the `?token=`
in the URL, held in page memory); Host/Origin validation; secret values only
in POST bodies; `Cache-Control: no-store`. There is no TLS and no login —
if you need network access to your vaults from another device, this is
deliberately not the tool.

Design: `docs/superpowers/specs/2026-07-08-web-ui-design.md`.
