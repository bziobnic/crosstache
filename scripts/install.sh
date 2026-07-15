#!/bin/bash

# crosstache (xv) installer for Linux, macOS, and Windows bash
# environments (Git Bash / MSYS2 / Cygwin)
# https://github.com/bziobnic/crosstache

set -e

# Configuration
GITHUB_REPO="bziobnic/crosstache"
BINARY_NAME="xv"
RELEASE_SIGNING_KEY="RWRuXFh34rB613dgsXyAMmtKvYK0SFwxq4i44dhGFXVTrhAQ7hJXf6Ym"
INSTALL_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
VERSION="${1:-latest}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Print functions
info() {
    printf "${BLUE}[INFO]${NC} %s\n" "$1"
}

success() {
    printf "${GREEN}[SUCCESS]${NC} %s\n" "$1"
}

warning() {
    printf "${YELLOW}[WARNING]${NC} %s\n" "$1"
}

error() {
    printf "${RED}[ERROR]${NC} %s\n" "$1" >&2
    exit 1
}

# Check if Azure CLI is installed
check_azure_cli() {
    if command -v az >/dev/null 2>&1; then
        local version=$(az version --output tsv --query '"azure-cli"' 2>/dev/null || echo "unknown")
        info "Azure CLI is already installed (version: $version)"
        return 0
    else
        return 1
    fi
}

# Install Azure CLI for different platforms
install_azure_cli() {
    local os arch install_method
    
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    arch=$(uname -m)
    
    info "Installing Azure CLI..."
    
    case "$os" in
        linux*)
            # Check for package managers
            if command -v apt-get >/dev/null 2>&1; then
                install_method="apt"
            elif command -v yum >/dev/null 2>&1; then
                install_method="yum"
            elif command -v dnf >/dev/null 2>&1; then
                install_method="dnf"
            elif command -v zypper >/dev/null 2>&1; then
                install_method="zypper"
            else
                install_method="script"
            fi
            
            case "$install_method" in
                apt)
                    info "Installing Azure CLI via apt..."
                    curl -sL https://aka.ms/InstallAzureCLIDeb | sudo bash
                    ;;
                yum)
                    info "Installing Azure CLI via yum..."
                    sudo rpm --import https://packages.microsoft.com/keys/microsoft.asc
                    sudo sh -c 'echo -e "[azure-cli]\nname=Azure CLI\nbaseurl=https://packages.microsoft.com/yumrepos/azure-cli\nenabled=1\ngpgcheck=1\ngpgkey=https://packages.microsoft.com/keys/microsoft.asc" > /etc/yum.repos.d/azure-cli.repo'
                    sudo yum install azure-cli
                    ;;
                dnf)
                    info "Installing Azure CLI via dnf..."
                    sudo rpm --import https://packages.microsoft.com/keys/microsoft.asc
                    sudo sh -c 'echo -e "[azure-cli]\nname=Azure CLI\nbaseurl=https://packages.microsoft.com/yumrepos/azure-cli\nenabled=1\ngpgcheck=1\ngpgkey=https://packages.microsoft.com/keys/microsoft.asc" > /etc/yum.repos.d/azure-cli.repo'
                    sudo dnf install azure-cli
                    ;;
                zypper)
                    info "Installing Azure CLI via zypper..."
                    sudo rpm --import https://packages.microsoft.com/keys/microsoft.asc
                    sudo zypper addrepo --name 'Azure CLI' --check https://packages.microsoft.com/yumrepos/azure-cli azure-cli
                    sudo zypper install --from azure-cli azure-cli
                    ;;
                script)
                    info "Installing Azure CLI via install script..."
                    curl -L https://aka.ms/InstallAzureCli | bash
                    ;;
            esac
            ;;
        darwin*)
            # macOS
            if command -v brew >/dev/null 2>&1; then
                info "Installing Azure CLI via Homebrew..."
                brew install azure-cli
            else
                info "Homebrew not found. Installing Azure CLI via install script..."
                curl -L https://aka.ms/InstallAzureCli | bash
            fi
            ;;
        mingw*|msys*|cygwin*|windows_nt*)
            if command -v winget.exe >/dev/null 2>&1; then
                info "Installing Azure CLI via winget..."
                winget.exe install --exact --id Microsoft.AzureCLI || error "winget installation failed. Install Azure CLI manually from https://learn.microsoft.com/cli/azure/install-azure-cli-windows"
            else
                error "Automatic Azure CLI installation requires winget. Install Azure CLI manually from https://learn.microsoft.com/cli/azure/install-azure-cli-windows"
            fi
            ;;
        *)
            error "Unsupported operating system for automatic Azure CLI installation: $os"
            ;;
    esac
    
    # Verify installation
    if command -v az >/dev/null 2>&1; then
        local version=$(az version --output tsv --query '"azure-cli"' 2>/dev/null || echo "unknown")
        success "Azure CLI installed successfully (version: $version)"
    else
        error "Azure CLI installation failed. Please install it manually from https://docs.microsoft.com/en-us/cli/azure/install-azure-cli"
    fi
}

# Detect platform and architecture
detect_platform() {
    local os arch
    
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    arch=$(uname -m)
    
    case "$os" in
        linux*)
            case "$arch" in
                x86_64|amd64) echo "linux-x64" ;;
                *) error "Unsupported architecture: $arch" ;;
            esac
            ;;
        darwin*)
            case "$arch" in
                x86_64) echo "macos-intel" ;;
                arm64) echo "macos-apple-silicon" ;;
                *) error "Unsupported architecture: $arch" ;;
            esac
            ;;
        mingw*|msys*|cygwin*|windows_nt*)
            case "$arch" in
                x86_64|amd64) echo "windows-x64" ;;
                *) error "Unsupported architecture: $arch" ;;
            esac
            ;;
        *)
            error "Unsupported operating system: $os"
            ;;
    esac
}

# Get the latest release version from GitHub API
get_latest_version() {
    local api_url="https://api.github.com/repos/$GITHUB_REPO/releases/latest"
    
    if command -v curl >/dev/null 2>&1; then
        curl -s "$api_url" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/'
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "$api_url" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/'
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

# Download and extract binary
download_and_install() {
    local platform version download_url archive_name
    
    platform=$(detect_platform)
    if [ "$platform" = "windows-x64" ]; then
        BINARY_NAME="xv.exe"
    fi

    if [ "$VERSION" = "latest" ]; then
        version=$(get_latest_version)
        if [ -z "$version" ]; then
            error "Failed to fetch latest version"
        fi
    else
        version="$VERSION"
    fi
    
    # Remove 'v' prefix if present
    version_clean=${version#v}
    
    if [ "$platform" = "windows-x64" ]; then
        archive_name="xv-${platform}.zip"
    else
        archive_name="xv-${platform}.tar.gz"
    fi
    download_url="https://github.com/$GITHUB_REPO/releases/download/$version/$archive_name"
    checksum_url="https://github.com/$GITHUB_REPO/releases/download/$version/$archive_name.sha256"
    signature_url="https://github.com/$GITHUB_REPO/releases/download/$version/$archive_name.minisig"
    
    info "Installing crosstache $version for $platform"
    info "Download URL: $download_url"
    
    # Create temporary directory
    tmp_dir=$(mktemp -d)
    trap "rm -rf $tmp_dir" EXIT
    
    cd "$tmp_dir"
    
    # Download archive
    info "Downloading $archive_name..."
    if command -v curl >/dev/null 2>&1; then
        curl -sSL "$download_url" -o "$archive_name" || error "Failed to download archive"
        curl -sSL "$checksum_url" -o "$archive_name.sha256" 2>/dev/null || error "Failed to download checksum file. Refusing to install without verification."
        curl -sSL "$signature_url" -o "$archive_name.minisig" 2>/dev/null || error "Failed to download signature file. Refusing to install an unsigned release."
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$download_url" -O "$archive_name" || error "Failed to download archive"
        wget -q "$checksum_url" -O "$archive_name.sha256" 2>/dev/null || error "Failed to download checksum file. Refusing to install without verification."
        wget -q "$signature_url" -O "$archive_name.minisig" 2>/dev/null || error "Failed to download signature file. Refusing to install an unsigned release."
    fi

    if ! command -v minisign >/dev/null 2>&1; then
        error "minisign is required to authenticate releases. Install minisign (e.g. 'brew install minisign', 'apt install minisign', or 'scoop install minisign' on Windows) and retry."
    fi
    if ! minisign -Vm "$archive_name" -x "$archive_name.minisig" -P "$RELEASE_SIGNING_KEY" >/dev/null; then
        error "Release signature verification failed. Refusing to install."
    fi
    # Windows-built minisig files have CRLF line endings; strip the CR so the
    # comparison below doesn't fail on an invisible trailing '\r'.
    trusted_comment=$(sed -n '3s/^trusted comment: //p' "$archive_name.minisig" | tr -d '\r')
    if [ "$trusted_comment" != "crosstache $version" ]; then
        error "Release signature belongs to '$trusted_comment', not 'crosstache $version'. Refusing a replayed archive."
    fi
    info "Release signature verified"

    # Verify checksum. Verification is mandatory: installing an unverified
    # archive is never acceptable, so every failure path below is fatal.
    info "Verifying checksum..."

    if [ ! -s "$archive_name.sha256" ]; then
        error "Checksum file is missing or empty. Refusing to install without verification."
    fi

    expected_checksum=$(cat "$archive_name.sha256" | tr -d '\r\n' | awk '{print $1}')

    if [ -z "$expected_checksum" ]; then
        error "Could not read checksum from file. Refusing to install without verification."
    fi

    if command -v shasum >/dev/null 2>&1; then
        actual_checksum=$(shasum -a 256 "$archive_name" | awk '{print $1}')
    elif command -v sha256sum >/dev/null 2>&1; then
        actual_checksum=$(sha256sum "$archive_name" | awk '{print $1}')
    else
        error "No checksum utility (shasum or sha256sum) found. Refusing to install without verification."
    fi

    if [ "$expected_checksum" = "$actual_checksum" ]; then
        info "Checksum verification passed"
    else
        error "Checksum verification failed. Expected: $expected_checksum, Got: $actual_checksum"
    fi
    
    # Extract archive
    info "Extracting archive..."
    case "$archive_name" in
        *.zip)
            if command -v unzip >/dev/null 2>&1; then
                unzip -oq "$archive_name" || error "Failed to extract archive"
            elif command -v powershell.exe >/dev/null 2>&1 && command -v cygpath >/dev/null 2>&1; then
                powershell.exe -NoProfile -Command "Expand-Archive -Force -Path '$(cygpath -w "$archive_name")' -DestinationPath '$(cygpath -w .)'" || error "Failed to extract archive"
            else
                error "unzip is required to extract the archive but was not found"
            fi
            ;;
        *)
            tar -xzf "$archive_name" || error "Failed to extract archive"
            ;;
    esac
    
    # Create install directory
    mkdir -p "$INSTALL_DIR" || error "Failed to create installation directory: $INSTALL_DIR"
    
    # Install binary
    if [ -f "$BINARY_NAME" ]; then
        cp -f "$BINARY_NAME" "$INSTALL_DIR/" || error "Failed to copy binary to $INSTALL_DIR"
        chmod +x "$INSTALL_DIR/$BINARY_NAME" || error "Failed to make binary executable"
        
        # On macOS, remove quarantine attribute to avoid "could not verify" error
        if [[ "$OSTYPE" == "darwin"* ]]; then
            info "Removing macOS quarantine attribute..."
            xattr -d com.apple.quarantine "$INSTALL_DIR/$BINARY_NAME" 2>/dev/null || true
        fi
    else
        error "Binary not found in archive"
    fi
}

# Check if installation directory is in PATH
check_path() {
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            return 0 # Already in PATH
            ;;
        *)
            return 1 # Not in PATH
            ;;
    esac
}

# Suggest PATH modification
suggest_path_modification() {
    local shell_name rc_file
    
    shell_name=$(basename "$SHELL")
    
    case "$shell_name" in
        bash)
            if [ -f "$HOME/.bash_profile" ]; then
                rc_file="$HOME/.bash_profile"
            else
                rc_file="$HOME/.bashrc"
            fi
            ;;
        zsh)
            rc_file="$HOME/.zshrc"
            ;;
        fish)
            rc_file="$HOME/.config/fish/config.fish"
            ;;
        *)
            rc_file=""
            ;;
    esac
    
    warning "$INSTALL_DIR is not in your PATH."
    echo ""
    
    if [ -n "$rc_file" ]; then
        echo "Add the following line to your $rc_file:"
        echo "export PATH=\"$INSTALL_DIR:\$PATH\""
    else
        echo "Add $INSTALL_DIR to your PATH environment variable."
    fi
    
    echo ""
    echo "For this session, you can run:"
    echo "export PATH=\"$INSTALL_DIR:\$PATH\""
    
    # Additional macOS note
    if [[ "$OSTYPE" == "darwin"* ]]; then
        echo ""
        warning "Note: If you get 'cannot be opened because the developer cannot be verified':"
        echo "Right-click the binary in Finder and select 'Open', then click 'Open' again."
    fi
}

# Verify installation
verify_installation() {
    local installed_version
    
    if [ -x "$INSTALL_DIR/$BINARY_NAME" ]; then
        # Test if binary runs and get version
        if installed_version=$("$INSTALL_DIR/$BINARY_NAME" --version 2>/dev/null); then
            success "crosstache installed successfully!"
            info "Installed version: $installed_version"
            info "Binary location: $INSTALL_DIR/$BINARY_NAME"
            
            if check_path; then
                info "You can now use '$BINARY_NAME' from any terminal."
            else
                suggest_path_modification
            fi
        else
            warning "Binary installed but version check failed."
            info "You can try running: $INSTALL_DIR/$BINARY_NAME --help"
        fi
    else
        error "Installation verification failed. Binary not found or not executable."
    fi
}

# Display usage information
show_usage() {
    echo ""
    info "Quick Start:"
    echo "  First, authenticate with Azure:"
    echo "  az login"
    echo ""
    echo "  Initialize with your Azure Key Vault:"
    echo "  $BINARY_NAME init --vault-name my-vault"
    echo ""
    echo "  Set a secret:"
    echo "  $BINARY_NAME secret set secret-name \"secret-value\""
    echo ""
    echo "  Get a secret:"
    echo "  $BINARY_NAME secret get secret-name"
    echo ""
    echo "  List secrets:"
    echo "  $BINARY_NAME secret list"
    echo ""
    info "Requirements:"
    echo "  - Azure CLI (az) must be installed and authenticated"
    echo "  - Active Azure subscription with Key Vault access"
    echo ""
    info "For more information:"
    echo "  $BINARY_NAME --help"
    echo "  https://github.com/$GITHUB_REPO"
}

# Main installation flow
main() {
    info "crosstache Installer"
    info "Repository: https://github.com/$GITHUB_REPO"
    echo ""
    
    # Check dependencies (Windows archives are .zip; everything else .tar.gz)
    if [ "$(detect_platform)" = "windows-x64" ]; then
        if ! command -v unzip >/dev/null 2>&1 && ! command -v powershell.exe >/dev/null 2>&1; then
            error "unzip (or powershell.exe) is required but not installed"
        fi
    elif ! command -v tar >/dev/null 2>&1; then
        error "tar is required but not installed"
    fi
    
    if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
        error "Either curl or wget is required but neither is installed"
    fi
    
    # Check and install Azure CLI if needed
    if ! check_azure_cli; then
        warning "Azure CLI is not installed. crosstache requires Azure CLI for authentication."
        echo "Would you like to install Azure CLI now? (y/N)"
        read -r response </dev/tty
        case "$response" in
            [yY][eE][sS]|[yY])
                install_azure_cli
                ;;
            *)
                warning "Skipping Azure CLI installation."
                warning "Please install Azure CLI manually from: https://docs.microsoft.com/en-us/cli/azure/install-azure-cli"
                warning "crosstache will not work properly without Azure CLI."
                ;;
        esac
    fi
    
    # Perform installation
    download_and_install
    verify_installation
    show_usage
}

# Handle command line arguments
case "${1:-}" in
    -h|--help)
        echo "Usage: $0 [VERSION]"
        echo ""
        echo "Install crosstache CLI tool"
        echo ""
        echo "Arguments:"
        echo "  VERSION    Specific version to install (default: latest)"
        echo ""
        echo "Examples:"
        echo "  $0              # Install latest version"
        echo "  $0 v0.1.0       # Install specific version"
        exit 0
        ;;
    *)
        main "$@"
        ;;
esac
