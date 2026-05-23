# Strategic Improvements — Phase 1 (v0.6 → v0.7) Design

> **Status:** ✅ Implemented in **v0.6.0–v0.7.0** (2026-04-29).
> Loved-features parity arc: errors+exit-codes (v0.6.0-rc.1), env profiles (v0.6.0-rc.2), fuzzy find (v0.6.1-rc.1), leak scanner (v0.7.0-rc.1), TUI (v0.7.0-rc.2).
> Retained as design history.
> Roadmap & open work tracked in `ROADMAP.md` at the repo root.
> Implementation history lives in `CHANGELOG.md`. This file is retained as design context — do not edit to reflect current behavior; open a new spec instead.


**Date:** 2026-04-29
**Status:** Design approved; spec under self-review
**Owner:** Scott Zionic
**Inputs:** `~/Documents/MAIN/Projects/crosstache/{Top features.md, Feature coverage.md, Pluggable backends.md}`; `dev/ROADMAP.md`; `dev/MISSING-FEATURES.md`; `docs/FEATURES.md`; current code under `src/`.

---

## 1. Strategic positioning

The reference docs make three competitive claims:

1. **15 features make a CLI secrets manager beloved** (injection, fzf, clipboard safety, dir-aware context, TUI, pre-commit scanning, offline mode, modern encryption, etc.).
2. **No competitor ships all 15** — there is open ground.
3. **No general-purpose CLI is truly backend-agnostic.** `aws-vault` is closest (AWS-only); HashiCorp Vault is heavyweight server territory. The cross-backend gap is the largest strategic prize.

Crosstache today already wins on injection, clipboard safety, history/rollback, RBAC sharing, audit (Azure Activity Log), output formats, shell completion, file storage + sync, and self-update. The two strategic gaps are:

- **Loved-features parity** for the Azure-bound developer audience: missing fuzzy/TUI/dir-aware-context/scanner.
- **Backend pluggability** — the cross-backend gap.

This spec addresses **phase 1** only: the loved-features parity push, sequenced as a one-quarter v0.6 → v0.7 arc. It is paired with a **soft commitment** to backend pluggability — features are written against a small read surface so phase 2 trait extraction is a mechanical refactor, not a rewrite. Phase 2 (backend abstraction + second backend) is a separate spec, started after v0.7.0 ships.

---

## 2. Phase 1 scope

### 2.1 Features shipping (in sequence order)

1. **Structured errors + suggestions + exit-code discipline** (cross-cutting foundation).
2. **`.xv.toml` env profiles + walk-up directory-aware context** (Doppler-style env switching by directory).
3. **`xv find` fuzzy search + clean `fzf` integration**.
4. **`xv scan` pre-commit leak scanner + installable git hook**.
5. **`xv tui` interactive terminal UI** (read-only at v0.7; edit mode deferred).

### 2.2 Features deliberately deferred

- Backend-trait extraction and second backend (phase 2).
- TUI edit/create/delete/rename (v0.8).
- Scanner disk fingerprint cache (v0.7.x or v0.8).
- Scheduled rotation policies (later phase).
- CI/CD ecosystem starters — GitHub Action, Terraform provider, K8s operator (later phase).
- Offline fallback file for `xv run` (later phase).
- Multi-theme TUI; configurable TUI keybindings (v0.7.x+).
- Telemetry / phone-home (won't do).
- Compliance attestations (SOC2/HIPAA — different kind of work, not on roadmap).
- Bundling gitleaks; competing on pattern count (won't do — composition pattern documented instead).

---

## 3. Per-feature design

### 3.1 Feature #8 — Errors + suggestions + exit codes

#### 3.1.1 Deliverables

1. **Stable error codes.** Add `code(&self) -> &'static str` on `CrosstacheError` returning kebab-case identifiers (`xv-vault-not-found`, `xv-secret-not-found`, `xv-permission-denied`, `xv-network-dns`, `xv-config-invalid`, etc.). Stable across releases — these become the contract for scripts and CI tooling.

2. **Documented exit codes.** `exit_code(&self) -> i32` method per variant. Code families:
   - `2` — invalid argument
   - `3` — config error
   - `10–19` — not-found family
   - `20–29` — permission/auth
   - `30–39` — network
   - `40–49` — Azure/backend
   - `50–59` — policy / scan findings (e.g., `50` = `xv-scan-leak-detected`)
   - `1` — unknown
   Documented in `docs/exit-codes.md` and the man page.

3. **"Did you mean…?" suggestions.** When `secret_not_found` or `vault_not_found` fires, the error path optionally consults the candidate list (already cached for most operations) and runs Levenshtein on it. If any candidate scores within distance ≤ 2, append `Did you mean: <best-match>?` to the user message. Suggestions only fire on TTY and only when the candidate list is already available — never trigger an extra API round-trip just to compute a suggestion.

4. **Machine-readable error envelope.** When `--format json|yaml` is in effect, errors print as

   ```json
   {"error": {"code": "xv-vault-not-found", "message": "vault 'myproj-prood' not found", "exit_code": 11, "suggestion": "myproj-prod"}}
   ```

   The `suggestion` field is the candidate name itself (omitted when no near-match was found, since machine consumers want a clean string-or-null shape).

   to stdout (not stderr) before exit. Today errors always go to stderr as plain text, which scripts can't parse cleanly.

5. **Static hint table.** A small `code → hint` map: e.g., `xv-vault-not-found → "Run 'xv vault list' to see available vaults"`. Hints are TTY-only; appended below the error message.

#### 3.1.2 Out of scope

Error i18n; severity levels (warn/info); restructuring the enum.

#### 3.1.3 Soft-commitment hygiene

No backend reads. Nothing for the trait checklist.

---

### 3.2 Feature #3 — `.xv.toml` env profiles + walk-up

#### 3.2.1 Schema

```toml
default_env = "dev"

[env.dev]
vault = "myproj-dev-kv"
resource_group = "myproj-rg"
group = "backend"          # optional
folder = "app/database"    # optional

[env.prod]
vault = "myproj-prod-kv"
resource_group = "myproj-prod-rg"
```

All non-env fields use `#[serde(default)]` so we can add fields (output-format defaults, mask lists, file-storage prefix) in v0.7.x without breaking existing files.

#### 3.2.2 Resolution algorithm

Replaces `ContextManager::load_local_context` (currently checks `current_dir()` only):

1. **Walk up** from `current_dir()` to filesystem root, stopping at the first `.xv.toml` found, or at a `.xv.boundary` marker file (a stopper users can drop to prevent crossing into a parent project).
2. If no `.xv.toml`, fall through to the legacy `.xv/context` JSON in cwd (back-compat for one minor; deprecation warning starting v0.6.0; removed in v0.8).
3. If neither, fall back to global context (existing behavior).
4. Determine **active env**: `XV_ENV` env var → global `--env <name>` CLI flag → `default_env` field → error `xv-env-not-defined` listing available envs.
5. Apply `[env.<active>]` fields as defaults for `--vault`, `--resource-group`, `--group`, `--folder`. CLI flags still override.

#### 3.2.3 Cross-boundary safety

When a `.xv.toml` is discovered above cwd (not in cwd itself), print a one-time-per-process info line to stderr:

```
using config from /path/to/.xv.toml (env: dev)
```

`XV_NO_PARENT_CONFIG=1` disables walk-up entirely.

#### 3.2.4 New commands

- `xv context init` — interactive scaffolder; prompts for env names + their vaults; can seed `dev` from current global context.
- `xv context envs` — list defined envs in the resolved `.xv.toml` and mark active.
- `xv context show` — extended to display "active env: dev (from /path/to/.xv.toml)" plus the resolved defaults.

#### 3.2.5 Coexistence with existing `xv env` namespace

`xv env create/use/list/pull/push` continue to manage **global, user-scoped** named profiles in the user config. `.xv.toml` env profiles are **project-scoped**. They coexist for v0.7 — when a project `.xv.toml` is found, it wins; outside any project, `xv env use` continues to drive context. Unifying the two (e.g., letting `.xv.toml` reference a global profile by name) is deferred to phase 2.

#### 3.2.6 Soft-commitment hygiene

Pure config plumbing. No backend reads. Nothing for the trait checklist.

---

### 3.3 Feature #1 — `xv find` fuzzy search

#### 3.3.1 Command surface

**`xv find <pattern>`** — non-interactive ranked fuzzy search over secret metadata in the active vault (env-resolved per 3.2). Implementation uses the `nucleo` crate (pure-Rust, fast; same matcher as Helix). One score per row, ranked descending, ties broken alphabetically by name.

- **Default search field:** secret name only.
- **Opt-in fields via flags:** `--in folder`, `--in groups`, `--in note`, `--in tags`. Multiple allowed; results union the matches.
- **Cross-vault:** `--all-vaults` searches every vault the caller has list rights on (uses existing cache; flagged "may be slow on cold cache").
- **Output:** default `table` shows name + folder + groups + score-bar; `--names-only` strips everything but the name (pipe-friendly, no headers, no ANSI even on TTY); JSON/YAML serialize a `[{name, score, folder, groups}]` array.
- **Sorting / limiting:** `--limit N` (default 50), `--min-score F` (default 0.3). Pagination flags from the existing list-pagination plan apply.

#### 3.3.2 `xv ls --names-only`

A new flag on the existing `xv list` that produces one-name-per-line, no headers, no separators, no ANSI. Pipe-into-fzf canonical form. Documented prominently:

```bash
xv get "$(xv ls --names-only | fzf)"
xv get "$(xv find db --names-only | fzf)"
```

`--names-only` overrides `--format` and disables the legacy "auto" format that resolves to JSON when stdout isn't a TTY — when the user asks for names only, that's what they get on stdout regardless of redirection.

#### 3.3.3 Stretch: dynamic shell completion of secret names

Hidden `xv __complete-secrets` subcommand the generated bash/zsh/fish completion scripts call on Tab. Hits the local cache only; never touches Azure on a Tab press. Lands in week 5 if foundation work clears in week 2; otherwise punted to v0.7.x.

#### 3.3.4 Soft-commitment hygiene — read surface

- `SecretManager::list_secrets()` (cached path)
- `VaultManager::list_vaults()` (only for `--all-vaults`)

Both go on the phase-2 trait checklist as read methods.

#### 3.3.5 Out of scope

Interactive picker (`xv pick`) — TUI scope. Live-as-you-type filtering — TUI scope.

---

### 3.4 Feature #4 — `xv scan` pre-commit leak scanner

#### 3.4.1 Scope decision

The scanner's unique value is matching files against the **user's actual secret values** — generic regex coverage is gitleaks's domain, and competing on pattern count (Infisical's 140+) is a losing game. So `xv scan` ships a *small* built-in pattern set (~20: AWS access keys, GitHub tokens, Stripe keys, Slack tokens, JWT, SSH private-key headers, generic high-entropy threshold) and treats user-secret matching as the headline. Composition with gitleaks for users wanting deeper pattern coverage is documented, not bundled.

#### 3.4.2 Commands

| Command | Behavior |
|---------|----------|
| `xv scan <path>...` | Scan files/dirs (default `.`); table output with finding count. |
| `xv scan --staged` | Scan only `git diff --cached` content; the pre-commit default. |
| `xv scan --all` | Scan whole HEAD tree. |
| `xv scan --hook` | Quiet on no findings; exits 50 (`xv-scan-leak-detected`) on findings; JSON to stdout when `--format json`. Designed for hook consumption. |
| `xv scan install` | Writes `.git/hooks/pre-commit`. Refuses if a non-`xv`-managed hook exists unless `--force`; appends if our marker comment is present; idempotent. |
| `xv scan uninstall` | Removes our hook (or our block from a managed hook). |

#### 3.4.3 Match engine

1. Fetch all secret values from active vault(s) into memory. Parallel fetch bounded by a semaphore (default 10 concurrent). Values held in `Zeroizing<String>`.
2. Build an Aho-Corasick automaton over (values + literal-prefix patterns); regex patterns compiled separately into a `RegexSet`.
3. For each candidate file: read content, run automaton + regex set, report findings. Skip binary files (magic-byte check), and files matching `[scan].exclude` globs (default: `.git/**`, `target/**`, `dist/**`, `node_modules/**`, `*.lock`, `*.min.*`).
4. **Output never echoes the matched value** — only file:line, the secret's *name* (by reverse lookup), and the pattern kind.

#### 3.4.4 Performance

In-memory plaintext, fetched fresh per process. Expected ~1–3 s on a 50-secret vault for `--staged`. No disk fingerprint cache in phase 1; designed in but not built. Two escape hatches documented: `[scan].min_value_length = 12` and `XV_SCAN_DISABLE=1`.

#### 3.4.5 `.xv.toml` schema additions

```toml
[scan]
exclude = ["dist/**", "*.lock"]
min_value_length = 8
patterns = ["aws", "github", "stripe", "slack", "jwt", "ssh", "high-entropy"]
```

`.xvignore` (line-based, `.gitignore` syntax) for path-level allowlists, with optional inline `# allow: <reason>` comments.

#### 3.4.6 Output formats

- Plain/table: `src/config.js:42: matches secret "DB_PASSWORD" (kind=value, vault=dev-kv)`
- JSON: `[{file, line, col, secret_name, vault, kind, severity}]`
- Findings always go to stderr; the JSON form goes to stdout when `--format json` is set (so CI can pipe).

#### 3.4.7 Soft-commitment hygiene — read surface

- `SecretManager::list_secrets` (already on checklist)
- `SecretManager::get_secret` (returns value) — **new entry**
- `VaultManager::list_vaults` (only for cross-vault scan)

#### 3.4.8 Out of scope

Disk fingerprint cache; daemon-based warm cache; auto-rotation on detection; gitleaks bundling.

---

### 3.5 Feature #2 — `xv tui` (read-only at v0.7)

#### 3.5.1 Stack & build

`ratatui` + `crossterm`. Behind a `tui` feature flag, **default off** at v0.7 to keep the default binary lean for users who only need scripting.

#### 3.5.2 Architecture

Elm-style Model–Update–View, async-decoupled:

- A single `App` struct owns the model: cursor positions, fetched data caches, current pane, filter state, async task handles, error toasts.
- A `tokio::sync::mpsc` channel carries `Message`s: `KeyPress`, `VaultsLoaded`, `SecretsLoaded`, `ValueLoaded`, `Tick`, etc.
- Render loop is synchronous: `terminal.draw(|f| view(&app, f))`. `view` is a pure function of `&App`.
- Background fetches run as `tokio::spawn`'d tasks that send `Message`s back. UI never blocks on Azure round-trips.
- Cached state survives across pane switches; `R` invalidates and refetches the current scope.

#### 3.5.3 Layout (single screen, three panes)

```
┌──────────────┬────────────────────────────┬──────────────────┐
│ Vaults       │ Secrets (filter: /db_)     │ Detail           │
│ > dev-kv     │ > DB_PASSWORD              │ name: DB_PASSWORD│
│   stage-kv   │   DB_HOST                  │ value: ●●●●●●    │
│   prod-kv    │   DB_PORT                  │ groups: backend  │
│              │                            │ folder: app/db   │
│              │                            │ updated: 2d ago  │
└──────────────┴────────────────────────────┴──────────────────┘
status: dev-kv · 24 secrets · cache ~ 30s ago               ?:help
```

#### 3.5.4 Keymap (read-only, vim-flavored)

| Key | Action |
|-----|--------|
| `h j k l` / arrows | move within pane |
| `Tab` / `Shift-Tab` | move between panes |
| `/` | live fuzzy filter on secrets pane (uses the same `nucleo` matcher from feature #1) |
| `Space` | toggle value reveal on detail pane |
| `y` | copy value to clipboard (with countdown indicator using `clipboard_timeout`) |
| `Y` | copy secret name |
| `a` | audit log overlay for current item |
| `H` | history (versions) overlay |
| `R` | refresh current scope (invalidate cache and refetch) |
| `?` | help overlay; lists reserved-but-unimplemented edit keys (`c`/`d`/`e`/`r` → "coming in v0.8") |
| `q` / `Esc` | quit (or close current overlay) |

#### 3.5.5 Loading & error states

- Every pane shows `(loading…)` with a spinner while a fetch is in flight; previous data stays visible until replaced.
- Errors render as a non-fatal toast at the bottom (auto-dismiss after 5 s), with `e` to expand into a modal showing the full `CrosstacheError` (code + message + hint — using the section-3.1 structured error infra).

#### 3.5.6 Backend-capability isolation

A few fields are Azure-specific (`resource_group`, soft-delete `recovery_level`, `deleted_date`). The TUI references these through a `BackendCapabilities` struct already present in config (`kind: BackendKind`); fields conditional on `kind == Azure`. Phase 2 trait extraction turns this into a method on the backend trait.

#### 3.5.7 On-demand value fetch

Listing secrets only fetches names/metadata. The value for the highlighted secret loads on demand (when the cursor lands on it for >150 ms — short debounce); results cache in-memory for the session. Values held in `Zeroizing<String>`; cache cleared on quit. Avoids fetching every value upfront for vaults with hundreds of secrets.

#### 3.5.8 Reserved edit-mode keys

`c`, `d`, `e`, `r` are explicitly in the help overlay as "coming in v0.8" — declares intent and prevents another contributor accidentally rebinding them later.

#### 3.5.9 Soft-commitment hygiene — read surface

- `SecretManager::list_secrets` (✓ already on checklist)
- `SecretManager::get_secret` (✓ already)
- `SecretManager::list_versions` — **new entry**
- `SecretManager::get_version` — **new entry**
- `SecretManager::get_audit_events` (or current Azure-Activity-Log path) — **new entry, may need wrapping**
- `VaultManager::list_vaults` (✓ already)

#### 3.5.10 Out of scope (read-only v0.7)

Edit / create / delete / rename; file-storage browser; vault-sharing UI; cross-vault search panes; mouse support beyond scroll; themes (one default theme; theme config seam in place); configurable keybindings.

---

## 4. Sequencing & milestones

### 4.1 Week-by-week (12-week target, ~1 week stretch buffer)

| Week | Theme | Deliverables |
|------|-------|--------------|
| 1 | Errors I | `code()` + `exit_code()` on `CrosstacheError`; `docs/exit-codes.md`; main.rs printer routes through them. No user-visible behavior change yet. |
| 2 | Errors II | Suggestion engine (Levenshtein, TTY-only); `--format json\|yaml` error envelope; static hint table; CLI integration tests assert codes & exit values. **v0.6.0-rc cut.** |
| 3 | Context I | `.xv.toml` schema + parser; walk-up traversal with `.xv.boundary` stopper; legacy `.xv/context` read-fallback (deprecation warning starts). |
| 4 | Context II | `xv context init` scaffolder; `xv context envs`; `xv context show` extended; `XV_ENV` + global `--env` flag; cross-boundary stderr line; `XV_NO_PARENT_CONFIG`. **v0.6.0 ships.** |
| 5 | Fuzzy search | `xv find` with `nucleo`; `xv ls --names-only`; README + man-page integration patterns. *Stretch:* dynamic shell completion. **v0.6.1 ships.** |
| 6 | Scanner I | `xv scan <path>` core engine: parallel value fetch, Aho-Corasick, regex set, file walker, exclude globs, `[scan]` config block, `.xvignore`. |
| 7 | Scanner II | `--staged` mode; `--all`; output formats (plain/JSON, value-never-leaked invariant); reverse-lookup name resolution. |
| 8 | Scanner III | `xv scan install` / `uninstall`; hook idempotency + safety checks; built-in pattern set finalized; perf docs. **v0.7.0-rc.1 ships (scanner-complete, no TUI).** |
| 9 | TUI I | ratatui+crossterm scaffolding behind `tui` feature flag; Elm-style App + Message channel; vaults pane + secrets pane; live `/` filter sharing nucleo. |
| 10 | TUI II | Detail pane; on-demand value fetch with debounce; clipboard yank with countdown; cache invalidation `R`; error toast layer. |
| 11 | TUI III | History overlay; audit overlay; help overlay; reserved edit-mode key declarations; backend-capabilities conditional rendering. |
| 12 | Polish | Snapshot tests; smoke tests via `expectrl`; perf pass on large vaults; doc updates; **v0.7.0 ships.** |

### 4.2 Release milestones

- **v0.6.0** (end of week 4): structured errors + env profiles. Day-1 DX wins.
- **v0.6.1** (end of week 5): + `xv find`, `--names-only`.
- **v0.7.0-rc.1** (end of week 8): + scanner + git hook installer. Gather a week of pre-commit feedback before TUI.
- **v0.7.0** (end of week 12): + TUI. Showcase release with coordinated docs + release notes.

### 4.3 Quality gates per release

1. `cargo test` (all green) and `cargo test -- --test-threads=1` (no flake).
2. `cargo clippy --all-targets -- -W clippy::all` with no new warnings against baseline.
3. `cargo audit` (no new advisories).
4. CLI integration smoke tests on Linux + macOS + Windows (existing matrix).
5. Manual smoke checklist for the new feature, captured in `docs/release-checklists/v0.X.md`.

### 4.4 Soft-commitment audit (continuous + final)

Each PR that adds a new manager-method call appends a line to `docs/superpowers/specs/backend-trait-checklist.md`. PR template grows a checkbox: "Have you logged any new read-surface usage in the trait checklist?" The end-of-quarter audit is then a *review*, not a discovery.

The checklist becomes the spec input for phase 2 ("Backend Pluggability Initiative").

### 4.5 Risk register

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Scanner perf unacceptable at vault sizes >200 | Medium | Document config escape hatches; pull disk fingerprint cache forward into v0.7.0 if rc.1 feedback demands it. |
| TUI scope balloons (history/audit overlays slip) | Medium | History + audit overlays are first cut from week 11 if behind; ship in v0.7.1. |
| Env-profile schema discovers a missing field mid-quarter | Low | Schema is `#[serde(default)]` for non-env fields; new fields land additively. |
| Phase-2 trait shape revealed as fundamentally wrong by audit | Low | The audit *is* the safety check; a surprise here is what we want to discover before phase 2 starts. |
| ratatui / nucleo / aho-corasick semver bump mid-quarter | Low | Pin minor versions in `Cargo.toml`; revisit at v0.7.0 release. |

---

## 5. Testing & cross-cutting concerns

### 5.1 Testing per feature

| Feature | Test layers |
|---------|-------------|
| Errors (#8) | Exhaustive `match`-based unit tests on `code()`/`exit_code()` (compile-time enforcement — adding a variant breaks the build until tests update); snapshot tests of error output for each variant in plain + JSON form; integration tests that trigger known errors via the binary and assert exit code & content; suggestion-engine unit tests with fixed candidate lists. |
| Context (#3) | TOML parser unit tests; walk-up traversal tests over tempdir trees with `.xv.toml` at varied depths; `.xv.boundary` stopper test; env-resolution priority chain (`XV_ENV` > `--env` > `default_env`) tested per branch; legacy `.xv/context` fallback test; `xv context init` produces a parseable, walk-up-discoverable file. |
| Find (#1) | Fixed corpus + known queries assert ranking order; `--names-only` capture asserts no headers, no ANSI even on forced-TTY; cross-vault flag opens cache vs. live-API paths via injected fake. |
| Scanner (#4) | Aho-Corasick correctness with known value/file pairs; built-in pattern set hits known sentinel tokens; `.xvignore` suppresses correctly; hook installer idempotency (install twice → one entry); install refuses to overwrite a foreign hook without `--force`; **value-never-leaked invariant**: snapshot test asserts the matched secret value's bytes do not appear anywhere in stdout/stderr/JSON output; perf benchmark scans a 5MB file with a 50-secret vault under 2s on CI. |
| TUI (#2) | Pure reducer tests on App + Message (no rendering); ratatui `TestBackend` snapshot tests of `view()` against expected character grids per pane; `expectrl`-style smoke test exercises the keymap on a real terminal; capability-gating test renders the same App with backend kind = Azure vs. mocked Generic and asserts resource-group field appears/disappears. |

### 5.2 Documentation deliverables

- README updates per feature with one asciicast each (`asciinema`)
- `docs/exit-codes.md` — full table from §3.1
- `docs/scan-patterns.md` — built-in pattern reference + composition guide for gitleaks/trufflehog users
- `docs/tui.md` — keymap reference + screenshots
- `docs/migration-v0.6.md` — legacy `.xv/context` deprecation guidance
- Man-page regeneration in CI (existing pipeline)

### 5.3 Build & distribution

- `tui` feature flag default-off; CI matrix builds `--no-default-features`, `--features file-ops`, and `--all-features` on Linux/macOS/Windows.
- Track release-binary size; budget +2 MB across the quarter (nucleo + aho-corasick + ratatui + crossterm). Alert on PRs that bust the budget.
- Existing release pipeline (homebrew, scoop, deb/rpm where present) — no new packaging.

### 5.4 Security & privacy

- Scanner holds plaintext values: enforce `Zeroizing<String>` end-to-end; CI grep for `let value: String` on values returned from `get_secret` to prevent regressions.
- Section 3.1 JSON error envelope: snapshot tests asserting no variant accidentally serializes a secret value into the `message` field (a `value: String` field on any error variant is a build-time lint failure).
- TUI value reveal: when toggled off, redraw the cell with spaces before swapping back to mask glyphs (helps avoid a frame of plaintext lingering in terminal scrollback) — documented as best-effort.
- Pre-commit hook: never persists staged content; reads from `git diff --cached` stream, scans in memory, drops.

### 5.5 Performance budgets

| Operation | Target |
|-----------|--------|
| Walk-up resolution | <10 ms (stat-bound) |
| `xv find` on 1k-secret vault, warm cache | <100 ms |
| `xv scan --staged` on a 50-secret vault, cold | <3 s |
| `xv scan --staged` on a 50-secret vault, warm in-process | <500 ms |
| TUI initial render after `list_vaults` returns | <300 ms |

Captured as criterion benchmarks; regressions over 20% break CI.

### 5.6 Backwards compatibility

- Existing exit codes for unchanged code paths stay unchanged — the §3.1 table is additive.
- Existing flag surface unchanged. New flags (`--names-only`, `--env`, `--staged`, `--hook`) are additive.
- `.xv/context` legacy file: read continues until v0.8 with deprecation warning starting v0.6.0; `xv context use --local` keeps writing `.xv/context` until v0.7 then writes `.xv.toml` instead.
- Cache schema: bump version key on any change so caches get rebuilt rather than misread.

---

## 6. Success criteria

1. **All five features shipped** in v0.6.0, v0.6.1, v0.7.0-rc.1, v0.7.0 per §4.2.
2. **Marquee user journey works end-to-end:** a developer can `cd` into a project with a `.xv.toml`, run `xv tui`, copy a secret to the clipboard, and run `xv find db` — without ever typing `--vault` or `--group`.
3. **Scanner is in real use:** at least one external user has installed `xv scan install` as a pre-commit hook and reported back (validates the hook installer's safety and the perf budget on real repos).
4. **Soft-commitment audit complete:** `docs/superpowers/specs/backend-trait-checklist.md` exists, enumerates every read-surface call site introduced during the quarter, and is the spec input ready for the phase-2 brainstorming session.
5. **No regressions:** existing test suite green; exit codes for unchanged paths unchanged; existing flag behavior preserved.
6. **Performance budgets met** for all operations in §5.5.
7. **Documentation shipped** per §5.2.

---

## 7. Open questions (punted from brainstorming — to resolve before phase 1 starts)

1. **Versioning policy.** Spec assumes 0.6.0 → 0.6.1 → 0.7.0-rc.1 → 0.7.0. Should phase-1 completion ship as v1.0 instead? Semantic weight of v1.0 might fit better than v0.7.0, since v0.7 is positioned as the "we're done polishing, ready for backend abstraction" milestone.
2. **Distribution channels.** Anything new to land alongside v0.7 — Snap, Nix, AUR, separate TUI-enabled binary path?
3. **Telemetry.** Spec assumes zero telemetry/phone-home throughout. Confirm or revise.
4. **Beta cohort.** Should v0.7.0-rc.1 (end of week 8) ship to a named set of dogfooders, or just publish on the existing release channel and listen?
5. **PR / issue backlog conflicts.** Are there in-flight branches or pinned issues touching `src/cli/secret_ops.rs`, `src/config/context.rs`, `src/error.rs`, `src/main.rs` that would need merge-resolution scheduling?
6. **Solo vs. multi-contributor execution.** Affects §4 sequencing — solo favors strict serial 2a; multiple contributors could parallelize errors + scanner subsystem prep starting week 1.

---

## 8. Phase 2 preview (not in scope, captured for context)

After v0.7.0 ships, phase 2 starts with a separate brainstorming session. Inputs to that session:

- This spec's `docs/superpowers/specs/backend-trait-checklist.md` (read-surface enumeration).
- The reference doc `Pluggable backends.md` (competitive analysis: the gap is real; closest competitor is `aws-vault`).
- Decision on which second backend ships first: local age-encrypted file (fastest path; offline-first appeal; minimal infra surface) vs. AWS Secrets Manager (broader market reach; bigger trait surface stress test) vs. Vault (smallest user base; most reusable trait).

Phase 2 is its own quarterly arc; this spec does not commit to a sequence or scope for it.
