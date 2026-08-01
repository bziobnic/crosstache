# Ponytail Audit — crosstache

Whole-repo over-engineering audit. Scope: complexity and bloat only (not
correctness, security, or performance). Findings ranked biggest cut first.
One line per finding: `<tag> <what to cut>. <replacement>. [path]`.

## Findings

1. **`native:`** `azure_security_keyvault` SDK crate pulled in for a *single* `client.delete()` call (`delete_secret`) — get/set/list all already go through your `reqwest` REST client. Route delete through the same REST path (`DELETE /secrets/{name}?api-version=7.4`) and drop the whole Azure Key Vault SDK dependency tree. [`src/secret/manager.rs:7,383,1134`, `Cargo.toml`]

2. **`delete:`** `config = "0.14"` is a declared dependency with **zero usage** — every `config::` in the tree resolves to your own `crate::config` module; no `File::with_name`/`Environment`/`add_source` anywhere. Your config hierarchy is hand-rolled. Remove the dep. [`Cargo.toml`]

3. **`delete:`** Five error constructors with **0 callers**: `code`, `exit_code`, `rate_limited`, `suggestion`, `with_suggestion` — plus the entire suggestion-on-error mechanism they back. Nothing. [`src/error.rs`]

4. **`native:`** `urlencoding` (1 call) + `percent_encoding` (1 call) both duplicate what `url` — already a required dep — ships: `url::form_urlencoded` (which you use in 3 other files) and its re-exported `percent_encoding`. Consolidate both onto `url`, drop 2 deps. [`src/utils/url_helpers.rs:40`, `src/vault/operations.rs:7`, `Cargo.toml`]

5. **`delete:`** `anyhow` earns its keep via one line: `Other(#[from] anyhow::Error)`. Replace with an existing variant (`unknown`/`Other(String)`) and drop the dep. [`src/backend/error.rs:78`, `Cargo.toml`]

6. **`delete:`** `time = { features = ["macros"] }` in `[dependencies]` (and dev-deps) is never used directly — no `format_description!`, no `OffsetDateTime::` construction; it only appears in comments describing Azure's return type. It stays available transitively through `azure_core`. Remove the direct dep. [`Cargo.toml`]

7. **`yagni:`** `strsim` exists for one `levenshtein` did-you-mean call while `nucleo` (already a dep) does fuzzy ranking. Rank suggestions with `nucleo`, drop `strsim`. [`src/utils/suggestions.rs:20`, `Cargo.toml`]

8. **`native:`** minor — `globset` declared directly though `ignore` (also a dep) bundles and can build the same glob sets; and `base64`+`hex` both duplicate `data_encoding` (already in for BASE32). Only collapse if you touch those files anyway; low payoff, several call sites. [`src/scan/walker.rs`, `Cargo.toml`]

net: -~110 lines, -7 deps possible (one — `azure_security_keyvault` — pulls a large transitive tree with it).

## Out of scope

This pass hunts complexity only. The `serde_yaml`-backed YAML output alongside
JSON/CSV/plain/raw/template is *format sprawl* worth questioning, but those are
shipped, user-facing features — killing them is a product call, not an
over-engineering cut, so they are left off the ledger.
