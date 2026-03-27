# xfunction Installer Design Spec

**Date:** 2026-03-27
**Status:** Approved
**Approach:** Hybrid — az CLI commands with Python orchestration

## Overview

A Python-based installer for the xfunction Azure Function that configures all required Azure resources, permissions, and deploys the function code. Designed for open-source users with interactive prompts and sensible defaults, plus a `--non-interactive` flag for automation. Idempotent (safe to re-run) with a teardown mode.

## Requirements

- **Language:** Python
- **Interaction:** Interactive by default, `--non-interactive` flag for automation
- **Scope:** Full end-to-end (infrastructure + app registration + RBAC + deployment + verification + xv credential storage)
- **Audience:** General public / open-source users
- **Authentication:** Requires `az login` beforehand
- **Idempotency:** Safe to re-run; includes `--uninstall` teardown mode

## Architecture

### Location & Entry Point

```
xfunction/installer/
  __main__.py          # Entry point (python -m installer)
  cli.py               # Argument parsing and orchestration
  az.py                # Azure CLI wrapper
  config.py            # InstallerConfig dataclass + defaults
  steps/
    __init__.py
    prerequisites.py   # Check az CLI, login, extensions
    resource_group.py  # Create/verify resource group
    storage_account.py # Create storage for Functions runtime
    function_app.py    # Create Function App + managed identity
    app_registration.py # App Registration + secret + Graph perms
    rbac.py            # Role assignments for managed identity
    deployment.py      # Deploy function code
    verification.py    # Health check and smoke test
    teardown.py        # Reverse all steps
  utils/
    __init__.py
    prompts.py         # Interactive prompts with defaults
    output.py          # Colored output, progress, verbose logging
```

### Execution Flow

```
prerequisites → resource_group → storage_account → function_app →
app_registration → rbac → deployment → verification
```

Each step module exports:
- `run(config, az_client) → dict` — Execute the step
- `check_exists(config, az_client) → bool` — For idempotency
- `teardown(config, az_client) → None` — Reverse the step

## CLI Interface

### Commands

```
python -m installer install [options]     # Setup everything
python -m installer uninstall [options]   # Tear down
python -m installer status [options]      # Show resource state
python -m installer verify [options]      # Health check only
```

### Install Options

| Flag | Description | Default |
|------|-------------|---------|
| `--subscription-id` | Azure subscription | Prompted |
| `--resource-group` | Resource group name | `rg-xfunction` |
| `--location` | Azure region | `eastus` |
| `--function-app-name` | Function App name | `fa-xfunction` |
| `--storage-account` | Storage account name | Auto-generated |
| `--non-interactive` | No prompts | `false` |
| `--verbose` | Print az commands | `false` |
| `--skip-deploy` | Infrastructure only | `false` |
| `--config-file` | Load from JSON/YAML | None |
| `--resume` | Skip completed steps | `false` |
| `--output` | Output format (text/json) | `text` |

### Uninstall Options

| Flag | Description | Default |
|------|-------------|---------|
| `--keep-resource-group` | Don't delete resource group | `false` |

## Configuration Dataclass

```python
@dataclass
class InstallerConfig:
    subscription_id: str
    resource_group: str = "rg-xfunction"
    location: str = "eastus"
    function_app_name: str = "fa-xfunction"
    storage_account: str = ""        # auto-generated if empty
    app_name: str = "xfunction-rbac" # App Registration display name
    non_interactive: bool = False
    verbose: bool = False
    skip_deploy: bool = False
    output_format: str = "text"      # "text" or "json"
```

In interactive mode, each value is prompted with the default shown in brackets. Enter accepts the default.

## Azure CLI Wrapper (az.py)

### AzCli Class

```python
class AzCli:
    def run(self, *args) -> dict           # Execute, parse JSON, raise on error
    def run_or_none(self, *args) -> dict | None  # Returns None on not-found
    def check_login(self) -> bool
    def get_subscription(self) -> str
    def get_tenant_id(self) -> str
```

### Error Hierarchy

- `AzCliError` — Base exception (includes command, stderr, return code)
- `AzNotFoundError` — Resource not found
- `AzAuthError` — Authentication failure

All error output redacts `--password` and `--secret` flag values.

Verbose mode prints every az command before execution (with secrets redacted).

Default timeout: 120 seconds per command.

## Step Details

### 1. Prerequisites (`prerequisites.py`)

- Verify `az` CLI installed (>= 2.50)
- Verify logged in (`az account show`)
- Verify Azure Functions Core Tools installed (`func --version`)
- Check required az extensions (`resource-graph`)
- Display subscription/tenant info and confirm with user

### 2. Resource Group (`resource_group.py`)

```
az group create --name {rg} --location {location}
```

Idempotent: `az group exists` check before creation.

### 3. Storage Account (`storage_account.py`)

- Auto-generate globally unique name: `xfunc{random8chars}` (lowercase alphanumeric, 3-24 chars)
- Tag with `xfunction-installer=true` for idempotent detection

```
az storage account create --name {sa} --resource-group {rg} --sku Standard_LRS --tags xfunction-installer=true
```

Idempotent: search for storage accounts in rg tagged with `xfunction-installer=true`.

### 4. Function App (`function_app.py`)

```
az functionapp create \
  --name {fa} --resource-group {rg} --storage-account {sa} \
  --consumption-plan-location {location} \
  --runtime python --runtime-version 3.11 \
  --functions-version 4 --os-type Linux
```

Post-creation:
- Enable system-assigned managed identity
- Set app settings: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `FUNCTIONS_WORKER_RUNTIME=python`

Idempotent: `az functionapp show` check; skip creation but update settings if exists.

### 5. App Registration (`app_registration.py`)

```
az ad app create --display-name {app_name}
az ad sp create --id {app_id}
az ad app credential reset --id {app_id} --years 2
```

Post-creation:
- Add Microsoft Graph API permission: `User.Read.All` (application type)
- Grant admin consent: `az ad app permission admin-consent --id {app_id}`

Idempotent: search by display name; skip if exists; optionally rotate secret.

### 6. RBAC (`rbac.py`)

Assign to Function App's managed identity:

| Role | Scope |
|------|-------|
| RBAC Administrator | Subscription |
| Key Vault Administrator | Subscription |

```
az role assignment create \
  --assignee {managed_identity_principal_id} \
  --role "Role Based Access Control Administrator" \
  --scope /subscriptions/{sub}
```

Idempotent: `az role assignment list` to check existing before creation.

### 7. Deployment (`deployment.py`)

```
func azure functionapp publish {fa}
```

Falls back to zip deployment if func CLI unavailable:
```
az functionapp deployment source config-zip --resource-group {rg} --name {fa} --src {zip_path}
```

### 8. Verification (`verification.py`)

- Poll function list with backoff (max 60s) until functions are registered
- Call health/status endpoint if available
- Print summary of all created resources

### 9. Credential Storage

- After App Registration creation, offer to store credentials in xv:
  ```
  xv set azure-tenant-id --value {tenant_id} --group xfunction
  xv set azure-client-id --value {client_id} --group xfunction
  xv set azure-client-secret --value {client_secret} --group xfunction
  ```
- Check if `xv` is available; skip with info message if not installed

## Teardown (`teardown.py`)

Reverse order:
1. Remove role assignments
2. Delete App Registration (and service principal)
3. Delete Function App
4. Delete Storage Account
5. Delete Resource Group (unless `--keep-resource-group`)

- Each step checks resource existence before deletion
- Confirmation prompt listing all resources to be deleted
- `--non-interactive` skips confirmation (for CI/CD)

## Error Handling

- Each step wrapped in try/except with clear error messages
- On failure: print what succeeded, what failed, and how to resume
- `--resume` flag: detect completed steps via existence checks, skip them
- Ctrl+C handler: graceful shutdown with state summary
- Non-zero exit code on any failure

## Output

### Progress Indicators

```
[1/8] Checking prerequisites...
  ✓ Azure CLI v2.58.0
  ✓ Logged in as user@example.com
  ✓ Subscription: My Subscription (abc-123)
  ✓ Functions Core Tools v4.0.5

[2/8] Creating resource group...
  ✓ Resource group 'rg-xfunction' created in eastus

...
```

### Final Summary Table

```
Resource          | Name              | Status
──────────────────|───────────────────|────────
Resource Group    | rg-xfunction      | Created
Storage Account   | xfuncab12cd34     | Created
Function App      | fa-xfunction      | Deployed
App Registration  | xfunction-rbac    | Created
Managed Identity  | (system-assigned) | Configured
RBAC Assignments  | 2 roles           | Assigned
```

### Credential Output

Print client ID and secret with reminder to store securely. Offer xv storage integration.

### Machine-Readable Output

`--output json` produces structured JSON for all commands.

## Dependencies

No additional Python packages required beyond the standard library. The installer relies entirely on:
- `az` CLI (Azure CLI)
- `func` CLI (Azure Functions Core Tools) — optional, falls back to az for deployment
- `xv` CLI (crosstache) — optional, for credential storage

## Testing Strategy

- Unit tests for `az.py` wrapper (mock subprocess)
- Unit tests for each step's `check_exists` logic
- Integration test: full install → verify → uninstall cycle (requires Azure credentials)
- Tests follow existing xfunction test patterns in `tests/`
