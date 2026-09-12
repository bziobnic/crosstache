# Secret Domain Model PR 3: Disclosure Boundaries and Proof — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every intentional plaintext disclosure explicit and enumerable, prove in both directions that nothing else discloses, and close the checklist items A05-06/A05-08 and the ROADMAP P1 entry.

**Architecture:** `DisclosedSecret { name, value: String }` is the only serializable carrier of plaintext, produced solely by `Secret::disclose(self)`. Boundaries that print rather than serialize keep `expose_secret()`. A canary suite drives the real local backend through the CLI, the web API (fake backend), and the cache, asserting the canary appears exactly where documented and nowhere else. `docs/security.md` gains a "Disclosure boundaries" section listing every boundary with its test.

**Tech Stack:** Rust 2021; PR 1/2's `src/secret/domain/`; `tests/common` harness; `src/web/testutil.rs`.

**Spec:** `docs/superpowers/specs/2026-09-11-secret-domain-model-design.md` ("Disclosure boundaries", "Verification")

## Global Constraints

- Never run `cargo` with `run_in_background`; foreground only, timeout up to 600000 ms, `| tail -40`. `CARGO_TARGET_DIR=/Users/scottzionic/crosstache/target`.
- Never `git stash`. Never push.
- No behavior change to what is disclosed: every path that shows a value today still does; every path that does not, still does not. Output formats byte-identical except where a test proves an existing leak (then fix the leak and record it in CHANGELOG under Fixed).
- `expose_secret()` (on `SecretValue`, not age's `ExposeSecret`) call sites: only provider writes, crypto inputs, the boundaries in `docs/security.md`, and tests. The doc list and the grep must agree.
- Gates per task: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, named tests. Task 5 runs the full workspace.

---

### Task 1: `DisclosedSecret` and `Secret::disclose`

**Files:** `src/secret/domain/disclosure.rs`, `src/secret/domain/secret.rs`, `src/secret/domain/mod.rs`

```rust
/// A secret whose plaintext has been deliberately released as a plain
/// `String` for serialization at a reviewed boundary. Constructed only by
/// [`Secret::disclose`]; `grep -rn "\.disclose(" src` is the complete list
/// of serializing disclosure boundaries.
#[derive(Debug, Clone, Serialize)]
pub struct DisclosedSecret {
    pub name: String,
    pub value: String,
    pub content_type: String,
    pub tags: HashMap<String, String>,
}

impl Secret {
    pub fn disclose(self) -> DisclosedSecret;
}
```

`Debug` on `DisclosedSecret` is derived on purpose: it exists to be shown. Tests: `disclose` carries name/value/content_type/tags; the `SecuritySurface` allowlist in `src/error.rs` gains `DisclosedSecret` with `allowed_value_like_fields: &["value"]`, and `ConnectionComponent` with `allowed_value_like_fields: &["value"]` (deferred from PR 1).

### Task 2: Route serializing boundaries through `disclose`

**Files:** `src/web/api.rs` (`reveal_secret` → `Json(secret.disclose())` — response gains `name`/`content_type`/`tags` alongside `value`; keep `value` key name; update the UI reader only if it reads anything but `value` — check `src/web/assets/*.js` for `/value` fetch), `src/cli/vault_ops.rs` (`vault export` JSON/env/txt/keeper paths take `DisclosedSecret`), `src/cli/config_ops.rs` (`xv export` JSON/YAML/CSV/dotenv), `src/cli/secret_ops.rs` (`xv diff --show-values` builds `DisclosedSecret` pairs).

Printing boundaries stay on `expose_secret`: `xv get --raw`, clipboard, `--record` printers, TUI reveal, scan orchestrator, `parse_connection_components`.

Rule: after this task, `grep -rn "expose_secret" src --include='*.rs' | grep -v "/domain/" | grep -vE "age::|ExposeSecret"` hits only the printing boundaries above, provider writes, crypto, and tests. Produce the list in the report.

### Task 3: Canary suite (negative direction)

**Files:** new `tests/e2e_disclosure.rs` (local backend, `tests/common::xv_isolated_local` plus a hermetic `XV_CACHE_DIR` with caching enabled in the generated config), `src/web/api.rs`/`src/web/secrets.rs` tests, `src/cache/manager.rs` tests.

Canary: `disclosure-canary-7f3e` as the value of secret `LEAKY` (plus a typed `login` record with a canary password).

Negative assertions (canary absent from stdout+stderr, and from every file under the cache dir):
- `xv ls`, `ls --format json|yaml|csv`, `ls --deleted`, `xv history LEAKY --format json`, `xv find leak`, `xv get LEAKY` (no `--raw`; the clipboard path — assert stdout/stderr only), `xv info LEAKY` if it exists, `xv vault export` without `--include-values`, `xv scan` over a file that does NOT contain the canary (the scanner must not print the vault value), `xv cache status`, a failing command against `LEAKY` (wrong vault) — stderr has no canary, `xv --debug ls` (if a debug/verbose flag exists) — no canary in tracing output, `xv audit` (local audit on) — no canary in the audit file.
- Web (fake backend seeded with the canary): every route except `POST /secrets/{name}/value` returns a body without the canary — enumerate routes from `src/web/mod.rs` and loop over all GET routes plus the mutating ones PR 1 covered.
- Error Display: `format!("{}", err)` and `{:?}` for a `BackendError`/`CrosstacheError` built from a request containing the canary contain no canary (unit test).

### Task 4: Positive direction

- `xv get LEAKY --raw` prints exactly the canary; `xv get --field password` on the record; `vault export --include-values` JSON/env/txt each contain it; `xv export` JSON contains it; `xv diff --show-values` shows it; web `POST /secrets/LEAKY/value` returns it; TUI reveal keystroke (existing `tui_view_tests.rs` harness) renders it after reveal and masks before.
- Each positive test is named after the boundary it pins and referenced from `docs/security.md`.

### Task 5: Docs, roadmap, changelog, gates

- `docs/security.md` (or the existing security doc — find it) "Disclosure boundaries" table: boundary, mechanism (`disclose` / `expose_secret`), test name.
- `CHANGELOG.md` Unreleased: `DisclosedSecret`; reveal endpoint body now includes `name`/`content_type`/`tags` (wire change, intentional); any leak the canary suite found, under Fixed.
- `ROADMAP.md`: delete the P1 "Split secret-domain types" entry; checklist A05 items are complete.
- `CLAUDE.md`: one line pointing at the disclosure table; note that `grep expose_secret` must exclude age's `ExposeSecret`.
- Full gates.
