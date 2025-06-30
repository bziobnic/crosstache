#!/bin/bash

# crosstache (xv) installer for Unix systems (Linux/macOS)
# https://github.com/bziobnic/crosstache

set -e

# Configuration
GITHUB_REPO="bziobnic/crosstache"
BINARY_NAME="xv"
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
    
    archive_name="xv-${platform}.tar.gz"
    download_url="https://github.com/$GITHUB_REPO/releases/download/$version/$archive_name"
    checksum_url="https://github.com/$GITHUB_REPO/releases/download/$version/$archive_name.sha256"
    
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
        curl -sSL "$checksum_url" -o "$archive_name.sha256" 2>/dev/null || warning "Could not download checksum"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$download_url" -O "$archive_name" || error "Failed to download archive"
        wget -q "$checksum_url" -O "$archive_name.sha256" 2>/dev/null || warning "Could not download checksum"
    fi
    
    # Verify checksum if available
    if [ -f "$archive_name.sha256" ]; then
        info "Verifying checksum..."
        if command -v shasum >/dev/null 2>&1; then
            echo "$(cat $archive_name.sha256)  $archive_name" | shasum -a 256 -c - || error "Checksum verification failed"
        elif command -v sha256sum >/dev/null 2>&1; then
            echo "$(cat $archive_name.sha256)  $archive_name" | sha256sum -c - || error "Checksum verification failed"
        else
            warning "No checksum utility found, skipping verification"
        fi
    fi
    
    # Extract archive
    info "Extracting archive..."
    tar -xzf "$archive_name" || error "Failed to extract archive"
    
    # Create install directory
    mkdir -p "$INSTALL_DIR" || error "Failed to create installation directory: $INSTALL_DIR"
    
    # Install binary
    if [ -f "$BINARY_NAME" ]; then
        cp "$BINARY_NAME" "$INSTALL_DIR/" || error "Failed to copy binary to $INSTALL_DIR"
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
    info "For more information:"
    echo "  $BINARY_NAME --help"
    echo "  https://github.com/$GITHUB_REPO"
}

# Main installation flow
main() {
    info "crosstache Installer"
    info "Repository: https://github.com/$GITHUB_REPO"
    echo ""
    
    # Check dependencies
    if ! command -v tar >/dev/null 2>&1; then
        error "tar is required but not installed"
    fi
    
    if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
        error "Either curl or wget is required but neither is installed"
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