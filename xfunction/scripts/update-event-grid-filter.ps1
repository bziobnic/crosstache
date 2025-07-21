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

# Get the current Event Grid subscription
Write-Host "Getting current Event Grid subscription..."
$eventSub = az eventgrid event-subscription show --name "KeyVaultCreationEvents" --source-resource-id "/subscriptions/$SubscriptionId" | ConvertFrom-Json
if (-not $eventSub) {
    Write-Error "Event Grid subscription 'KeyVaultCreationEvents' not found"
    exit 1
}

Write-Host "Current Event Grid subscription details:"
Write-Host "----------------------------------------"
Write-Host "Name: $($eventSub.name)"
Write-Host "Endpoint: $($eventSub.destination.endpointUrl)"
Write-Host "Event Types: $($eventSub.filter.includedEventTypes -join ', ')"
Write-Host "----------------------------------------"

# Update with improved filters
Write-Host "Updating Event Grid subscription with better filters..."

# Step 1: Update the event types to include all Resource events
Write-Host "Updating included event types..."
az eventgrid event-subscription update `
    --name "KeyVaultCreationEvents" `
    --source-resource-id "/subscriptions/$SubscriptionId" `
    --included-event-types Microsoft.Resources.ResourceWriteSuccess

# Step 2: Update with advanced filters for Key Vault operations
Write-Host "Adding advanced filters for Key Vault operations..."

# First, remove any existing advanced filters
$currentAdvancedFilters = $eventSub.filter.advancedFilters
if ($currentAdvancedFilters) {
    Write-Host "Removing existing advanced filters..."
    az eventgrid event-subscription update `
        --name "KeyVaultCreationEvents" `
        --source-resource-id "/subscriptions/$SubscriptionId" `
        --advanced-filter ""
}

# Method 1: Filter by operationName
Write-Host "Adding filter for operationName..."
az eventgrid event-subscription update `
    --name "KeyVaultCreationEvents" `
    --source-resource-id "/subscriptions/$SubscriptionId" `
    --advanced-filter data.operationName StringContains Microsoft.KeyVault/vaults/write

# Method 2: Filter by resourceUri
Write-Host "Adding filter for resourceUri..."
az eventgrid event-subscription update `
    --name "KeyVaultCreationEvents" `
    --source-resource-id "/subscriptions/$SubscriptionId" `
    --advanced-filter data.resourceUri StringContains Microsoft.KeyVault/vaults

# Step 3: Update subject filters to add flexibility
Write-Host "Updating subject filters..."
az eventgrid event-subscription update `
    --name "KeyVaultCreationEvents" `
    --source-resource-id "/subscriptions/$SubscriptionId" `
    --subject-begins-with "/subscriptions/$SubscriptionId/resourceGroups/" `
    --subject-ends-with ""

# Verify the update
Write-Host "Verifying update..."
$updatedEventSub = az eventgrid event-subscription show --name "KeyVaultCreationEvents" --source-resource-id "/subscriptions/$SubscriptionId" | ConvertFrom-Json

Write-Host "`nUpdated Event Grid subscription details:"
Write-Host "----------------------------------------"
Write-Host "Name: $($updatedEventSub.name)"
Write-Host "Endpoint: $($updatedEventSub.destination.endpointUrl)"
Write-Host "Event Types: $($updatedEventSub.filter.includedEventTypes -join ', ')"
Write-Host "Advanced Filters: $(ConvertTo-Json -InputObject $updatedEventSub.filter.advancedFilters -Compress)"
Write-Host "Subject Begins With: $($updatedEventSub.filter.subjectBeginsWith)"
Write-Host "Subject Ends With: $($updatedEventSub.filter.subjectEndsWith)"
Write-Host "----------------------------------------"

Write-Host "`n✅ Event Grid subscription updated with improved filters!" -ForegroundColor Green
Write-Host "You can test it by creating a new Key Vault in the Azure portal with RBAC authorization enabled."
Write-Host "Check your function logs for detailed event information using:"
Write-Host "az functionapp logs tail --name $FunctionAppName --resource-group $ResourceGroup" 