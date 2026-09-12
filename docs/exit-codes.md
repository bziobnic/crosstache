# Exit Codes

`xv` exits with a documented code per error family. Codes are stable across
releases — they are part of the scripting contract.

## Table

| Code  | Family                | Examples                                        |
|-------|-----------------------|-------------------------------------------------|
| `0`   | Success               | command completed                               |
| `1`   | Unknown / catch-all   | unrecoverable I/O, JSON parse, regex, etc.      |
| `2`   | Invalid argument / attachment integrity | bad CLI flag; clap parse failure; typed `xv-attachment-*` failures (see [attachment errors](attachments.md#structured-attachment-errors)) |
| `3`   | Configuration error   | missing required config; invalid config file; env not defined in `.xv.toml`; backend unavailable (`xv-backend-unavailable`); `xv doctor` unresolved problems; a rotation schedule's recorded target drifted or could not be read (`xv schedule status`/`schedule run`, see [rotation.md](rotation.md#exit-codes)) |
| `10`  | Secret not found      | `xv get` on a missing secret                    |
| `11`  | Vault not found       | `xv vault info` on a missing vault              |
| `12`  | Invalid secret name   | name fails sanitization rules                   |
| `13`  | Ambiguous secret      | unqualified `get` matched the same name in ≥2 attached workspace vaults (`xv-ambiguous-secret`); qualify with `alias:name` |
| `20`  | Authentication failed | bad token, expired credential, no Azure login   |
| `21`  | Permission denied     | RBAC check failed                               |
| `30`  | Network error         | generic transport failure                       |
| `31`  | DNS resolution failed | vault hostname did not resolve                  |
| `32`  | Connection timeout    | TCP connect or request timeout                  |
| `33`  | Connection refused    | TCP refused                                     |
| `34`  | SSL/TLS error         | certificate or handshake failure                |
| `35`  | Invalid URL           | malformed URL passed to a network call          |
| `40`  | Azure API error       | Azure returned an error response                |
| `43`  | Rename incomplete     | rename created the new secret but failed to delete the original; both copies still exist (`xv-rename-incomplete`) |
| `50`  | Scan: leak detected   | `xv scan` found a finding (file with a secret value or pattern match) |
| `51`  | Rotation due          | `xv rotate --check` found at least one secret past its rotation policy (`xv-rotation-due`) |
| `52`  | Audit chain broken    | `xv audit --verify` found the local audit log's hash chain altered (`xv-audit-chain-broken`) |

## Error codes

Every error also has a stable kebab-case code (e.g. `xv-vault-not-found`,
`xv-network-dns`). Use these for scripting:

```bash
if ! out=$(xv get DB_PASSWORD --format json 2>/dev/null); then
  code=$(echo "$out" | jq -r '.error.code')
  case "$code" in
    xv-secret-not-found) echo "secret missing — provisioning…" ;;
    xv-permission-denied) echo "access denied — escalate" ;;
    *) echo "unexpected: $code" ; exit 1 ;;
  esac
fi
```

`xv doctor` uses this family for unresolved problems (syntax, schema, or
backend semantics it will not guess). It never emits `--format json|yaml`;
those flags are rejected with exit `2` before diagnosis. See
[`doctor.md`](doctor.md).

For env-resolution failures specifically:

```bash
xv get DB_PASSWORD --env staging
# error[xv-env-not-defined]: Environment 'staging' not defined in .xv.toml; available: dev, prod
# exit 3
```

## JSON error envelope

When `--format json` or `--format yaml` is in effect, errors render to
**stdout** (not stderr) as a structured envelope:

```json
{
  "error": {
    "code": "xv-vault-not-found",
    "message": "Vault not found: myproj-prood",
    "exit_code": 11,
    "suggestion": "myproj-prod"
  }
}
```

`suggestion` is omitted when no near-match was found. The rendered
plain-text form for non-JSON outputs is:

```text
error[xv-vault-not-found]: Vault not found: myproj-prood
  did you mean: myproj-prod?
  hint: Run 'xv vault list' to see available vaults.
```

The `hint` line is TTY-only.

## Machine mode: exactly one document on stdout

A run is in **machine mode** when `--format` is given explicitly and resolves
to `json`, `yaml`, or `csv`. In machine mode **stdout never holds more than one
document**: commands that produce a result document emit exactly one, and
commands that produce none — including, for example, single
`set`/`update`/`delete`, single `file upload`, single `file download`,
`attach`, `detach`, `vault create`, `audit --verify`, `schedule run`, group
`delete`, `rotate NAME`, `inject`, `vault export --output` — leave stdout
empty. See
[ROADMAP.md](../ROADMAP.md) for that zero-document class and the other known
gaps. Nothing else is ever written to stdout:

- **Success:** the command's data document, in the requested format.
- **Failure before any result:** the error envelope above, unchanged.
- **Failure after the command produced a structured result** (a partial batch,
  scan findings, secrets due for rotation): the same envelope plus an additive
  `report` key holding that result.

Human status text — plan banners, per-item `[ok]`/`[error]` lines, batch
summaries, confirmations, advisories — always goes to stderr and never to
stdout, and is suppressed entirely in machine mode when a `report` replaces it.
Every exit code is unchanged by this contract.

### The additive `report` key

`xv scan` finds a leak, exits `50`, and its findings travel inside the one
envelope instead of being printed as a second document:

```console
$ xv scan --format json; echo "exit=$?"
{"error":{"code":"xv-scan-leak-detected","message":"Scan detected 1 potential leak(s)","exit_code":50},"report":[{"file":"./leak.txt","line":1,"col":5,"kind":"pattern","severity":"high"}]}
exit=50
```

Scripts that read `.error.code` are unaffected; `.report` is purely additive.
`xv rotate --check` attaches its due rows the same way under `xv-rotation-due`
(exit `51`), and batch commands attach an item report:

```json
{ "summary": { "total": 2, "succeeded": 1, "skipped": 0, "failed": 1 },
  "items": [ { "name": "GOOD", "status": "ok" },
             { "name": "xv-attachment-key", "status": "failed",
               "error": "reserved name" } ] }
```

### CSV

CSV cannot carry an error object, so in `--format csv`:

- stdout holds **only rows** — the header plus whatever rows the command
  produced before failing, possibly none at all;
- the error is rendered as plain text on **stderr**
  (`error[xv-scan-leak-detected]: …`);
- therefore **check the exit code**, not stdout, to detect failure. A
  successful-looking header row is not proof the run succeeded.

A document that is not row-shaped has no CSV form, so it degrades to a
single-column fallback: a `report` header and one cell holding the document's
JSON text, quoted per RFC 4180. `version`, `copy`, `move`, and `file sync` all
take this path — their documents are single objects, not lists of records:

```console
$ xv file sync --dry-run --format csv
report
"{""uploaded"":0,""downloaded"":0,""deleted"":0,""skipped"":3,""dry_run"":true}"
```

An `ItemReport` renders as `name,status,detail,error` rows, and a flat array of
objects renders as the union of its keys, in a deterministic order (the widest
object's own key order, with any keys it lacks appended sorted) so the header
does not shift across runs depending on which row happens to come first; only
those two shapes produce real columns. An empty array renders as completely
empty stdout — no header, no newline — rather than a lone blank line.

### What machine mode does not change

- `--format auto` piped to a non-TTY behaves exactly as before (JSON body,
  plain-text error on stderr). It is not machine mode.
- Raw values (`--raw`), `--names-only`, `completion`, `schedule install
  --print`, and `run` passthrough are never machine documents.
- `xv doctor` still refuses `--format json|yaml` with exit `2` before
  diagnosis, so its human report can never mix into an envelope.
