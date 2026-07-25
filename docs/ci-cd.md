# CI/CD integration

crosstache ships a first-party GitHub Action and can authenticate to Azure
directly from a workflow's OIDC token — no client secret stored anywhere, and no
separate `azure/login` step.

- [Quick start](#quick-start)
- [Action inputs and outputs](#action-inputs-and-outputs)
- [OIDC setup (Azure)](#oidc-setup-azure)
- [What the install step verifies](#what-the-install-step-verifies)
- [Other platforms](#other-platforms)
- [Rotation gates in CI](#rotation-gates-in-ci)

---

## Quick start

```yaml
jobs:
  deploy:
    runs-on: ubuntu-latest
    permissions:
      id-token: write        # required for OIDC
      contents: read
    steps:
      - uses: bziobnic/crosstache@v1
        with:
          version: v0.28.0                       # pin for reproducible builds
          vault: myproj-prod-kv
          client-id: ${{ vars.AZURE_CLIENT_ID }}
          tenant-id: ${{ vars.AZURE_TENANT_ID }}
          secrets: |
            DEPLOY_TOKEN=deploy-token
            DATABASE_URL=prod-database-url

      - run: ./scripts/deploy.sh     # $DEPLOY_TOKEN and $DATABASE_URL are set
```

`client-id` and `tenant-id` are identifiers, not secrets — `vars` is the right
home for them, though `secrets` works too.

Install the CLI without reading anything by omitting `secrets`:

```yaml
      - uses: bziobnic/crosstache@v1
        with:
          client-id: ${{ vars.AZURE_CLIENT_ID }}
          tenant-id: ${{ vars.AZURE_TENANT_ID }}
      - run: xv ls --format json | jq -r '.[].name'
```

---

## Action inputs and outputs

| Input | Default | Description |
|-------|---------|-------------|
| `version` | `latest` | Release tag (`v0.28.0`) or `latest`. **Pin it** — `latest` changes over time. |
| `backend` | `azure` | `azure`, `aws`, or `local`. |
| `vault` | — | Vault to read from. |
| `secrets` | — | `ENV_NAME=secret-name` per line. Each value is masked and exported to `GITHUB_ENV`. |
| `auth` | `oidc` | `oidc`, `default`, or `none`. See below. |
| `client-id` | — | Azure app registration ID. Required for `auth: oidc`. |
| `tenant-id` | — | Azure tenant ID. Required for `auth: oidc`. |
| `subscription-id` | — | Azure subscription ID. |
| `verify-signature` | `false` | Also verify the release's minisign signature. Requires `minisign` on `PATH`. |

| Output | Description |
|--------|-------------|
| `version` | The release tag that was installed. |
| `path` | Absolute path to the `xv` binary. |

### `auth` modes

- **`oidc`** — federates this job's GitHub OIDC token into Azure AD. Nothing is
  stored; the token is minted per job and expires in minutes. Azure only.
- **`default`** — uses the standard credential chain, which picks up whatever a
  prior step configured (`azure/login`, `aws-actions/configure-aws-credentials`,
  a managed identity on a self-hosted runner). Use this for AWS.
- **`none`** — installs the CLI and configures nothing. Useful for the local
  backend, or when a later step handles auth itself.

### Secrets land in `GITHUB_ENV`

Fetched values become environment variables for **every later step in the job**.
That is the point, but it is worth stating plainly: any later step, including a
third-party action, can read them. Fetch only what the job needs, and put the
`uses:` for this action as late in the job as you can.

Values are registered with `::add-mask::` before anything else logs, line by line
for multi-line values (the runner matches masks per line, so a PEM key registered
as one blob would still leak line by line). Masking is best-effort by nature — it
redacts values it recognizes in log output; it cannot stop a step that
deliberately exfiltrates them.

---

## OIDC setup (Azure)

One-time setup on the app registration. Replace `OWNER/REPO` and the branch.

```bash
# 1. Create an app registration (or reuse one) and note its appId + tenant.
az ad app create --display-name crosstache-ci
APP_ID=$(az ad app list --display-name crosstache-ci --query '[0].appId' -o tsv)

# 2. Add a federated credential for the workflow that should be trusted.
az ad app federated-credential create --id "$APP_ID" --parameters '{
  "name": "github-main",
  "issuer": "https://token.actions.githubusercontent.com",
  "subject": "repo:OWNER/REPO:ref:refs/heads/main",
  "audiences": ["api://AzureADTokenExchange"]
}'

# 3. Give its service principal read access to the vault.
az role assignment create \
  --assignee "$APP_ID" \
  --role "Key Vault Secrets User" \
  --scope "/subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.KeyVault/vaults/<vault>"
```

The `subject` is what pins *which* workflow can authenticate. Scope it as
narrowly as the job allows:

| Trust | `subject` |
|-------|-----------|
| A branch | `repo:OWNER/REPO:ref:refs/heads/main` |
| A tag | `repo:OWNER/REPO:ref:refs/tags/v1.0.0` |
| An environment | `repo:OWNER/REPO:environment:production` |
| Any PR | `repo:OWNER/REPO:pull_request` |

Prefer an **environment** subject for anything that touches production: GitHub
environments support required reviewers, so a fork PR cannot reach the
credential. A bare `repo:OWNER/REPO:pull_request` subject trusts every pull
request that can trigger the workflow.

### Using OIDC without the Action

The credential works from any OIDC-capable runner, not just through the Action:

```bash
export AZURE_CREDENTIAL_PRIORITY=oidc
export AZURE_CLIENT_ID=<app-id>
export AZURE_TENANT_ID=<tenant-id>
xv get DEPLOY_TOKEN --raw
```

`xv` reads GitHub's `ACTIONS_ID_TOKEN_REQUEST_URL` / `ACTIONS_ID_TOKEN_REQUEST_TOKEN`
pair, requests an ID token for the `api://AzureADTokenExchange` audience, and
presents it to Azure AD as a `client_assertion`. With
`AZURE_CREDENTIAL_PRIORITY` unset, OIDC is tried first automatically *when those
variables are present*, then the normal chain.

Set it explicitly (`oidc`) when you want a hard failure rather than a silent
fallback to some other identity — worth doing in production pipelines, where
authenticating as the wrong principal is worse than not authenticating.

### Troubleshooting

| Symptom | Cause |
|---------|-------|
| `ACTIONS_ID_TOKEN_REQUEST_URL is not set` | The job lacks `permissions: id-token: write`. |
| `AADSTS700213: No matching federated identity record found` | The `subject` on the federated credential does not match this workflow's ref/environment. |
| `AADSTS700016: Application not found` | Wrong `client-id`, or the app lives in a different tenant. |
| `Forbidden` / `403` from Key Vault | Federation worked; the service principal has no RBAC role on the vault. |

The first three come from Azure AD and are reported verbatim, minus the trace
IDs. The fourth means auth succeeded — fix the role assignment, not the
credential.

---

## What the install step verifies

The action always verifies the release archive's **SHA-256** against the
`.sha256` published beside it, and refuses to install on a mismatch.

Be clear about what that does and does not prove:

- ✅ Catches a corrupted or truncated download.
- ✅ Catches an archive swapped without also swapping the digest.
- ❌ Does **not** prove authorship. Both files come from the same release, so an
  attacker who can publish releases can publish a matching digest.

For authorship, set `verify-signature: true`. It verifies the `.minisig` against
the same minisign public key embedded in `xv upgrade`, which is held outside the
release pipeline. It needs `minisign` on `PATH`:

```yaml
      - run: sudo apt-get install -y minisign
      - uses: bziobnic/crosstache@v1
        with:
          verify-signature: true
          version: v0.28.0
```

Independently of both: **pin the action to a commit SHA** (`uses:
bziobnic/crosstache@<sha>`) if you want the action's own code to be immutable.
A tag like `@v1` can be moved.

---

## Other platforms

There is no first-party GitLab component or CircleCI orb yet. Both work fine
with a plain install step:

```yaml
# GitLab CI
leak_scan:
  stage: test
  before_script:
    - curl -fsSL https://github.com/bziobnic/crosstache/releases/download/v0.28.0/xv-linux-x64.tar.gz | tar xz
    - install -m755 xv /usr/local/bin/xv
  script:
    - xv scan --hook          # exit 50 on findings
```

Published archives: `xv-linux-x64.tar.gz`, `xv-macos-intel.tar.gz`,
`xv-macos-apple-silicon.tar.gz`, `xv-windows-x64.zip`, each with `.sha256` and
`.minisig`.

Exit codes are stable and documented in [`exit-codes.md`](exit-codes.md) — that
is the contract to script against.

---

## Rotation gates in CI

`xv rotate --check` reports rotation status and exits **51** when any secret is
due, which makes it a drop-in staleness gate:

```yaml
  secret-freshness:
    runs-on: ubuntu-latest
    permissions:
      id-token: write
      contents: read
    steps:
      - uses: bziobnic/crosstache@v1
        with:
          vault: myproj-prod-kv
          client-id: ${{ vars.AZURE_CLIENT_ID }}
          tenant-id: ${{ vars.AZURE_TENANT_ID }}
      - run: xv rotate --check
```

And a scheduled workflow can perform the rotation itself:

```yaml
on:
  schedule:
    - cron: '0 3 * * *'

jobs:
  rotate:
    runs-on: ubuntu-latest
    permissions:
      id-token: write
      contents: read
    steps:
      - uses: bziobnic/crosstache@v1
        with:
          vault: myproj-prod-kv
          client-id: ${{ vars.AZURE_CLIENT_ID }}
          tenant-id: ${{ vars.AZURE_TENANT_ID }}
      - run: xv rotate --due --force
```

For host-driven rather than CI-driven rotation, `xv schedule install` manages the
equivalent job in the machine's own scheduler (launchd / systemd user timer / Task
Scheduler) — see [`rotation.md`](rotation.md#xv-schedule--run-the-sweep-automatically).
Use CI when the credential belongs to a pipeline and the runner can authenticate
non-interactively; use `xv schedule` when rotation belongs to a particular host.

Either way, note that a rotating credential an app read at startup still needs a
restart or redeploy to pick up the new value — rotation alone is not rollout.
