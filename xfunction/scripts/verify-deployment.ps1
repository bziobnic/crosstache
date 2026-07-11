param(
    [Parameter(Mandatory=$true)][string]$ResourceGroup,
    [Parameter(Mandatory=$true)][string]$FunctionAppName,
    [Parameter(Mandatory=$true)][string]$SubscriptionId
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$script:ErrorCount = 0
$script:WarningCount = 0

function Write-Status {
    param([string]$Component, [string]$Status, [string]$Details = "")
    if ($Status -eq "ERROR") { $script:ErrorCount++ }
    if ($Status -eq "WARNING") { $script:WarningCount++ }
    $color = if ($Status -eq "ERROR") { "Red" } elseif ($Status -eq "WARNING") { "Yellow" } else { "Green" }
    Write-Host "[$Component] $Status$(if ($Details) { ": $Details" })" -ForegroundColor $color
}

function Invoke-AzJson {
    param([Parameter(Mandatory=$true)][scriptblock]$Command)
    $raw = & $Command
    if ($LASTEXITCODE -ne 0) { throw "Azure CLI command failed with exit code $LASTEXITCODE" }
    return $raw | ConvertFrom-Json
}

try {
    $currentSubscription = az account show --query id --output tsv --only-show-errors
    if ($LASTEXITCODE -ne 0 -or $currentSubscription -ne $SubscriptionId) {
        throw "Expected subscription $SubscriptionId, current subscription is $currentSubscription"
    }

    $functionApp = Invoke-AzJson { az functionapp show --name $FunctionAppName --resource-group $ResourceGroup --only-show-errors }
    Write-Status "Function App" "OK" "Found $FunctionAppName"
    if ($functionApp.state -eq "Running") { Write-Status "Function App State" "OK" "Running" }
    else { Write-Status "Function App State" "ERROR" "Expected Running, got $($functionApp.state)" }

    $functionInventory = @(Invoke-AzJson { az functionapp function list --name $FunctionAppName --resource-group $ResourceGroup --only-show-errors })
    $directFunction = $functionInventory | Where-Object {
        ([string]$_.name).Split('/')[-1] -eq "DirectVaultRbacProcessor"
    } | Select-Object -First 1
    if ($null -ne $directFunction) { Write-Status "Function Inventory" "OK" "DirectVaultRbacProcessor is deployed" }
    else { Write-Status "Function Inventory" "ERROR" "DirectVaultRbacProcessor is not deployed" }

    $settings = @(Invoke-AzJson { az functionapp config appsettings list --name $FunctionAppName --resource-group $ResourceGroup --only-show-errors })
    $requiredSettings = @("FUNCTIONS_EXTENSION_VERSION", "FUNCTIONS_WORKER_RUNTIME", "AZURE_TENANT_ID", "AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET", "ALLOWED_RESOURCE_GROUP_ID", "ALLOWED_PRINCIPAL_ID")
    foreach ($name in $requiredSettings) {
        $setting = $settings | Where-Object { $_.name -eq $name } | Select-Object -First 1
        if ($null -ne $setting -and -not [string]::IsNullOrWhiteSpace([string]$setting.value)) {
            Write-Status "App Setting" "OK" "$name is configured"
        } else {
            Write-Status "App Setting" "ERROR" "Missing required setting: $name"
        }
    }

    $clientId = ($settings | Where-Object { $_.name -eq "AZURE_CLIENT_ID" } | Select-Object -First 1).value
    $principalId = az ad sp show --id $clientId --query id --output tsv --only-show-errors
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($principalId)) {
        Write-Status "Service Principal" "ERROR" "Cannot resolve AZURE_CLIENT_ID"
    } else {
        $expectedScope = "/subscriptions/$SubscriptionId/resourceGroups/$ResourceGroup"
        $assignments = @(Invoke-AzJson { az role assignment list --assignee-object-id $principalId --scope $expectedScope --include-inherited false --only-show-errors })
        foreach ($requiredRole in @("Role Based Access Control Administrator", "Reader")) {
            $match = $assignments | Where-Object { $_.roleDefinitionName -eq $requiredRole -and $_.scope -ieq $expectedScope } | Select-Object -First 1
            if ($null -eq $match) {
                Write-Status "RBAC Role" "ERROR" "Missing $requiredRole at $expectedScope"
            } elseif ($requiredRole -eq "Role Based Access Control Administrator") {
                $allowedPrincipalId = ($settings | Where-Object { $_.name -eq "ALLOWED_PRINCIPAL_ID" } | Select-Object -First 1).value
                $requiredRoleIds = @(
                    "8e3af657-a8ff-443c-a75c-2fe8c4bcb635",
                    "00482a5a-887f-4fb3-b363-3b7fe8e74483",
                    "17d1049b-9a84-46fb-8f53-869881c3d3ab",
                    "b7e6dc6d-f1e8-4753-8033-0f276bb0955b",
                    "ba92f5b4-2d11-453d-a403-e96b0029c9fe"
                )
                $conditionText = [string]$match.condition
                $conditionValid = $match.conditionVersion -eq "2.0" -and $conditionText -match [regex]::Escape($allowedPrincipalId)
                foreach ($roleId in $requiredRoleIds) { $conditionValid = $conditionValid -and $conditionText.Contains($roleId) }
                if ($conditionValid) { Write-Status "RBAC Role" "OK" "RBAC delegation has exact principal and role constraints" }
                else { Write-Status "RBAC Role" "ERROR" "RBAC delegation condition is missing or broader than expected" }
            } else {
                Write-Status "RBAC Role" "OK" "$requiredRole at exact resource-group scope"
            }
        }
    }

    # The deployed architecture exposes an authenticated HTTP trigger. A
    # legacy subscription-level Event Grid trigger would bypass that request
    # boundary and references a function that no longer exists.
    $sourceScope = "/subscriptions/$SubscriptionId"
    $null = az eventgrid event-subscription show --name KeyVaultCreationEvents --source-resource-id $sourceScope --only-show-errors 2>$null
    if ($LASTEXITCODE -eq 0) { Write-Status "Legacy Event Grid" "ERROR" "Obsolete KeyVaultCreationEvents subscription is still active" }
    else { Write-Status "Legacy Event Grid" "OK" "No obsolete subscription-level trigger" }
}
catch {
    Write-Status "Verification" "ERROR" $_.Exception.Message
}

Write-Host "Verification summary: $script:ErrorCount errors, $script:WarningCount warnings"
if ($script:ErrorCount -gt 0) { exit 1 }
exit 0
