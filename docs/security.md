# Security notes

This file documents the boundaries at which crosstache deliberately turns a
secret's plaintext into something that leaves the process — printed to a
terminal or clipboard, or serialized into JSON/YAML/CSV/dotenv/Keeper output.
User-facing behavior belongs in `README.md` and `docs/`; this page is the
map agents and reviewers use to check that no new code path discloses a
value without being added here.

## The two disclosure primitives

- **`Secret::disclose` → `DisclosedSecret`** (`src/secret/domain/disclosure.rs`)
  is the only way a secret's value becomes *serializable*.
  `DisclosedSecret { name, value, content_type, tags }` derives `Serialize`
  (no `Deserialize`) on purpose — it exists to be emitted, never parsed back
  in. Because `disclose` is the sole constructor, `grep -rn "\.disclose(" src`
  lists every boundary that serializes a **whole secret value**. The one
  field-level exception is `xv get --record --format json|yaml`, which
  serializes decoded envelope fields (via `expose_secret`, not `disclose`) —
  see the table below.
- **`SecretValue::expose_secret`** (`src/secret/domain/value.rs`) is the only
  read access to plaintext held as a `SecretValue`. It backs every *printing*
  boundary (terminal, clipboard, TUI) plus the sanctioned record-field
  exception above. `expose_secret` is also the method name of age's
  `secrecy::ExposeSecret` trait (used for age identities under
  `src/secret/attachment*`, `src/backend/local/`,
  `src/backend/attachment_key*`, and two individually verified lines in
  `src/backend/azure/secrets.rs` and `src/agent/decision_log.rs`). A grep for
  `expose_secret` must be filtered to `SecretValue` receivers — the age
  identity hits are not secret-value disclosures.

## Disclosure boundaries

| Boundary | Command or route | Mechanism | Pinned by (test name) |
| --- | --- | --- | --- |
| Raw value to stdout | `xv get <name> --raw` | `SecretValue::expose_secret` | `boundary_get_raw_prints_value` |
| Raw record field to stdout | `xv get <name> --field <f> --raw` | `SecretValue::expose_secret` | `boundary_get_field_raw_prints_record_field` |
| Record envelope fields (field-level exception) | `xv get <name> --record` (`--format json\|yaml`) | `SecretValue::expose_secret` | `boundary_get_record_prints_envelope_fields` |
| Clipboard copy | `xv get <name>` / `xv get <name> --field <f>` (no `--raw`) | `SecretValue::expose_secret` | `get_without_raw_never_prints_the_value` (negative: stdout/stderr stay canary-free) |
| Vault export with values | `xv vault export --include-values` (json, env, txt) | `Secret::disclose` | `boundary_vault_export_include_values_prints_value` |
| Vault export, Keeper format | `xv vault export --fmt keeper --include-values` | `Secret::disclose` | covered by `tests/e2e_local_backend.rs::keeper_export_requires_include_values` / `::keeper_export_round_trips_an_imported_file` |
| Whole-vault plaintext export | `xv env pull` (json, yaml, csv, dotenv) | `Secret::disclose` | `boundary_env_pull_prints_value` |
| Diff of differing values | `xv diff --show-values` | `Secret::disclose` | `boundary_diff_show_values_prints_value` (also asserts silence without the flag) |
| Web reveal endpoint | `POST /api/secrets/{name}/value` | `Secret::disclose` | `boundary_web_reveal_returns_value` |
| TUI reveal | `Space` on a selected secret in `xv tui` | `SecretValue::expose_secret` | `boundary_tui_reveal_renders_value` |
| Scan match (value never printed) | `xv scan <dir>` | `SecretValue::expose_secret` | `boundary_scan_matches_the_value_without_printing_it` |
| Template/env injection to stdout or `--output` | `xv inject` (`{{ secret:name }}` / `xv://…` references) | `SecretValue::expose_secret` via `record_field_value` | not pinned by a canary test; covered functionally by `tests/e2e_local_backend.rs` (e.g. `inject_happy_path_renders_output`) |
| Injection into a child process environment | `xv run` (masks stdout/stderr by default; see README) | `SecretValue::expose_secret` via `record_field_value` | not pinned by a canary test; covered functionally by `tests/e2e_local_backend.rs` (e.g. `run_happy_path_launches_child`) |
| Connection-string components | `ConnectionComponent` table (`xv parse`, etc.) | value already exposed by the caller | not canary-testable — the parser takes a caller-provided `&str`; its `value` is a display type over an already-disclosed connection string, not a new disclosure |

The web reveal endpoint's body is exactly `{"value": ..}` — narrower than
`DisclosedSecret` — by deliberate choice: `name` and `content_type` add
nothing the caller doesn't already have, and `tags` would put internal record
markers on a second endpoint.

**Not a boundary:** `xv totp` (`src/cli/totp_ops.rs`) exposes the seed only to
derive the current code; it prints the generated code, never the seed.

## What the negative suite proves canary-free

`tests/e2e_disclosure.rs` (20 tests: 13 negative surfaces + 7
`boundary_*` positive tests) and `src/web/disclosure_tests.rs` (3
route-sweep tests + `boundary_web_reveal_returns_value`) plant two canaries —
an untyped secret's value and a typed record's encrypted-field value — and
assert both stay absent everywhere they must not appear:

- every secret listing, in every output format (table, json, yaml, csv,
  plain), including `--long`, `--recursive`, `--names-only`, `--type`, and
  `--deleted`
- `xv history`, `xv find` (every `--in` scope), `xv info`, `xv group list`
- `xv get` / `xv get --field` without `--raw` (stdout and stderr only —
  the value still reaches the clipboard, which is the printing boundary
  above, not a leak)
- `xv vault export` without `--include-values`
- `xv scan` over content that does not contain the canary
- cache files under `XV_CACHE_DIR` (a cache that is never written can't
  prove this, so the harness forces `cache_enabled = true`)
- the on-disk age-encrypted store (ciphertext only)
- the local audit log (`[local].audit`) and its rendered `xv audit` output
- `RUST_LOG=trace` output under `xv --debug` for list/get/history/export
- stderr for not-found secrets, fields, and vaults
- every GET and mutating route in the web `build_router` except
  `POST /api/files` (multipart upload; the request is rejected by the
  `enforce_upload_envelope` middleware layer before the handler runs, so a
  JSON probe would prove nothing — it is instead covered by `src/web/files.rs`'s
  own tests), including a round trip where the canary is sent inbound so an
  echoing route would be caught (`every_get_route_is_value_free`,
  `every_mutating_route_is_value_free`, `conversion_apply_is_value_free`)
- `SecretRequest`'s redacted `Debug`, and `Display`/`Debug`/`code()` of the
  `BackendError`/`CrosstacheError` variants built from it, in `src/error.rs`
- parse-error paths in `src/records/envelope.rs` never echo the envelope's
  contents (`parse_errors_never_echo_the_envelope_contents`)

No leak was found by this suite; the suites named above —
`tests/e2e_disclosure.rs`, `src/web/disclosure_tests.rs`, `src/error.rs`,
and `src/records/envelope.rs` — are the complete negative coverage.
