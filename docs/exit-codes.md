# Exit Codes

`xv` exits with a documented code per error family. Codes are stable across
releases — they are part of the scripting contract.

## Table

| Code  | Family                | Examples                                        |
|-------|-----------------------|-------------------------------------------------|
| `0`   | Success               | command completed                               |
| `1`   | Unknown / catch-all   | unrecoverable I/O, JSON parse, regex, etc.      |
| `2`   | Invalid argument      | bad CLI flag; clap parse failure                |
| `3`   | Configuration error   | missing required config; invalid config file; env not defined in `.xv.toml` |
| `10`  | Secret not found      | `xv get` on a missing secret                    |
| `11`  | Vault not found       | `xv vault info` on a missing vault              |
| `12`  | Invalid secret name   | name fails sanitization rules                   |
| `20`  | Authentication failed | bad token, expired credential, no Azure login   |
| `21`  | Permission denied     | RBAC check failed                               |
| `30`  | Network error         | generic transport failure                       |
| `31`  | DNS resolution failed | vault hostname did not resolve                  |
| `32`  | Connection timeout    | TCP connect or request timeout                  |
| `33`  | Connection refused    | TCP refused                                     |
| `34`  | SSL/TLS error         | certificate or handshake failure                |
| `35`  | Invalid URL           | malformed URL passed to a network call          |
| `40`  | Azure API error       | Azure returned an error response                |
| `50`  | Scan: leak detected   | `xv scan` found a finding (file with a secret value or pattern match) |

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
