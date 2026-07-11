param(
    [string]$AppName = "xfunction-rbac",
    [Parameter(Mandatory=$true)][string]$FunctionAppName,
    [Parameter(Mandatory=$true)][string]$ResourceGroup
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Invoke-AzJson {
    param([Parameter(Mandatory=$true)][scriptblock]$Command)
    $raw = & $Command
    if ($LASTEXITCODE -ne 0) { throw "Azure CLI command failed with exit code $LASTEXITCODE" }
    return $raw | ConvertFrom-Json
}

try {
    # Always create a fresh application object. Never adopt an existing app by
    # display name, and do not grant directory-wide Microsoft Graph scopes.
    $app = Invoke-AzJson { az ad app create --display-name $AppName --only-show-errors }
    $servicePrincipal = Invoke-AzJson { az ad sp create --id $app.appId --only-show-errors }
    $secret = Invoke-AzJson { az ad app credential reset --id $app.appId --append --years 2 --only-show-errors }
    $subscriptionId = az account show --query id --output tsv --only-show-errors
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($subscriptionId)) {
        throw "Could not resolve active Azure subscription"
    }

    $settings = @{
        AZURE_CLIENT_ID = $app.appId
        AZURE_CLIENT_SECRET = $secret.password
        AZURE_TENANT_ID = $secret.tenant
        EXPECTED_AUDIENCE = $app.appId
        ALLOWED_RESOURCE_GROUP_ID = "/subscriptions/$subscriptionId/resourceGroups/$ResourceGroup"
    }
    $settingsPath = [System.IO.Path]::GetTempFileName()
    try {
        $settings | ConvertTo-Json -Compress | Set-Content -LiteralPath $settingsPath -NoNewline
        if ($PSVersionTable.PSEdition -eq "Core" -and ($IsLinux -or $IsMacOS)) {
            chmod 600 $settingsPath
            if ($LASTEXITCODE -ne 0) { throw "Could not restrict temporary settings file permissions" }
        }
        az functionapp config appsettings set `
            --name $FunctionAppName `
            --resource-group $ResourceGroup `
            --settings "@$settingsPath" `
            --only-show-errors `
            --output none
        if ($LASTEXITCODE -ne 0) { throw "Function App settings update failed" }
    }
    finally {
        Remove-Item -LiteralPath $settingsPath -Force -ErrorAction SilentlyContinue
    }

    Write-Host "App registration created without Graph permissions." -ForegroundColor Green
    Write-Host "Application ID: $($app.appId)"
    Write-Host "Service principal object ID: $($servicePrincipal.id)"
}
catch {
    Write-Error "App registration setup failed: $($_.Exception.Message)"
    exit 1
}
