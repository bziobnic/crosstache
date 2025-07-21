# Key Vault RBAC Processor

This Azure Function automatically assigns RBAC permissions to Key Vault creators. When a new Key Vault with RBAC-authorization enabled is created, this function:

1. Creates a custom "Vault Role Manager" role with permissions to manage role assignments on the vault
2. Assigns this role to the user or service principal that created the vault

## Project Structure

This project uses the Azure Functions v4 Python programming model:

```
vaultserver/
├── function_app.py           # Main function app file with the event grid trigger
├── VaultRbacProcessor/       # Python package containing function code
│   ├── __init__.py           # Package initialization
│   └── vault_role_manager.py # Class to handle RBAC operations
├── host.json                 # Host configuration
├── local.settings.json       # Local settings (not checked into source control)
└── requirements.txt          # Python dependencies
```

## Prerequisites

- Azure subscription
- Azure CLI installed and logged in
- Azure Functions Core Tools (if developing locally)
- Python 3.8+

## Deployment

### Option 1: Using PowerShell Scripts

Three PowerShell scripts are provided to simplify deployment:

1. `setup-managed-identity.ps1` - Creates all required Azure resources and configures permissions
2. `deploy-function.ps1` - Deploys the function code to Azure
3. `test-function.ps1` - Tests the function by creating a Key Vault

See `README-POWERSHELL.md` for detailed instructions.

### Option 2: Manual Deployment

#### 1. Create Resource Group (if needed)

```bash
az group create --name <resourceGroupName> --location <location>
```

#### 2. Create a Storage Account for the Function

```bash
az storage account create --name <storageAccountName> --location <location> --resource-group <resourceGroupName> --sku Standard_LRS
```

#### 3. Create the Function App with Managed Identity

```bash
az functionapp create \
  --name <functionAppName> \
  --storage-account <storageAccountName> \
  --consumption-plan-location <location> \
  --resource-group <resourceGroupName> \
  --functions-version 4 \
  --os-type Linux \
  --runtime python \
  --runtime-version 3.9 \
  --assign-identity [system]
```

#### 4. Assign Role Permissions to the Managed Identity

```bash
# Get the principal ID of the function app's managed identity
principalId=$(az functionapp identity show --name <functionAppName> --resource-group <resourceGroupName> --query principalId -o tsv)

# Assign the built-in Role Management role 
az role assignment create \
  --assignee $principalId \
  --role "Role Based Access Control Administrator" \
  --scope /subscriptions/<subscriptionId>

# Also assign Key Vault Administrator role
az role assignment create \
  --assignee $principalId \
  --role "Key Vault Administrator" \
  --scope /subscriptions/<subscriptionId>
```

#### 5. Deploy the Function Code

```bash
cd vaultserver
func azure functionapp publish <functionAppName>
```

#### 6. Set Up Event Grid Subscription

```bash
az eventgrid event-subscription create \
  --name "KeyVaultCreationEvents" \
  --source-resource-id /subscriptions/<subscriptionId> \
  --endpoint-type azurefunction \
  --endpoint /subscriptions/<subscriptionId>/resourceGroups/<resourceGroupName>/providers/Microsoft.Web/sites/<functionAppName>/functions/VaultRbacProcessor \
  --included-event-types "Microsoft.Resources.ResourceWriteSuccess" \
  --advanced-filter data.operationName StringContains "Microsoft.KeyVault/vaults/write"
```

## Local Development

1. Clone the repository
2. Navigate to the `vaultserver` directory
3. Create a virtual environment and install dependencies:

```bash
python -m venv .venv
source .venv/bin/activate  # On Windows: .venv\Scripts\activate
pip install -r requirements.txt
```

4. Update local.settings.json with your Azure credentials
5. Run the function locally:

```bash
func start
```

## Testing

To test the function:

1. Create a new Key Vault with RBAC authorization enabled:

```bash
az keyvault create \
  --name <vaultName> \
  --resource-group <resourceGroupName> \
  --location <location> \
  --enable-rbac-authorization true
```

2. Check the function logs to verify that the role assignment was created
3. Verify the role assignment was created on the Key Vault:

```bash
az role assignment list --scope /subscriptions/<subscriptionId>/resourceGroups/<resourceGroupName>/providers/Microsoft.KeyVault/vaults/<vaultName>
``` 