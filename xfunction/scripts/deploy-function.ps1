# Set these variables before running the script
$FunctionAppName = "fa-user-keyvault"
$ResourceGroup = "Vaults"

# Check if Azure CLI is installed
try {
    $null = Get-Command az -ErrorAction Stop
}
catch {
    Write-Error "Azure CLI is not installed. Please install it first."
    exit 1
}

# Check if Azure Functions Core Tools is installed
try {
    $null = Get-Command func -ErrorAction Stop
}
catch {
    Write-Error "Azure Functions Core Tools is not installed. Please install it first."
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

# Navigate to the vaultserver directory
$ScriptPath = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location -Path $ScriptPath

# Install dependencies
Write-Host "Installing required Python packages..."
pip install -r requirements.txt

# Deploy the function to Azure
Write-Host "Deploying function app to Azure..."
func azure functionapp publish $FunctionAppName --python

# Verify deployment
Write-Host "Verifying deployment..."
az functionapp show `
  --name $FunctionAppName `
  --resource-group $ResourceGroup `
  --query defaultHostName `
  --output tsv

Write-Host "---------------------------------------------"
Write-Host "Deployment complete!"
Write-Host "You can test the function by creating a Key Vault with RBAC authorization enabled:"
Write-Host ""
Write-Host "az keyvault create \"
Write-Host "  --name <vault-name> \"
Write-Host "  --resource-group <resource-group> \"
Write-Host "  --location <location> \"
Write-Host "  --enable-rbac-authorization true"
Write-Host ""
Write-Host "Then verify the role assignment was created using:"
Write-Host ""
Write-Host "az role assignment list --scope /subscriptions/<subscription-id>/resourceGroups/<resource-group>/providers/Microsoft.KeyVault/vaults/<vault-name>"
Write-Host "---------------------------------------------" 