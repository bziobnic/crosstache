param(
    [Parameter(Mandatory=$true)]
    [string]$ResourceGroup,
    
    [Parameter(Mandatory=$true)]
    [string]$FunctionAppName,
    
    [Parameter(Mandatory=$false)]
    [int]$MaxLogEntries = 100
)

# Get the raw logs from Azure Function
Write-Host "Fetching logs for $FunctionAppName..." -ForegroundColor Cyan

# Use a temporary file to store the logs
$tempLogFile = [System.IO.Path]::GetTempFileName()

try {
    # Run the Azure CLI command to get logs and save to temp file
    Write-Host "Running: az functionapp logs --name $FunctionAppName --resource-group $ResourceGroup --limit $MaxLogEntries"
    az functionapp log --name $FunctionAppName --resource-group $ResourceGroup --limit $MaxLogEntries | Out-File -FilePath $tempLogFile
    
    # Read the logs file
    $allLogs = Get-Content -Path $tempLogFile
    
    # If no logs found
    if (-not $allLogs -or $allLogs.Count -eq 0) {
        Write-Host "No logs found for $FunctionAppName" -ForegroundColor Yellow
        exit 0
    }
    
    # Find the latest invocation of vault_rbac_processor
    $invocations = @()
    $currentInvocation = @()
    $inInvocation = $false
    $foundAnyInvocation = $false
    
    Write-Host "Processing logs..."
    
    foreach ($line in $allLogs) {
        # Check if this line indicates start of a VaultRbacProcessor invocation
        if ($line -match "Executing 'Functions\.VaultRbacProcessor'" -or $line -match "Executing 'vault_rbac_processor'") {
            # If we were already in an invocation, save it before starting a new one
            if ($inInvocation) {
                $invocations += ,@($currentInvocation)
            }
            
            # Start a new invocation
            $currentInvocation = @($line)
            $inInvocation = $true
            $foundAnyInvocation = $true
            continue
        }
        
        # Check if this line indicates the end of a function execution
        if ($inInvocation -and $line -match "Executed 'Functions\.VaultRbacProcessor'" -or $line -match "Executed 'vault_rbac_processor'") {
            $currentInvocation += $line
            $invocations += ,@($currentInvocation)
            $currentInvocation = @()
            $inInvocation = $false
            continue
        }
        
        # If we're in an invocation and this doesn't indicate a new one starting, add the line
        if ($inInvocation) {
            $currentInvocation += $line
        }
    }
    
    # Add the last invocation if it wasn't ended
    if ($inInvocation -and $currentInvocation.Count -gt 0) {
        $invocations += ,@($currentInvocation)
    }
    
    # If no invocations found
    if (-not $foundAnyInvocation) {
        Write-Host "No invocations of 'vault_rbac_processor' found in the logs" -ForegroundColor Yellow
        
        # Check if there are specific logs for the function without the invocation marker
        $functionLogs = $allLogs | Where-Object { $_ -match "VaultRbacProcessor" -or $_ -match "vault_rbac_processor" }
        
        if ($functionLogs -and $functionLogs.Count -gt 0) {
            Write-Host "However, found $($functionLogs.Count) log entries mentioning this function:" -ForegroundColor Cyan
            foreach ($log in $functionLogs) {
                Write-Host $log -ForegroundColor Gray
            }
        } else {
            # Show all logs if nothing specific found
            Write-Host "Showing the most recent logs ($($allLogs.Count) entries) for reference:" -ForegroundColor Cyan
            foreach ($log in $allLogs) {
                Write-Host $log -ForegroundColor Gray
            }
        }
        
        exit 0
    }
    
    # Get the most recent invocation
    $mostRecentInvocation = $invocations[$invocations.Count - 1]
    
    # Output the most recent invocation logs with formatting
    Write-Host "`n======== MOST RECENT INVOCATION OF VAULT_RBAC_PROCESSOR ========" -ForegroundColor Green
    $mostRecentInvocation | ForEach-Object {
        # Highlight error messages in red
        if ($_ -match "Error|Exception|Failed|failure|error") {
            Write-Host $_ -ForegroundColor Red
        } 
        # Highlight warnings in yellow
        elseif ($_ -match "Warning|warning") {
            Write-Host $_ -ForegroundColor Yellow
        }
        # Highlight specific event info in cyan
        elseif ($_ -match "Event Type:|Subject:|Resource URI:|Operation Name:|Creator identified|Key Vault check|Claims") {
            Write-Host $_ -ForegroundColor Cyan
        }
        # Normal logging output
        else {
            Write-Host $_
        }
    }
    Write-Host "================================================================" -ForegroundColor Green
    
    # Show a count of all invocations found
    Write-Host "`nFound $($invocations.Count) total invocation(s) of vault_rbac_processor in the logs" -ForegroundColor Cyan
    Write-Host "To see more logs, increase the MaxLogEntries parameter:" -ForegroundColor Cyan
    Write-Host "./get-function-logs.ps1 -ResourceGroup $ResourceGroup -FunctionAppName $FunctionAppName -MaxLogEntries 200" -ForegroundColor Cyan
}
finally {
    # Clean up the temp file
    if (Test-Path $tempLogFile) {
        Remove-Item -Path $tempLogFile -Force
    }
} 