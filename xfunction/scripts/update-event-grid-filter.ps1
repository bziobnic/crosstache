param(
    [Parameter(Mandatory=$true)][string]$ResourceGroup,
    [Parameter(Mandatory=$true)][string]$FunctionAppName,
    [Parameter(Mandatory=$true)][string]$SubscriptionId
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Write-Error (
    "This legacy Event Grid workflow is disabled. The deployed " +
    "DirectVaultRbacProcessor is an authenticated HTTP trigger, not an Event Grid trigger. " +
    "Remove any KeyVaultCreationEvents subscription and use the direct API."
)
exit 1
