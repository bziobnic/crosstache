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
xv scan --hook              # quiet on no findings; exit 50 on findings
xv scan --all-vaults        # match against every vault you can list
xv scan install [--force]   # write .git/hooks/pre-commit
xv scan uninstall           # remove the managed hook
```

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

## Memory hygiene

Fetched secret values are stored as `Zeroizing<String>` in scanner input refs, so
they are wiped when those refs are dropped. During a scan, the match engine also
copies plaintext needles into its Aho-Corasick automaton; those copies live only
for the lifetime of the `MatchEngine`, which is built per scan and dropped
after results are rendered. Scanner output and serialized findings still contain
only location metadata plus the secret name, never the matched value.

## `.xv.toml` `[scan]` block

```toml
[scan]
exclude = ["dist/**", "*.lock"]
min_value_length = 12
patterns = ["aws", "github", "stripe"]
```

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
- `XV_SCAN_DISABLE=1` — bypass entirely (escape hatch for emergencies).
