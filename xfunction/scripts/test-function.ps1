# Set these variables before running the script
$SubscriptionId = "250d9a86-64a4-457e-a34e-fb2898eda332"
$ResourceGroup = "Vaults"
$Location = "eastus2"
$VaultName = "kv-test-rbac-$(Get-Random -Minimum 100000 -Maximum 999999)"  # Ensure unique name

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

# Create a Key Vault with RBAC authorization enabled
Write-Host "Creating Key Vault $VaultName with RBAC authorization enabled..."
az keyvault create `
  --name $VaultName `
  --resource-group $ResourceGroup `
  --location $Location `
  --enable-rbac-authorization true

Write-Host "Waiting for role assignments to be created (15 seconds)..."
Start-Sleep -Seconds 15

# Verify the role assignment was created
Write-Host "Checking role assignments on the Key Vault..."
$VaultResourceId = "/subscriptions/$SubscriptionId/resourceGroups/$ResourceGroup/providers/Microsoft.KeyVault/vaults/$VaultName"
az role assignment list --scope $VaultResourceId --output table

Write-Host "---------------------------------------------"
Write-Host "Test completed!"
Write-Host "Key Vault created: $VaultName"
Write-Host "Check the Azure Function logs to verify the function was triggered"
Write-Host "---------------------------------------------" 