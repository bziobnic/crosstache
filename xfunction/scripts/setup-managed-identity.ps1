# Set these variables before running the script
$SubscriptionId = "250d9a86-64a4-457e-a34e-fb2898eda332"
$ResourceGroup = "Vaults"
$Location = "eastus2"  # e.g., eastus
$StorageAccountName = "sauserkeyvault"  # Must be globally unique
$FunctionAppName = "fa-user-keyvault"  # Must be globally unique

# Check if Azure CLI is installed
try {
    $null = Get-Command az -ErrorAction Stop
}
catch {
    Write-Error "Azure CLI is not installed. Please install it first."
    exit 1
}

# Ensure user is logged in
try {
    $null = az account show
}
catch {
    Write-Error "Not logged in to Azure. Please run 'az login' first."
    exit 1
}

Write-Host "Setting subscription context..."
az account set --subscription $SubscriptionId

# Create resource group if it doesn't exist
try {
    $null = az group show --name $ResourceGroup
    Write-Host "Resource group $ResourceGroup already exists."
}
catch {
    Write-Host "Creating resource group $ResourceGroup..."
    az group create --name $ResourceGroup --location $Location
}

# Create storage account for the function
Write-Host "Creating storage account $StorageAccountName..."
az storage account create `
  --name $StorageAccountName `
  --location $Location `
  --resource-group $ResourceGroup `
  --sku Standard_LRS `
  --kind StorageV2

# Create the function app with system-assigned managed identity
Write-Host "Creating function app $FunctionAppName with managed identity..."
az functionapp create `
  --name $FunctionAppName `
  --storage-account $StorageAccountName `
  --consumption-plan-location $Location `
  --resource-group $ResourceGroup `
  --functions-version 4 `
  --os-type Linux `
  --runtime python `
  --runtime-version 3.9 `
  --assign-identity "[system]"

# Get the principal ID of the function app's managed identity
Write-Host "Getting managed identity principal ID..."
$PrincipalId = $(az functionapp identity show `
  --name $FunctionAppName `
  --resource-group $ResourceGroup `
  --query principalId -o tsv)

Write-Host "Function app managed identity principal ID: $PrincipalId"

# Grant permissions for the function app to manage role assignments
# This allows the function to create and assign custom roles
Write-Host "Assigning Role Based Access Control Administrator role to managed identity..."
az role assignment create `
  --assignee $PrincipalId `
  --role "Role Based Access Control Administrator" `
  --scope "/subscriptions/$SubscriptionId"

# Also assign Key Vault Administrator role to manage Key Vault resources
Write-Host "Assigning Key Vault Administrator role to managed identity..."
az role assignment create `
  --assignee $PrincipalId `
  --role "Key Vault Administrator" `
  --scope "/subscriptions/$SubscriptionId"

# Create an Event Grid subscription to trigger the function when Key Vaults are created
Write-Host "Creating Event Grid subscription for Key Vault creation events..."
$FunctionAppEndpoint = "/subscriptions/$SubscriptionId/resourceGroups/$ResourceGroup/providers/Microsoft.Web/sites/$FunctionAppName/functions/VaultRbacProcessor"

az eventgrid event-subscription create `
  --name "KeyVaultCreationEvents" `
  --source-resource-id "/subscriptions/$SubscriptionId" `
  --endpoint-type azurefunction `
  --endpoint $FunctionAppEndpoint `
  --included-event-types "Microsoft.Resources.ResourceWriteSuccess" `
  --advanced-filter data.operationName StringContains "Microsoft.KeyVault/vaults/write"

Write-Host "---------------------------------------------"
Write-Host "Setup complete! Summary:"
Write-Host "- Created function app: $FunctionAppName"
Write-Host "- Assigned necessary RBAC roles to managed identity"
Write-Host "- Set up Event Grid subscription for Key Vault creation events"
Write-Host ""
Write-Host "Next steps:"
Write-Host "1. Deploy the function code to the function app"
Write-Host "2. Test by creating a Key Vault with RBAC authorization enabled"
Write-Host 