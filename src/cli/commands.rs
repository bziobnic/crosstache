//! CLI commands and argument parsing
//!
//! This module defines the command-line interface structure using clap,
//! including all commands, subcommands, and their arguments.

#[cfg(feature = "file-ops")]
use crate::blob::manager::{create_blob_manager, BlobManager};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use crate::vault::{VaultCreateRequest, VaultManager};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

// Include the built information generated at compile time
pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

/// Get the full version string with build information
fn get_version() -> &'static str {
    built_info::PKG_VERSION
}

/// Determine if options should be hidden based on environment or command line
fn should_hide_options() -> bool {
    // Check if --show-options is present in command line args
    !std::env::args().any(|arg| arg == "--show-options")
}

/// Get the help template based on whether options should be shown
fn get_help_template() -> &'static str {
    if std::env::args().any(|arg| arg == "--show-options") {
        // Full template with options
        "{about-with-newline}\
Usage: {usage}\n\n\
Commands:\n{subcommands}\n\
Options:\n{options}\n\
Use 'xv help <command>' for more information about a specific command.\n"
    } else {
        // Minimal template without options
        "{about-with-newline}\
Usage: {usage}\n\n\
Commands:\n{subcommands}\n\
Options:\n\
  -h, --help       Print help (see more with '--show-options')\n\
  -V, --version    Print version\n\n\
Use 'xv help <command>' for more information about a specific command.\n\
Use 'xv --help --show-options' to see all global options.\n"
    }
}

/// Get build information for display
pub fn get_build_info() -> BuildInfo {
    BuildInfo {
        version: built_info::PKG_VERSION,
        git_hash: built_info::GIT_COMMIT_HASH_SHORT.unwrap_or("unknown"),
        git_branch: built_info::GIT_HEAD_REF
            .map(|r| r.strip_prefix("refs/heads/").unwrap_or(r))
            .unwrap_or("unknown"),
    }
}

#[derive(Debug)]
pub struct BuildInfo {
    pub version: &'static str,
    pub git_hash: &'static str,
    pub git_branch: &'static str,
}

#[derive(Parser)]
#[command(name = "xv")]
#[command(about = "A comprehensive tool for managing Azure Key Vault")]
#[command(version = get_version(), author)]
#[command(help_template = get_help_template())]
pub struct Cli {
    /// Enable debug logging
    #[arg(long, global = true, hide = should_hide_options())]
    pub debug: bool,

    /// Output format
    #[arg(long, global = true, value_enum, default_value = "table", hide = should_hide_options())]
    pub format: OutputFormat,

    /// Custom template string for template format
    #[arg(long, global = true, hide = should_hide_options())]
    pub template: Option<String>,

    /// Select specific columns for table output (comma-separated)
    #[arg(long, global = true, hide = should_hide_options())]
    pub columns: Option<String>,

    /// Azure credential type to use first (cli, managed_identity, environment, default)
    #[arg(
        long,
        global = true,
        value_name = "TYPE",
        help = "Azure credential type to use first (cli, managed_identity, environment, default)",
        env = "AZURE_CREDENTIAL_PRIORITY",
        hide = should_hide_options()
    )]
    pub credential_type: Option<String>,

    /// Show global options in help output
    #[arg(long)]
    pub show_options: bool,

    #[command(subcommand)]
    pub command: Commands,
}

/// Resource type for the info command
#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
pub enum ResourceType {
    /// Azure Key Vault
    Vault,
    /// Key Vault Secret
    Secret,
    /// Blob Storage File
    #[cfg(feature = "file-ops")]
    File,
}

impl std::fmt::Display for ResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResourceType::Vault => write!(f, "vault"),
            ResourceType::Secret => write!(f, "secret"),
            #[cfg(feature = "file-ops")]
            ResourceType::File => write!(f, "file"),
        }
    }
}

/// Character set type for secret rotation
#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
pub enum CharsetType {
    /// Alphanumeric characters (A-Z, a-z, 0-9)
    Alphanumeric,
    /// Alphanumeric with symbols
    AlphanumericSymbols,
    /// Hexadecimal (0-9, A-F)
    Hex,
    /// Base64 characters (A-Z, a-z, 0-9, +, /)
    Base64,
    /// Numeric only (0-9)
    Numeric,
    /// Uppercase letters only (A-Z)
    Uppercase,
    /// Lowercase letters only (a-z)
    Lowercase,
}

impl CharsetType {
    /// Get the character set string for this type
    pub fn chars(&self) -> &'static str {
        match self {
            CharsetType::Alphanumeric => "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            CharsetType::AlphanumericSymbols => "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()_+-=[]{}|;:,.<>?",
            CharsetType::Hex => "0123456789ABCDEF",
            CharsetType::Base64 => "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
            CharsetType::Numeric => "0123456789",
            CharsetType::Uppercase => "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            CharsetType::Lowercase => "abcdefghijklmnopqrstuvwxyz",
        }
    }
}

#[derive(Subcommand)]
pub enum Commands {
    /// Set a secret in the current vault context
    Set {
        /// Secret name (for single secret) or multiple KEY=value pairs (for bulk set)
        /// Supports @/path/to/file to load value from file (e.g., KEY=@/path/to/file)
        #[arg(required = true, num_args = 1..)]
        args: Vec<String>,
        /// Read value from stdin instead of prompting (only for single secret)
        #[arg(long)]
        stdin: bool,
        /// Note to attach to the secret(s)
        #[arg(long)]
        note: Option<String>,
        /// Folder path for the secret(s) (e.g., 'app/database', 'config/dev')
        #[arg(long)]
        folder: Option<String>,
        /// Set expiration date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS)
        #[arg(long)]
        expires: Option<String>,
        /// Set not-before date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS)
        #[arg(long)]
        not_before: Option<String>,
    },
    /// Get a secret from the current vault context
    Get {
        /// Secret name
        name: String,
        /// Raw output (print value instead of copying to clipboard)
        #[arg(short, long)]
        raw: bool,
        /// Get a specific version of the secret
        #[arg(long)]
        version: Option<String>,
    },
    /// List secrets in the current vault context (alias: ls)
    #[command(alias = "ls")]
    List {
        /// Filter by group
        #[arg(short, long)]
        group: Option<String>,
        /// Show all secrets including disabled ones
        #[arg(long)]
        all: bool,
        /// Show secrets expiring within specified period (e.g., 30d, 7d, 1h)
        #[arg(long)]
        expiring: Option<String>,
        /// Show expired secrets only
        #[arg(long)]
        expired: bool,
    },
    /// Delete a secret from the current vault context (alias: rm)
    #[command(alias = "rm")]
    Delete {
        /// Secret name (mutually exclusive with --group)
        name: Option<String>,
        /// Delete all secrets in the specified group (mutually exclusive with name)
        #[arg(long, conflicts_with = "name")]
        group: Option<String>,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Show version history of a secret
    History {
        /// Secret name
        name: String,
    },
    /// Rollback a secret to a previous version
    Rollback {
        /// Secret name
        name: String,
        /// Version ID to rollback to
        #[arg(long)]
        version: String,
        /// Force rollback without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Rotate a secret with a new random value
    Rotate {
        /// Secret name
        name: String,
        /// Length of the generated value (default: 32)
        #[arg(long, default_value = "32")]
        length: usize,
        /// Character set to use for generation
        #[arg(long, value_enum, default_value = "alphanumeric")]
        charset: CharsetType,
        /// Custom generator script path (overrides charset and length)
        #[arg(long)]
        generator: Option<String>,
        /// Show the generated value (default: hidden for security)
        #[arg(long)]
        show_value: bool,
        /// Force rotation without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Run a command with secrets injected as environment variables
    Run {
        /// Filter secrets by group (can be specified multiple times)
        #[arg(short, long)]
        group: Vec<String>,
        /// Disable masking of secret values in output
        #[arg(long)]
        no_masking: bool,
        /// Command and arguments to run
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Inject secrets into a template file using {{ secret:name }} syntax
    Inject {
        /// Template file path (reads from stdin if not specified)
        #[arg(short, long)]
        template: Option<String>,
        /// Output file path (writes to stdout if not specified)
        #[arg(short, long)]
        out: Option<String>,
        /// Filter secrets by group (can be specified multiple times)
        #[arg(short, long)]
        group: Vec<String>,
    },
    /// Update secret properties in the current vault context
    Update {
        /// Secret name
        name: String,
        /// New value (if not provided, will prompt)
        value: Option<String>,
        /// Read value from stdin
        #[arg(long)]
        stdin: bool,
        /// Tags for the secret in key=value format
        #[arg(short, long, value_parser = parse_key_val::<String, String>)]
        tags: Vec<(String, String)>,
        /// Groups for the secret (can be specified multiple times)
        #[arg(short, long)]
        group: Vec<String>,
        /// New name for the secret (rename operation)
        #[arg(long)]
        rename: Option<String>,
        /// Note to attach to the secret
        #[arg(long)]
        note: Option<String>,
        /// Folder path for the secret (e.g., 'app/database', 'config/dev')
        #[arg(long)]
        folder: Option<String>,
        /// Replace existing tags instead of merging
        #[arg(long)]
        replace_tags: bool,
        /// Replace existing groups instead of merging
        #[arg(long)]
        replace_groups: bool,
        /// Set expiration date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS)
        #[arg(long)]
        expires: Option<String>,
        /// Set not-before date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS)
        #[arg(long)]
        not_before: Option<String>,
        /// Clear expiration date
        #[arg(long, conflicts_with = "expires")]
        clear_expires: bool,
        /// Clear not-before date
        #[arg(long, conflicts_with = "not_before")]
        clear_not_before: bool,
    },
    /// Compare secrets between two vaults
    Diff {
        /// First vault name
        vault1: String,
        /// Second vault name
        vault2: String,
        /// Show actual secret values in diff output
        #[arg(long)]
        show_values: bool,
        /// Filter by group in both vaults
        #[arg(short, long)]
        group: Option<String>,
    },
    /// Copy a secret from one vault to another
    Copy {
        /// Secret name
        name: String,
        /// Source vault name
        #[arg(long, required = true)]
        from: String,
        /// Destination vault name
        #[arg(long, required = true)]
        to: String,
        /// New name for the secret in the destination vault (optional, defaults to original name)
        #[arg(long)]
        new_name: Option<String>,
    },
    /// Move a secret from one vault to another (copy then delete from source)
    Move {
        /// Secret name
        name: String,
        /// Source vault name
        #[arg(long, required = true)]
        from: String,
        /// Destination vault name
        #[arg(long, required = true)]
        to: String,
        /// New name for the secret in the destination vault (optional, defaults to original name)
        #[arg(long)]
        new_name: Option<String>,
        /// Force move without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Permanently delete (purge) a secret from the current vault context
    Purge {
        /// Secret name
        name: String,
        /// Force purge without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Restore a deleted secret in the current vault context
    Restore {
        /// Secret name
        name: String,
    },
    /// Parse connection strings (vault-independent utility)
    Parse {
        /// Connection string to parse
        connection_string: String,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: String,
    },
    /// Secret-level access management
    Share {
        #[command(subcommand)]
        command: ShareCommands,
    },
    /// Vault management commands
    Vault {
        #[command(subcommand)]
        command: VaultCommands,
    },
    /// File management commands
    #[cfg(feature = "file-ops")]
    File {
        #[command(subcommand)]
        command: FileCommands,
    },
    /// Configuration management commands
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Vault context management (alias: cx)
    #[command(alias = "cx")]
    Context {
        #[command(subcommand)]
        command: ContextCommands,
    },
    /// Environment profile management
    Env {
        #[command(subcommand)]
        command: EnvCommands,
    },
    /// Show audit history for secrets or vaults
    Audit {
        /// Secret name to show audit history for (exclusive with --vault)
        name: Option<String>,
        /// Show audit history for entire vault
        #[arg(long, conflicts_with = "name")]
        vault: Option<String>,
        /// Number of days to look back (default: 30)
        #[arg(long, default_value = "30")]
        days: u32,
        /// Filter by operation type (get, set, delete, list)
        #[arg(long)]
        operation: Option<String>,
        /// Show raw Azure Activity Log output
        #[arg(long)]
        raw: bool,
    },
    /// Initialize default configuration
    Init,
    /// Show information about a resource (vault, secret, or file)
    Info {
        /// Resource identifier (vault name, secret name, or file name)
        resource: String,
        /// Explicitly specify resource type (auto-detects if not specified)
        #[arg(short = 't', long = "type", value_enum)]
        resource_type: Option<ResourceType>,
        /// Resource group (required for vaults)
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Subscription ID (optional for vaults)
        #[arg(short, long)]
        subscription: Option<String>,
    },
    /// Show detailed version and build information
    Version,
    /// Generate shell completion scripts
    Completion {
        /// Shell type
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Show authenticated identity and context information
    Whoami,
    /// Quick file upload (alias for file upload)
    #[cfg(feature = "file-ops")]
    #[command(alias = "up")]
    Upload {
        /// Local file path
        file_path: String,
        /// Remote name (optional, defaults to filename)
        #[arg(long)]
        name: Option<String>,
        /// Groups to assign to the file
        #[arg(long)]
        groups: Option<String>,
        /// Additional metadata (key=value pairs)
        #[arg(long)]
        metadata: Vec<String>,
    },
    /// Quick file download (alias for file download)
    #[cfg(feature = "file-ops")]
    #[command(alias = "down")]
    Download {
        /// Remote file name
        name: String,
        /// Local output path (optional, defaults to current directory)
        #[arg(long)]
        output: Option<String>,
        /// Open file after download
        #[arg(long)]
        open: bool,
    },
}

#[derive(Subcommand)]
pub enum VaultCommands {
    /// Create a new vault
    Create {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Location
        #[arg(short, long)]
        location: Option<String>,
    },
    /// List vaults
    List {
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Output format
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Delete a vault
    Delete {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Show vault information
    Info {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
    },
    /// Restore a soft-deleted vault
    Restore {
        /// Vault name
        name: String,
        /// Location (region) where the vault was deleted
        #[arg(short, long)]
        location: String,
    },
    /// Permanently purge a soft-deleted vault
    Purge {
        /// Vault name
        name: String,
        /// Location (region) where the vault was deleted
        #[arg(short, long)]
        location: String,
        /// Force purge without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Export vault secrets to a file
    Export {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Output file path (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
        /// Export format (json, env, txt)
        #[arg(short, long, default_value = "json")]
        format: String,
        /// Include secret values (requires appropriate permissions)
        #[arg(long)]
        include_values: bool,
        /// Filter by secret group
        #[arg(short, long)]
        group: Option<String>,
    },
    /// Import secrets from a file
    Import {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Input file path (default: stdin)
        #[arg(short, long)]
        input: Option<String>,
        /// Import format (json, env, txt)
        #[arg(short, long, default_value = "json")]
        format: String,
        /// Overwrite existing secrets
        #[arg(long)]
        overwrite: bool,
        /// Dry run (show what would be imported)
        #[arg(long)]
        dry_run: bool,
    },
    /// Update vault properties and tags
    Update {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Add or update tags (key=value format)
        #[arg(long, value_parser = parse_key_val::<String, String>)]
        tag: Vec<(String, String)>,
        /// Enable vault for deployment
        #[arg(long)]
        enable_deployment: Option<bool>,
        /// Enable vault for disk encryption
        #[arg(long)]
        enable_disk_encryption: Option<bool>,
        /// Enable vault for template deployment
        #[arg(long)]
        enable_template_deployment: Option<bool>,
        /// Enable purge protection
        #[arg(long)]
        enable_purge_protection: Option<bool>,
        /// Soft delete retention in days (7-90)
        #[arg(long)]
        retention_days: Option<i32>,
    },
    /// Vault-level access management
    Share {
        #[command(subcommand)]
        command: VaultShareCommands,
    },
}

#[derive(Subcommand)]
pub enum VaultShareCommands {
    /// Grant access to a vault
    Grant {
        /// Vault name
        vault_name: String,
        /// User email or service principal ID
        user: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Access level (reader, contributor, admin)
        #[arg(short, long, default_value = "reader")]
        level: String,
    },
    /// Revoke access to a vault
    Revoke {
        /// Vault name
        vault_name: String,
        /// User email or service principal ID
        user: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
    },
    /// List vault access assignments
    List {
        /// Vault name
        vault_name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: String,
    },
}

#[cfg(feature = "file-ops")]
#[derive(Subcommand)]
pub enum FileCommands {
    /// Upload one or more files to blob storage
    Upload {
        /// Local file path(s) to upload
        #[arg(required = true, num_args = 1..)]
        files: Vec<String>,
        /// Remote name (only valid when uploading single file)
        #[arg(short, long)]
        name: Option<String>,
        /// Upload directory recursively
        #[arg(short = 'r', long)]
        recursive: bool,
        /// Flatten directory structure (upload all files to container root)
        #[arg(long, requires = "recursive")]
        flatten: bool,
        /// Prefix to add to all uploaded blob names
        #[arg(long)]
        prefix: Option<String>,
        /// Groups to assign to the file(s)
        #[arg(short, long)]
        group: Vec<String>,
        /// Metadata key-value pairs
        #[arg(short, long, value_parser = parse_key_val::<String, String>)]
        metadata: Vec<(String, String)>,
        /// Tags key-value pairs
        #[arg(short, long, value_parser = parse_key_val::<String, String>)]
        tag: Vec<(String, String)>,
        /// Content type override (only valid for single file)
        #[arg(long)]
        content_type: Option<String>,
        /// Show progress during upload
        #[arg(long)]
        progress: bool,
        /// Continue on error when uploading multiple files
        #[arg(long)]
        continue_on_error: bool,
    },
    /// Download one or more files from blob storage
    Download {
        /// Remote file name(s) or prefix patterns to download
        #[arg(required = true, num_args = 1..)]
        files: Vec<String>,
        /// Local output path (optional, defaults to current directory)
        #[arg(short, long)]
        output: Option<String>,
        /// Rename file (only valid for single file download)
        #[arg(long)]
        rename: Option<String>,
        /// Download all files matching prefix recursively
        #[arg(short = 'r', long)]
        recursive: bool,
        /// Flatten directory structure (download all files to output root)
        #[arg(long, requires = "recursive")]
        flatten: bool,
        /// Stream download for large files
        #[arg(long)]
        stream: bool,
        /// Force overwrite if file exists
        #[arg(short, long)]
        force: bool,
        /// Continue on error when downloading multiple files
        #[arg(long)]
        continue_on_error: bool,
    },
    /// List files in blob storage
    ///
    /// By default, lists only immediate children (files and directories) at the
    /// current prefix level. Use --recursive to list all files recursively.
    ///
    /// Directories are shown with a trailing '/' character and listed first.
    #[command(alias = "ls")]
    List {
        /// Filter by prefix
        #[arg(short, long)]
        prefix: Option<String>,
        /// Filter by group
        #[arg(short, long)]
        group: Option<String>,
        /// Include metadata in output
        #[arg(long)]
        metadata: bool,
        /// Maximum number of results
        #[arg(long)]
        limit: Option<usize>,
        /// List all files recursively (show all nested files instead of directory structure)
        #[arg(short, long)]
        recursive: bool,
    },
    /// Delete one or more files from blob storage
    #[command(alias = "rm")]
    Delete {
        /// Remote file name(s) to delete
        #[arg(required = true, num_args = 1..)]
        files: Vec<String>,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
        /// Continue on error when deleting multiple files
        #[arg(long)]
        continue_on_error: bool,
    },
    /// Get file information
    Info {
        /// Remote file name
        name: String,
    },
    /// Sync files between local and remote
    Sync {
        /// Local directory path
        local_path: String,
        /// Remote prefix (optional)
        #[arg(short, long)]
        prefix: Option<String>,
        /// Direction: upload, download, or both
        #[arg(short, long, default_value = "up")]
        direction: SyncDirection,
        /// Dry run (show what would be done)
        #[arg(long)]
        dry_run: bool,
        /// Delete remote files not in local
        #[arg(long)]
        delete: bool,
    },
}

#[cfg(feature = "file-ops")]
#[derive(Clone, Debug, clap::ValueEnum)]
pub enum SyncDirection {
    Up,
    Down,
    Both,
}

#[derive(Subcommand)]
pub enum ShareCommands {
    /// Grant access to a secret in the current vault context
    Grant {
        /// Secret name
        secret_name: String,
        /// User email or service principal ID
        user: String,
        /// Access level (read, write, admin)
        #[arg(short, long, default_value = "read")]
        level: String,
    },
    /// Revoke access to a secret in the current vault context
    Revoke {
        /// Secret name
        secret_name: String,
        /// User email or service principal ID
        user: String,
    },
    /// List access permissions for a secret in the current vault context
    List {
        /// Secret name
        secret_name: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show current configuration
    Show,
    /// Set a configuration value
    Set {
        /// Setting name
        key: String,
        /// Setting value
        value: String,
    },
    /// Show configuration file path
    Path,
}

#[derive(Subcommand)]
pub enum ContextCommands {
    /// Show current vault context
    Show,
    /// Switch to a vault context
    Use {
        /// Vault name
        vault_name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Make this the global default
        #[arg(long)]
        global: bool,
        /// Set for current directory only
        #[arg(long)]
        local: bool,
    },
    /// List recent vault contexts
    List,
    /// Clear current context
    Clear {
        /// Clear global context
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
pub enum EnvCommands {
    /// List available environment profiles
    List,
    /// Use an environment profile (sets vault and group context)
    Use {
        /// Profile name
        name: String,
    },
    /// Create a new environment profile
    Create {
        /// Profile name
        name: String,
        /// Vault name for this profile
        #[arg(long)]
        vault: String,
        /// Resource group for the vault
        #[arg(long)]
        group: String,
        /// Subscription ID (optional)
        #[arg(long)]
        subscription: Option<String>,
        /// Set this profile as global default
        #[arg(long)]
        global: bool,
    },
    /// Delete an environment profile
    Delete {
        /// Profile name
        name: String,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Show current environment profile
    Show,
    /// Pull secrets to .env file format
    Pull {
        /// Output format (only 'dotenv' supported currently)
        #[arg(long, default_value = "dotenv")]
        format: String,
        /// Filter secrets by group (can be specified multiple times)
        #[arg(short, long)]
        group: Vec<String>,
        /// Output file path (writes to stdout if not specified)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Push .env file to vault as secrets
    Push {
        /// Input .env file path (reads from stdin if not specified)
        file: Option<String>,
        /// Overwrite existing secrets
        #[arg(long)]
        overwrite: bool,
    },
}

impl Cli {
    pub async fn execute(self, mut config: Config) -> Result<()> {
        // Apply CLI credential type if specified (CLI flag overrides config/env)
        if let Some(cred_type) = self.credential_type {
            use crate::config::settings::AzureCredentialType;
            use std::str::FromStr;

            config.azure_credential_priority =
                AzureCredentialType::from_str(&cred_type).map_err(CrosstacheError::config)?;
        }

        match self.command {
            Commands::Set {
                args,
                stdin,
                note,
                folder,
                expires,
                not_before,
            } => {
                execute_secret_set_direct(args, stdin, note, folder, expires, not_before, config)
                    .await
            }
            Commands::Get { name, raw, version } => {
                execute_secret_get_direct(&name, raw, version, config).await
            }
            Commands::List {
                group,
                all,
                expiring,
                expired,
            } => execute_secret_list_direct(group, all, expiring, expired, config).await,
            Commands::Delete { name, group, force } => {
                execute_secret_delete_direct(name, group, force, config).await
            }
            Commands::History { name } => execute_secret_history_direct(&name, config).await,
            Commands::Rollback {
                name,
                version,
                force,
            } => execute_secret_rollback_direct(&name, &version, force, config).await,
            Commands::Rotate {
                name,
                length,
                charset,
                generator,
                show_value,
                force,
            } => {
                execute_secret_rotate_direct(
                    &name, length, charset, generator, show_value, force, config,
                )
                .await
            }
            Commands::Run {
                group,
                no_masking,
                command,
            } => execute_secret_run_direct(group, no_masking, command, config).await,
            Commands::Inject {
                template,
                out,
                group,
            } => execute_secret_inject_direct(template, out, group, config).await,
            Commands::Update {
                name,
                value,
                stdin,
                tags,
                group,
                rename,
                note,
                folder,
                replace_tags,
                replace_groups,
                expires,
                not_before,
                clear_expires,
                clear_not_before,
            } => {
                execute_secret_update_direct(
                    &name,
                    value,
                    stdin,
                    tags,
                    group,
                    rename,
                    note,
                    folder,
                    replace_tags,
                    replace_groups,
                    expires,
                    not_before,
                    clear_expires,
                    clear_not_before,
                    config,
                )
                .await
            }
            Commands::Diff {
                vault1,
                vault2,
                show_values,
                group,
            } => execute_diff_command(&vault1, &vault2, show_values, group, config).await,
            Commands::Copy {
                name,
                from,
                to,
                new_name,
            } => execute_secret_copy_direct(&name, &from, &to, new_name, config).await,
            Commands::Move {
                name,
                from,
                to,
                new_name,
                force,
            } => execute_secret_move_direct(&name, &from, &to, new_name, force, config).await,
            Commands::Purge { name, force } => {
                execute_secret_purge_direct(&name, force, config).await
            }
            Commands::Restore { name } => execute_secret_restore_direct(&name, config).await,
            Commands::Parse {
                connection_string,
                format,
            } => execute_secret_parse_direct(&connection_string, &format, config).await,
            Commands::Share { command } => execute_secret_share_direct(command, config).await,
            Commands::Vault { command } => execute_vault_command(command, config).await,
            #[cfg(feature = "file-ops")]
            Commands::File { command } => execute_file_command(command, config).await,
            Commands::Config { command } => execute_config_command(command, config).await,
            Commands::Context { command } => execute_context_command(command, config).await,
            Commands::Env { command } => execute_env_command(command, config).await,
            Commands::Audit {
                name,
                vault,
                days,
                operation,
                raw,
            } => execute_audit_command(name, vault, days, operation, raw, config).await,
            Commands::Init => execute_init_command(config).await,
            Commands::Info {
                resource,
                resource_type,
                resource_group,
                subscription,
            } => {
                execute_info_command(
                    resource,
                    resource_type,
                    resource_group,
                    subscription,
                    config,
                )
                .await
            }
            Commands::Version => execute_version_command().await,
            Commands::Completion { shell } => execute_completion_command(shell).await,
            Commands::Whoami => execute_whoami_command(config).await,
            #[cfg(feature = "file-ops")]
            Commands::Upload {
                file_path,
                name,
                groups,
                metadata,
            } => execute_file_upload_quick(&file_path, name, groups, metadata, &config).await,
            #[cfg(feature = "file-ops")]
            Commands::Download { name, output, open } => {
                execute_file_download_quick(&name, output, open, &config).await
            }
        }
    }
}

#[cfg(feature = "file-ops")]
async fn execute_file_command(command: FileCommands, config: Config) -> Result<()> {
    // Create blob manager
    let blob_manager = create_blob_manager(&config).map_err(|e| {
        if e.to_string().contains("No storage account configured") {
            CrosstacheError::config(
                "No blob storage configured. Run 'xv init' to set up blob storage.",
            )
        } else {
            e
        }
    })?;

    match command {
        FileCommands::Upload {
            files,
            name,
            recursive,
            flatten,
            prefix,
            group,
            metadata,
            tag,
            content_type,
            progress,
            continue_on_error,
        } => {
            // Handle recursive directory upload
            if recursive {
                // Validate that --name and --content-type are not used with --recursive
                if name.is_some() || content_type.is_some() {
                    return Err(CrosstacheError::invalid_argument(
                        "--name and --content-type cannot be used with --recursive",
                    ));
                }
                // Validate that --prefix is not used with --name
                if prefix.is_some() && name.is_some() {
                    return Err(CrosstacheError::invalid_argument(
                        "--prefix cannot be used with --name",
                    ));
                }
                execute_file_upload_recursive(
                    &blob_manager,
                    files,
                    group,
                    metadata,
                    tag,
                    progress,
                    continue_on_error,
                    flatten,
                    prefix,
                    &config,
                )
                .await?;
            } else if files.len() == 1 {
                // Single file upload - use existing function
                execute_file_upload(
                    &blob_manager,
                    &files[0],
                    name,
                    group,
                    metadata,
                    tag,
                    content_type,
                    progress,
                    &config,
                )
                .await?;
            } else {
                // Multiple file upload
                if name.is_some() || content_type.is_some() {
                    return Err(CrosstacheError::invalid_argument(
                        "--name and --content-type can only be used when uploading a single file",
                    ));
                }
                execute_file_upload_multiple(
                    &blob_manager,
                    files,
                    group,
                    metadata,
                    tag,
                    progress,
                    continue_on_error,
                    &config,
                )
                .await?;
            }
        }
        FileCommands::Download {
            files,
            output,
            rename,
            recursive,
            flatten,
            stream,
            force,
            continue_on_error,
        } => {
            // Handle recursive download
            if recursive {
                // Validate that --rename and --stream are not used with --recursive
                if rename.is_some() || stream {
                    return Err(CrosstacheError::invalid_argument(
                        "--rename and --stream cannot be used with --recursive",
                    ));
                }
                execute_file_download_recursive(
                    &blob_manager,
                    files,
                    output,
                    force,
                    flatten,
                    continue_on_error,
                    &config,
                )
                .await?;
            } else {
                // Validate --rename only works with single file
                if rename.is_some() && files.len() > 1 {
                    return Err(CrosstacheError::invalid_argument(
                        "--rename can only be used when downloading a single file",
                    ));
                }

                // Handle single vs multiple file download
                if files.len() == 1 {
                    // For single file, use rename if provided, otherwise use output as directory
                    let output_path = if let Some(new_name) = rename {
                        Some(new_name)
                    } else {
                        output
                    };
                    execute_file_download(
                        &blob_manager,
                        &files[0],
                        output_path,
                        stream,
                        force,
                        &config,
                    )
                    .await?;
                } else {
                    execute_file_download_multiple(
                        &blob_manager,
                        files,
                        output,
                        stream,
                        force,
                        continue_on_error,
                        &config,
                    )
                    .await?;
                }
            }
        }
        FileCommands::List {
            prefix,
            group,
            metadata,
            limit,
            recursive,
        } => {
            execute_file_list(
                &blob_manager,
                prefix,
                group,
                metadata,
                limit,
                recursive,
                &config,
            )
            .await?;
        }
        FileCommands::Delete {
            files,
            force,
            continue_on_error,
        } => {
            // Handle single vs multiple file delete
            if files.len() == 1 {
                execute_file_delete(&blob_manager, &files[0], force, &config).await?;
            } else {
                execute_file_delete_multiple(
                    &blob_manager,
                    files,
                    force,
                    continue_on_error,
                    &config,
                )
                .await?;
            }
        }
        FileCommands::Info { name } => {
            execute_file_info(&blob_manager, &name, &config).await?;
        }
        FileCommands::Sync {
            local_path,
            prefix,
            direction,
            dry_run,
            delete,
        } => {
            execute_file_sync(
                &blob_manager,
                &local_path,
                prefix,
                &direction,
                dry_run,
                delete,
                &config,
            )
            .await?;
        }
    }

    Ok(())
}

async fn execute_vault_command(command: VaultCommands, config: Config) -> Result<()> {
    use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
    use std::sync::Arc;

    // Create authentication provider with credential priority from config
    let auth_provider: Arc<dyn AzureAuthProvider> = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create vault manager
    let vault_manager = VaultManager::new(
        auth_provider.clone(),
        config.subscription_id.clone(),
        config.no_color,
    )?;

    match command {
        VaultCommands::Create {
            name,
            resource_group,
            location,
        } => {
            execute_vault_create(&vault_manager, &name, resource_group, location, &config).await?;
        }
        VaultCommands::List {
            resource_group,
            format,
        } => {
            execute_vault_list(&vault_manager, resource_group, format, &config).await?;
        }
        VaultCommands::Delete {
            name,
            resource_group,
            force,
        } => {
            execute_vault_delete(&vault_manager, &name, resource_group, force, &config).await?;
        }
        VaultCommands::Info {
            name,
            resource_group,
        } => {
            execute_vault_info(&vault_manager, &name, resource_group, &config).await?;
        }
        VaultCommands::Restore { name, location } => {
            execute_vault_restore(&vault_manager, &name, &location, &config).await?;
        }
        VaultCommands::Purge {
            name,
            location,
            force,
        } => {
            execute_vault_purge(&vault_manager, &name, &location, force, &config).await?;
        }
        VaultCommands::Export {
            name,
            resource_group,
            output,
            format,
            include_values,
            group,
        } => {
            execute_vault_export(
                &vault_manager,
                &name,
                resource_group,
                output,
                &format,
                include_values,
                group,
                &config,
            )
            .await?;
        }
        VaultCommands::Import {
            name,
            resource_group,
            input,
            format,
            overwrite,
            dry_run,
        } => {
            execute_vault_import(
                &vault_manager,
                &name,
                resource_group,
                input,
                &format,
                overwrite,
                dry_run,
                &config,
            )
            .await?;
        }
        VaultCommands::Update {
            name,
            resource_group,
            tag,
            enable_deployment,
            enable_disk_encryption,
            enable_template_deployment,
            enable_purge_protection,
            retention_days,
        } => {
            execute_vault_update(
                &vault_manager,
                &name,
                resource_group,
                tag,
                enable_deployment,
                enable_disk_encryption,
                enable_template_deployment,
                enable_purge_protection,
                retention_days,
                &config,
            )
            .await?;
        }
        VaultCommands::Share { command } => {
            execute_vault_share(&vault_manager, &auth_provider, command, &config).await?;
        }
    }
    Ok(())
}

async fn execute_vault_create(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    location: Option<String>,
    config: &Config,
) -> Result<()> {
    // Use defaults from config if not provided
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
    let location = location.unwrap_or_else(|| config.default_location.clone());

    println!(
        "Creating vault '{name}' in resource group '{resource_group}' at location '{location}'..."
    );

    let create_request = VaultCreateRequest {
        name: name.to_string(),
        location: location.clone(),
        resource_group: resource_group.clone(),
        subscription_id: config.subscription_id.clone(),
        sku: Some("standard".to_string()),
        enabled_for_deployment: Some(false),
        enabled_for_disk_encryption: Some(false),
        enabled_for_template_deployment: Some(false),
        soft_delete_retention_in_days: Some(90),
        purge_protection: None, // Let the manager set safe defaults
        tags: Some(std::collections::HashMap::from([
            ("created_by".to_string(), "crosstache".to_string()),
            (
                "created_at".to_string(),
                chrono::Utc::now().format("%Y-%m-%d").to_string(),
            ),
        ])),
        access_policies: None, // Will be set automatically by the manager
    };

    let vault = vault_manager
        .create_vault_with_setup(name, &location, &resource_group, Some(create_request))
        .await?;

    println!("✅ Successfully created vault '{}'", vault.name);
    println!("   Resource Group: {}", vault.resource_group);
    println!("   Location: {}", vault.location);
    println!("   URI: {}", vault.uri);

    Ok(())
}

async fn execute_vault_list(
    vault_manager: &VaultManager,
    resource_group: Option<String>,
    format: OutputFormat,
    config: &Config,
) -> Result<()> {
    vault_manager
        .list_vaults_formatted(
            Some(&config.subscription_id),
            resource_group.as_deref(),
            format,
        )
        .await?;

    Ok(())
}

async fn execute_vault_delete(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    // Use provided resource group or fall back to config default
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    vault_manager
        .delete_vault_safe(name, &resource_group, force)
        .await?;

    Ok(())
}

async fn execute_vault_info(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    config: &Config,
) -> Result<()> {
    // Use provided resource group or fall back to config default
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    if config.output_json {
        let vault = vault_manager
            .get_vault_properties(name, &resource_group)
            .await?;
        let json_output = serde_json::to_string_pretty(&vault).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize vault info: {e}"))
        })?;
        println!("{json_output}");
    } else {
        let _vault = vault_manager.get_vault_info(name, &resource_group).await?;
        // Display will be handled by the vault manager
    }

    Ok(())
}

// Direct secret command execution functions (context-aware)
async fn execute_secret_set_direct(
    args: Vec<String>,
    stdin: bool,
    note: Option<String>,
    folder: Option<String>,
    expires: Option<String>,
    not_before: Option<String>,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Check if this is a bulk set operation (multiple KEY=value pairs)
    if args.len() == 1 && !args[0].contains('=') {
        // Single secret operation (original behavior)
        let name = &args[0];
        execute_secret_set(
            &secret_manager,
            name,
            None,
            stdin,
            note,
            folder,
            expires,
            not_before,
            &config,
        )
        .await
    } else {
        // Bulk set operation
        if stdin {
            return Err(CrosstacheError::invalid_argument(
                "--stdin cannot be used with bulk set operation",
            ));
        }
        if expires.is_some() || not_before.is_some() {
            return Err(CrosstacheError::invalid_argument(
                "--expires and --not-before cannot be used with bulk set operation",
            ));
        }
        execute_secret_set_bulk(&secret_manager, args, note, folder, &config).await
    }
}

async fn execute_secret_get_direct(
    name: &str,
    raw: bool,
    version: Option<String>,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_get(&secret_manager, name, None, raw, version, &config).await
}

async fn execute_secret_list_direct(
    group: Option<String>,
    all: bool,
    expiring: Option<String>,
    expired: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_list(
        &secret_manager,
        None,
        group,
        all,
        expiring,
        expired,
        &config,
    )
    .await
}

async fn execute_secret_delete_direct(
    name: Option<String>,
    group: Option<String>,
    force: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Check if this is a group delete operation
    if let Some(group_name) = group {
        execute_secret_delete_group(&secret_manager, &group_name, force, &config).await
    } else if let Some(secret_name) = name {
        execute_secret_delete(&secret_manager, &secret_name, None, force, &config).await
    } else {
        Err(CrosstacheError::invalid_argument(
            "Either secret name or --group must be specified",
        ))
    }
}

async fn execute_secret_history_direct(name: &str, config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_history(&secret_manager, name, None, &config).await
}

async fn execute_secret_rollback_direct(
    name: &str,
    version: &str,
    force: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_rollback(&secret_manager, name, None, version, force, &config).await
}

async fn execute_secret_rotate_direct(
    name: &str,
    length: usize,
    charset: CharsetType,
    generator: Option<String>,
    show_value: bool,
    force: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_rotate(
        &secret_manager,
        name,
        None,
        length,
        charset,
        generator,
        show_value,
        force,
        &config,
    )
    .await
}

async fn execute_secret_run_direct(
    group: Vec<String>,
    no_masking: bool,
    command: Vec<String>,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_run(&secret_manager, None, group, no_masking, command, &config).await
}

async fn execute_secret_inject_direct(
    template: Option<String>,
    out: Option<String>,
    group: Vec<String>,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_inject(&secret_manager, None, template, out, group, &config).await
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_update_direct(
    name: &str,
    value: Option<String>,
    stdin: bool,
    tags: Vec<(String, String)>,
    groups: Vec<String>,
    rename: Option<String>,
    note: Option<String>,
    folder: Option<String>,
    replace_tags: bool,
    replace_groups: bool,
    expires: Option<String>,
    not_before: Option<String>,
    clear_expires: bool,
    clear_not_before: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_update(
        &secret_manager,
        name,
        None,
        value,
        stdin,
        tags,
        groups,
        rename,
        note,
        folder,
        replace_tags,
        replace_groups,
        expires,
        not_before,
        clear_expires,
        clear_not_before,
        &config,
    )
    .await
}

async fn execute_secret_purge_direct(name: &str, force: bool, config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_purge(&secret_manager, name, None, force, &config).await
}

async fn execute_secret_restore_direct(name: &str, config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_restore(&secret_manager, name, None, &config).await
}

async fn execute_diff_command(
    vault1: &str,
    vault2: &str,
    show_values: bool,
    group: Option<String>,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // List secrets from both vaults
    let secrets_a = secret_manager
        .list_secrets_formatted(
            vault1,
            group.as_deref(),
            crate::utils::format::OutputFormat::Json,
            false,
            true,
        )
        .await?;

    let secrets_b = secret_manager
        .list_secrets_formatted(
            vault2,
            group.as_deref(),
            crate::utils::format::OutputFormat::Json,
            false,
            true,
        )
        .await?;

    // Build name sets
    let names_a: BTreeSet<String> = secrets_a.iter().map(|s| s.name.clone()).collect();
    let names_b: BTreeSet<String> = secrets_b.iter().map(|s| s.name.clone()).collect();
    let all_names: BTreeSet<String> = names_a.union(&names_b).cloned().collect();

    // Fetch values from both vaults for comparison
    let mut values_a = std::collections::HashMap::new();
    let mut values_b = std::collections::HashMap::new();

    for name in &names_a {
        match secret_manager
            .get_secret_safe(vault1, name, true, true)
            .await
        {
            Ok(props) => {
                if let Some(val) = props.value {
                    values_a.insert(name.clone(), val);
                }
            }
            Err(e) => {
                eprintln!("⚠️  Failed to get '{}' from {}: {}", name, vault1, e);
            }
        }
    }

    for name in &names_b {
        match secret_manager
            .get_secret_safe(vault2, name, true, true)
            .await
        {
            Ok(props) => {
                if let Some(val) = props.value {
                    values_b.insert(name.clone(), val);
                }
            }
            Err(e) => {
                eprintln!("⚠️  Failed to get '{}' from {}: {}", name, vault2, e);
            }
        }
    }

    // Compare and output
    println!("Comparing {} → {}", vault1, vault2);
    println!();

    let mut added = 0u32;
    let mut removed = 0u32;
    let mut changed = 0u32;
    let mut identical = 0u32;

    // Find max name length for alignment
    let max_len = all_names.iter().map(|n| n.len()).max().unwrap_or(0);

    for name in &all_names {
        let in_a = names_a.contains(name);
        let in_b = names_b.contains(name);

        match (in_a, in_b) {
            (false, true) => {
                println!(
                    "  + {:<width$}  (only in {})",
                    name,
                    vault2,
                    width = max_len
                );
                added += 1;
            }
            (true, false) => {
                println!(
                    "  - {:<width$}  (only in {})",
                    name,
                    vault1,
                    width = max_len
                );
                removed += 1;
            }
            (true, true) => {
                let val_a = values_a.get(name);
                let val_b = values_b.get(name);
                if val_a == val_b {
                    println!("  = {:<width$}  (identical)", name, width = max_len);
                    identical += 1;
                } else {
                    println!("  ~ {:<width$}  (value differs)", name, width = max_len);
                    if show_values {
                        let a_str = val_a.map(|v| v.as_str()).unwrap_or("<empty>");
                        let b_str = val_b.map(|v| v.as_str()).unwrap_or("<empty>");
                        println!("      {} : {}", vault1, a_str);
                        println!("      {} : {}", vault2, b_str);
                    }
                    changed += 1;
                }
            }
            (false, false) => unreachable!(),
        }
    }

    println!();
    println!(
        "Summary: {} added, {} removed, {} changed, {} identical",
        added, removed, changed, identical
    );

    Ok(())
}

async fn execute_secret_copy_direct(
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_copy(
        &secret_manager,
        name,
        from_vault,
        to_vault,
        new_name,
        &config,
    )
    .await
}

async fn execute_secret_move_direct(
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    force: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_move(
        &secret_manager,
        name,
        from_vault,
        to_vault,
        new_name,
        force,
        &config,
    )
    .await
}

async fn execute_secret_parse_direct(
    connection_string: &str,
    format: &str,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_parse(&secret_manager, connection_string, format, &config).await
}

async fn execute_secret_share_direct(command: ShareCommands, config: Config) -> Result<()> {
    use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
    use crate::vault::manager::VaultManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider: Arc<dyn AzureAuthProvider> = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    // Create vault manager for secret-level RBAC
    let vault_manager = VaultManager::new(
        auth_provider.clone(),
        config.subscription_id.clone(),
        config.no_color,
    )?;

    execute_secret_share(&vault_manager, &auth_provider, command, &config).await
}

async fn execute_config_command(command: ConfigCommands, config: Config) -> Result<()> {
    match command {
        ConfigCommands::Show => {
            execute_config_show(&config).await?;
        }
        ConfigCommands::Set { key, value } => {
            execute_config_set(&key, &value, config).await?;
        }
        ConfigCommands::Path => {
            execute_config_path().await?;
        }
    }
    Ok(())
}

async fn execute_context_command(command: ContextCommands, config: Config) -> Result<()> {
    match command {
        ContextCommands::Show => {
            execute_context_show(&config).await?;
        }
        ContextCommands::Use {
            vault_name,
            resource_group,
            global,
            local,
        } => {
            execute_context_use(&vault_name, resource_group, global, local, &config).await?;
        }
        ContextCommands::List => {
            execute_context_list(&config).await?;
        }
        ContextCommands::Clear { global } => {
            execute_context_clear(global, &config).await?;
        }
    }
    Ok(())
}

/// Environment profile structure  
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentProfile {
    pub name: String,
    pub vault_name: String,
    pub resource_group: String,
    pub subscription_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used: Option<chrono::DateTime<chrono::Utc>>,
}

impl EnvironmentProfile {
    pub fn new(
        name: String,
        vault_name: String,
        resource_group: String,
        subscription_id: Option<String>,
    ) -> Self {
        Self {
            name,
            vault_name,
            resource_group,
            subscription_id,
            created_at: chrono::Utc::now(),
            last_used: None,
        }
    }

    pub fn update_usage(&mut self) {
        self.last_used = Some(chrono::Utc::now());
    }
}

/// Environment profile manager
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvironmentProfileManager {
    pub profiles: std::collections::HashMap<String, EnvironmentProfile>,
    pub current_profile: Option<String>,
}

impl EnvironmentProfileManager {
    /// Load profiles from configuration file
    pub async fn load() -> Result<Self> {
        let profile_path = Self::get_profile_path()?;

        if !profile_path.exists() {
            return Ok(Self::default());
        }

        let content = tokio::fs::read_to_string(&profile_path).await?;
        let manager = serde_json::from_str(&content)?;
        Ok(manager)
    }

    /// Save profiles to configuration file
    pub async fn save(&self) -> Result<()> {
        let profile_path = Self::get_profile_path()?;

        // Create parent directories if they don't exist
        if let Some(parent) = profile_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let content = serde_json::to_string_pretty(self)?;
        crate::utils::helpers::write_sensitive_file_async(&profile_path, content.as_bytes())
            .await?;
        Ok(())
    }

    /// Get the profile configuration file path
    fn get_profile_path() -> Result<PathBuf> {
        // Check for local .xv.json file first
        let local_path = std::env::current_dir()?.join(".xv.json");
        if local_path.exists() {
            return Ok(local_path);
        }

        // Use global profile path
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::env;
            let config_dir = if let Ok(xdg_config_home) = env::var("XDG_CONFIG_HOME") {
                PathBuf::from(xdg_config_home)
            } else {
                let home_dir = env::var("HOME")
                    .map_err(|_| CrosstacheError::config("HOME environment variable not set"))?;
                PathBuf::from(home_dir).join(".config")
            };
            Ok(config_dir.join("xv").join("profiles.json"))
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let config_dir = dirs::config_dir()
                .ok_or_else(|| CrosstacheError::config("Unable to determine config directory"))?;
            Ok(config_dir.join("xv").join("profiles.json"))
        }
    }

    /// Add a new environment profile
    pub fn create_profile(&mut self, profile: EnvironmentProfile) -> Result<()> {
        if self.profiles.contains_key(&profile.name) {
            return Err(CrosstacheError::config(format!(
                "Environment profile '{}' already exists",
                profile.name
            )));
        }

        self.profiles.insert(profile.name.clone(), profile);
        Ok(())
    }

    /// Delete an environment profile
    pub fn delete_profile(&mut self, name: &str) -> Result<()> {
        if !self.profiles.contains_key(name) {
            return Err(CrosstacheError::config(format!(
                "Environment profile '{}' not found",
                name
            )));
        }

        // Clear current profile if it's the one being deleted
        if self.current_profile.as_ref() == Some(&name.to_string()) {
            self.current_profile = None;
        }

        self.profiles.remove(name);
        Ok(())
    }

    /// Use an environment profile (set it as current)
    pub fn use_profile(&mut self, name: &str) -> Result<&EnvironmentProfile> {
        let profile = self.profiles.get_mut(name).ok_or_else(|| {
            CrosstacheError::config(format!("Environment profile '{}' not found", name))
        })?;

        profile.update_usage();
        self.current_profile = Some(name.to_string());
        Ok(profile)
    }

    /// Get the current environment profile
    #[allow(dead_code)]
    pub fn current_profile(&self) -> Option<&EnvironmentProfile> {
        self.current_profile
            .as_ref()
            .and_then(|name| self.profiles.get(name))
    }
}

async fn execute_env_command(command: EnvCommands, config: Config) -> Result<()> {
    match command {
        EnvCommands::List => execute_env_list(&config).await,
        EnvCommands::Use { name } => execute_env_use(&name, &config).await,
        EnvCommands::Create {
            name,
            vault,
            group,
            subscription,
            global,
        } => execute_env_create(&name, &vault, &group, subscription, global, &config).await,
        EnvCommands::Delete { name, force } => execute_env_delete(&name, force, &config).await,
        EnvCommands::Show => execute_env_show(&config).await,
        EnvCommands::Pull {
            format,
            group,
            output,
        } => execute_env_pull(&format, group, output, &config).await,
        EnvCommands::Push { file, overwrite } => execute_env_push(file, overwrite, &config).await,
    }
}

async fn execute_env_list(_config: &Config) -> Result<()> {
    let manager = EnvironmentProfileManager::load().await?;

    if manager.profiles.is_empty() {
        println!("No environment profiles found.");
        println!("Create one with: xv env create <name> --vault <vault> --group <group>");
        return Ok(());
    }

    println!("Environment Profiles:");
    println!("────────────────────");

    for (name, profile) in &manager.profiles {
        let current_marker = if manager.current_profile.as_ref() == Some(name) {
            "* "
        } else {
            "  "
        };

        println!(
            "{}{} → {} ({})",
            current_marker, name, profile.vault_name, profile.resource_group
        );

        if let Some(last_used) = profile.last_used {
            println!(
                "    Last used: {}",
                last_used.format("%Y-%m-%d %H:%M:%S UTC")
            );
        }
    }

    if let Some(current_name) = &manager.current_profile {
        println!("\nCurrent profile: {}", current_name);
    } else {
        println!("\nNo profile currently active");
    }

    Ok(())
}

async fn execute_env_use(name: &str, _config: &Config) -> Result<()> {
    let mut manager = EnvironmentProfileManager::load().await?;

    // Get profile data before using (to avoid borrow checker issues)
    let (vault_name, resource_group, subscription_id) = {
        let profile = manager.use_profile(name)?;
        (
            profile.vault_name.clone(),
            profile.resource_group.clone(),
            profile.subscription_id.clone(),
        )
    };

    // Update the vault context using the profile
    use crate::config::context::VaultContext;
    use crate::config::ContextManager;

    let vault_context = VaultContext::new(
        vault_name.clone(),
        Some(resource_group.clone()),
        subscription_id.clone(),
    );

    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    context_manager.set_context(vault_context).await?;

    // Save the profile manager
    manager.save().await?;

    println!("✓ Using environment profile: {}", name);
    println!("  Vault: {}", vault_name);
    println!("  Resource Group: {}", resource_group);
    if let Some(subscription) = &subscription_id {
        println!("  Subscription: {}", subscription);
    }

    Ok(())
}

async fn execute_env_create(
    name: &str,
    vault: &str,
    group: &str,
    subscription: Option<String>,
    global: bool,
    _config: &Config,
) -> Result<()> {
    let mut manager = EnvironmentProfileManager::load().await?;

    let profile = EnvironmentProfile::new(
        name.to_string(),
        vault.to_string(),
        group.to_string(),
        subscription.clone(),
    );

    manager.create_profile(profile.clone())?;

    if global {
        // Set as current profile
        manager.use_profile(name)?;

        // Update the vault context
        use crate::config::context::VaultContext;
        use crate::config::ContextManager;

        let vault_context = VaultContext::new(
            vault.to_string(),
            Some(group.to_string()),
            subscription.clone(),
        );

        let mut context_manager = ContextManager::load().await.unwrap_or_default();
        context_manager.set_context(vault_context).await?;
    }

    manager.save().await?;

    println!("✓ Created environment profile: {}", name);
    println!("  Vault: {}", vault);
    println!("  Resource Group: {}", group);
    if let Some(subscription) = &subscription {
        println!("  Subscription: {}", subscription);
    }

    if global {
        println!("  Set as current profile");
    }

    Ok(())
}

async fn execute_env_delete(name: &str, force: bool, _config: &Config) -> Result<()> {
    let mut manager = EnvironmentProfileManager::load().await?;

    if !manager.profiles.contains_key(name) {
        return Err(CrosstacheError::config(format!(
            "Environment profile '{}' not found",
            name
        )));
    }

    if !force {
        use crate::utils::interactive::InteractivePrompt;

        let prompt = InteractivePrompt::new();
        let confirmation_message = format!("Delete environment profile '{}'?", name);
        if !prompt.confirm(&confirmation_message, false)? {
            println!("Delete cancelled");
            return Ok(());
        }
    }

    manager.delete_profile(name)?;
    manager.save().await?;

    println!("✓ Deleted environment profile: {}", name);

    Ok(())
}

async fn execute_env_show(_config: &Config) -> Result<()> {
    let manager = EnvironmentProfileManager::load().await?;

    if let Some(current_name) = &manager.current_profile {
        if let Some(profile) = manager.profiles.get(current_name) {
            println!("Current Environment Profile: {}", current_name);
            println!("──────────────────────────");
            println!("Vault: {}", profile.vault_name);
            println!("Resource Group: {}", profile.resource_group);
            if let Some(subscription) = &profile.subscription_id {
                println!("Subscription: {}", subscription);
            }
            println!(
                "Created: {}",
                profile.created_at.format("%Y-%m-%d %H:%M:%S UTC")
            );
            if let Some(last_used) = profile.last_used {
                println!("Last Used: {}", last_used.format("%Y-%m-%d %H:%M:%S UTC"));
            }
        } else {
            println!(
                "Current profile '{}' not found (corrupted state)",
                current_name
            );
        }
    } else {
        println!("No environment profile is currently active");
        println!("Use 'xv env list' to see available profiles");
        println!("Use 'xv env use <name>' to activate a profile");
    }

    Ok(())
}

async fn execute_env_pull(
    format: &str,
    groups: Vec<String>,
    output: Option<String>,
    config: &Config,
) -> Result<()> {
    if format != "dotenv" {
        return Err(CrosstacheError::invalid_argument(format!(
            "Unsupported format '{}'. Only 'dotenv' is currently supported.",
            format
        )));
    }

    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider and secret manager
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Determine vault name
    let vault_name = config.resolve_vault_name(None).await?;

    println!(
        "Pulling secrets from vault '{}' to dotenv format...",
        vault_name
    );

    // Get all secrets or filtered by group
    let mut all_secrets = Vec::new();
    if groups.is_empty() {
        // Get all secrets
        let secrets = secret_manager
            .list_secrets_formatted(
                &vault_name,
                None,
                crate::utils::format::OutputFormat::Json, // We don't use the output, just need the list
                false,
                true,
            )
            .await?;
        for secret_summary in secrets {
            match secret_manager
                .get_secret_safe(&vault_name, &secret_summary.name, true, true)
                .await
            {
                Ok(secret) => all_secrets.push(secret),
                Err(e) => eprintln!(
                    "Warning: Failed to get secret '{}': {}",
                    secret_summary.name, e
                ),
            }
        }
    } else {
        // Get secrets filtered by groups
        for group in &groups {
            let secrets = secret_manager
                .list_secrets_formatted(
                    &vault_name,
                    Some(group),
                    crate::utils::format::OutputFormat::Json, // We don't use the output, just need the list
                    false,
                    true,
                )
                .await?;
            for secret_summary in secrets {
                match secret_manager
                    .get_secret_safe(&vault_name, &secret_summary.name, true, true)
                    .await
                {
                    Ok(secret) => all_secrets.push(secret),
                    Err(e) => eprintln!(
                        "Warning: Failed to get secret '{}': {}",
                        secret_summary.name, e
                    ),
                }
            }
        }
    }

    // Convert to dotenv format
    let mut dotenv_content = String::new();
    for secret in &all_secrets {
        if let Some(ref value) = secret.value {
            // Use original name if available, otherwise use sanitized name
            let key = &secret.original_name;

            // Escape value if it contains special characters
            let escaped_value =
                if value.contains('\n') || value.contains('"') || value.contains('\\') {
                    format!(
                        "\"{}\"",
                        value
                            .replace('\\', "\\\\")
                            .replace('"', "\\\"")
                            .replace('\n', "\\n")
                    )
                } else if value.contains(' ') || value.starts_with('#') {
                    format!("\"{}\"", value.as_str())
                } else {
                    value.to_string()
                };

            dotenv_content.push_str(&format!("{}={}\n", key, escaped_value));
        }
    }

    // Output to file or stdout
    if let Some(output_path) = output {
        crate::utils::helpers::write_sensitive_file(
            std::path::Path::new(&output_path),
            dotenv_content.as_bytes(),
        )?;
        println!(
            "✅ Successfully exported {} secret(s) to '{}' (permissions: owner-only)",
            all_secrets.len(),
            output_path
        );
    } else {
        print!("{}", dotenv_content);
    }

    if !groups.is_empty() {
        println!("# Filtered by groups: {}", groups.join(", "));
    }

    Ok(())
}

async fn execute_env_push(file: Option<String>, overwrite: bool, config: &Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use crate::secret::manager::SecretRequest;
    use std::collections::HashMap;
    use std::io::Read;
    use std::sync::Arc;

    // Create authentication provider and secret manager
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Determine vault name
    let vault_name = config.resolve_vault_name(None).await?;

    // Read .env content from file or stdin
    let env_content = if let Some(file_path) = file {
        println!("Reading .env file from '{}'...", file_path);
        std::fs::read_to_string(&file_path)?
    } else {
        println!("Reading .env content from stdin...");
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    };

    // Parse .env content
    let mut secrets = HashMap::new();
    for (line_num, line) in env_content.lines().enumerate() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse KEY=VALUE format
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            let value = line[eq_pos + 1..].trim();

            // Handle quoted values
            let processed_value =
                if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
                    let unquoted = &value[1..value.len() - 1];
                    // Unescape quoted content
                    unquoted
                        .replace("\\\"", "\"")
                        .replace("\\n", "\n")
                        .replace("\\\\", "\\")
                } else {
                    value.to_string()
                };

            if key.is_empty() {
                eprintln!("Warning: Empty key on line {} - skipping", line_num + 1);
                continue;
            }

            secrets.insert(key.to_string(), processed_value);
        } else {
            eprintln!(
                "Warning: Invalid format on line {} - skipping: {}",
                line_num + 1,
                line
            );
        }
    }

    if secrets.is_empty() {
        println!("No valid key=value pairs found in input");
        return Ok(());
    }

    println!(
        "Pushing {} secret(s) to vault '{}'...",
        secrets.len(),
        vault_name
    );

    // Check for existing secrets if not overwriting
    if !overwrite {
        let mut existing_secrets = Vec::new();
        for key in secrets.keys() {
            if secret_manager
                .get_secret_safe(&vault_name, key, false, false)
                .await
                .is_ok()
            {
                existing_secrets.push(key);
            }
        }

        if !existing_secrets.is_empty() {
            return Err(CrosstacheError::config(format!(
                "The following secret(s) already exist: {}. Use --overwrite to replace them.",
                existing_secrets
                    .into_iter()
                    .map(|s| format!("'{}'", s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    // Set each secret
    let mut success_count = 0;
    let mut error_count = 0;

    for (key, value) in secrets {
        let secret_request = SecretRequest {
            name: key.clone(),
            value: Zeroizing::new(value.clone()),
            content_type: Some("text/plain".to_string()),
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: Some(HashMap::new()),
            groups: None,
            note: None,
            folder: None,
        };

        match secret_manager
            .set_secret_safe(&vault_name, &key, &value, Some(secret_request))
            .await
        {
            Ok(_) => {
                println!("  ✅ Set '{}'", key);
                success_count += 1;
            }
            Err(e) => {
                eprintln!("  ❌ Failed to set '{}': {}", key, e);
                error_count += 1;
            }
        }
    }

    if error_count > 0 {
        println!(
            "Completed with {} successful and {} failed operations",
            success_count, error_count
        );
    } else {
        println!(
            "✅ Successfully pushed {} secret(s) to vault '{}'",
            success_count, vault_name
        );
    }

    Ok(())
}

/// Azure Activity Log entry for audit purposes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub operation: String,
    pub resource_name: String,
    pub resource_type: String,
    pub caller: String,
    pub status: String,
    pub correlation_id: String,
    pub vault_name: Option<String>,
    pub subscription_id: String,
    pub resource_group: String,
    pub properties: serde_json::Value,
}

impl std::fmt::Display for AuditLogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} | {} | {} | {} | {}",
            self.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
            self.operation,
            self.resource_name,
            self.caller,
            self.status
        )
    }
}

/// Azure Activity Log client for fetching audit data
pub struct AzureActivityLogClient {
    auth_provider: std::sync::Arc<dyn crate::auth::provider::AzureAuthProvider>,
}

impl AzureActivityLogClient {
    pub fn new(
        auth_provider: std::sync::Arc<dyn crate::auth::provider::AzureAuthProvider>,
    ) -> Self {
        Self { auth_provider }
    }

    /// Fetch audit logs for a specific vault
    pub async fn get_vault_audit_logs(
        &self,
        subscription_id: &str,
        resource_group: &str,
        vault_name: &str,
        days: u32,
    ) -> Result<Vec<AuditLogEntry>> {
        let end_time = chrono::Utc::now();
        let start_time = end_time - chrono::Duration::days(days as i64);

        let start_time_str = start_time.format("%Y-%m-%dT%H:%M:%S.%3fZ");
        let end_time_str = end_time.format("%Y-%m-%dT%H:%M:%S.%3fZ");

        // Build the Azure Activity Log API URL
        let activity_url = format!(
            "https://management.azure.com/subscriptions/{}/providers/microsoft.insights/eventtypes/management/values?api-version=2015-04-01&$filter=eventTimestamp ge '{}' and eventTimestamp le '{}' and resourceUri eq '/subscriptions/{}/resourceGroups/{}/providers/Microsoft.KeyVault/vaults/{}'",
            subscription_id, start_time_str, end_time_str, subscription_id, resource_group, vault_name
        );

        // Get access token from auth provider
        let token = self
            .auth_provider
            .get_token(&["https://management.azure.com/.default"])
            .await?;

        // Make HTTP request
        let client = reqwest::Client::new();
        let response = client
            .get(&activity_url)
            .header("Authorization", format!("Bearer {}", token.token.secret()))
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| {
                CrosstacheError::network(format!("Failed to fetch activity logs: {}", e))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(CrosstacheError::azure_api(format!(
                "Activity Log API returned {}: {}",
                status, error_text
            )));
        }

        let activity_response: serde_json::Value = response.json().await.map_err(|e| {
            CrosstacheError::serialization(format!("Failed to parse activity logs: {}", e))
        })?;

        // Parse the Azure Activity Log response
        self.parse_activity_log_response(activity_response, vault_name)
    }

    /// Fetch audit logs for a specific secret
    pub async fn get_secret_audit_logs(
        &self,
        subscription_id: &str,
        resource_group: &str,
        vault_name: &str,
        secret_name: &str,
        days: u32,
    ) -> Result<Vec<AuditLogEntry>> {
        // Get all vault logs and filter for the specific secret
        let vault_logs = self
            .get_vault_audit_logs(subscription_id, resource_group, vault_name, days)
            .await?;

        let secret_logs: Vec<AuditLogEntry> = vault_logs
            .into_iter()
            .filter(|log| {
                log.resource_name.contains(secret_name)
                    || log.properties.get("secretName").and_then(|v| v.as_str())
                        == Some(secret_name)
            })
            .collect();

        Ok(secret_logs)
    }

    /// Parse Azure Activity Log API response
    fn parse_activity_log_response(
        &self,
        response: serde_json::Value,
        vault_name: &str,
    ) -> Result<Vec<AuditLogEntry>> {
        let mut entries = Vec::new();

        if let Some(value) = response.get("value").and_then(|v| v.as_array()) {
            for event in value {
                if let Ok(entry) = self.parse_activity_log_entry(event, vault_name) {
                    entries.push(entry);
                }
            }
        }

        // Sort by timestamp (newest first)
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        Ok(entries)
    }

    /// Parse individual activity log entry
    fn parse_activity_log_entry(
        &self,
        event: &serde_json::Value,
        vault_name: &str,
    ) -> Result<AuditLogEntry> {
        let timestamp = event
            .get("eventTimestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .ok_or_else(|| CrosstacheError::serialization("Invalid timestamp in activity log"))?;

        let operation = event
            .get("operationName")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let resource_name = event
            .get("resourceId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let resource_type = event
            .get("resourceType")
            .and_then(|v| v.as_str())
            .unwrap_or("Microsoft.KeyVault/vaults")
            .to_string();

        let caller = event
            .get("caller")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let status = event
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or(
                event
                    .get("subStatus")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
            )
            .to_string();

        let correlation_id = event
            .get("correlationId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let subscription_id = event
            .get("subscriptionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let resource_group = event
            .get("resourceGroupName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let properties = event
            .get("properties")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        Ok(AuditLogEntry {
            timestamp,
            operation,
            resource_name,
            resource_type,
            caller,
            status,
            correlation_id,
            vault_name: Some(vault_name.to_string()),
            subscription_id,
            resource_group,
            properties,
        })
    }
}

async fn execute_audit_command(
    name: Option<String>,
    vault: Option<String>,
    days: u32,
    operation: Option<String>,
    raw: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {}", e))
        })?,
    );

    // Create audit log client
    let audit_client = AzureActivityLogClient::new(auth_provider);

    // Determine vault and context
    let (vault_name, resource_group, subscription_id) = if let Some(vault_name) = vault {
        // Use specified vault, need to get resource group and subscription
        let rg = config.default_resource_group.clone();
        let sub = config.subscription_id.clone();

        if rg.is_empty() {
            return Err(CrosstacheError::config(
                "No default resource group configured. Use 'xv init' to configure or specify with --resource-group"
            ));
        }

        (vault_name, rg, sub)
    } else {
        // Use current vault context
        let vault_name = config.resolve_vault_name(None).await?;
        let rg = config.default_resource_group.clone();
        let sub = config.subscription_id.clone();

        if rg.is_empty() {
            return Err(CrosstacheError::config(
                "No default resource group configured. Use 'xv init' to configure",
            ));
        }

        (vault_name, rg, sub)
    };

    println!("🔍 Fetching audit logs for {} days...", days);

    // Fetch audit logs
    let mut logs = if let Some(secret_name) = name {
        println!("  Secret: {}", secret_name);
        println!("  Vault: {}", vault_name);
        audit_client
            .get_secret_audit_logs(
                &subscription_id,
                &resource_group,
                &vault_name,
                &secret_name,
                days,
            )
            .await?
    } else {
        println!("  Vault: {}", vault_name);
        audit_client
            .get_vault_audit_logs(&subscription_id, &resource_group, &vault_name, days)
            .await?
    };

    // Filter by operation if specified
    if let Some(op_filter) = operation {
        logs.retain(|log| {
            log.operation
                .to_lowercase()
                .contains(&op_filter.to_lowercase())
        });
    }

    if logs.is_empty() {
        println!("📭 No audit log entries found for the specified criteria");
        return Ok(());
    }

    println!("\n📊 Found {} audit log entries:\n", logs.len());

    if raw {
        // Show raw JSON output
        for log in logs {
            let json_output = serde_json::to_string_pretty(&log).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize log entry: {}", e))
            })?;
            println!("{}", json_output);
            println!("---");
        }
    } else {
        // Show formatted output
        println!(
            "{:<20} | {:<25} | {:<20} | {:<30} | {:<10}",
            "Timestamp", "Operation", "Resource", "Caller", "Status"
        );
        println!("{}", "-".repeat(120));

        for log in logs {
            // Extract resource name (last part after /)
            let resource_display = log
                .resource_name
                .split('/')
                .next_back()
                .unwrap_or(&log.resource_name);

            // Truncate long strings for better display
            let operation = if log.operation.len() > 25 {
                format!("{}...", &log.operation[..22])
            } else {
                log.operation.clone()
            };

            let caller = if log.caller.len() > 30 {
                format!("{}...", &log.caller[..27])
            } else {
                log.caller.clone()
            };

            let resource = if resource_display.len() > 20 {
                format!("{}...", &resource_display[..17])
            } else {
                resource_display.to_string()
            };

            println!(
                "{:<20} | {:<25} | {:<20} | {:<30} | {:<10}",
                log.timestamp.format("%m-%d %H:%M:%S"),
                operation,
                resource,
                caller,
                log.status
            );
        }

        println!(
            "\n💡 Use --raw to see full details, or --operation <type> to filter by operation type"
        );
    }

    Ok(())
}

async fn execute_init_command(_config: Config) -> Result<()> {
    use crate::config::init::ConfigInitializer;

    // Create the initializer and run the interactive setup
    let initializer = ConfigInitializer::new();
    let new_config = initializer.run_interactive_setup().await?;

    // Show setup summary
    initializer.show_setup_summary(&new_config)?;

    Ok(())
}

async fn execute_info_command(
    resource: String,
    resource_type: Option<ResourceType>,
    resource_group: Option<String>,
    subscription: Option<String>,
    config: Config,
) -> Result<()> {
    use crate::utils::resource_detector::ResourceDetector;

    // Detect the resource type
    let detected_type =
        ResourceDetector::detect_resource_type(&resource, resource_type, resource_group.is_some());

    // If auto-detected and verbose, show why we detected it
    if resource_type.is_none() && config.debug {
        let reason = ResourceDetector::get_detection_reason(
            &resource,
            detected_type,
            resource_group.is_some(),
        );
        eprintln!("Auto-detected resource type: {detected_type} ({reason})");
    }

    // Route to the appropriate handler
    match detected_type {
        ResourceType::Vault => {
            execute_vault_info_from_root(&resource, resource_group, subscription, &config).await
        }
        ResourceType::Secret => execute_secret_info_from_root(&resource, &config).await,
        #[cfg(feature = "file-ops")]
        ResourceType::File => execute_file_info_from_root(&resource, &config).await,
    }
}

/// Execute vault info from root info command
async fn execute_vault_info_from_root(
    vault_name: &str,
    resource_group: Option<String>,
    _subscription: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(DefaultAzureCredentialProvider::with_credential_priority(
        config.azure_credential_priority.clone(),
    )?);

    // Create vault manager
    let vault_manager = VaultManager::new(
        auth_provider,
        config.subscription_id.clone(),
        config.no_color,
    )?;

    // Use provided resource group or fall back to config default
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Call the existing vault info function
    execute_vault_info(&vault_manager, vault_name, Some(resource_group), config).await
}

/// Execute secret info from root info command
async fn execute_secret_info_from_root(secret_name: &str, config: &Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Check if we have a vault context
    let vault_name = if !config.default_vault.is_empty() {
        &config.default_vault
    } else {
        return Err(CrosstacheError::config(
            "No vault context set. Use 'xv context set <vault>' to set a default vault",
        ));
    };

    // Create authentication provider
    let auth_provider = Arc::new(DefaultAzureCredentialProvider::with_credential_priority(
        config.azure_credential_priority.clone(),
    )?);

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Get secret info
    let secret_info = secret_manager
        .get_secret_info(vault_name, secret_name)
        .await?;

    // Display based on output format
    if config.output_json {
        let json_output = serde_json::to_string_pretty(&secret_info).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize secret info: {e}"))
        })?;
        println!("{json_output}");
    } else {
        println!("{secret_info}");
    }

    Ok(())
}

#[cfg(feature = "file-ops")]
/// Execute file info from root info command
async fn execute_file_info_from_root(file_name: &str, config: &Config) -> Result<()> {
    // Create blob manager
    let blob_manager = create_blob_manager(config).map_err(|e| {
        if e.to_string().contains("No storage account configured") {
            CrosstacheError::config(
                "No blob storage configured. Run 'xv init' to set up blob storage.",
            )
        } else {
            e
        }
    })?;

    // Call the existing file info function
    execute_file_info(&blob_manager, file_name, config).await
}

async fn execute_version_command() -> Result<()> {
    let build_info = get_build_info();

    println!("crosstache Rust CLI");
    println!("===================");
    println!("Version:      {}", build_info.version);
    println!("Git Hash:     {}", build_info.git_hash);
    println!("Git Branch:   {}", build_info.git_branch);

    Ok(())
}

async fn execute_completion_command(shell: Shell) -> Result<()> {
    use clap_complete::generate;
    use std::io;

    let mut cmd = Cli::command();
    let name = "xv";

    generate(shell, &mut cmd, name, &mut io::stdout());

    Ok(())
}

async fn execute_whoami_command(config: Config) -> Result<()> {
    use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
    use crate::config::ContextManager;

    println!("🔍 Checking authentication and context...\n");

    // Create authentication provider
    let auth_provider = DefaultAzureCredentialProvider::with_credential_priority(
        config.azure_credential_priority.clone(),
    )
    .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?;

    // Get access token to validate authentication
    let token = match auth_provider
        .get_token(&["https://vault.azure.net/.default"])
        .await
    {
        Ok(token) => token,
        Err(e) => {
            println!("❌ Authentication failed: {}", e);
            return Ok(());
        }
    };

    println!("✅ Authentication successful\n");

    // Try to get tenant and subscription information
    let management_token = auth_provider
        .get_token(&["https://management.azure.com/.default"])
        .await
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to get management token: {e}"))
        })?;

    // Parse token to get tenant ID (from JWT)
    let tenant_id = extract_tenant_from_token(token.token.secret())?;

    println!("👤 Identity Information:");
    println!("   Tenant ID: {}", tenant_id);

    // Get subscription information
    if let Ok(subscription_id) = get_current_subscription(management_token.token.secret()).await {
        println!("   Subscription ID: {}", subscription_id);
    } else {
        println!("   Subscription ID: Unable to determine");
    }

    // Show current context information
    println!("\n📊 Context Information:");

    let context_manager = ContextManager::load().await.unwrap_or_default();

    if let Some(current_vault) = context_manager.current_vault() {
        println!("   Default Vault: {}", current_vault);
    } else {
        println!("   Default Vault: None set");
    }

    if let Some(current_sub) = context_manager.current_subscription_id() {
        println!("   Current Subscription: {}", current_sub);
    } else {
        println!("   Current Subscription: None set");
    }

    // Show recent vaults
    let recent_contexts = context_manager.list_recent();
    if !recent_contexts.is_empty() {
        println!("\n📝 Recent Vaults:");
        for context in recent_contexts.iter().take(5) {
            println!(
                "   {} (last used: {})",
                context.vault_name,
                context.last_used.format("%Y-%m-%d %H:%M:%S")
            );
        }
    }

    println!("\n🔧 Configuration:");
    println!("   Default vault: {}", config.default_vault);
    println!("   Default subscription: {}", config.subscription_id);
    println!("   No color mode: {}", config.no_color);
    println!(
        "   Credential priority: {:?}",
        config.azure_credential_priority
    );

    Ok(())
}

/// Extract tenant ID from JWT token
fn extract_tenant_from_token(token: &str) -> Result<String> {
    // JWT tokens have 3 parts separated by dots: header.payload.signature
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(CrosstacheError::authentication("Invalid JWT token format"));
    }

    // Decode the payload (second part)
    let payload = parts[1];

    // Add padding if needed for base64 decoding
    let padded = match payload.len() % 4 {
        0 => payload.to_string(),
        n => format!("{}{}", payload, "=".repeat(4 - n)),
    };

    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let decoded = STANDARD
        .decode(&padded)
        .map_err(|_| CrosstacheError::authentication("Failed to decode token payload"))?;

    let payload_str = String::from_utf8(decoded)
        .map_err(|_| CrosstacheError::authentication("Invalid UTF-8 in token payload"))?;

    let payload_json: serde_json::Value = serde_json::from_str(&payload_str)
        .map_err(|_| CrosstacheError::authentication("Invalid JSON in token payload"))?;

    payload_json["tid"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| CrosstacheError::authentication("Tenant ID not found in token"))
}

/// Get current subscription ID from Azure management API
async fn get_current_subscription(token: &str) -> Result<String> {
    use crate::utils::network::{create_http_client, NetworkConfig};

    let network_config = NetworkConfig::default();
    let http_client = create_http_client(&network_config)?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", token)
            .parse()
            .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
    );

    let response = http_client
        .get("https://management.azure.com/subscriptions?api-version=2020-01-01")
        .headers(headers)
        .send()
        .await
        .map_err(|e| CrosstacheError::azure_api(format!("Failed to get subscriptions: {e}")))?;

    if !response.status().is_success() {
        return Err(CrosstacheError::azure_api(
            "Failed to get subscription information",
        ));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| CrosstacheError::azure_api(format!("Failed to parse response: {e}")))?;

    if let Some(subscriptions) = json["value"].as_array() {
        if let Some(first_sub) = subscriptions.first() {
            if let Some(sub_id) = first_sub["subscriptionId"].as_str() {
                return Ok(sub_id.to_string());
            }
        }
    }

    Err(CrosstacheError::azure_api("No subscriptions found"))
}

async fn execute_config_show(config: &Config) -> Result<()> {
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    #[derive(Tabled)]
    struct ConfigItem {
        #[tabled(rename = "Setting")]
        key: String,
        #[tabled(rename = "Value")]
        value: String,
        #[tabled(rename = "Source")]
        source: String,
    }

    let items = vec![
        ConfigItem {
            key: "debug".to_string(),
            value: config.debug.to_string(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "subscription_id".to_string(),
            value: if config.subscription_id.is_empty() {
                "<not set>".to_string()
            } else {
                config.subscription_id.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "default_vault".to_string(),
            value: if config.default_vault.is_empty() {
                "<not set>".to_string()
            } else {
                config.default_vault.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "default_resource_group".to_string(),
            value: config.default_resource_group.clone(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "default_location".to_string(),
            value: config.default_location.clone(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "tenant_id".to_string(),
            value: if config.tenant_id.is_empty() {
                "<not set>".to_string()
            } else {
                config.tenant_id.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "function_app_url".to_string(),
            value: if config.function_app_url.is_empty() {
                "<not set>".to_string()
            } else {
                config.function_app_url.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "cache_ttl".to_string(),
            value: format!("{}s", config.cache_ttl.as_secs()),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "output_json".to_string(),
            value: config.output_json.to_string(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "no_color".to_string(),
            value: config.no_color.to_string(),
            source: "config".to_string(),
        },
    ];

    // Add blob storage configuration items
    let mut items = items;
    let blob_config = config.get_blob_config();

    // Add credential priority
    items.push(ConfigItem {
        key: "azure_credential_priority".to_string(),
        value: config.azure_credential_priority.to_string(),
        source: "config".to_string(),
    });

    items.push(ConfigItem {
        key: "storage_account".to_string(),
        value: if blob_config.storage_account.is_empty() {
            "<not set>".to_string()
        } else {
            blob_config.storage_account
        },
        source: "config".to_string(),
    });

    items.push(ConfigItem {
        key: "storage_container".to_string(),
        value: blob_config.container_name,
        source: "config".to_string(),
    });

    if let Some(endpoint) = blob_config.endpoint {
        items.push(ConfigItem {
            key: "storage_endpoint".to_string(),
            value: endpoint,
            source: "config".to_string(),
        });
    }

    items.push(ConfigItem {
        key: "blob_chunk_size_mb".to_string(),
        value: blob_config.chunk_size_mb.to_string(),
        source: "config".to_string(),
    });

    items.push(ConfigItem {
        key: "blob_max_concurrent_uploads".to_string(),
        value: blob_config.max_concurrent_uploads.to_string(),
        source: "config".to_string(),
    });

    let items = items;

    if config.output_json {
        let json_output = serde_json::to_string_pretty(config).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize config: {e}"))
        })?;
        println!("{json_output}");
    } else {
        let table = Table::new(&items);
        println!("{}", format_table(table, config.no_color));
    }

    Ok(())
}

async fn execute_config_path() -> Result<()> {
    let config_path = Config::get_config_path()?;
    println!("{}", config_path.display());
    Ok(())
}

async fn execute_config_set(key: &str, value: &str, mut config: Config) -> Result<()> {
    match key {
        "debug" => {
            config.debug = value.to_lowercase() == "true" || value == "1";
        }
        "subscription_id" => {
            config.subscription_id = value.to_string();
        }
        "default_vault" => {
            config.default_vault = value.to_string();
        }
        "default_resource_group" => {
            config.default_resource_group = value.to_string();
        }
        "default_location" => {
            config.default_location = value.to_string();
        }
        "tenant_id" => {
            config.tenant_id = value.to_string();
        }
        "function_app_url" => {
            config.function_app_url = value.to_string();
        }
        "cache_ttl" => {
            let seconds = value.parse::<u64>().map_err(|_| {
                CrosstacheError::config(format!("Invalid value for cache_ttl: {value}"))
            })?;
            config.cache_ttl = std::time::Duration::from_secs(seconds);
        }
        "output_json" => {
            config.output_json = value.to_lowercase() == "true" || value == "1";
        }
        "no_color" => {
            config.no_color = value.to_lowercase() == "true" || value == "1";
        }
        "azure_credential_priority" => {
            use crate::config::settings::AzureCredentialType;
            use std::str::FromStr;
            config.azure_credential_priority =
                AzureCredentialType::from_str(value).map_err(CrosstacheError::config)?;
        }
        // Blob storage configuration
        "storage_account" => {
            let mut blob_config = config.get_blob_config();
            blob_config.storage_account = value.to_string();
            config.set_blob_config(blob_config);
        }
        "storage_container" => {
            let mut blob_config = config.get_blob_config();
            blob_config.container_name = value.to_string();
            config.set_blob_config(blob_config);
        }
        "storage_endpoint" => {
            let mut blob_config = config.get_blob_config();
            blob_config.endpoint = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            config.set_blob_config(blob_config);
        }
        "blob_chunk_size_mb" => {
            let chunk_size = value.parse::<usize>().map_err(|_| {
                CrosstacheError::config(format!("Invalid value for blob_chunk_size_mb: {value}"))
            })?;
            let mut blob_config = config.get_blob_config();
            blob_config.chunk_size_mb = chunk_size;
            config.set_blob_config(blob_config);
        }
        "blob_max_concurrent_uploads" => {
            let max_uploads = value.parse::<usize>().map_err(|_| {
                CrosstacheError::config(format!(
                    "Invalid value for blob_max_concurrent_uploads: {value}"
                ))
            })?;
            let mut blob_config = config.get_blob_config();
            blob_config.max_concurrent_uploads = max_uploads;
            config.set_blob_config(blob_config);
        }
        "clipboard_timeout" => {
            config.clipboard_timeout = value.parse::<u64>().map_err(|_| {
                CrosstacheError::config(format!(
                    "Invalid value for clipboard_timeout: {value} (expected seconds as integer, 0 to disable)"
                ))
            })?;
        }
        _ => {
            return Err(CrosstacheError::config(format!(
                "Unknown configuration key: {key}. Available keys: debug, subscription_id, default_vault, default_resource_group, default_location, tenant_id, function_app_url, cache_ttl, output_json, no_color, azure_credential_priority, storage_account, storage_container, storage_endpoint, blob_chunk_size_mb, blob_max_concurrent_uploads, clipboard_timeout"
            )));
        }
    }

    config.save().await?;
    println!("✅ Configuration updated: {key} = {value}");

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_set(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    stdin: bool,
    note: Option<String>,
    folder: Option<String>,
    expires: Option<String>,
    not_before: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use std::io::{self, Read};

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get secret value
    let value = if stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        buffer.trim().to_string()
    } else {
        // Use rpassword for secure input
        rpassword::prompt_password(format!("Enter value for secret '{name}': "))?
    };

    if value.is_empty() {
        return Err(CrosstacheError::config("Secret value cannot be empty"));
    }

    // Parse expiry dates if provided
    let expires_on = if let Some(expires_str) = expires.as_deref() {
        use crate::utils::datetime::parse_datetime_or_duration;
        Some(parse_datetime_or_duration(expires_str)?)
    } else {
        None
    };

    let not_before_on = if let Some(not_before_str) = not_before.as_deref() {
        use crate::utils::datetime::parse_datetime_or_duration;
        Some(parse_datetime_or_duration(not_before_str)?)
    } else {
        None
    };

    // Create secret request with note, folder, and/or expiry dates if provided
    let secret_request =
        if note.is_some() || folder.is_some() || expires_on.is_some() || not_before_on.is_some() {
            Some(crate::secret::manager::SecretRequest {
                name: name.to_string(),
                value: Zeroizing::new(value.clone()),
                content_type: None,
                enabled: Some(true),
                expires_on,
                not_before: not_before_on,
                tags: None,
                groups: None,
                note,
                folder,
            })
        } else {
            None
        };

    // Set the secret
    let secret = secret_manager
        .set_secret_safe(&vault_name, name, &value, secret_request)
        .await?;

    println!("✅ Successfully set secret '{}'", secret.original_name);
    println!("   Vault: {vault_name}");
    println!("   Version: {}", secret.version);

    Ok(())
}

async fn execute_secret_get(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    raw: bool,
    version: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get the secret (specific version or current)
    let secret = secret_manager
        .get_secret_with_version(&vault_name, name, version.as_deref(), true, true)
        .await?;

    if raw {
        // Raw output - print the value
        if let Some(value) = secret.value {
            print!("{}", value.as_str());
        }
    } else {
        // Default behavior - copy to clipboard
        if let Some(ref value) = secret.value {
            match arboard::Clipboard::new() {
                Ok(mut clipboard) => match clipboard.set_text(value.to_string()) {
                    Ok(_) => {
                        let timeout = config.clipboard_timeout;
                        if timeout > 0 {
                            println!(
                                "✅ Secret '{name}' copied to clipboard (auto-clears in {timeout}s)"
                            );
                            schedule_clipboard_clear(timeout);
                        } else {
                            println!("✅ Secret '{name}' copied to clipboard");
                        }
                    }
                    Err(e) => {
                        eprintln!("⚠️  Failed to copy to clipboard: {e}");
                        eprintln!(
                            "Use 'xv get {name} --raw' to print the value to stdout instead."
                        );
                    }
                },
                Err(e) => {
                    eprintln!("⚠️  Failed to access clipboard: {e}");
                    eprintln!("Use 'xv get {name} --raw' to print the value to stdout instead.");
                }
            }
        } else {
            println!("⚠️  Secret '{name}' has no value");
        }
    }

    Ok(())
}

/// Spawn a detached child process that clears the clipboard after `seconds`.
/// The child outlives the parent process, fixing the issue where std::thread::spawn
/// would be killed when the CLI exits.
fn schedule_clipboard_clear(seconds: u64) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("sh")
            .args(["-c", &format!("sleep {seconds} && printf '' | pbcopy")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    #[cfg(target_os = "linux")]
    {
        // Try xclip first (most common), fall back to xsel
        let cmd = format!(
            "sleep {seconds} && \
             (xclip -selection clipboard < /dev/null 2>/dev/null || \
              xsel --clipboard --delete 2>/dev/null || true)"
        );
        let _ = std::process::Command::new("sh")
            .args(["-c", &cmd])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    #[cfg(target_os = "windows")]
    {
        let cmd = format!("Start-Sleep -Seconds {seconds}; Set-Clipboard ''");
        let _ = std::process::Command::new("powershell")
            .args(["-WindowStyle", "Hidden", "-Command", &cmd])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

async fn execute_secret_history(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use tabled::{Table, Tabled};

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get secret versions using the secret operations
    let versions = secret_manager
        .secret_ops()
        .get_secret_versions(&vault_name, name)
        .await?;

    if versions.is_empty() {
        println!("No versions found for secret '{name}'");
        return Ok(());
    }

    // Display versions in a table
    #[derive(Tabled)]
    struct VersionInfo {
        #[tabled(rename = "Version")]
        version: String,
        #[tabled(rename = "Created")]
        created: String,
        #[tabled(rename = "Updated")]
        updated: String,
        #[tabled(rename = "Enabled")]
        enabled: String,
    }

    let version_infos: Vec<VersionInfo> = versions
        .into_iter()
        .map(|v| VersionInfo {
            version: v.version,
            created: v.created_on,
            updated: v.updated_on,
            enabled: if v.enabled { "Yes" } else { "No" }.to_string(),
        })
        .collect();

    let table = Table::new(&version_infos).to_string();
    println!("Version history for secret '{name}' in vault '{vault_name}':");
    println!();
    println!("{table}");

    Ok(())
}

async fn execute_secret_rollback(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    version: &str,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::interactive::InteractivePrompt;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Confirm rollback unless force flag is used
    if !force {
        let prompt = InteractivePrompt::new();
        let confirm = prompt.confirm(
            &format!("Are you sure you want to rollback secret '{name}' to version '{version}'?"),
            false,
        )?;

        if !confirm {
            println!("Rollback cancelled.");
            return Ok(());
        }
    }

    // Perform rollback using the secret operations
    let result = secret_manager
        .secret_ops()
        .rollback_secret(&vault_name, name, version)
        .await?;

    println!("✅ Successfully rolled back secret '{name}' to version '{version}'");
    println!("New version: {}", result.version);

    Ok(())
}

/// Generate a random value using the specified parameters
fn generate_random_value(
    length: usize,
    charset: CharsetType,
    custom_generator: Option<String>,
) -> Result<Zeroizing<String>> {
    use rand::prelude::*;

    if let Some(generator_script) = custom_generator {
        // Execute custom generator script
        return execute_custom_generator(&generator_script, length).map(Zeroizing::new);
    }

    if length == 0 {
        return Err(CrosstacheError::invalid_argument(
            "Length must be greater than 0",
        ));
    }

    let charset_str = charset.chars();
    let charset_bytes = charset_str.as_bytes();

    if charset_bytes.is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "Character set cannot be empty",
        ));
    }

    let mut rng = thread_rng();
    let random_value: String = (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..charset_bytes.len());
            charset_bytes[idx] as char
        })
        .collect();

    Ok(Zeroizing::new(random_value))
}

/// Execute a custom generator script
fn execute_custom_generator(script_path: &str, length: usize) -> Result<String> {
    use std::process::{Command, Stdio};

    let script = std::path::Path::new(script_path);

    // Check if the script exists
    if !script.exists() {
        return Err(CrosstacheError::config(format!(
            "Generator script not found: {}",
            script_path
        )));
    }

    // Security: validate script ownership and permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(script)
            .map_err(|e| CrosstacheError::config(format!("Cannot read script metadata: {e}")))?;
        let uid = unsafe { libc::getuid() };
        if meta.uid() != uid && meta.uid() != 0 {
            return Err(CrosstacheError::config(format!(
                "Generator script '{}' is not owned by you or root — refusing to execute",
                script_path
            )));
        }
        if meta.mode() & 0o002 != 0 {
            return Err(CrosstacheError::config(format!(
                "Generator script '{}' is world-writable — refusing to execute (chmod o-w to fix)",
                script_path
            )));
        }
    }

    // Set up environment for the script
    let mut cmd = Command::new(script_path);
    cmd.env("XV_SECRET_LENGTH", length.to_string());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Execute the script
    let output = cmd.output().map_err(|e| {
        CrosstacheError::config(format!(
            "Failed to execute generator script '{}': {}",
            script_path, e
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CrosstacheError::config(format!(
            "Generator script failed with exit code {}: {}",
            output.status.code().unwrap_or(-1),
            stderr
        )));
    }

    let generated_value = String::from_utf8(output.stdout)
        .map_err(|e| {
            CrosstacheError::config(format!("Generator script output is not valid UTF-8: {}", e))
        })?
        .trim()
        .to_string();

    if generated_value.is_empty() {
        return Err(CrosstacheError::config(
            "Generator script produced empty output",
        ));
    }

    Ok(generated_value)
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_rotate(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    length: usize,
    charset: CharsetType,
    custom_generator: Option<String>,
    show_value: bool,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::secret::manager::SecretRequest;
    use crate::utils::interactive::InteractivePrompt;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Check if the secret exists first
    let existing_secret = secret_manager
        .secret_ops()
        .get_secret(&vault_name, name, true)
        .await
        .map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to verify secret exists: {}. Use 'xv set' to create a new secret.",
                e
            ))
        })?;

    println!("🔄 Rotating secret: {}", name);

    // Show generation parameters
    if let Some(ref script) = custom_generator {
        println!("  Generator: {} (length: {})", script, length);
    } else {
        println!("  Character set: {:?}", charset);
        println!("  Length: {}", length);
    }

    // Confirm rotation unless force flag is used
    if !force {
        let prompt = InteractivePrompt::new();
        let confirm = prompt.confirm(
            &format!(
                "Are you sure you want to rotate secret '{}'? This will generate a new value and increment the version.",
                name
            ),
            false,
        )?;

        if !confirm {
            println!("Rotation cancelled.");
            return Ok(());
        }
    }

    // Generate the new value
    let new_value = generate_random_value(length, charset, custom_generator)?;

    // Preserve existing secret metadata
    let set_request = SecretRequest {
        name: name.to_string(),
        value: new_value.clone(),
        content_type: if existing_secret.content_type.is_empty() {
            None
        } else {
            Some(existing_secret.content_type)
        },
        enabled: Some(true),
        expires_on: existing_secret.expires_on,
        not_before: existing_secret.not_before,
        tags: if existing_secret.tags.is_empty() {
            None
        } else {
            Some(existing_secret.tags)
        },
        groups: None, // Groups are managed via tags
        note: None,
        folder: None,
    };

    // Set the rotated secret
    let result = secret_manager
        .secret_ops()
        .set_secret(&vault_name, &set_request)
        .await?;

    println!("✅ Successfully rotated secret '{}'", name);
    println!("New version: {}", result.version);

    if show_value {
        println!("Generated value: {}", new_value.as_str());
    } else {
        println!("Generated value: [hidden] (use --show-value to display)");
    }

    println!("💡 Use 'xv history {}' to see version history", name);

    Ok(())
}

async fn execute_secret_run(
    secret_manager: &crate::secret::manager::SecretManager,
    vault: Option<String>,
    groups: Vec<String>,
    no_masking: bool,
    command: Vec<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::helpers::to_env_var_name;
    use regex::Regex;
    use std::collections::HashMap;
    use std::process::{Command, Stdio};

    if command.is_empty() {
        return Err(CrosstacheError::config("No command specified"));
    }

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Parse current environment for xv:// URI references
    let mut uri_secrets: Vec<(String, String)> = Vec::new(); // (vault, secret) pairs
    let uri_regex = Regex::new(r"xv://([^/]+)/([^/\s]+)").unwrap();

    for (_env_name, env_value) in std::env::vars() {
        for captures in uri_regex.captures_iter(&env_value) {
            if let Some(vault_match) = captures.get(1) {
                if let Some(secret_match) = captures.get(2) {
                    let target_vault = vault_match.as_str().to_string();
                    let secret_name = secret_match.as_str().to_string();
                    let pair = (target_vault, secret_name);
                    if !uri_secrets.contains(&pair) {
                        uri_secrets.push(pair);
                    }
                }
            }
        }
    }

    // Get all secrets from the vault
    let secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, None)
        .await?;

    // Filter secrets by groups if specified
    let filtered_secrets = if !groups.is_empty() {
        secrets
            .into_iter()
            .filter(|secret| {
                if let Some(secret_groups) = &secret.groups {
                    // Secret can have multiple groups (comma-separated)
                    let secret_group_list: Vec<&str> =
                        secret_groups.split(',').map(|g| g.trim()).collect();
                    groups
                        .iter()
                        .any(|filter_group| secret_group_list.contains(&filter_group.as_str()))
                } else {
                    false
                }
            })
            .collect()
    } else {
        secrets
    };

    if filtered_secrets.is_empty() {
        println!("No secrets found to inject");
        return Ok(());
    }

    println!(
        "🔐 Injecting {} secret(s) as environment variables...",
        filtered_secrets.len()
    );

    // Fetch secret values and build environment map
    let mut env_vars: HashMap<String, Zeroizing<String>> = HashMap::new();
    let mut secret_values: Vec<Zeroizing<String>> = Vec::new(); // For masking
    let mut uri_values: HashMap<String, Zeroizing<String>> = HashMap::new(); // URI -> value mapping

    // Fetch secrets from current vault (group-filtered)
    for secret in filtered_secrets {
        // Get the secret value
        match secret_manager
            .secret_ops()
            .get_secret(&vault_name, &secret.name, true)
            .await
        {
            Ok(secret_props) => {
                if let Some(value) = secret_props.value {
                    let env_name = to_env_var_name(&secret.name);
                    env_vars.insert(env_name, value.clone());

                    // Store for masking (if enabled)
                    if !no_masking && !value.is_empty() {
                        secret_values.push(value.clone());
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "⚠️  Failed to get value for secret '{}': {}",
                    secret.name, e
                );
            }
        }
    }

    // Fetch cross-vault secrets referenced by URIs in environment
    if !uri_secrets.is_empty() {
        println!(
            "🔗 Found {} cross-vault URI reference(s) in environment",
            uri_secrets.len()
        );

        for (target_vault, secret_name) in &uri_secrets {
            let uri = format!("xv://{}/{}", target_vault, secret_name);

            match secret_manager
                .secret_ops()
                .get_secret(target_vault, secret_name, true)
                .await
            {
                Ok(secret_props) => {
                    if let Some(value) = secret_props.value {
                        uri_values.insert(uri.clone(), value.clone());

                        // Store for masking (if enabled)
                        if !no_masking && !value.is_empty() {
                            secret_values.push(value);
                        }
                    } else {
                        eprintln!(
                            "⚠️  Secret '{}' in vault '{}' has no value",
                            secret_name, target_vault
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "⚠️  Failed to get secret '{}' from vault '{}': {}",
                        secret_name, target_vault, e
                    );
                }
            }
        }
    }

    // Set up the command
    let mut cmd = Command::new(&command[0]);
    if command.len() > 1 {
        cmd.args(&command[1..]);
    }

    // Set environment variables from vault secrets
    cmd.envs(&env_vars);

    // Resolve URI references in existing environment variables
    if !uri_values.is_empty() {
        for (env_name, env_value) in std::env::vars() {
            let mut resolved_value = env_value.clone();

            // Replace any xv:// URIs with actual secret values
            for (uri, secret_value) in &uri_values {
                resolved_value = resolved_value.replace(uri, secret_value);
            }

            // Only set if the value changed (had URI references)
            if resolved_value != env_value {
                cmd.env(env_name, resolved_value);
            }
        }
    }

    // Set up stdio for output capture and masking
    if no_masking {
        // Direct passthrough
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        // Capture output for masking
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    }

    println!("🚀 Executing: {}", command.join(" "));

    // Execute the command
    let output = cmd.output().map_err(|e| {
        CrosstacheError::config(format!("Failed to execute command '{}': {}", command[0], e))
    })?;

    // Explicitly drop secret-holding variables to zeroize them immediately after child process
    // Note: secret_values is still needed for masking, so we can't drop it yet
    drop(env_vars);
    drop(uri_values);

    // Handle output with masking if needed
    if no_masking {
        // Exit with the same code as the child process
        std::process::exit(output.status.code().unwrap_or(1));
    } else {
        // Mask secret values in output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let masked_stdout = mask_secrets(&stdout, &secret_values);
        let masked_stderr = mask_secrets(&stderr, &secret_values);

        // Zeroize secret values now that masking is complete
        drop(secret_values);

        print!("{}", masked_stdout);
        eprint!("{}", masked_stderr);

        // Exit with the same code as the child process
        std::process::exit(output.status.code().unwrap_or(1));
    }
}

/// Mask secret values in text output
fn mask_secrets(text: &str, secrets: &[Zeroizing<String>]) -> String {
    let mut result = text.to_string();

    for secret in secrets {
        if secret.len() >= 4 {
            // Only mask secrets that are at least 4 characters
            // Replace with [MASKED] to indicate redaction
            result = result.replace(secret.as_str(), "[MASKED]");
        }
    }

    result
}

async fn execute_secret_inject(
    secret_manager: &crate::secret::manager::SecretManager,
    vault: Option<String>,
    template_file: Option<String>,
    output_file: Option<String>,
    groups: Vec<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use regex::Regex;
    use std::collections::HashMap;
    use std::fs;
    use std::io::{self, Read};

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Read template content
    let template_content = match template_file {
        Some(path) => fs::read_to_string(&path).map_err(|e| {
            CrosstacheError::config(format!("Failed to read template file '{}': {}", path, e))
        })?,
        None => {
            // Read from stdin
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer).map_err(|e| {
                CrosstacheError::config(format!("Failed to read from stdin: {}", e))
            })?;
            buffer
        }
    };

    // Parse template for secret references
    // Supports: {{ secret:name }} and xv://vault-name/secret-name
    let secret_regex = Regex::new(r"\{\{\s*secret:([^}\s]+)\s*\}\}").unwrap();
    let uri_regex = Regex::new(r"xv://([^/]+)/([^/\s]+)").unwrap();

    let mut required_secrets: Vec<String> = Vec::new();
    let mut cross_vault_secrets: Vec<(String, String)> = Vec::new(); // (vault, secret) pairs

    // Find {{ secret:name }} references (current vault)
    for captures in secret_regex.captures_iter(&template_content) {
        if let Some(secret_name) = captures.get(1) {
            let name = secret_name.as_str().to_string();
            if !required_secrets.contains(&name) {
                required_secrets.push(name);
            }
        }
    }

    // Find xv://vault/secret URI references
    for captures in uri_regex.captures_iter(&template_content) {
        if let Some(vault_match) = captures.get(1) {
            if let Some(secret_match) = captures.get(2) {
                let vault = vault_match.as_str().to_string();
                let secret = secret_match.as_str().to_string();
                let pair = (vault, secret);
                if !cross_vault_secrets.contains(&pair) {
                    cross_vault_secrets.push(pair);
                }
            }
        }
    }

    if required_secrets.is_empty() && cross_vault_secrets.is_empty() {
        println!("⚠️  No secret references found in template");
        println!("    Use {{ secret:name }} syntax or xv://vault-name/secret-name URIs");

        // Still write the template content as-is to output
        match output_file {
            Some(path) => {
                crate::utils::helpers::write_sensitive_file(
                    std::path::Path::new(&path),
                    template_content.as_bytes(),
                )
                .map_err(|e| {
                    CrosstacheError::config(format!(
                        "Failed to write to output file '{}': {}",
                        path, e
                    ))
                })?;
                println!("Template written to '{}'", path);
            }
            None => {
                print!("{}", template_content);
            }
        }
        return Ok(());
    }

    let total_references = required_secrets.len() + cross_vault_secrets.len();
    println!(
        "📋 Found {} secret reference(s) in template",
        total_references
    );

    if !required_secrets.is_empty() {
        println!(
            "  Current vault ({}): {} secret(s)",
            vault_name,
            required_secrets.len()
        );
    }
    if !cross_vault_secrets.is_empty() {
        println!("  Cross-vault: {} secret(s)", cross_vault_secrets.len());
    }

    // Get all secrets from the vault
    let secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, None)
        .await?;

    // Filter secrets by groups if specified
    let available_secrets = if !groups.is_empty() {
        secrets
            .into_iter()
            .filter(|secret| {
                if let Some(secret_groups) = &secret.groups {
                    let secret_group_list: Vec<&str> =
                        secret_groups.split(',').map(|g| g.trim()).collect();
                    groups
                        .iter()
                        .any(|filter_group| secret_group_list.contains(&filter_group.as_str()))
                } else {
                    false
                }
            })
            .collect()
    } else {
        secrets
    };

    // Build a map of secret names/URIs to values
    let mut secret_values: HashMap<String, Zeroizing<String>> = HashMap::new();
    let mut cross_vault_values: HashMap<String, Zeroizing<String>> = HashMap::new(); // URI -> value
    let mut missing_secrets: Vec<String> = Vec::new();

    // Fetch secrets from current vault
    for secret_name in &required_secrets {
        // Check if the secret exists in the available secrets
        if let Some(secret_summary) = available_secrets.iter().find(|s| s.name == *secret_name) {
            // Get the secret value
            match secret_manager
                .secret_ops()
                .get_secret(&vault_name, &secret_summary.name, true)
                .await
            {
                Ok(secret_props) => {
                    if let Some(value) = secret_props.value {
                        secret_values.insert(secret_name.clone(), value);
                    } else {
                        missing_secrets.push(secret_name.clone());
                    }
                }
                Err(e) => {
                    eprintln!(
                        "⚠️  Failed to get value for secret '{}' from vault '{}': {}",
                        secret_name, vault_name, e
                    );
                    missing_secrets.push(secret_name.clone());
                }
            }
        } else {
            missing_secrets.push(secret_name.clone());
        }
    }

    // Fetch cross-vault secrets
    for (target_vault, secret_name) in &cross_vault_secrets {
        let uri = format!("xv://{}/{}", target_vault, secret_name);

        match secret_manager
            .secret_ops()
            .get_secret(target_vault, secret_name, true)
            .await
        {
            Ok(secret_props) => {
                if let Some(value) = secret_props.value {
                    cross_vault_values.insert(uri.clone(), value);
                } else {
                    eprintln!(
                        "⚠️  Secret '{}' in vault '{}' has no value",
                        secret_name, target_vault
                    );
                    missing_secrets.push(uri);
                }
            }
            Err(e) => {
                eprintln!(
                    "⚠️  Failed to get secret '{}' from vault '{}': {}",
                    secret_name, target_vault, e
                );
                missing_secrets.push(uri);
            }
        }
    }

    if !missing_secrets.is_empty() {
        return Err(CrosstacheError::config(format!(
            "Missing secrets: {}",
            missing_secrets.join(", ")
        )));
    }

    let total_injected = secret_values.len() + cross_vault_values.len();
    println!("🔐 Injecting {} secret(s) into template...", total_injected);

    // Replace secret references with actual values
    let mut result_content = Zeroizing::new(template_content);

    // Replace {{ secret:name }} references (current vault)
    for (secret_name, secret_value) in &secret_values {
        let pattern = format!(r"\{{\{{\s*secret:{}\s*\}}\}}", regex::escape(secret_name));
        let regex_pattern = Regex::new(&pattern).unwrap();
        *result_content = regex_pattern
            .replace_all(&result_content, secret_value.as_str())
            .to_string();
    }

    // Replace xv://vault/secret URI references
    for (uri, secret_value) in &cross_vault_values {
        *result_content = result_content.replace(uri, secret_value.as_str());
    }

    // Write result
    match output_file {
        Some(path) => {
            crate::utils::helpers::write_sensitive_file(
                std::path::Path::new(&path),
                result_content.as_bytes(),
            )
            .map_err(|e| {
                CrosstacheError::config(format!("Failed to write to output file '{}': {}", path, e))
            })?;
            println!(
                "✅ Template resolved and written to '{}' (permissions: owner-only)",
                path
            );
            eprintln!("⚠️  Output file contains resolved secrets — treat as sensitive");
        }
        None => {
            print!("{}", result_content.as_str());
        }
    }

    Ok(())
}

async fn execute_secret_list(
    secret_manager: &crate::secret::manager::SecretManager,
    vault: Option<String>,
    group: Option<String>,
    show_all: bool,
    expiring: Option<String>,
    expired: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    let output_format = if config.output_json {
        crate::utils::format::OutputFormat::Json
    } else {
        crate::utils::format::OutputFormat::Table
    };

    // Get the basic secret list first
    let mut secrets = secret_manager
        .list_secrets_formatted(
            &vault_name,
            group.as_deref(),
            output_format.clone(),
            false,
            show_all,
        )
        .await?;

    // Apply expiry filtering if requested
    if expired || expiring.is_some() {
        use crate::utils::datetime::{is_expired, is_expiring_within};

        // We need to get full secret details to check expiry dates
        let mut filtered_secrets = Vec::new();

        for secret_summary in secrets {
            // Get full secret details to access expiry dates
            match secret_manager
                .get_secret_safe(&vault_name, &secret_summary.name, false, true)
                .await
            {
                Ok(secret_props) => {
                    let should_include = if expired {
                        // Show only expired secrets
                        is_expired(secret_props.expires_on)
                    } else if let Some(ref duration) = expiring {
                        // Show secrets expiring within the specified duration
                        match is_expiring_within(secret_props.expires_on, duration) {
                            Ok(is_exp) => is_exp,
                            Err(e) => {
                                eprintln!("Warning: Invalid duration '{}': {}", duration, e);
                                false
                            }
                        }
                    } else {
                        true // Include all if no expiry filter
                    };

                    if should_include {
                        filtered_secrets.push(secret_summary);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to get details for secret '{}': {}",
                        secret_summary.name, e
                    );
                }
            }
        }

        secrets = filtered_secrets;

        // Display the filtered results
        if secrets.is_empty() {
            let filter_desc = if expired {
                "expired"
            } else if expiring.is_some() {
                "expiring"
            } else {
                "matching"
            };
            println!(
                "No {} secrets found in vault '{}'.",
                filter_desc, vault_name
            );
        } else {
            // Re-display with the filtered list
            if output_format == crate::utils::format::OutputFormat::Table {
                use crate::utils::format::format_table;
                use tabled::Table;

                let table = Table::new(&secrets);
                println!("{}", format_table(table, config.no_color));

                let filter_desc = if expired {
                    "expired".to_string()
                } else if let Some(ref duration) = expiring {
                    format!("expiring within {}", duration)
                } else {
                    "matching".to_string()
                };
                println!(
                    "\nShowing {} {} secret(s) in vault '{}'",
                    secrets.len(),
                    filter_desc,
                    vault_name
                );
            } else {
                let json_output = serde_json::to_string_pretty(&secrets).map_err(|e| {
                    CrosstacheError::serialization(format!("Failed to serialize secrets: {e}"))
                })?;
                println!("{}", json_output);
            }
        }
    }

    Ok(())
}

async fn execute_secret_delete(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Confirmation unless forced
    if !force {
        let confirm = rpassword::prompt_password(format!(
            "Are you sure you want to delete secret '{name}' from vault '{vault_name}'? (y/N): "
        ))?;

        if confirm.to_lowercase() != "y" && confirm.to_lowercase() != "yes" {
            println!("Delete operation cancelled.");
            return Ok(());
        }
    }

    secret_manager
        .delete_secret_safe(&vault_name, name, force)
        .await?;
    println!("✅ Successfully deleted secret '{name}'");

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_update(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    value: Option<String>,
    stdin: bool,
    tags: Vec<(String, String)>,
    groups: Vec<String>,
    rename: Option<String>,
    note: Option<String>,
    folder: Option<String>,
    replace_tags: bool,
    replace_groups: bool,
    expires: Option<String>,
    not_before: Option<String>,
    clear_expires: bool,
    clear_not_before: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::secret::manager::SecretUpdateRequest;
    use std::collections::HashMap;
    use std::io::{self, Read};

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get new value if explicitly provided (but don't prompt)
    let new_value = if let Some(v) = value {
        // Validate provided value
        if v.is_empty() {
            return Err(CrosstacheError::config("Secret value cannot be empty"));
        }
        Some(Zeroizing::new(v))
    } else if stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        let trimmed = buffer.trim().to_string();
        if trimmed.is_empty() {
            return Err(CrosstacheError::config("Secret value cannot be empty"));
        }
        Some(Zeroizing::new(trimmed))
    } else {
        None // Don't update value, just metadata
    };

    // Ensure at least one update is specified
    if new_value.is_none()
        && tags.is_empty()
        && groups.is_empty()
        && rename.is_none()
        && note.is_none()
        && folder.is_none()
        && expires.is_none()
        && not_before.is_none()
        && !clear_expires
        && !clear_not_before
    {
        return Err(CrosstacheError::invalid_argument(
            "No updates specified. Use 'secret update' to modify metadata (groups, tags, folder, note, expiry) or rename secrets. Use 'secret set' to update secret values."
        ));
    }

    // Convert tags vector to HashMap
    let tags_map = if !tags.is_empty() {
        Some(tags.into_iter().collect::<HashMap<String, String>>())
    } else {
        None
    };

    // Convert groups vector to Option
    let groups_vec = if !groups.is_empty() {
        Some(groups)
    } else {
        None
    };

    // Validate rename if provided
    if let Some(ref new_name) = rename {
        if new_name.is_empty() {
            return Err(CrosstacheError::invalid_argument(
                "New secret name cannot be empty",
            ));
        }
        if new_name == name {
            return Err(CrosstacheError::invalid_argument(
                "New secret name must be different from current name",
            ));
        }
    }

    // Parse expiry dates if provided
    let expires_on = if clear_expires {
        None // Explicitly clear the expiry date
    } else if let Some(expires_str) = expires.as_deref() {
        use crate::utils::datetime::parse_datetime_or_duration;
        Some(parse_datetime_or_duration(expires_str)?)
    } else {
        None // No change to expiry
    };

    let not_before_on = if clear_not_before {
        None // Explicitly clear the not-before date
    } else if let Some(not_before_str) = not_before.as_deref() {
        use crate::utils::datetime::parse_datetime_or_duration;
        Some(parse_datetime_or_duration(not_before_str)?)
    } else {
        None // No change to not-before
    };

    // Create update request with enhanced parameters
    let update_request = SecretUpdateRequest {
        name: name.to_string(),
        new_name: rename.clone(),
        value: new_value.clone(),
        content_type: None,
        enabled: None,
        expires_on,
        not_before: not_before_on,
        tags: tags_map,
        groups: groups_vec,
        note: note.clone(),
        folder: folder.clone(),
        replace_tags,
        replace_groups,
    };

    // Show update summary
    println!("Updating secret '{name}'...");
    if let Some(ref new_name) = rename {
        println!("  → Renaming to: {new_name}");
    }
    if new_value.is_some() {
        println!("  → Updating value");
    }
    if !update_request
        .tags
        .as_ref()
        .map(|t| t.is_empty())
        .unwrap_or(true)
    {
        let action = if replace_tags { "Replacing" } else { "Merging" };
        println!(
            "  → {} tags: {}",
            action,
            update_request.tags.as_ref().unwrap().len()
        );
    }
    if !update_request
        .groups
        .as_ref()
        .map(|g| g.is_empty())
        .unwrap_or(true)
    {
        let action = if replace_groups {
            "Replacing"
        } else {
            "Adding to"
        };
        println!(
            "  → {} groups: {:?}",
            action,
            update_request.groups.as_ref().unwrap()
        );
    }
    if let Some(ref note_text) = note {
        println!("  → Adding note: {note_text}");
    }
    if let Some(ref folder_path) = folder {
        println!("  → Setting folder: {folder_path}");
    }
    if clear_expires {
        println!("  → Clearing expiry date");
    } else if let Some(ref expires_str) = expires {
        println!("  → Setting expiry: {expires_str}");
    }
    if clear_not_before {
        println!("  → Clearing not-before date");
    } else if let Some(ref not_before_str) = not_before {
        println!("  → Setting not-before: {not_before_str}");
    }

    // Perform enhanced secret update
    let secret = secret_manager
        .update_secret_enhanced(&vault_name, &update_request)
        .await?;

    println!("✅ Successfully updated secret '{}'", secret.original_name);
    println!("   Vault: {vault_name}");
    println!("   Version: {}", secret.version);

    if let Some(ref new_name) = rename {
        println!("   New Name: {new_name}");
    }

    Ok(())
}

async fn execute_secret_purge(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Confirmation unless forced
    if !force {
        let confirm = rpassword::prompt_password(format!(
            "Are you sure you want to PERMANENTLY DELETE secret '{name}' from vault '{vault_name}'? This cannot be undone! (y/N): "
        ))?;

        if confirm.to_lowercase() != "y" && confirm.to_lowercase() != "yes" {
            println!("Purge operation cancelled.");
            return Ok(());
        }
    }

    // Permanently purge the secret using the secret manager
    secret_manager
        .purge_secret_safe(&vault_name, name, force)
        .await?;
    println!("✅ Successfully purged secret '{name}'");

    Ok(())
}

async fn execute_secret_restore(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    println!("Restoring deleted secret '{name}'...");

    // Restore the secret using the secret manager
    let restored_secret = secret_manager
        .restore_secret_safe(&vault_name, name)
        .await?;

    println!(
        "✅ Successfully restored secret '{}'",
        restored_secret.original_name
    );
    println!("   Vault: {vault_name}");
    println!("   Version: {}", restored_secret.version);
    println!("   Enabled: {}", restored_secret.enabled);
    println!("   Created: {}", restored_secret.created_on);
    println!("   Updated: {}", restored_secret.updated_on);

    if !restored_secret.tags.is_empty() {
        println!("   Tags: {}", restored_secret.tags.len());
    }

    Ok(())
}

async fn execute_secret_copy(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    _config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::secret::manager::SecretRequest;

    // Determine target name (use new_name if provided, otherwise use original)
    let target_name = new_name.as_deref().unwrap_or(name);

    println!(
        "Copying secret '{}' from vault '{}' to vault '{}' as '{}'...",
        name, from_vault, to_vault, target_name
    );

    // Get the source secret with all its metadata
    let source_secret = secret_manager
        .get_secret_safe(from_vault, name, true, true)
        .await?;

    // Check if target secret already exists
    if secret_manager
        .get_secret_safe(to_vault, target_name, false, true)
        .await
        .is_ok()
    {
        return Err(CrosstacheError::config(format!(
            "Secret '{}' already exists in vault '{}'. Use 'xv move' with --force or delete the target secret first.",
            target_name, to_vault
        )));
    }

    // Create the request for the target vault preserving all metadata
    let secret_request = SecretRequest {
        name: target_name.to_string(),
        value: source_secret.value.unwrap_or_default(),
        content_type: Some(source_secret.content_type),
        enabled: Some(source_secret.enabled),
        expires_on: source_secret.expires_on,
        not_before: source_secret.not_before,
        tags: Some(source_secret.tags),
        groups: None, // Will be preserved through tags
        note: None,   // Will be preserved through tags
        folder: None, // Will be preserved through tags
    };

    // Set the secret in the target vault
    let value = secret_request.value.clone();
    let copied_secret = secret_manager
        .set_secret_safe(to_vault, target_name, &value, Some(secret_request))
        .await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(to_vault).await;

    println!(
        "✅ Successfully copied secret '{}' to vault '{}'",
        copied_secret.original_name, to_vault
    );
    println!("   Source: {}/{}", from_vault, name);
    println!("   Target: {}/{}", to_vault, target_name);
    println!("   Version: {}", copied_secret.version);
    println!("   Enabled: {}", copied_secret.enabled);

    if let Some(expires_on) = copied_secret.expires_on {
        use crate::utils::datetime::format_datetime;
        println!("   Expires: {}", format_datetime(Some(expires_on)));
    }

    Ok(())
}

async fn execute_secret_move(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::utils::interactive::InteractivePrompt;

    // Determine target name (use new_name if provided, otherwise use original)
    let target_name = new_name.as_deref().unwrap_or(name);

    println!(
        "Moving secret '{}' from vault '{}' to vault '{}' as '{}'...",
        name, from_vault, to_vault, target_name
    );

    // Confirmation prompt if not forced
    if !force {
        let prompt = InteractivePrompt::new();
        let message = format!(
            "This will delete secret '{}' from vault '{}' after copying it to vault '{}'. Continue?",
            name, from_vault, to_vault
        );
        if !prompt.confirm(&message, false)? {
            println!("Move operation cancelled.");
            return Ok(());
        }
    }

    // Check if target secret already exists and handle accordingly
    if secret_manager
        .get_secret_safe(to_vault, target_name, false, true)
        .await
        .is_ok()
    {
        if !force {
            return Err(CrosstacheError::config(format!(
                "Secret '{}' already exists in vault '{}'. Use --force to overwrite.",
                target_name, to_vault
            )));
        } else {
            println!(
                "⚠️  Overwriting existing secret '{}' in vault '{}'",
                target_name, to_vault
            );
        }
    }

    // First copy the secret
    execute_secret_copy(
        secret_manager,
        name,
        from_vault,
        to_vault,
        new_name.clone(),
        config,
    )
    .await?;

    // Then delete from source
    println!(
        "Deleting source secret '{}' from vault '{}'...",
        name, from_vault
    );
    secret_manager
        .delete_secret_safe(from_vault, name, true)
        .await?;

    println!(
        "✅ Successfully moved secret '{}' from '{}' to '{}'",
        name, from_vault, to_vault
    );

    Ok(())
}

async fn execute_secret_parse(
    secret_manager: &crate::secret::manager::SecretManager,
    connection_string: &str,
    format: &str,
    config: &Config,
) -> Result<()> {
    let components = secret_manager
        .parse_connection_string(connection_string)
        .await?;

    match format.to_lowercase().as_str() {
        "json" => {
            let json_output = serde_json::to_string_pretty(&components).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize components: {e}"))
            })?;
            println!("{json_output}");
        }
        "table" => {
            if components.is_empty() {
                println!("No components found in connection string");
            } else {
                use crate::utils::format::format_table;
                use tabled::Table;

                let table = Table::new(&components);
                println!("{}", format_table(table, config.no_color));
            }
        }
        _ => {
            println!("Unimnplemented format selected: {format}");
        }
    }

    Ok(())
}

async fn execute_secret_share(
    vault_manager: &crate::vault::manager::VaultManager,
    auth_provider: &std::sync::Arc<dyn crate::auth::provider::AzureAuthProvider>,
    command: ShareCommands,
    config: &Config,
) -> Result<()> {
    use crate::vault::models::AccessLevel;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(None).await?;
    let resource_group = config.default_resource_group.clone();

    match command {
        ShareCommands::Grant {
            secret_name,
            user,
            level,
        } => {
            let object_id = auth_provider.resolve_user_to_object_id(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            let access_level = match level.to_lowercase().as_str() {
                "reader" | "read" => AccessLevel::Reader,
                "contributor" | "write" => AccessLevel::Contributor,
                "admin" | "administrator" => AccessLevel::Admin,
                _ => {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "Invalid access level: {level}"
                    )))
                }
            };

            vault_manager
                .grant_secret_access(
                    &vault_name,
                    &resource_group,
                    &secret_name,
                    &object_id,
                    access_level,
                )
                .await?;

            println!(
                "Successfully granted {} access to secret '{}' for '{}' in vault '{}'",
                level, secret_name, user, vault_name
            );
        }
        ShareCommands::Revoke { secret_name, user } => {
            let object_id = auth_provider.resolve_user_to_object_id(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            vault_manager
                .revoke_secret_access(&vault_name, &resource_group, &secret_name, &object_id)
                .await?;

            println!(
                "Successfully revoked access to secret '{}' for '{}' in vault '{}'",
                secret_name, user, vault_name
            );
        }
        ShareCommands::List { secret_name } => {
            let roles = vault_manager
                .list_secret_access(&vault_name, &resource_group, &secret_name)
                .await?;

            if roles.is_empty() {
                println!(
                    "No access assignments found for secret '{}' in vault '{}'",
                    secret_name, vault_name
                );
            } else {
                println!(
                    "Access assignments for secret '{}' in vault '{}':",
                    secret_name, vault_name
                );
                let formatter = crate::utils::format::TableFormatter::new(
                    crate::utils::format::OutputFormat::Table,
                    config.no_color,
                );
                let table_output = formatter.format_table(&roles)?;
                println!("{table_output}");
            }
        }
    }

    Ok(())
}

async fn execute_vault_restore(
    vault_manager: &VaultManager,
    name: &str,
    location: &str,
    _config: &Config,
) -> Result<()> {
    vault_manager.restore_vault(name, location).await?;
    Ok(())
}

async fn execute_vault_purge(
    vault_manager: &VaultManager,
    name: &str,
    location: &str,
    force: bool,
    _config: &Config,
) -> Result<()> {
    vault_manager
        .purge_vault_permanent(name, location, force)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_vault_export(
    _vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    output: Option<String>,
    format: &str,
    include_values: bool,
    group: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    let _resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Create secret manager to get secrets from vault
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Get all secrets from vault (including disabled ones for export)
    let secrets = secret_manager
        .list_secrets_formatted(
            name,
            group.as_deref(),
            OutputFormat::Json,
            false,
            true, // show_all = true for export
        )
        .await?;

    // Prepare export data based on format
    let export_data = match format.to_lowercase().as_str() {
        "json" => {
            let mut export_json = serde_json::Map::new();
            export_json.insert(
                "vault".to_string(),
                serde_json::Value::String(name.to_string()),
            );
            export_json.insert(
                "exported_at".to_string(),
                serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
            );

            let mut secrets_json = Vec::new();
            for secret in &secrets {
                let mut secret_data = serde_json::Map::new();
                secret_data.insert(
                    "name".to_string(),
                    serde_json::Value::String(secret.original_name.clone()),
                );
                secret_data.insert(
                    "enabled".to_string(),
                    serde_json::Value::Bool(secret.enabled),
                );
                secret_data.insert(
                    "content_type".to_string(),
                    serde_json::Value::String(secret.content_type.clone()),
                );

                if include_values {
                    // Get actual secret value
                    match secret_manager
                        .get_secret_safe(name, &secret.original_name, true, true)
                        .await
                    {
                        Ok(secret_props) => {
                            if let Some(value) = secret_props.value {
                                secret_data.insert(
                                    "value".to_string(),
                                    serde_json::Value::String(value.to_string()),
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to get value for secret '{}': {}",
                                secret.original_name, e
                            );
                        }
                    }
                }

                secrets_json.push(serde_json::Value::Object(secret_data));
            }
            export_json.insert(
                "secrets".to_string(),
                serde_json::Value::Array(secrets_json),
            );

            serde_json::to_string_pretty(&export_json).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize export data: {e}"))
            })?
        }
        "env" => {
            let mut env_lines = Vec::new();
            env_lines.push(format!(
                "# Exported from vault '{}' on {}",
                name,
                chrono::Utc::now().to_rfc3339()
            ));

            for secret in &secrets {
                if include_values {
                    match secret_manager
                        .get_secret_safe(name, &secret.original_name, true, true)
                        .await
                    {
                        Ok(secret_props) => {
                            if let Some(value) = secret_props.value {
                                let env_name = secret
                                    .original_name
                                    .to_uppercase()
                                    .replace("-", "_")
                                    .replace(".", "_");
                                env_lines.push(format!("{env_name}={}", value.as_str()));
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to get value for secret '{}': {}",
                                secret.original_name, e
                            );
                        }
                    }
                } else {
                    let env_name = secret
                        .original_name
                        .to_uppercase()
                        .replace("-", "_")
                        .replace(".", "_");
                    env_lines.push(format!("# {env_name}"));
                }
            }

            env_lines.join("\n")
        }
        "txt" => {
            let mut txt_lines = Vec::new();
            txt_lines.push(format!("Vault: {name}"));
            txt_lines.push(format!("Exported: {}", chrono::Utc::now().to_rfc3339()));
            txt_lines.push("".to_string());

            for secret in &secrets {
                txt_lines.push(format!("Secret: {}", secret.original_name));
                txt_lines.push(format!("  Enabled: {}", secret.enabled));
                txt_lines.push(format!("  Content Type: {}", secret.content_type));
                txt_lines.push(format!("  Updated: {}", secret.updated_on));

                if include_values {
                    match secret_manager
                        .get_secret_safe(name, &secret.original_name, true, true)
                        .await
                    {
                        Ok(secret_props) => {
                            if let Some(value) = secret_props.value {
                                txt_lines.push(format!("  Value: {}", value.as_str()));
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to get value for secret '{}': {}",
                                secret.original_name, e
                            );
                        }
                    }
                }
                txt_lines.push("".to_string());
            }

            txt_lines.join("\n")
        }
        _ => {
            return Err(CrosstacheError::invalid_argument(format!(
                "Unsupported export format: {format}"
            )));
        }
    };

    // Write to output
    match output {
        Some(file_path) => {
            crate::utils::helpers::write_sensitive_file(
                std::path::Path::new(&file_path),
                export_data.as_bytes(),
            )
            .map_err(|e| {
                CrosstacheError::unknown(format!("Failed to write to output file: {e}"))
            })?;
            println!(
                "Exported {} secrets to {} (permissions: owner-only)",
                secrets.len(),
                file_path
            );
        }
        None => {
            println!("{export_data}");
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_vault_import(
    _vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    input: Option<String>,
    format: &str,
    overwrite: bool,
    dry_run: bool,
    config: &Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::{SecretManager, SecretRequest};
    use std::fs;
    use std::io::{self, Read};
    use std::sync::Arc;

    let _resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Read import data
    let import_data = match input {
        Some(file_path) => fs::read_to_string(file_path)
            .map_err(|e| CrosstacheError::unknown(format!("Failed to read input file: {e}")))?,
        None => {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|e| CrosstacheError::unknown(format!("Failed to read from stdin: {e}")))?;
            buffer
        }
    };

    // Parse import data based on format
    let secrets_to_import = match format.to_lowercase().as_str() {
        "json" => {
            let json_data: serde_json::Value = serde_json::from_str(&import_data).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to parse JSON: {e}"))
            })?;

            let secrets_array = json_data
                .get("secrets")
                .and_then(|s| s.as_array())
                .ok_or_else(|| CrosstacheError::serialization("Missing 'secrets' array in JSON"))?;

            let mut secrets = Vec::new();
            for secret_value in secrets_array {
                let secret_obj = secret_value.as_object().ok_or_else(|| {
                    CrosstacheError::serialization("Invalid secret object in JSON")
                })?;

                let name = secret_obj
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| CrosstacheError::serialization("Missing secret name"))?;

                let value = secret_obj
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| CrosstacheError::serialization("Missing secret value"))?;

                let content_type = secret_obj
                    .get("content_type")
                    .and_then(|ct| ct.as_str())
                    .map(|s| s.to_string());

                let enabled = secret_obj.get("enabled").and_then(|e| e.as_bool());

                secrets.push(SecretRequest {
                    name: name.to_string(),
                    value: Zeroizing::new(value.to_string()),
                    content_type,
                    enabled,
                    expires_on: None,
                    not_before: None,
                    tags: None,
                    groups: None,
                    note: None,
                    folder: None,
                });
            }

            secrets
        }
        "env" => {
            let mut secrets = Vec::new();
            for line in import_data.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                if let Some(pos) = line.find('=') {
                    let key = line[..pos].trim().to_lowercase().replace("_", "-");
                    let value = line[pos + 1..].trim();

                    secrets.push(SecretRequest {
                        name: key,
                        value: Zeroizing::new(value.to_string()),
                        content_type: Some("text/plain".to_string()),
                        enabled: Some(true),
                        expires_on: None,
                        not_before: None,
                        tags: None,
                        groups: None,
                        note: None,
                        folder: None,
                    });
                }
            }

            secrets
        }
        _ => {
            return Err(CrosstacheError::invalid_argument(format!(
                "Unsupported import format: {format}"
            )));
        }
    };

    if dry_run {
        println!(
            "Dry run: Would import {} secrets to vault '{}':",
            secrets_to_import.len(),
            name
        );
        for secret in &secrets_to_import {
            println!("  - {}", secret.name);
        }
        return Ok(());
    }

    // Create secret manager to import secrets
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    let mut imported_count = 0;
    let mut skipped_count = 0;

    for secret_request in secrets_to_import {
        let secret_name = secret_request.name.clone();
        let secret_value = secret_request.value.clone();

        // Check if secret exists if not overwriting
        if !overwrite {
            match secret_manager
                .get_secret_safe(name, &secret_name, false, true)
                .await
            {
                Ok(_) => {
                    println!("Skipping existing secret: {secret_name}");
                    skipped_count += 1;
                    continue;
                }
                Err(_) => {
                    // Secret doesn't exist, proceed with import
                }
            }
        }

        match secret_manager
            .set_secret_safe(name, &secret_name, &secret_value, Some(secret_request))
            .await
        {
            Ok(_) => {
                println!("Imported secret: {secret_name}");
                imported_count += 1;
            }
            Err(e) => {
                eprintln!("Failed to import secret '{secret_name}': {e}");
            }
        }
    }

    println!("Import completed: {imported_count} imported, {skipped_count} skipped");

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_vault_update(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    tags: Vec<(String, String)>,
    enable_deployment: Option<bool>,
    enable_disk_encryption: Option<bool>,
    enable_template_deployment: Option<bool>,
    enable_purge_protection: Option<bool>,
    retention_days: Option<i32>,
    config: &Config,
) -> Result<()> {
    use crate::vault::models::VaultUpdateRequest;
    use std::collections::HashMap;

    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Convert tags vector to HashMap
    let tags_map = if !tags.is_empty() {
        Some(tags.into_iter().collect::<HashMap<String, String>>())
    } else {
        None
    };

    let update_request = VaultUpdateRequest {
        enabled_for_deployment: enable_deployment,
        enabled_for_disk_encryption: enable_disk_encryption,
        enabled_for_template_deployment: enable_template_deployment,
        soft_delete_retention_in_days: retention_days,
        purge_protection: enable_purge_protection,
        tags: tags_map,
        access_policies: None, // Don't modify access policies in update
    };

    let vault = vault_manager
        .update_vault(name, &resource_group, &update_request)
        .await?;

    println!("Successfully updated vault '{}'", vault.name);

    Ok(())
}

async fn execute_vault_share(
    vault_manager: &VaultManager,
    auth_provider: &std::sync::Arc<dyn crate::auth::provider::AzureAuthProvider>,
    command: VaultShareCommands,
    config: &Config,
) -> Result<()> {
    use crate::vault::models::AccessLevel;

    match command {
        VaultShareCommands::Grant {
            vault_name,
            user,
            resource_group,
            level,
        } => {
            let resource_group =
                resource_group.unwrap_or_else(|| config.default_resource_group.clone());

            let object_id = auth_provider.resolve_user_to_object_id(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            let access_level = match level.to_lowercase().as_str() {
                "reader" | "read" => AccessLevel::Reader,
                "contributor" | "write" => AccessLevel::Contributor,
                "admin" | "administrator" => AccessLevel::Admin,
                _ => {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "Invalid access level: {level}"
                    )))
                }
            };

            vault_manager
                .grant_vault_access(
                    &vault_name,
                    &resource_group,
                    &object_id,
                    access_level,
                    Some(&user),
                )
                .await?;
        }
        VaultShareCommands::Revoke {
            vault_name,
            user,
            resource_group,
        } => {
            let resource_group =
                resource_group.unwrap_or_else(|| config.default_resource_group.clone());

            let object_id = auth_provider.resolve_user_to_object_id(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            vault_manager
                .revoke_vault_access(&vault_name, &resource_group, &object_id, Some(&user))
                .await?;
        }
        VaultShareCommands::List {
            vault_name,
            resource_group,
            format,
        } => {
            let resource_group =
                resource_group.unwrap_or_else(|| config.default_resource_group.clone());

            let output_format = match format.to_lowercase().as_str() {
                "json" => OutputFormat::Json,
                _ => OutputFormat::Table,
            };

            vault_manager
                .list_vault_access(&vault_name, &resource_group, output_format)
                .await?;
        }
    }

    Ok(())
}

async fn execute_context_show(config: &Config) -> Result<()> {
    use crate::config::ContextManager;

    let context_manager = ContextManager::load().await.unwrap_or_default();

    if let Some(ref context) = context_manager.current {
        println!("Current Vault Context:");
        println!("  Vault: {}", context.vault_name);
        if let Some(ref rg) = context.resource_group {
            println!("  Resource Group: {rg}");
        }
        if let Some(ref sub) = context.subscription_id {
            println!("  Subscription: {sub}");
        }
        println!(
            "  Last Used: {}",
            context.last_used.format("%Y-%m-%d %H:%M:%S UTC")
        );
        println!("  Usage Count: {}", context.usage_count);

        // Show context source
        println!("  Scope: {}", context_manager.scope_description());
    } else {
        println!("No vault context set");
        if !config.default_vault.is_empty() {
            println!("Using config default: {}", config.default_vault);
        } else {
            println!("Hint: Use 'xv context use <vault-name>' to set a context");
        }
    }

    Ok(())
}

async fn execute_context_use(
    vault_name: &str,
    resource_group: Option<String>,
    global: bool,
    local: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::{ContextManager, VaultContext};

    let mut context_manager = if local {
        // Create local context
        ContextManager::new_local()?
    } else if global {
        // Use global context
        ContextManager::new_global()?
    } else {
        // Load existing or create new (defaults to global)
        ContextManager::load()
            .await
            .unwrap_or_else(|_| ContextManager::new_global().unwrap())
    };

    // Create new context
    let new_context = VaultContext::new(
        vault_name.to_string(),
        resource_group.or_else(|| {
            if !config.default_resource_group.is_empty() {
                Some(config.default_resource_group.clone())
            } else {
                None
            }
        }),
        if !config.subscription_id.is_empty() {
            Some(config.subscription_id.clone())
        } else {
            None
        },
    );

    // Update context manager
    context_manager.set_context(new_context).await?;

    let scope = if local { "local" } else { "global" };
    println!("✅ Switched to vault '{vault_name}' ({scope} context)");

    if let Some(ref rg) = context_manager.current_resource_group() {
        println!("   Resource Group: {rg}");
    }

    Ok(())
}

async fn execute_context_list(_config: &Config) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    let context_manager = ContextManager::load().await.unwrap_or_default();

    if context_manager.recent.is_empty() && context_manager.current.is_none() {
        println!("No vault contexts found");
        println!("Hint: Use 'xv context use <vault-name>' to create a context");
        return Ok(());
    }

    #[derive(Tabled)]
    struct ContextItem {
        #[tabled(rename = "Status")]
        status: String,
        #[tabled(rename = "Vault")]
        vault: String,
        #[tabled(rename = "Resource Group")]
        resource_group: String,
        #[tabled(rename = "Last Used")]
        last_used: String,
        #[tabled(rename = "Usage Count")]
        usage_count: String,
    }

    let mut items = Vec::new();

    // Add current context
    if let Some(ref context) = context_manager.current {
        items.push(ContextItem {
            status: "● Current".to_string(),
            vault: context.vault_name.clone(),
            resource_group: context.resource_group.as_deref().unwrap_or("-").to_string(),
            last_used: context.last_used.format("%Y-%m-%d %H:%M").to_string(),
            usage_count: context.usage_count.to_string(),
        });
    }

    // Add recent contexts
    for context in context_manager.list_recent() {
        // Skip if it's the current context
        if let Some(ref current) = context_manager.current {
            if current.vault_name == context.vault_name {
                continue;
            }
        }

        items.push(ContextItem {
            status: "  Recent".to_string(),
            vault: context.vault_name.clone(),
            resource_group: context.resource_group.as_deref().unwrap_or("-").to_string(),
            last_used: context.last_used.format("%Y-%m-%d %H:%M").to_string(),
            usage_count: context.usage_count.to_string(),
        });
    }

    if !items.is_empty() {
        let table = Table::new(&items);
        println!("{}", format_table(table, false));

        println!("\nScope: {}", context_manager.scope_description());
        if ContextManager::local_context_exists() {
            println!("Note: Local context file found in current directory (.xv/context)");
        }
    }

    Ok(())
}

async fn execute_context_clear(global: bool, _config: &Config) -> Result<()> {
    use crate::config::ContextManager;

    let mut context_manager = if global {
        ContextManager::new_global()?
    } else {
        ContextManager::load().await.unwrap_or_default()
    };

    if context_manager.current.is_none() {
        println!("No active context to clear");
        return Ok(());
    }

    let vault_name = context_manager.current_vault().unwrap().to_string();
    context_manager.clear_context().await?;

    let scope = if global {
        "global"
    } else {
        context_manager.scope_description()
    };
    println!("✅ Cleared vault context for '{vault_name}' ({scope} scope)");

    Ok(())
}

// File operation functions
#[cfg(feature = "file-ops")]
#[allow(clippy::too_many_arguments)]
async fn execute_file_upload(
    blob_manager: &BlobManager,
    file_path: &str,
    name: Option<String>,
    groups: Vec<String>,
    metadata: Vec<(String, String)>,
    tags: Vec<(String, String)>,
    content_type: Option<String>,
    progress: bool,
    _config: &Config,
) -> Result<()> {
    use crate::blob::models::FileUploadRequest;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    // Check if file exists
    if !Path::new(file_path).exists() {
        return Err(CrosstacheError::config(format!(
            "File not found: {file_path}"
        )));
    }

    // Read file content
    let content = fs::read(file_path)
        .map_err(|e| CrosstacheError::config(format!("Failed to read file {file_path}: {e}")))?;

    // Determine remote file name
    let remote_name = name.unwrap_or_else(|| {
        Path::new(file_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });

    // Convert metadata and tags to HashMap
    let metadata_map: HashMap<String, String> = metadata.into_iter().collect();
    let tags_map: HashMap<String, String> = tags.into_iter().collect();

    // Create upload request
    let upload_request = FileUploadRequest {
        name: remote_name.clone(),
        content,
        content_type,
        groups,
        metadata: metadata_map,
        tags: tags_map,
    };

    // Upload file
    println!("Uploading file '{file_path}' as '{remote_name}'...");

    if progress {
        // TODO: Use progress callback when implemented
        let file_info = blob_manager.upload_file(upload_request).await?;
        println!("✅ Successfully uploaded file '{}'", file_info.name);
        println!("   Size: {} bytes", file_info.size);
        println!("   Content-Type: {}", file_info.content_type);
        if !file_info.groups.is_empty() {
            println!("   Groups: {:?}", file_info.groups);
        }
    } else {
        let file_info = blob_manager.upload_file(upload_request).await?;
        println!("✅ Successfully uploaded file '{}'", file_info.name);
        println!("   Size: {} bytes", file_info.size);
        println!("   Content-Type: {}", file_info.content_type);
        if !file_info.groups.is_empty() {
            println!("   Groups: {:?}", file_info.groups);
        }
    }

    Ok(())
}

#[cfg(feature = "file-ops")]
async fn execute_file_download(
    blob_manager: &BlobManager,
    name: &str,
    output: Option<String>,
    stream: bool,
    force: bool,
    _config: &Config,
) -> Result<()> {
    use crate::blob::models::FileDownloadRequest;
    use std::fs;
    use std::path::Path;

    // Determine output path
    let output_path = output.unwrap_or_else(|| name.to_string());

    // Check if file exists and handle force flag
    if Path::new(&output_path).exists() && !force {
        return Err(CrosstacheError::config(format!(
            "File '{output_path}' already exists. Use --force to overwrite."
        )));
    }

    // Create download request
    let download_request = FileDownloadRequest {
        name: name.to_string(),
        output_path: Some(output_path.clone()),
        stream,
    };

    println!("Downloading file '{name}' to '{output_path}'...");

    if stream {
        // TODO: Use streaming download when implemented
        match blob_manager.download_file(download_request).await {
            Ok(content) => {
                fs::write(&output_path, content).map_err(|e| {
                    CrosstacheError::config(format!("Failed to write file {output_path}: {e}"))
                })?;
                println!("✅ Successfully downloaded file '{name}'");
            }
            Err(e) => {
                return Err(e);
            }
        }
    } else {
        match blob_manager.download_file(download_request).await {
            Ok(content) => {
                fs::write(&output_path, content).map_err(|e| {
                    CrosstacheError::config(format!("Failed to write file {output_path}: {e}"))
                })?;
                println!("✅ Successfully downloaded file '{name}'");
            }
            Err(e) => {
                return Err(e);
            }
        }
    }

    Ok(())
}

#[cfg(feature = "file-ops")]
async fn execute_file_list(
    blob_manager: &BlobManager,
    prefix: Option<String>,
    group: Option<String>,
    _include_metadata: bool,
    limit: Option<usize>,
    recursive: bool,
    config: &Config,
) -> Result<()> {
    use crate::blob::manager::format_size;
    use crate::blob::models::{BlobListItem, FileListRequest};
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    // Create list request
    let list_request = FileListRequest {
        prefix: prefix.clone(),
        groups: group.map(|g| vec![g]),
        limit,
        delimiter: if recursive {
            None
        } else {
            Some("/".to_string())
        },
        recursive,
    };

    // Get items based on recursive flag
    let items = if recursive {
        // Old behavior: flat list of all files
        let files = blob_manager.list_files(list_request).await?;
        files
            .into_iter()
            .map(BlobListItem::File)
            .collect::<Vec<_>>()
    } else {
        // New behavior: hierarchical listing
        blob_manager.list_files_hierarchical(list_request).await?
    };

    if items.is_empty() {
        println!("No files found");
        return Ok(());
    }

    if config.output_json {
        let json_output = serde_json::to_string_pretty(&items).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize items: {e}"))
        })?;
        println!("{json_output}");
    } else {
        #[derive(Tabled)]
        struct ListItem {
            #[tabled(rename = "Name")]
            name: String,
            #[tabled(rename = "Size")]
            size: String,
            #[tabled(rename = "Content-Type")]
            content_type: String,
            #[tabled(rename = "Modified")]
            modified: String,
            #[tabled(rename = "Groups")]
            groups: String,
        }

        let display_items: Vec<ListItem> = items
            .iter()
            .map(|item| match item {
                BlobListItem::Directory { name, .. } => ListItem {
                    name: name.clone(),
                    size: "<DIR>".to_string(),
                    content_type: "-".to_string(),
                    modified: "-".to_string(),
                    groups: "-".to_string(),
                },
                BlobListItem::File(file) => ListItem {
                    name: file.name.clone(),
                    size: format_size(file.size),
                    content_type: file.content_type.clone(),
                    modified: file.last_modified.format("%Y-%m-%d %H:%M:%S").to_string(),
                    groups: file.groups.join(", "),
                },
            })
            .collect();

        let table = Table::new(&display_items);
        println!("{}", format_table(table, config.no_color));

        // Count files and directories separately
        let file_count = items
            .iter()
            .filter(|i| matches!(i, BlobListItem::File(_)))
            .count();
        let dir_count = items
            .iter()
            .filter(|i| matches!(i, BlobListItem::Directory { .. }))
            .count();

        if recursive {
            println!("\nTotal files: {}", file_count);
        } else if dir_count > 0 {
            println!("\nTotal: {} directories, {} files", dir_count, file_count);
        } else {
            println!("\nTotal files: {}", file_count);
        }
    }

    Ok(())
}

#[cfg(feature = "file-ops")]
async fn execute_file_delete(
    blob_manager: &BlobManager,
    name: &str,
    force: bool,
    _config: &Config,
) -> Result<()> {
    // Confirmation unless forced
    if !force {
        let confirm = rpassword::prompt_password(format!(
            "Are you sure you want to delete file '{name}'? (y/N): "
        ))?;

        if confirm.to_lowercase() != "y" && confirm.to_lowercase() != "yes" {
            println!("Delete operation cancelled.");
            return Ok(());
        }
    }

    // Delete file
    println!("Deleting file '{name}'...");
    blob_manager.delete_file(name).await?;
    println!("✅ Successfully deleted file '{name}'");

    Ok(())
}

#[cfg(feature = "file-ops")]
async fn execute_file_info(blob_manager: &BlobManager, name: &str, config: &Config) -> Result<()> {
    // Get file info
    let file_info = blob_manager.get_file_info(name).await?;

    if config.output_json {
        let json_output = serde_json::to_string_pretty(&file_info).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize file info: {e}"))
        })?;
        println!("{json_output}");
    } else {
        println!("File Information:");
        println!("  Name: {}", file_info.name);
        println!("  Size: {} bytes", file_info.size);
        println!("  Content-Type: {}", file_info.content_type);
        println!(
            "  Last Modified: {}",
            file_info.last_modified.format("%Y-%m-%d %H:%M:%S UTC")
        );
        println!("  ETag: {}", file_info.etag);

        if !file_info.groups.is_empty() {
            println!("  Groups: {:?}", file_info.groups);
        }

        if !file_info.metadata.is_empty() {
            println!("  Metadata:");
            for (key, value) in &file_info.metadata {
                println!("    {key}: {value}");
            }
        }

        if !file_info.tags.is_empty() {
            println!("  Tags:");
            for (key, value) in &file_info.tags {
                println!("    {key}: {value}");
            }
        }
    }

    Ok(())
}

/// Information about a file to upload with path tracking
#[cfg(feature = "file-ops")]
#[derive(Debug, Clone)]
struct FileUploadInfo {
    /// Full local file path
    local_path: PathBuf,
    /// Relative path from base directory (for blob name calculation)
    _relative_path: String,
    /// Final blob name (includes prefix and converted path separators)
    blob_name: String,
}

/// Convert a path to blob name format (forward slashes, no leading slash)
#[cfg(feature = "file-ops")]
fn path_to_blob_name(path: &Path, prefix: Option<&str>) -> String {
    // Convert path components to forward-slash separated string
    let components: Vec<String> = path
        .components()
        .filter_map(|c| {
            match c {
                std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
                _ => None, // Skip prefix, root, current dir, parent dir components
            }
        })
        .collect();

    let relative_path = components.join("/");

    // Add prefix if provided
    if let Some(p) = prefix {
        let p = p.trim_matches('/');
        if p.is_empty() {
            relative_path
        } else {
            format!("{}/{}", p, relative_path)
        }
    } else {
        relative_path
    }
}

/// Recursively collect all files from a directory
#[cfg(feature = "file-ops")]
fn _collect_files_recursive(path: &Path) -> Result<Vec<PathBuf>> {
    use std::fs;

    let mut files = Vec::new();

    if path.is_file() {
        files.push(path.to_path_buf());
    } else if path.is_dir() {
        let entries = fs::read_dir(path).map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to read directory {}: {}",
                path.display(),
                e
            ))
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                CrosstacheError::config(format!("Failed to read directory entry: {e}"))
            })?;

            let entry_path = entry.path();
            if entry_path.is_file() {
                files.push(entry_path);
            } else if entry_path.is_dir() {
                // Recursively collect files from subdirectory
                files.extend(_collect_files_recursive(&entry_path)?);
            }
        }
    } else {
        return Err(CrosstacheError::config(format!(
            "Path {} is neither a file nor a directory",
            path.display()
        )));
    }

    Ok(files)
}

/// Recursively collect files with path structure information
///
/// # Arguments
/// * `path` - The path to traverse (file or directory)
/// * `base_path` - The base directory to calculate relative paths from
/// * `prefix` - Optional prefix to add to blob names
/// * `flatten` - If true, use only filename (no directory structure)
///
/// # Returns
/// Vector of FileUploadInfo with path mappings for blob storage
#[cfg(feature = "file-ops")]
fn collect_files_with_structure(
    path: &Path,
    base_path: &Path,
    prefix: Option<&str>,
    flatten: bool,
) -> Result<Vec<FileUploadInfo>> {
    use std::fs;

    let mut files = Vec::new();

    // Skip symlinks to avoid loops
    if path.is_symlink() {
        return Ok(files);
    }

    if path.is_file() {
        // Calculate relative path from base
        let relative = path.strip_prefix(base_path).unwrap_or(path);

        let blob_name = if flatten {
            // Use only filename
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        } else {
            // Preserve structure with forward slashes
            path_to_blob_name(relative, prefix)
        };

        files.push(FileUploadInfo {
            local_path: path.to_path_buf(),
            _relative_path: relative.to_string_lossy().to_string(),
            blob_name,
        });
    } else if path.is_dir() {
        let entries = fs::read_dir(path).map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to read directory {}: {}",
                path.display(),
                e
            ))
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                CrosstacheError::config(format!("Failed to read directory entry: {e}"))
            })?;

            let entry_path = entry.path();

            // Skip hidden files and directories by default
            if let Some(name) = entry_path.file_name() {
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.') {
                    continue; // Skip hidden files
                }
            }

            // Recursively collect files
            files.extend(collect_files_with_structure(
                &entry_path,
                base_path,
                prefix,
                flatten,
            )?);
        }
    } else {
        return Err(CrosstacheError::config(format!(
            "Path {} is neither a file nor a directory",
            path.display()
        )));
    }

    Ok(files)
}

#[cfg(feature = "file-ops")]
#[allow(clippy::too_many_arguments)]
async fn execute_file_upload_recursive(
    blob_manager: &BlobManager,
    paths: Vec<String>,
    group: Vec<String>,
    metadata: Vec<(String, String)>,
    tag: Vec<(String, String)>,
    progress: bool,
    continue_on_error: bool,
    flatten: bool,
    prefix: Option<String>,
    config: &Config,
) -> Result<()> {
    use std::path::Path;

    let mut all_files = Vec::new();

    // Collect all files recursively from all provided paths
    for path_str in &paths {
        let path = Path::new(path_str);
        if !path.exists() {
            if continue_on_error {
                eprintln!("❌ Path not found: {path_str}");
                continue;
            } else {
                return Err(CrosstacheError::config(format!(
                    "Path not found: {path_str}"
                )));
            }
        }
        // Use the parent directory as base path so the top-level folder name
        // is preserved in blob paths (e.g., docs/api/users.md, not api/users.md)
        let base_path = path.parent().unwrap_or(path);

        let files = collect_files_with_structure(path, base_path, prefix.as_deref(), flatten)?;
        all_files.extend(files);
    }

    if all_files.is_empty() {
        println!("No files found to upload");
        return Ok(());
    }

    println!("Found {} file(s) to upload", all_files.len());

    let mut success_count = 0;
    let mut failure_count = 0;

    // Validate blob name lengths
    for file_info in &all_files {
        if file_info.blob_name.len() > 1024 {
            let error_msg = format!(
                "Blob name too long ({} chars, max 1024): {}",
                file_info.blob_name.len(),
                file_info.blob_name
            );
            if continue_on_error {
                eprintln!("❌ {}", error_msg);
                failure_count += 1;
                continue;
            } else {
                return Err(CrosstacheError::invalid_argument(error_msg));
            }
        }
    }

    for file_info in &all_files {
        let local_path_str = file_info.local_path.to_string_lossy();

        if !flatten {
            println!("Uploading: {} → {}", local_path_str, file_info.blob_name);
        } else {
            println!("Uploading: {}", local_path_str);
        }
        let result = execute_file_upload(
            blob_manager,
            &local_path_str,
            Some(file_info.blob_name.clone()), // Use the calculated blob name
            group.clone(),
            metadata.clone(),
            tag.clone(),
            None, // No content type override for batch uploads
            progress,
            config,
        )
        .await;

        match result {
            Ok(_) => {
                success_count += 1;
            }
            Err(e) => {
                eprintln!("❌ Failed to upload '{}': {}", local_path_str, e);
                failure_count += 1;
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }

    // Print summary
    println!("\n📊 Upload Summary:");
    println!("  ✅ Successful: {success_count}");
    if failure_count > 0 {
        println!("  ❌ Failed: {failure_count}");
    }

    if failure_count > 0 && continue_on_error {
        return Err(CrosstacheError::azure_api(format!(
            "{failure_count} file(s) failed to upload"
        )));
    }

    Ok(())
}

#[cfg(feature = "file-ops")]
#[allow(clippy::too_many_arguments)]
async fn execute_file_upload_multiple(
    blob_manager: &BlobManager,
    files: Vec<String>,
    group: Vec<String>,
    metadata: Vec<(String, String)>,
    tag: Vec<(String, String)>,
    progress: bool,
    continue_on_error: bool,
    config: &Config,
) -> Result<()> {
    println!("Uploading {} file(s)...", files.len());

    let mut success_count = 0;
    let mut error_count = 0;

    for file_path in files {
        match execute_file_upload(
            blob_manager,
            &file_path,
            None, // name is not allowed for multiple files
            group.clone(),
            metadata.clone(),
            tag.clone(),
            None, // content_type is not allowed for multiple files
            progress,
            config,
        )
        .await
        {
            Ok(_) => {
                println!("  ✅ {file_path}");
                success_count += 1;
            }
            Err(e) => {
                eprintln!("  ❌ {file_path}: {e}");
                error_count += 1;
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }

    println!("\nUpload completed: {success_count} succeeded, {error_count} failed");

    if error_count > 0 && !continue_on_error {
        return Err(CrosstacheError::azure_api(format!(
            "{error_count} file(s) failed to upload"
        )));
    }

    Ok(())
}

#[cfg(feature = "file-ops")]
async fn execute_file_download_multiple(
    blob_manager: &BlobManager,
    files: Vec<String>,
    output: Option<String>,
    stream: bool,
    force: bool,
    continue_on_error: bool,
    config: &Config,
) -> Result<()> {
    println!("Downloading {} file(s)...", files.len());

    let mut success_count = 0;
    let mut error_count = 0;

    for file_name in files {
        match execute_file_download(
            blob_manager,
            &file_name,
            output.clone(),
            stream,
            force,
            config,
        )
        .await
        {
            Ok(_) => {
                println!("  ✅ {file_name}");
                success_count += 1;
            }
            Err(e) => {
                eprintln!("  ❌ {file_name}: {e}");
                error_count += 1;
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }

    println!("\nDownload completed: {success_count} succeeded, {error_count} failed");

    if error_count > 0 && !continue_on_error {
        return Err(CrosstacheError::azure_api(format!(
            "{error_count} file(s) failed to download"
        )));
    }

    Ok(())
}

#[cfg(feature = "file-ops")]
async fn execute_file_download_recursive(
    blob_manager: &BlobManager,
    prefixes: Vec<String>,
    output: Option<String>,
    force: bool,
    flatten: bool,
    continue_on_error: bool,
    config: &Config,
) -> Result<()> {
    use crate::blob::models::FileListRequest;
    use std::fs;
    use std::path::Path;

    // Determine output directory (default to current directory)
    let output_dir = output.unwrap_or_else(|| ".".to_string());
    let output_path = Path::new(&output_dir);

    // Create output directory if it doesn't exist
    if !output_path.exists() {
        fs::create_dir_all(output_path).map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to create output directory {}: {}",
                output_dir, e
            ))
        })?;
    }

    let mut all_files_to_download = Vec::new();

    // List all blobs matching each prefix
    for prefix in &prefixes {
        let list_request = FileListRequest {
            prefix: Some(prefix.clone()),
            groups: None,
            limit: None,
            delimiter: None,
            recursive: true,
        };

        let files = blob_manager.list_files(list_request).await?;

        if files.is_empty() {
            eprintln!("⚠️  No files found matching prefix: {}", prefix);
            continue;
        }

        all_files_to_download.extend(files);
    }

    if all_files_to_download.is_empty() {
        println!("No files found to download");
        return Ok(());
    }

    println!("Found {} file(s) to download", all_files_to_download.len());

    let mut success_count = 0;
    let mut failure_count = 0;

    for file_info in &all_files_to_download {
        let blob_name = &file_info.name;

        // Determine local file path
        let local_path = if flatten {
            // Flatten: use only filename
            let filename = Path::new(blob_name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            output_path.join(filename.as_ref())
        } else {
            // Preserve structure: use full blob path
            output_path.join(blob_name)
        };

        // Security: prevent path traversal via malicious blob names (e.g. "../../etc/passwd")
        {
            let canonical_output = output_path
                .canonicalize()
                .unwrap_or_else(|_| output_path.to_path_buf());
            // Resolve what we can — parent dirs may not exist yet, so normalize components
            let mut resolved = canonical_output.clone();
            for component in local_path
                .strip_prefix(output_path)
                .unwrap_or(&local_path)
                .components()
            {
                match component {
                    std::path::Component::ParentDir => {
                        resolved.pop();
                    }
                    std::path::Component::Normal(c) => {
                        resolved.push(c);
                    }
                    _ => {}
                }
            }
            if !resolved.starts_with(&canonical_output) {
                eprintln!(
                    "⚠️  Skipping '{}': path traversal detected in blob name",
                    blob_name
                );
                failure_count += 1;
                if continue_on_error {
                    continue;
                } else {
                    return Err(CrosstacheError::config(format!(
                        "Path traversal detected in blob name: {blob_name}"
                    )));
                }
            }
        }

        let local_path_str = local_path.to_string_lossy().to_string();

        // Create parent directories if needed (for structure preservation)
        if !flatten {
            if let Some(parent) = local_path.parent() {
                if !parent.exists() {
                    fs::create_dir_all(parent).map_err(|e| {
                        CrosstacheError::config(format!(
                            "Failed to create directory {}: {}",
                            parent.display(),
                            e
                        ))
                    })?;
                }
            }
        }

        // Check if file exists and handle force flag
        if local_path.exists() && !force {
            eprintln!(
                "⚠️  File already exists: {} (use --force to overwrite)",
                local_path_str
            );
            failure_count += 1;
            if !continue_on_error {
                return Err(CrosstacheError::config(format!(
                    "File already exists: {}",
                    local_path_str
                )));
            }
            continue;
        }

        if !flatten {
            println!("Downloading: {} → {}", blob_name, local_path_str);
        } else {
            println!("Downloading: {}", blob_name);
        }

        // Download the file
        let result = execute_file_download(
            blob_manager,
            blob_name,
            Some(local_path_str.clone()),
            false, // stream
            force,
            config,
        )
        .await;

        match result {
            Ok(_) => {
                success_count += 1;
            }
            Err(e) => {
                eprintln!("❌ Failed to download '{}': {}", blob_name, e);
                failure_count += 1;
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }

    // Print summary
    println!("\n📊 Download Summary:");
    println!("  ✅ Successful: {}", success_count);
    if failure_count > 0 {
        println!("  ❌ Failed: {}", failure_count);
    }

    if failure_count > 0 && continue_on_error {
        return Err(CrosstacheError::azure_api(format!(
            "{} file(s) failed to download",
            failure_count
        )));
    }

    Ok(())
}

#[cfg(feature = "file-ops")]
async fn execute_file_delete_multiple(
    blob_manager: &BlobManager,
    files: Vec<String>,
    force: bool,
    continue_on_error: bool,
    config: &Config,
) -> Result<()> {
    // Confirmation prompt for multiple files without --force
    if !force && files.len() > 1 {
        println!("You are about to delete {} files:", files.len());
        for (i, file) in files.iter().enumerate() {
            if i < 5 {
                println!("  - {file}");
            } else if i == 5 {
                println!("  ... and {} more", files.len() - 5);
                break;
            }
        }

        // Use rpassword for confirmation like other commands do
        let confirm =
            rpassword::prompt_password("Are you sure you want to delete these files? (y/N): ")?;

        if confirm.to_lowercase() != "y" && confirm.to_lowercase() != "yes" {
            println!("Delete operation cancelled");
            return Ok(());
        }
    }

    println!("Deleting {} file(s)...", files.len());

    let mut success_count = 0;
    let mut error_count = 0;

    for file_name in files {
        match execute_file_delete(blob_manager, &file_name, force, config).await {
            Ok(_) => {
                println!("  ✅ {file_name}");
                success_count += 1;
            }
            Err(e) => {
                eprintln!("  ❌ {file_name}: {e}");
                error_count += 1;
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }

    println!("\nDelete completed: {success_count} succeeded, {error_count} failed");

    if error_count > 0 && !continue_on_error {
        return Err(CrosstacheError::azure_api(format!(
            "{error_count} file(s) failed to delete"
        )));
    }

    Ok(())
}

#[cfg(feature = "file-ops")]
async fn execute_file_sync(
    blob_manager: &BlobManager,
    local_path: &str,
    prefix: Option<String>,
    direction: &SyncDirection,
    dry_run: bool,
    delete: bool,
    config: &Config,
) -> Result<()> {
    let _ = (
        blob_manager,
        local_path,
        prefix,
        direction,
        dry_run,
        delete,
        config,
    );
    eprintln!("File sync is not yet implemented.");

    Ok(())
}

/// Execute bulk secret set operation
async fn execute_secret_set_bulk(
    secret_manager: &crate::secret::manager::SecretManager,
    args: Vec<String>,
    note: Option<String>,
    folder: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use std::fs;
    use std::path::Path;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(None).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Parse KEY=value pairs
    let mut secrets_to_set = Vec::new();

    for arg in args {
        if let Some(pos) = arg.find('=') {
            let key = arg[..pos].trim();
            let value_part = arg[pos + 1..].trim();

            if key.is_empty() {
                return Err(CrosstacheError::invalid_argument(format!(
                    "Invalid KEY=value pair: empty key in '{}'",
                    arg
                )));
            }

            // Handle @file syntax for value
            let value = if value_part.starts_with('@') {
                let file_path = value_part.strip_prefix('@').unwrap();

                if !Path::new(file_path).exists() {
                    return Err(CrosstacheError::config(format!(
                        "File not found: {}",
                        file_path
                    )));
                }

                fs::read_to_string(file_path).map_err(|e| {
                    CrosstacheError::config(format!("Failed to read file '{}': {}", file_path, e))
                })?
            } else {
                value_part.to_string()
            };

            if value.is_empty() {
                return Err(CrosstacheError::config(format!(
                    "Secret value cannot be empty for key '{}'",
                    key
                )));
            }

            secrets_to_set.push((key.to_string(), value));
        } else {
            return Err(CrosstacheError::invalid_argument(format!(
                "Invalid format: '{}'. Expected KEY=value or KEY=@/path/to/file",
                arg
            )));
        }
    }

    if secrets_to_set.is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "No valid KEY=value pairs provided",
        ));
    }

    println!(
        "🔐 Setting {} secret(s) in vault '{}'...",
        secrets_to_set.len(),
        vault_name
    );

    let mut success_count = 0;
    let mut error_count = 0;

    for (key, value) in secrets_to_set {
        // Create secret request with note and/or folder if provided
        let secret_request = if note.is_some() || folder.is_some() {
            Some(crate::secret::manager::SecretRequest {
                name: key.clone(),
                value: Zeroizing::new(value.clone()),
                content_type: None,
                enabled: Some(true),
                expires_on: None,
                not_before: None,
                tags: None,
                groups: None,
                note: note.clone(),
                folder: folder.clone(),
            })
        } else {
            None
        };

        match secret_manager
            .set_secret_safe(&vault_name, &key, &value, secret_request)
            .await
        {
            Ok(secret) => {
                println!(
                    "  ✅ {}: {} (version {})",
                    key, secret.original_name, secret.version
                );
                success_count += 1;
            }
            Err(e) => {
                eprintln!("  ❌ {}: {}", key, e);
                error_count += 1;
            }
        }
    }

    println!("\n📊 Bulk Set Summary:");
    println!("  ✅ Successful: {}", success_count);
    if error_count > 0 {
        println!("  ❌ Failed: {}", error_count);
    }

    if error_count > 0 {
        Err(CrosstacheError::config(format!(
            "{} secret(s) failed to set",
            error_count
        )))
    } else {
        Ok(())
    }
}

/// Execute group delete operation
async fn execute_secret_delete_group(
    secret_manager: &crate::secret::manager::SecretManager,
    group_name: &str,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(None).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get all secrets from the vault
    let secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, Some(group_name))
        .await?;

    if secrets.is_empty() {
        println!("No secrets found in group '{}'", group_name);
        return Ok(());
    }

    println!(
        "Found {} secret(s) in group '{}' to delete:",
        secrets.len(),
        group_name
    );

    for secret in &secrets {
        println!("  - {}", secret.name);
    }

    // Confirmation unless forced
    if !force {
        let confirm = rpassword::prompt_password(format!(
            "Are you sure you want to delete ALL {} secret(s) in group '{}'? (y/N): ",
            secrets.len(),
            group_name
        ))?;

        if confirm.to_lowercase() != "y" && confirm.to_lowercase() != "yes" {
            println!("Group delete operation cancelled.");
            return Ok(());
        }
    }

    println!(
        "🗑️  Deleting {} secret(s) from group '{}'...",
        secrets.len(),
        group_name
    );

    let mut success_count = 0;
    let mut error_count = 0;

    for secret in secrets {
        match secret_manager
            .delete_secret_safe(&vault_name, &secret.name, true) // force=true to avoid individual prompts
            .await
        {
            Ok(_) => {
                println!("  ✅ Deleted: {}", secret.name);
                success_count += 1;
            }
            Err(e) => {
                eprintln!("  ❌ Failed to delete '{}': {}", secret.name, e);
                error_count += 1;
            }
        }
    }

    println!("\n📊 Group Delete Summary:");
    println!("  ✅ Successful: {}", success_count);
    if error_count > 0 {
        println!("  ❌ Failed: {}", error_count);
    }

    if error_count > 0 {
        Err(CrosstacheError::config(format!(
            "{} secret(s) failed to delete from group '{}'",
            error_count, group_name
        )))
    } else {
        println!(
            "✅ Successfully deleted all secrets from group '{}'",
            group_name
        );
        Ok(())
    }
}

/// Parse a single key-value pair
fn parse_key_val<T, U>(
    s: &str,
) -> std::result::Result<(T, U), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: std::error::Error + Send + Sync + 'static,
{
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

/// Quick file upload command (alias for file upload)
#[cfg(feature = "file-ops")]
async fn execute_file_upload_quick(
    file_path: &str,
    name: Option<String>,
    groups: Option<String>,
    metadata: Vec<String>,
    config: &Config,
) -> Result<()> {
    // Create blob manager
    let blob_manager = create_blob_manager(config).map_err(|e| {
        if e.to_string().contains("No storage account configured") {
            CrosstacheError::config(
                "No blob storage configured. Run 'xv init' to set up blob storage.",
            )
        } else {
            e
        }
    })?;

    // Convert parameters to match FileCommands::Upload format
    let groups_vec = groups
        .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let metadata_map = metadata
        .into_iter()
        .filter_map(|m| {
            let parts: Vec<&str> = m.splitn(2, '=').collect();
            if parts.len() == 2 {
                Some((parts[0].trim().to_string(), parts[1].trim().to_string()))
            } else {
                None
            }
        })
        .collect();

    execute_file_upload(
        &blob_manager,
        file_path,
        name,
        groups_vec,
        metadata_map,
        Vec::new(),
        None,
        true,
        config,
    )
    .await
}

/// Quick file download command (alias for file download)
#[cfg(feature = "file-ops")]
async fn execute_file_download_quick(
    name: &str,
    output: Option<String>,
    open: bool,
    config: &Config,
) -> Result<()> {
    // Create blob manager
    let blob_manager = create_blob_manager(config).map_err(|e| {
        if e.to_string().contains("No storage account configured") {
            CrosstacheError::config(
                "No blob storage configured. Run 'xv init' to set up blob storage.",
            )
        } else {
            e
        }
    })?;

    let output_path = output.clone();
    execute_file_download(
        &blob_manager,
        name,
        output,
        false, // stream
        false, // force
        config,
    )
    .await?;

    // Handle --open flag
    if open {
        let final_output_path = output_path.unwrap_or_else(|| name.to_string());
        if let Ok(path) = std::fs::canonicalize(&final_output_path) {
            println!("Opening file: {}", path.display());
            // Note: opener crate would need to be added to dependencies for this to work
            // For now, just print the path
        }
    }

    Ok(())
}
