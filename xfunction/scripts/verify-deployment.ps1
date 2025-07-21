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

$errors = 0
$warnings = 0

# Function to standardize status output
function Write-Status {
    param(
        [string]$Component,
        [string]$Status,
        [string]$Details = ""
    )
    
    $color = "Green"
    if ($Status -eq "WARNING") { 
        $color = "Yellow"
        $global:warnings++
    }
    elseif ($Status -eq "ERROR") { 
        $color = "Red"
        $global:errors++
    }
    
    Write-Host "[$Component] " -NoNewline
    Write-Host $Status -ForegroundColor $color -NoNewline
    if ($Details) {
        Write-Host ": $Details"
    } else {
        Write-Host ""
    }
}

# Check Function App existence and status
Write-Host "`nChecking Function App..."
$functionApp = az functionapp show --name $FunctionAppName --resource-group $ResourceGroup 2>$null | ConvertFrom-Json
if ($functionApp) {
    Write-Status "Function App" "OK" "Found $FunctionAppName"
    
    # Check if function app is running
    $state = $functionApp.state
    if ($state -eq "Running") {
        Write-Status "Function App State" "OK" "Running"
    } else {
        Write-Status "Function App State" "WARNING" "Not running (State: $state)"
    }
    
    # Check managed identity
    $identity = $functionApp.identity
    if ($identity.type -eq "SystemAssigned") {
        Write-Status "Managed Identity" "OK" "System assigned identity enabled"
        $principalId = $identity.principalId
        
        # Check RBAC assignments
        Write-Host "`nChecking RBAC assignments..."
        $roleAssignments = az role assignment list --assignee $principalId | ConvertFrom-Json
        
        $hasKeyVaultAdmin = $false
        foreach ($role in $roleAssignments) {
            if ($role.roleDefinitionName -eq "Key Vault Administrator") {
                $hasKeyVaultAdmin = $true
                break
            }
        }
        
        if ($hasKeyVaultAdmin) {
            Write-Status "RBAC Roles" "OK" "Has Key Vault Administrator role"
        } else {
            Write-Status "RBAC Roles" "ERROR" "Missing Key Vault Administrator role"
        }
    } else {
        Write-Status "Managed Identity" "ERROR" "System assigned identity not enabled"
    }
} else {
    Write-Status "Function App" "ERROR" "Function App $FunctionAppName not found"
}

# Check Event Grid subscription
Write-Host "`nChecking Event Grid subscription..."
$eventSub = az eventgrid event-subscription show --name "KeyVaultCreationEvents" --source-resource-id "/subscriptions/$SubscriptionId" 2>$null | ConvertFrom-Json
if ($eventSub) {
    Write-Status "Event Grid Sub" "OK" "Found KeyVaultCreationEvents subscription"
    
    # Verify endpoint
    $expectedEndpoint = "/subscriptions/$SubscriptionId/resourceGroups/$ResourceGroup/providers/Microsoft.Web/sites/$FunctionAppName/functions/VaultRbacProcessor"
    if ($eventSub.destination.resourceId -eq $expectedEndpoint) {
        Write-Status "Event Grid Endpoint" "OK" "Correctly configured to VaultRbacProcessor"
    } else {
        Write-Status "Event Grid Endpoint" "ERROR" "Incorrect endpoint configuration. Expected: $expectedEndpoint, Got: $($eventSub.destination.resourceId)"
    }
    
    # Verify filters
    $filters = $eventSub.filter.advancedFilters
    $hasCorrectFilter = $false
    foreach ($filter in $filters) {
        if ($filter.operatorType -eq "StringContains" -and 
            $filter.key -eq "data.operationName" -and 
            $filter.values -contains "Microsoft.KeyVault/vaults/write") {
            $hasCorrectFilter = $true
            break
        }
    }
    
    if ($hasCorrectFilter) {
        Write-Status "Event Grid Filter" "OK" "Correctly configured for Key Vault creation events"
    } else {
        Write-Status "Event Grid Filter" "ERROR" "Missing or incorrect event filter configuration"
    }
} else {
    Write-Status "Event Grid Sub" "ERROR" "Event Grid subscription not found"
}

# Check Function App application settings
Write-Host "`nChecking Function App settings..."
$appSettings = az functionapp config appsettings list --name $FunctionAppName --resource-group $ResourceGroup | ConvertFrom-Json

# Check for required settings (add any specific settings your function needs)
$requiredSettings = @(
    "FUNCTIONS_EXTENSION_VERSION",
    "FUNCTIONS_WORKER_RUNTIME"
)

foreach ($setting in $requiredSettings) {
    if ($appSettings.name -contains $setting) {
        Write-Status "App Setting" "OK" "$setting is configured"
    } else {
        Write-Status "App Setting" "ERROR" "Missing required setting: $setting"
    }
}

# Summary
Write-Host "`n----------------------------------------"
Write-Host "Verification Summary:"
Write-Host "----------------------------------------"
Write-Host "Total Errors: $errors"
Write-Host "Total Warnings: $warnings"

if ($errors -gt 0) {
    Write-Host "`nDeployment verification failed with $errors errors and $warnings warnings." -ForegroundColor Red
    exit 1
} elseif ($warnings -gt 0) {
    Write-Host "`nDeployment verification completed with $warnings warnings." -ForegroundColor Yellow
    exit 0
} else {
    Write-Host "`nDeployment verification completed successfully!" -ForegroundColor Green
    exit 0
}
