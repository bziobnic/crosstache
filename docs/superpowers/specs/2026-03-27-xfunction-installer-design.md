# xfunction Installer Design Spec

**Date:** 2026-03-27
**Status:** Approved
**Approach:** Hybrid — az CLI commands with Python orchestration

## Overview

A Python-based installer for the xfunction Azure Function that configures all required Azure resources, permissions, and deploys the function code. Designed for open-source users with interactive prompts and sensible defaults, plus a `--non-interactive` flag for automation. Idempotent (safe to re-run) with a teardown mode.

## Requirements

- **Language:** Python 3.10+ (uses `X | None` union syntax)
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
| `--config-file` | Load from JSON file | None |
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
- Check name availability first: `az storage account check-name --name {sa}` — retry with new random suffix if taken
- Tag with `xfunction-installer=true` for idempotent detection

```
az storage account check-name --name {sa}
az storage account create --name {sa} --resource-group {rg} --sku Standard_LRS --tags xfunction-installer=true
```

Idempotent: search for storage accounts in rg tagged with `xfunction-installer=true`.

### 4. Function App (`function_app.py`)

```
az functionapp create \
  --name {fa} --resource-group {rg} --storage-account {sa} \
  --consumption-plan-location {location} \
  --runtime python --runtime-version 3.11 \
  --functions-version 4 --os-type Linux \
  --assign-identity "[system]"
```

Note: `--assign-identity "[system]"` enables the system-assigned managed identity at creation time rather than as a separate step. Python 3.11 is used as the target runtime; Azure Functions v4 supports 3.9-3.12. The `--storage-account` flag automatically configures `AzureWebJobsStorage` with the storage account's connection string.

Post-creation app settings:
```
az functionapp config appsettings set --name {fa} --resource-group {rg} --settings \
  AZURE_TENANT_ID={tenant_id} \
  AZURE_CLIENT_ID={app_registration_client_id} \
  AZURE_CLIENT_SECRET={app_registration_client_secret} \
  FUNCTIONS_WORKER_RUNTIME=python \
  EXPECTED_AUDIENCE={app_registration_client_id}
```

Note: `EXPECTED_AUDIENCE` is set to the App Registration's client ID. This is used by the function's JWT validation to verify the token audience claim. It must match the application ID that tokens are issued for.

Idempotent: `az functionapp show` check; skip creation but update settings if exists.

### 5. App Registration (`app_registration.py`)

```
az ad app create --display-name {app_name}
az ad sp create --id {app_id}
az ad app credential reset --id {app_id} --years 2
```

Post-creation:
- Add Microsoft Graph API permissions (application type):
  - `User.Read.All` (ID: `df021288-bdef-4463-88db-98f22de89214`)
  - `Application.Read.All` (ID: `9a5d68dd-52b0-4cc2-bd40-abcf44ac3a30`)
  ```
  az ad app permission add --id {app_id} \
    --api 00000003-0000-0000-c000-000000000000 \
    --api-permissions df021288-bdef-4463-88db-98f22de89214=Role 9a5d68dd-52b0-4cc2-bd40-abcf44ac3a30=Role
  ```
- Grant admin consent: `az ad app permission admin-consent --id {app_id}`

Note: `00000003-0000-0000-c000-000000000000` is the Microsoft Graph API ID. `User.Read.All` is needed for principal type detection. `Application.Read.All` is needed for service principal lookups.

Idempotent: search by display name; skip if exists; optionally rotate secret.

### 6. RBAC (`rbac.py`)

The xfunction authenticates to Azure using the **App Registration's ClientSecretCredential** (not managed identity). Therefore, RBAC roles must be assigned to the **App Registration's service principal**.

Assign to the App Registration's service principal:

| Role | Scope | Purpose |
|------|-------|---------|
| Role Based Access Control Administrator | Subscription | Create/manage role assignments for vault users |
| Key Vault Administrator | Subscription | Read vault tags (CreatedByID) for creator verification |
| Reader | Subscription | List storage accounts for discovery |

```
# Get the service principal object ID for the App Registration
sp_object_id=$(az ad sp show --id {app_registration_client_id} --query id -o tsv)

az role assignment create \
  --assignee-object-id {sp_object_id} \
  --assignee-principal-type ServicePrincipal \
  --role "Role Based Access Control Administrator" \
  --scope /subscriptions/{sub}

az role assignment create \
  --assignee-object-id {sp_object_id} \
  --assignee-principal-type ServicePrincipal \
  --role "Key Vault Administrator" \
  --scope /subscriptions/{sub}

az role assignment create \
  --assignee-object-id {sp_object_id} \
  --assignee-principal-type ServicePrincipal \
  --role "Reader" \
  --scope /subscriptions/{sub}
```

Note: Using `--assignee-object-id` with `--assignee-principal-type ServicePrincipal` avoids ambiguity and the extra Graph API lookup that `--assignee` triggers.

Idempotent: `az role assignment list --assignee {sp_object_id}` to check existing before creation.

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
  xv set function-app-url --value https://{fa}.azurewebsites.net/api --group xfunction
  ```
- Also update the crosstache config to set `FUNCTION_APP_URL`:
  ```
  xv config set function_app_url https://{fa}.azurewebsites.net/api
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
- Deletes `.xfunction-installer-state.json` on successful teardown
- Note: Event Grid subscriptions are out of scope (the function uses HTTP triggers, not Event Grid). If Event Grid was configured separately, it must be cleaned up manually.

## State Persistence

The installer writes intermediate state to `.xfunction-installer-state.json` in the working directory. This file tracks:
- Which steps have completed
- Resource names and IDs created in each step (storage account name, app registration client ID, managed identity principal ID, etc.)
- The App Registration client secret (encrypted or omitted — see below)

The state file is used by `--resume` to skip completed steps and recover values needed by later steps (e.g., the client secret generated in step 5 is needed by step 4's app settings configuration).

**Security:** The client secret is NOT stored in the state file. If `--resume` is used after step 5 (App Registration) but before step 4 (app settings), the installer prompts for the secret or offers to rotate it. The state file is added to `.gitignore`.

The `status` command reads this state file (and validates against Azure) to show current resource state.

## Error Handling

- Each step wrapped in try/except with clear error messages
- On failure: print what succeeded, what failed, and how to resume
- `--resume` flag: uses state file + existence checks to skip completed steps
- Ctrl+C handler: graceful shutdown, write current state, print summary
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

- **Python 3.10+** (for `X | None` union type syntax)
- No additional Python packages required beyond the standard library

External CLI tools:
- `az` CLI (Azure CLI) — **required**
- `func` CLI (Azure Functions Core Tools) — optional, falls back to az for deployment
- `xv` CLI (crosstache) — optional, for credential storage and config

## Testing Strategy

- Unit tests for `az.py` wrapper (mock subprocess)
- Unit tests for each step's `check_exists` logic
- Integration test: full install → verify → uninstall cycle (requires Azure credentials)
- Tests follow existing xfunction test patterns in `tests/`
