# `xv scan` — Pre-Commit Leak Scanner

`xv scan` matches files against the **actual values** of secrets in
your active vault, plus a small set of built-in patterns (AWS access
keys, GitHub tokens, Stripe keys, Slack tokens, JWTs, SSH/PEM private-key
headers, high-entropy strings).

The unique value: when you accidentally paste your real `DB_PASSWORD`
into a config file, `xv scan` says *"this file contains the value of
secret DB_PASSWORD from vault dev-kv"* — not just "high-entropy string."

## Usage

```bash
xv scan [PATH]...           # scan paths (default: .)
xv scan --staged            # scan only files staged for commit
xv scan --all               # scan the full tracked HEAD tree
xv scan --hook              # quiet on no findings; exit 50 on findings
xv scan --all-vaults        # match against every vault you can list
xv scan --format json       # machine-readable findings on stdout
xv scan install [--force]   # write .git/hooks/pre-commit
xv scan uninstall           # remove the managed hook
```

## Scan modes

| Mode | Reads from | Typical use | Notes |
|------|------------|-------------|-------|
| `xv scan [PATH]...` | Working tree files under the requested paths | Local checks before staging | Honors default excludes, `.xvignore`, and `[scan].exclude`; prints skipped-file warnings outside hook mode. |
| `xv scan --staged` | Git index via `git diff --cached` + `git show :PATH` | Pre-commit hooks | Scans exactly what would be committed, not unstaged edits; honors the same default and `[scan].exclude` globs as path scans (this is a behavior change from earlier releases, which scanned every staged file regardless of excludes). |
| `xv scan --all` | Committed `HEAD` tree via `git ls-tree HEAD` + `git show HEAD:PATH` | CI sweeps of the current revision | Ignores unstaged and staged-but-uncommitted edits; honors the same default and `[scan].exclude` globs as path scans. |

`--staged` and `--all` are mutually exclusive. Use a path scan for working-tree
content, `--staged` for index content, and `--all` for the last committed tree.

`--all-vaults` broadens the secret-value match set by enumerating vaults through
the active backend. It works only when the backend can list vaults; otherwise
the command fails with a capability error instead of silently scanning one
vault.

## Exit codes

- `0` — no findings.
- `50` — at least one finding (`xv-scan-leak-detected`).
- `3` — config error (e.g., not in a git repo for `install`).
- Other codes per the standard families in [`docs/exit-codes.md`](exit-codes.md).

## Output

Plain (default): one finding per line on **stderr**:

```
src/config.js:42:10: matches DB_PASSWORD (kind=SecretValue, severity=Critical, vault=dev-kv)
```

JSON (`--format json`): array of `{file, line, col, secret_name, vault, kind, severity}` on **stdout**.

**Findings never echo the matched value.** That invariant is enforced by a hand-maintained banned-key test against the `Finding` struct's serialized form.

In hook mode, skipped/unreadable files are treated as a failure because an
unscanned file could hide a leak. Outside hook mode, skipped files are reported
on stderr and the command continues.

## `.xv.toml` `[scan]` block

```toml
[scan]
exclude = ["dist/**", "*.lock"]
min_value_length = 12
patterns = ["aws-access-key-id", "github-token", "stripe-secret-key"]
```

`patterns` is an allowlist matched against the built-in pattern names
exactly: `aws-access-key-id`, `github-token`, `stripe-secret-key`,
`slack-token`, `jwt`, `ssh-private-key`, `low-confidence-high-entropy`.
Leave it empty (or omit it) to enable all built-ins. If it is non-empty but
none of the names match a known pattern, `xv scan` fails with a config error
listing the valid names — this is deliberate: a typo'd allowlist must never
silently disable the whole built-in safety net.

## `.xvignore`

Per-repo allowlist using `.gitignore` syntax, scanner-specific:

```
node_modules/
*.snap
test/fixtures/**
```

## Pre-commit hook

```bash
xv scan install
```

Writes `.git/hooks/pre-commit` with an `xv-scan-managed` marker. Re-runs are idempotent. Existing non-managed hooks are refused unless `--force`.

The installed hook is just:

```bash
#!/usr/bin/env bash
# xv-scan-managed
set -e
xv scan --staged --hook
```

## Composition with gitleaks

`xv scan` ships ~7 patterns by design — for broader coverage, layer gitleaks alongside:

```bash
gitleaks protect --staged && xv scan --staged --hook
```

## Performance

Scanner is in-memory and re-fetches values per process. Expect 1–3 s on a 50-secret vault for `--staged`. To speed up:

- `[scan].min_value_length = 12` — skip short values.
- `XV_SCAN_DISABLE=1` (or `=true`, case-insensitive) — bypass entirely (escape hatch for emergencies).
