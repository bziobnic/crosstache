# Key Vault RBAC Processor - PowerShell Scripts

This directory contains PowerShell scripts to set up and deploy the Azure Function that automatically assigns RBAC permissions to Key Vault creators.

## Prerequisites

- Azure subscription
- Azure CLI installed and logged in
- Azure Functions Core Tools (if developing locally)
- PowerShell 5.1 or higher
- Python 3.8+

## Available Scripts

### 1. setup-managed-identity.ps1

This script creates the Azure resources and configures the Managed Identity for the function app:

- Creates a resource group (if it doesn't exist)
- Creates a storage account
- Creates a function app with system-assigned managed identity
- Assigns necessary RBAC permissions to the managed identity
- Creates an Event Grid subscription to trigger the function

To run:

```powershell
.\setup-managed-identity.ps1
```

### 2. deploy-function.ps1

This script deploys the function code to the Azure Function App:

- Installs required Python packages
- Deploys the function code to Azure
- Verifies the deployment

To run:

```powershell
.\deploy-function.ps1
```

### 3. test-function.ps1

This script tests the function by creating a Key Vault with RBAC authorization enabled:

- Creates a new Key Vault with a unique name
- Enables RBAC authorization on the vault
- Waits for the function to process
- Checks the role assignments on the vault

To run:

```powershell
.\test-function.ps1
```

## Usage Workflow

1. First, customize the variables at the top of each script with your own values
2. Run `setup-managed-identity.ps1` to create all Azure resources
3. Run `deploy-function.ps1` to deploy the function code
4. Run `test-function.ps1` to test the function by creating a Key Vault

## Monitoring

To view the function logs after deployment:

```powershell
az functionapp logs tail --name fa-user-keyvault --resource-group Vaults
```

## Cleanup

To clean up all resources when you're done testing:

```powershell
az group delete --name Vaults --yes
``` 