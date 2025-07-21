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

# Get the Function App's managed identity object ID
Write-Host "Getting Function App's managed identity..."
$functionApp = az functionapp show --name $FunctionAppName --resource-group $ResourceGroup | ConvertFrom-Json

if (-not $functionApp.identity -or $functionApp.identity.type -ne "SystemAssigned") {
    Write-Error "Function App doesn't have a System-Assigned Managed Identity. Please enable it first."
    exit 1
}

$managedIdentityObjectId = $functionApp.identity.principalId
$managedIdentityClientId = $functionApp.identity.tenantId
Write-Host "Managed Identity Object ID: $managedIdentityObjectId"

# Determine tenant ID
$tenantId = az account show --query tenantId -o tsv
Write-Host "Tenant ID: $tenantId"

# Method 1: Grant API permissions through Azure CLI with Microsoft Graph
Write-Host "`nMethod 1: Configuring Graph permissions using Azure CLI..."
try {
    # Get Microsoft Graph resource ID
    $graphResourceId = "00000003-0000-0000-c000-000000000000" # Microsoft Graph App ID
    $userReadPermissionId = "e1fe6dd8-ba31-4d61-89e7-88639da4683d" # User.Read permission ID

    # Create API permission
    Write-Host "Adding Microsoft Graph User.Read permission..."
    az ad app permission add --id $managedIdentityObjectId --api $graphResourceId --api-permissions "$userReadPermissionId=Scope"
    
    # Grant admin consent
    Write-Host "Granting admin consent..."
    az ad app permission admin-consent --id $managedIdentityObjectId
    
    Write-Host "✅ Permissions configured using Azure CLI" -ForegroundColor Green
} 
catch {
    Write-Host "⚠️ Method 1 failed. Trying alternative method..." -ForegroundColor Yellow
}

# Method 2: Using Microsoft Graph PowerShell module
Write-Host "`nMethod 2: Configuring Graph permissions using Graph PowerShell module..."
try {
    # Check if Microsoft Graph PowerShell module is installed
    if (-not (Get-Module -ListAvailable -Name Microsoft.Graph.Applications)) {
        Write-Host "Installing Microsoft Graph PowerShell module..."
        Install-Module -Name Microsoft.Graph.Applications -Scope CurrentUser -Force
    }
    
    # Connect to Microsoft Graph
    Connect-MgGraph -Scopes "Application.ReadWrite.All", "Directory.ReadWrite.All"
    
    # Create app registration for the managed identity if it doesn't exist
    $app = Get-MgServicePrincipal -Filter "appId eq '$managedIdentityClientId'"
    
    if (-not $app) {
        Write-Error "Could not find service principal for the managed identity"
    } else {
        # Add Microsoft Graph API permissions
        $graphSp = Get-MgServicePrincipal -Filter "appId eq '00000003-0000-0000-c000-000000000000'"
        $permission = $graphSp.AppRoles | Where-Object { $_.Value -eq "User.Read" }
        
        # Create the app role assignment
        New-MgServicePrincipalAppRoleAssignment -ServicePrincipalId $app.Id -AppRoleId $permission.Id -ResourceId $graphSp.Id -PrincipalId $app.Id
        
        Write-Host "✅ Permissions configured using Microsoft Graph PowerShell module" -ForegroundColor Green
    }
}
catch {
    Write-Host "⚠️ Method 2 failed. Trying alternative method..." -ForegroundColor Yellow
}

# Method 3: Using Azure Portal instructions
Write-Host "`nMethod 3: Manual configuration instructions for Azure Portal:"
Write-Host "------------------------------------------------------------------"
Write-Host "1. Go to Azure Portal: https://portal.azure.com"
Write-Host "2. Navigate to Azure Active Directory > Enterprise applications"
Write-Host "3. Search for your function app name: $FunctionAppName"
Write-Host "4. Click on 'Permissions' in the left menu"
Write-Host "5. Click on '+ Add permission'"
Write-Host "6. Select 'Microsoft Graph' under 'Microsoft APIs'"
Write-Host "7. Select 'Delegated permissions'"
Write-Host "8. Find and check 'User.Read'"
Write-Host "9. Click 'Add permissions'"
Write-Host "10. Click 'Grant admin consent for [your tenant]'"
Write-Host "------------------------------------------------------------------"

Write-Host "`nPermission configuration complete. Verify by running a test request using the managed identity."
Write-Host "Note: It may take a few minutes for permissions to propagate." 