param(
    [Parameter(Mandatory=$true)]
    [string]$ResourceGroup,

    [Parameter(Mandatory=$true)]
    [string]$FunctionAppName,

    [Parameter(Mandatory=$true)]
    [string]$SubscriptionId
)

$ErrorActionPreference = 'Stop'

Write-Host "No Microsoft Graph permissions are required."
Write-Host "The function accepts only validated Entra object IDs and no longer performs directory-wide lookups."
Write-Host "No permissions were changed."
