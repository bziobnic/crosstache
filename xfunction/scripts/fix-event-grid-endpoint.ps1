param(
    [Parameter(Mandatory=$true)][string]$ResourceGroup,
    [Parameter(Mandatory=$true)][string]$FunctionAppName,
    [Parameter(Mandatory=$true)][string]$SubscriptionId
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Write-Error (
    "This legacy Event Grid endpoint repair is disabled. No Event Grid function is deployed; " +
    "remove any KeyVaultCreationEvents subscription and use DirectVaultRbacProcessor through " +
    "its authenticated HTTP route."
)
exit 1
