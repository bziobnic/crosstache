# Script to create and configure App Registration for Graph API access

param(
    [Parameter(Mandatory=$true)]
    [string]$AppName = "VaultServer-GraphAPI",
    
    [Parameter(Mandatory=$true)]
    [string]$FunctionAppName,
    
    [Parameter(Mandatory=$true)]
    [string]$ResourceGroup
)

# Create the App Registration
Write-Host "Creating App Registration..."
$app = az ad app create --display-name $AppName | ConvertFrom-Json

# Create a client secret
Write-Host "Creating client secret..."
$secret = az ad app credential reset --id $app.appId --append | ConvertFrom-Json

# Add Graph API permissions
Write-Host "Adding Graph API permissions..."
$graphUserReadAll = "df021288-bdef-4463-88db-98f22de89214" # User.Read.All permission ID
$graphId = "00000003-0000-0000-c000-000000000000" # Microsoft Graph

az ad app permission add --id $app.appId --api $graphId --api-permissions "$graphUserReadAll=Role"

# Grant admin consent
Write-Host "Granting admin consent..."
az ad app permission admin-consent --id $app.appId

# Update function app settings
Write-Host "Updating Function App settings..."
az functionapp config appsettings set --name $FunctionAppName --resource-group $ResourceGroup --settings "AZURE_CLIENT_ID=$($app.appId)" "AZURE_CLIENT_SECRET=$($secret.password)" "AZURE_TENANT_ID=$($secret.tenant)"

Write-Host "`n----------------------------------------"
Write-Host "App Registration Setup Complete!"
Write-Host "----------------------------------------"
Write-Host "Application (client) ID: $($app.appId)"
Write-Host "Directory (tenant) ID: $($secret.tenant)"
Write-Host "Client Secret: $($secret.password)"
Write-Host "`nThese values have been automatically added to your Function App settings."
Write-Host "Please save these values securely for your records."
