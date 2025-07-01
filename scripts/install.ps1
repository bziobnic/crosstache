#!/usr/bin/env pwsh

# crosstache (xv) installer for Windows
# https://github.com/bziobnic/crosstache

[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\crosstache",
    [switch]$Help
)

# Configuration
$GitHubRepo = "bziobnic/crosstache"
$BinaryName = "xv.exe"
$ErrorActionPreference = 'Stop'

# Show help
if ($Help) {
    Write-Host @"
crosstache Installer for Windows

Usage: .\install.ps1 [OPTIONS]

Options:
    -Version <string>     Specific version to install (default: latest)
    -InstallDir <string>  Installation directory (default: $env:LOCALAPPDATA\Programs\crosstache)
    -Help                 Show this help message

Examples:
    .\install.ps1                    # Install latest version
    .\install.ps1 -Version v0.1.0    # Install specific version
    
Installation via one-liner:
    iwr -useb https://raw.githubusercontent.com/$GitHubRepo/main/scripts/install.ps1 | iex
"@
    exit 0
}

# Print functions with colors
function Write-Info {
    param([string]$Message)
    Write-Host "[INFO] $Message" -ForegroundColor Blue
}

function Write-Success {
    param([string]$Message)
    Write-Host "[SUCCESS] $Message" -ForegroundColor Green
}

function Write-Warning {
    param([string]$Message)
    Write-Host "[WARNING] $Message" -ForegroundColor Yellow
}

function Write-Error {
    param([string]$Message)
    Write-Host "[ERROR] $Message" -ForegroundColor Red
    exit 1
}

# Check if Azure CLI is installed
function Test-AzureCLI {
    try {
        $azVersion = az version --output json | ConvertFrom-Json
        if ($azVersion.'azure-cli') {
            Write-Info "Azure CLI is already installed (version: $($azVersion.'azure-cli'))"
            return $true
        }
    }
    catch {
        return $false
    }
    return $false
}

# Install Azure CLI for Windows
function Install-AzureCLI {
    Write-Info "Installing Azure CLI..."
    
    # Check if running with administrator privileges
    $currentPrincipal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    $isAdmin = $currentPrincipal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    
    try {
        # Try to install via winget first (if available)
        if (Get-Command winget -ErrorAction SilentlyContinue) {
            Write-Info "Installing Azure CLI via winget..."
            winget install Microsoft.AzureCLI --accept-source-agreements --accept-package-agreements
        }
        # Try chocolatey if available
        elseif (Get-Command choco -ErrorAction SilentlyContinue) {
            Write-Info "Installing Azure CLI via Chocolatey..."
            choco install azure-cli -y
        }
        # Fall back to MSI installer
        else {
            Write-Info "Installing Azure CLI via MSI installer..."
            $msiUrl = "https://aka.ms/installazurecliwindows"
            $tempFile = "$env:TEMP\AzureCLI.msi"
            
            Write-Info "Downloading Azure CLI installer..."
            Invoke-WebRequest -Uri $msiUrl -OutFile $tempFile
            
            if ($isAdmin) {
                Write-Info "Installing Azure CLI (requires administrator privileges)..."
                Start-Process msiexec.exe -ArgumentList "/i", $tempFile, "/quiet" -Wait
            }
            else {
                Write-Warning "Administrator privileges required for installation."
                Write-Warning "Please run as administrator or install Azure CLI manually."
                Write-Warning "You can download it from: https://docs.microsoft.com/en-us/cli/azure/install-azure-cli-windows"
                return $false
            }
            
            # Clean up temp file
            Remove-Item $tempFile -Force -ErrorAction SilentlyContinue
        }
        
        # Verify installation
        if (Test-AzureCLI) {
            Write-Success "Azure CLI installed successfully"
            return $true
        }
        else {
            Write-Error "Azure CLI installation failed. Please install it manually from https://docs.microsoft.com/en-us/cli/azure/install-azure-cli-windows"
            return $false
        }
    }
    catch {
        Write-Error "Failed to install Azure CLI: $($_.Exception.Message)"
        return $false
    }
}

# Get latest version from GitHub API
function Get-LatestVersion {
    try {
        $apiUrl = "https://api.github.com/repos/$GitHubRepo/releases/latest"
        $response = Invoke-RestMethod -Uri $apiUrl -Method Get
        return $response.tag_name
    }
    catch {
        Write-Error "Failed to fetch latest version: $_"
    }
}

# Download and verify file
function Download-File {
    param(
        [string]$Url,
        [string]$OutFile,
        [string]$Description
    )
    
    try {
        Write-Info "Downloading $Description..."
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing
        $ProgressPreference = 'Continue'
        
        if (-not (Test-Path $OutFile)) {
            Write-Error "Failed to download $Description"
        }
    }
    catch {
        Write-Error "Download failed: $_"
    }
}

# Verify checksum
function Test-Checksum {
    param(
        [string]$FilePath,
        [string]$ChecksumPath
    )
    
    if (-not (Test-Path $ChecksumPath)) {
        Write-Warning "Checksum file not found, skipping verification"
        return
    }
    
    try {
        Write-Info "Verifying checksum..."
        
        # Wait a moment to ensure file is fully written
        Start-Sleep -Milliseconds 500
        
        # Check if checksum file has content
        if ((Get-Item $ChecksumPath).Length -eq 0) {
            Write-Warning "Checksum file is empty, skipping verification"
            return
        }
        
        $expectedHash = (Get-Content $ChecksumPath -Raw).Trim() -replace '\r?\n.*', '' -replace '\s.*', ''
        
        if ([string]::IsNullOrWhiteSpace($expectedHash)) {
            Write-Warning "Could not read valid checksum from file, skipping verification"
            return
        }
        
        $actualHash = (Get-FileHash -Path $FilePath -Algorithm SHA256).Hash.ToLower()
        $expectedHash = $expectedHash.ToLower()
        
        if ($expectedHash -ne $actualHash) {
            Write-Error "Checksum verification failed. Expected: $expectedHash, Got: $actualHash"
        }
        else {
            Write-Info "Checksum verification passed"
        }
    }
    catch {
        Write-Warning "Checksum verification failed: $($_.Exception.Message)"
    }
}

# Add directory to PATH
function Add-ToPath {
    param([string]$Directory)
    
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    
    if ($userPath -notlike "*$Directory*") {
        Write-Info "Adding crosstache to your PATH..."
        $newPath = if ($userPath) { "$userPath;$Directory" } else { $Directory }
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        $env:Path = "$env:Path;$Directory"
        Write-Success "Added to PATH. You may need to restart your terminal."
    }
    else {
        Write-Info "Installation directory already in PATH"
    }
}

# Verify installation
function Test-Installation {
    param([string]$BinaryPath)
    
    if (-not (Test-Path $BinaryPath)) {
        Write-Error "Binary not found at $BinaryPath"
    }
    
    try {
        Write-Info "Verifying installation..."
        $versionOutput = & $BinaryPath --version 2>$null
        
        if ($LASTEXITCODE -eq 0) {
            Write-Success "crosstache installed successfully!"
            Write-Info "Installed version: $versionOutput"
            Write-Info "Binary location: $BinaryPath"
            Write-Info "You can now use 'xv' from any terminal."
        }
        else {
            Write-Warning "Binary installed but version check failed."
            Write-Info "You can try running: $BinaryPath --help"
        }
    }
    catch {
        Write-Warning "Installation verification failed: $_"
        Write-Info "You can try running: $BinaryPath --help"
    }
}

# Show usage information
function Show-Usage {
    Write-Host ""
    Write-Info "Quick Start:"
    Write-Host "  First, authenticate with Azure:"
    Write-Host "  az login" -ForegroundColor White
    Write-Host ""
    Write-Host "  Initialize with your Azure Key Vault:"
    Write-Host "  xv init --vault-name my-vault" -ForegroundColor White
    Write-Host ""
    Write-Host "  Set a secret:"
    Write-Host "  xv secret set secret-name `"secret-value`"" -ForegroundColor White
    Write-Host ""
    Write-Host "  Get a secret:"
    Write-Host "  xv secret get secret-name" -ForegroundColor White
    Write-Host ""
    Write-Host "  List secrets:"
    Write-Host "  xv secret list" -ForegroundColor White
    Write-Host ""
    Write-Info "Requirements:"
    Write-Host "  - Azure CLI (az) must be installed and authenticated" -ForegroundColor White
    Write-Host "  - Active Azure subscription with Key Vault access" -ForegroundColor White
    Write-Host ""
    Write-Info "For more information:"
    Write-Host "  xv --help" -ForegroundColor White
    Write-Host "  https://github.com/$GitHubRepo" -ForegroundColor Cyan
}

# Main installation function
function Install-crosstache {
    Write-Info "crosstache Installer for Windows"
    Write-Info "Repository: https://github.com/$GitHubRepo"
    Write-Host ""
    
    # Check and install Azure CLI if needed
    if (-not (Test-AzureCLI)) {
        Write-Warning "Azure CLI is not installed. crosstache requires Azure CLI for authentication."
        $response = Read-Host "Would you like to install Azure CLI now? (y/N)"
        if ($response -match '^[yY]([eE][sS])?$') {
            $installResult = Install-AzureCLI
            if (-not $installResult) {
                Write-Warning "Azure CLI installation failed or was skipped."
                Write-Warning "Please install Azure CLI manually from: https://docs.microsoft.com/en-us/cli/azure/install-azure-cli-windows"
                Write-Warning "crosstache will not work properly without Azure CLI."
            }
        }
        else {
            Write-Warning "Skipping Azure CLI installation."
            Write-Warning "Please install Azure CLI manually from: https://docs.microsoft.com/en-us/cli/azure/install-azure-cli-windows"
            Write-Warning "crosstache will not work properly without Azure CLI."
        }
    }
    
    # Determine version to install
    if ($Version -eq "latest") {
        $targetVersion = Get-LatestVersion
        Write-Info "Latest version: $targetVersion"
    }
    else {
        $targetVersion = $Version
    }
    
    # Clean version string
    $versionClean = $targetVersion -replace '^v', ''
    
    # Construct download URLs
    $archiveName = "xv-windows-x64.zip"
    $downloadUrl = "https://github.com/$GitHubRepo/releases/download/$targetVersion/$archiveName"
    $checksumUrl = "https://github.com/$GitHubRepo/releases/download/$targetVersion/$archiveName.sha256"
    
    Write-Info "Installing crosstache $targetVersion for Windows x64"
    Write-Info "Download URL: $downloadUrl"
    
    # Create temporary directory
    $tempDir = New-TemporaryFile | ForEach-Object { Remove-Item $_; New-Item -ItemType Directory -Path $_ }
    $archivePath = Join-Path $tempDir $archiveName
    $checksumPath = Join-Path $tempDir "$archiveName.sha256"
    
    try {
        # Download files
        Download-File -Url $downloadUrl -OutFile $archivePath -Description $archiveName
        
        try {
            Download-File -Url $checksumUrl -OutFile $checksumPath -Description "checksum"
            Test-Checksum -FilePath $archivePath -ChecksumPath $checksumPath
        }
        catch {
            Write-Warning "Could not download or verify checksum: $_"
        }
        
        # Create installation directory
        if (-not (Test-Path $InstallDir)) {
            Write-Info "Creating installation directory: $InstallDir"
            New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        }
        
        # Extract archive
        Write-Info "Extracting archive..."
        Expand-Archive -Path $archivePath -DestinationPath $tempDir -Force
        
        # Find and copy binary
        $extractedBinary = Get-ChildItem -Path $tempDir -Name $BinaryName -Recurse | Select-Object -First 1
        if (-not $extractedBinary) {
            Write-Error "Binary '$BinaryName' not found in archive"
        }
        
        $sourceBinary = Join-Path $tempDir $extractedBinary
        $targetBinary = Join-Path $InstallDir $BinaryName
        
        Write-Info "Installing binary to $targetBinary"
        Copy-Item -Path $sourceBinary -Destination $targetBinary -Force
        
        # Add to PATH
        Add-ToPath -Directory $InstallDir
        
        # Verify installation
        Test-Installation -BinaryPath $targetBinary
        
        # Show usage
        Show-Usage
        
        Write-Success "Installation completed successfully!"
    }
    finally {
        # Cleanup
        if (Test-Path $tempDir) {
            Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

# Check PowerShell version
if ($PSVersionTable.PSVersion.Major -lt 5) {
    Write-Error "PowerShell 5.0 or higher is required"
}

# Check if running as administrator (optional warning)
$isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole] "Administrator")
if ($isAdmin) {
    Write-Warning "Running as administrator. Consider running as a regular user for user-local installation."
}

# Run installation
try {
    Install-crosstache
}
catch {
    Write-Error "Installation failed: $_"
}