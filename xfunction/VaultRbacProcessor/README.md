# Key Vault RBAC Processor

This Azure Function assigns vault-scoped roles to an authenticated vault creator. A mutable `CreatedByID` tag is only a secondary check: the caller must also already hold Owner, Role Based Access Control Administrator, or User Access Administrator authority at the vault, resource-group, or subscription scope.

The supported deployment path is the Python installer from the `xfunction` directory:

```bash
python -m installer install
```

The installer creates a fresh app registration, transfers its client secret through a private temporary settings file, and grants the service principal only Reader plus conditioned Role Based Access Control Administrator at the installer resource-group scope. It does not grant Microsoft Graph permissions or create an unused managed identity.

Required Function App settings are:

- `AZURE_TENANT_ID`
- `AZURE_CLIENT_ID`
- `AZURE_CLIENT_SECRET`
- `EXPECTED_AUDIENCE`
- `ALLOWED_RESOURCE_GROUP_ID`

Storage role propagation is disabled unless an administrator supplies `VAULT_STORAGE_BINDINGS`. This setting is a JSON object mapping an exact Key Vault resource ID to exact storage-account resource IDs in the same configured resource group, for example:

```json
{
  "/subscriptions/SUB/resourceGroups/RG/providers/Microsoft.KeyVault/vaults/VAULT": [
    "/subscriptions/SUB/resourceGroups/RG/providers/Microsoft.Storage/storageAccounts/ACCOUNT"
  ]
}
```

Names, naming conventions, and mutable resource tags are never used to discover storage accounts.

For local development:

```bash
python -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
func start
```

Run unit tests with:

```bash
.venv/bin/python tests/run_tests.py
```
