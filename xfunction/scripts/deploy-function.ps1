param(
    [string]$FunctionAppName = "fa-user-keyvault",
    [string]$ResourceGroup = "Vaults"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Invoke-Native {
    param([Parameter(Mandatory=$true)][scriptblock]$Command)
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "Native command failed with exit code $LASTEXITCODE"
    }
}

try {
    $null = Get-Command az -ErrorAction Stop
    $null = Get-Command func -ErrorAction Stop
    $null = Get-Command python -ErrorAction Stop
    Invoke-Native { az account show --only-show-errors --output none }

    # The function host files and requirements.txt live one directory above
    # this script. Publishing from scripts/ can report success without finding
    # the intended Azure Functions project.
    $projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
    if (-not (Test-Path (Join-Path $projectRoot "host.json") -PathType Leaf)) {
        throw "Azure Functions project root is missing host.json: $projectRoot"
    }
    if (-not (Test-Path (Join-Path $projectRoot "requirements.txt") -PathType Leaf)) {
        throw "Azure Functions project root is missing requirements.txt: $projectRoot"
    }

    Push-Location $projectRoot
    try {
        Write-Host "Installing required Python packages..."
        Invoke-Native { python -m pip install -r requirements.txt }

        Write-Host "Deploying function app to Azure..."
        Invoke-Native { func azure functionapp publish $FunctionAppName --python }

        Write-Host "Verifying deployment..."
        $hostName = az functionapp show --name $FunctionAppName --resource-group $ResourceGroup --query defaultHostName --output tsv --only-show-errors
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($hostName)) {
            throw "Function App verification failed"
        }
    }
    finally {
        Pop-Location
    }

    Write-Host "Deployment complete: https://$hostName" -ForegroundColor Green
}
catch {
    Write-Error "Deployment failed: $($_.Exception.Message)"
    exit 1
}
