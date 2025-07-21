param(
    [Parameter(Mandatory=$true)]
    [string]$ResourceGroup,
    
    [Parameter(Mandatory=$true)]
    [string]$FunctionAppName,
    
    [Parameter(Mandatory=$true)]
    [string]$SubscriptionId
)

# Ensure we're logged in and in the correct subscription
Write-Host "Verifying Azure CLI login and subscription..."
$currentContext = az account show --query id -o tsv
if ($currentContext -ne $SubscriptionId) {
    Write-Error "Not logged in to the correct subscription. Expected: $SubscriptionId, Current: $currentContext"
    exit 1
}

# Check if the function exists
Write-Host "Checking if function exists..."
$function = az functionapp function show --name $FunctionAppName --resource-group $ResourceGroup --function-name "VaultRbacProcessor" | ConvertFrom-Json
if (-not $function) {
    Write-Error "Function 'VaultRbacProcessor' not found in function app '$FunctionAppName'"
    Write-Host "Available functions:"
    az functionapp function list --name $FunctionAppName --resource-group $ResourceGroup --output table
    exit 1
}

# Get the current Event Grid subscription
Write-Host "Getting current Event Grid subscription..."
$eventSub = az eventgrid event-subscription show --name "KeyVaultCreationEvents" --source-resource-id "/subscriptions/$SubscriptionId" | ConvertFrom-Json
if (-not $eventSub) {
    Write-Error "Event Grid subscription 'KeyVaultCreationEvents' not found"
    exit 1
}

Write-Host "Current endpoint: $($eventSub.destination.endpointUrl)"

# Get the function app ID
$functionAppId = "/subscriptions/$SubscriptionId/resourceGroups/$ResourceGroup/providers/Microsoft.Web/sites/$FunctionAppName"

# Get the function's trigger URL (without code)
$correctEndpoint = "$functionAppId/functions/VaultRbacProcessor"
Write-Host "Correct endpoint should include: $correctEndpoint"

# Update the Event Grid subscription
Write-Host "Updating Event Grid subscription endpoint..."
az eventgrid event-subscription update `
    --name "KeyVaultCreationEvents" `
    --source-resource-id "/subscriptions/$SubscriptionId" `
    --endpoint-type azurefunction `
    --endpoint $correctEndpoint

# Verify the update
Write-Host "Verifying update..."
$updatedEventSub = az eventgrid event-subscription show --name "KeyVaultCreationEvents" --source-resource-id "/subscriptions/$SubscriptionId" | ConvertFrom-Json
Write-Host "Updated endpoint: $($updatedEventSub.destination.endpointUrl)"

if ($updatedEventSub.destination.endpointUrl -like "*$correctEndpoint*") {
    Write-Host "✅ Event Grid endpoint successfully updated!" -ForegroundColor Green
} else {
    Write-Host "❌ Event Grid endpoint update failed. Please check manually." -ForegroundColor Red
}

# Check for any missing advanced filters
Write-Host "Checking advanced filters..."
$hasFilter = $false
foreach ($filter in $updatedEventSub.filter.advancedFilters) {
    if ($filter.operatorType -eq "StringContains" -and 
        $filter.key -eq "data.operationName" -and 
        $filter.values -contains "Microsoft.KeyVault/vaults/write") {
        $hasFilter = $true
        break
    }
}

if (-not $hasFilter) {
    Write-Host "Adding missing advanced filter for Key Vault operations..."
    az eventgrid event-subscription update `
        --name "KeyVaultCreationEvents" `
        --source-resource-id "/subscriptions/$SubscriptionId" `
        --advanced-filter data.operationName StringContains "Microsoft.KeyVault/vaults/write"
    
    Write-Host "✅ Advanced filter added!" -ForegroundColor Green
} else {
    Write-Host "✅ Advanced filters are correctly configured." -ForegroundColor Green
}

Write-Host "`nRun the verify-deployment.ps1 script again to check all settings." -ForegroundColor Cyan 