//! CLI commands and argument parsing
//!
//! This module defines the command-line interface structure using clap,
//! including all commands, subcommands, and their arguments.

#[cfg(feature = "file-ops")]
use crate::cli::file::FileCommands;
use crate::cli::helpers::parse_key_val;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

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

    /// Output format (default: auto = table on TTY, json for pipes/redirects)
    #[arg(long, global = true, value_enum, default_value = "auto", hide = should_hide_options())]
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
#[derive(Debug, Clone, Copy, PartialEq, Default, ValueEnum)]
pub enum CharsetType {
    /// Alphanumeric characters (A-Z, a-z, 0-9)
    #[default]
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

impl std::fmt::Display for CharsetType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Alphanumeric => write!(f, "alphanumeric"),
            Self::AlphanumericSymbols => write!(f, "alphanumeric-symbols"),
            Self::Hex => write!(f, "hex"),
            Self::Base64 => write!(f, "base64"),
            Self::Numeric => write!(f, "numeric"),
            Self::Uppercase => write!(f, "uppercase"),
            Self::Lowercase => write!(f, "lowercase"),
        }
    }
}

impl std::str::FromStr for CharsetType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "alphanumeric" => Ok(Self::Alphanumeric),
            "alphanumeric-symbols" | "alphanumeric_symbols" => Ok(Self::AlphanumericSymbols),
            "hex" => Ok(Self::Hex),
            "base64" => Ok(Self::Base64),
            "numeric" => Ok(Self::Numeric),
            "uppercase" => Ok(Self::Uppercase),
            "lowercase" => Ok(Self::Lowercase),
            _ => Err(format!(
                "Invalid charset: '{s}'. Valid options: alphanumeric, alphanumeric-symbols, hex, base64, numeric, uppercase, lowercase"
            )),
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
    /// Interactively find and copy a secret by name pattern (alias: search)
    #[command(alias = "search")]
    Find {
        /// Search term — substring match, or prefix with trailing * (e.g. claude-*)
        /// Omit to browse all secrets interactively.
        term: Option<String>,
        /// Print value to stdout instead of copying to clipboard
        #[arg(short, long)]
        raw: bool,
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
        /// Bypass the local cache and fetch fresh data
        #[arg(long)]
        no_cache: bool,
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
    /// Generate a random password and copy it to the clipboard
    Gen {
        /// Password length — must be between 6 and 100 (default: 15)
        #[arg(short, long, default_value = "15")]
        length: usize,
        /// Character set to use (default: alphanumeric, or gen_default_charset config)
        #[arg(short, long, value_enum)]
        charset: Option<CharsetType>,
        /// Save the generated password as a secret in the vault
        #[arg(long)]
        save: Option<String>,
        /// Target vault for --save (overrides context/config default)
        #[arg(long)]
        vault: Option<String>,
        /// Print to stdout instead of copying to clipboard
        #[arg(long)]
        raw: bool,
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
        #[arg(
            short = 'f',
            long = "fmt",
            default_value = "table",
            id = "parse_format"
        )]
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
        /// Azure resource group (defaults to config value)
        #[arg(long)]
        resource_group: Option<String>,
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
    /// Check for and install new versions
    Upgrade {
        /// Only check if an update is available (exit code 0 = up-to-date, 1 = update available)
        #[arg(long)]
        check: bool,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
    /// Cache management commands
    Cache {
        #[command(subcommand)]
        command: CacheCommands,
    },
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
        /// Print full file path after download
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
        /// Output format (default: auto = table on TTY, json for pipes/redirects)
        #[arg(long, value_enum, default_value = "auto")]
        format: OutputFormat,
        /// Bypass the local cache and fetch fresh data
        #[arg(long)]
        no_cache: bool,
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
        #[arg(
            short = 'f',
            long = "fmt",
            default_value = "json",
            id = "export_format"
        )]
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
        #[arg(
            short = 'f',
            long = "fmt",
            default_value = "json",
            id = "import_format"
        )]
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
        #[arg(
            short = 'f',
            long = "fmt",
            default_value = "auto",
            id = "share_list_format"
        )]
        format: String,
        /// Include service accounts in output
        #[arg(long)]
        all: bool,
    },
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
        /// Include service accounts in output
        #[arg(long)]
        all: bool,
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
pub enum CacheCommands {
    /// Remove cached data
    Clear {
        #[arg(long)]
        vault: Option<String>,
    },
    /// Show cache status and statistics
    Status,
    /// Internal: refresh a cache entry in the background
    #[command(hide = true)]
    Refresh {
        #[arg(long)]
        key: String,
    },
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
        /// Output format (currently only 'dotenv' is supported)
        #[arg(long = "fmt", default_value = "dotenv", id = "pull_format")]
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
        let resolved = self.format.resolve_for_stdout();
        config.runtime_output_format = resolved;
        config.output_json = matches!(resolved, OutputFormat::Json);

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
                crate::cli::secret_ops::execute_secret_set_direct(args, stdin, note, folder, expires, not_before, config)
                    .await
            }
            Commands::Get { name, raw, version } => {
                crate::cli::secret_ops::execute_secret_get_direct(&name, raw, version, config).await
            }
            Commands::Find { term, raw } => crate::cli::secret_ops::execute_secret_find_direct(term, raw, config).await,
            Commands::List {
                group,
                all,
                expiring,
                expired,
                no_cache,
            } => crate::cli::secret_ops::execute_secret_list_direct(group, all, expiring, expired, no_cache, config).await,
            Commands::Delete { name, group, force } => {
                crate::cli::secret_ops::execute_secret_delete_direct(name, group, force, config).await
            }
            Commands::History { name } => crate::cli::secret_ops::execute_secret_history_direct(&name, config).await,
            Commands::Rollback {
                name,
                version,
                force,
            } => crate::cli::secret_ops::execute_secret_rollback_direct(&name, &version, force, config).await,
            Commands::Rotate {
                name,
                length,
                charset,
                generator,
                show_value,
                force,
            } => {
                crate::cli::secret_ops::execute_secret_rotate_direct(
                    &name, length, charset, generator, show_value, force, config,
                )
                .await
            }
            Commands::Gen {
                length,
                charset,
                save,
                vault,
                raw,
            } => crate::cli::system_ops::execute_gen_command(length, charset, save, vault, raw, config).await,
            Commands::Run {
                group,
                no_masking,
                command,
            } => crate::cli::secret_ops::execute_secret_run_direct(group, no_masking, command, config).await,
            Commands::Inject {
                template,
                out,
                group,
            } => crate::cli::secret_ops::execute_secret_inject_direct(template, out, group, config).await,
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
                crate::cli::secret_ops::execute_secret_update_direct(
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
            } => crate::cli::secret_ops::execute_diff_command(&vault1, &vault2, show_values, group, config).await,
            Commands::Copy {
                name,
                from,
                to,
                new_name,
            } => crate::cli::secret_ops::execute_secret_copy_direct(&name, &from, &to, new_name, config).await,
            Commands::Move {
                name,
                from,
                to,
                new_name,
                force,
            } => crate::cli::secret_ops::execute_secret_move_direct(&name, &from, &to, new_name, force, config).await,
            Commands::Purge { name, force } => {
                crate::cli::secret_ops::execute_secret_purge_direct(&name, force, config).await
            }
            Commands::Restore { name } => crate::cli::secret_ops::execute_secret_restore_direct(&name, config).await,
            Commands::Parse {
                connection_string,
                format,
            } => crate::cli::secret_ops::execute_secret_parse_direct(&connection_string, &format, config).await,
            Commands::Share { command } => crate::cli::secret_ops::execute_secret_share_direct(command, config).await,
            Commands::Vault { command } => {
                crate::cli::vault_ops::execute_vault_command(command, config).await
            }
            #[cfg(feature = "file-ops")]
            Commands::File { command } => {
                crate::cli::file_ops::execute_file_command(command, config).await
            }
            Commands::Config { command } => {
                crate::cli::config_ops::execute_config_command(command, config).await
            }
            Commands::Context { command } => {
                crate::cli::config_ops::execute_context_command(command, config).await
            }
            Commands::Env { command } => {
                crate::cli::config_ops::execute_env_command(command, config).await
            }
            Commands::Audit {
                name,
                vault,
                days,
                operation,
                resource_group,
                raw,
            } => {
                crate::cli::system_ops::execute_audit_command(name, vault, days, operation, resource_group, raw, config)
                    .await
            }
            Commands::Init => crate::cli::system_ops::execute_init_command(config).await,
            Commands::Info {
                resource,
                resource_type,
                resource_group,
                subscription,
            } => {
                crate::cli::system_ops::execute_info_command(
                    resource,
                    resource_type,
                    resource_group,
                    subscription,
                    config,
                )
                .await
            }
            Commands::Version => crate::cli::system_ops::execute_version_command().await,
            Commands::Completion { shell } => crate::cli::system_ops::execute_completion_command(shell).await,
            Commands::Whoami => crate::cli::system_ops::execute_whoami_command(config).await,
            // Upgrade does not need Azure config — only talks to GitHub API
            Commands::Upgrade { check, force } => {
                crate::cli::upgrade_ops::execute_upgrade_command(check, force).await
            }
            Commands::Cache { command } => {
                crate::cli::config_ops::execute_cache_command(command, config).await
            }
            #[cfg(feature = "file-ops")]
            Commands::Upload {
                file_path,
                name,
                groups,
                metadata,
            } => {
                crate::cli::file_ops::execute_file_upload_quick(
                    &file_path, name, groups, metadata, &config,
                )
                .await
            }
            #[cfg(feature = "file-ops")]
            Commands::Download { name, output, open } => {
                crate::cli::file_ops::execute_file_download_quick(&name, output, open, &config)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::helpers::generate_random_value;

    #[test]
    fn test_charset_default_is_alphanumeric() {
        assert_eq!(CharsetType::default(), CharsetType::Alphanumeric);
    }

    #[test]
    fn test_charset_display() {
        assert_eq!(CharsetType::Alphanumeric.to_string(), "alphanumeric");
        assert_eq!(
            CharsetType::AlphanumericSymbols.to_string(),
            "alphanumeric-symbols"
        );
        assert_eq!(CharsetType::Hex.to_string(), "hex");
        assert_eq!(CharsetType::Base64.to_string(), "base64");
        assert_eq!(CharsetType::Numeric.to_string(), "numeric");
        assert_eq!(CharsetType::Uppercase.to_string(), "uppercase");
        assert_eq!(CharsetType::Lowercase.to_string(), "lowercase");
    }

    #[test]
    fn test_charset_from_str_valid() {
        assert_eq!(
            "alphanumeric".parse::<CharsetType>().unwrap(),
            CharsetType::Alphanumeric
        );
        assert_eq!(
            "alphanumeric-symbols".parse::<CharsetType>().unwrap(),
            CharsetType::AlphanumericSymbols
        );
        assert_eq!(
            "alphanumeric_symbols".parse::<CharsetType>().unwrap(),
            CharsetType::AlphanumericSymbols
        );
        assert_eq!("hex".parse::<CharsetType>().unwrap(), CharsetType::Hex);
        assert_eq!(
            "base64".parse::<CharsetType>().unwrap(),
            CharsetType::Base64
        );
        assert_eq!(
            "numeric".parse::<CharsetType>().unwrap(),
            CharsetType::Numeric
        );
        assert_eq!(
            "uppercase".parse::<CharsetType>().unwrap(),
            CharsetType::Uppercase
        );
        assert_eq!(
            "lowercase".parse::<CharsetType>().unwrap(),
            CharsetType::Lowercase
        );
        assert_eq!(
            "ALPHANUMERIC".parse::<CharsetType>().unwrap(),
            CharsetType::Alphanumeric
        );
    }

    #[test]
    fn test_charset_from_str_invalid() {
        assert!("alpha".parse::<CharsetType>().is_err());
        assert!("unknown".parse::<CharsetType>().is_err());
        assert!("".parse::<CharsetType>().is_err());
    }

    // ── gen command unit tests ───────────────────────────────────────────────

    #[test]
    fn test_gen_length_validation_lower_bound() {
        let result = generate_random_value(6, CharsetType::Alphanumeric, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 6);
    }

    #[test]
    fn test_gen_length_validation_upper_bound() {
        let result = generate_random_value(100, CharsetType::Alphanumeric, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 100);
    }

    #[test]
    fn test_gen_default_length_is_15() {
        let result = generate_random_value(15, CharsetType::Alphanumeric, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 15);
    }

    #[test]
    fn test_gen_alphanumeric_chars_only() {
        let value = generate_random_value(200, CharsetType::Alphanumeric, None).unwrap();
        let valid = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        for ch in value.chars() {
            assert!(
                valid.contains(ch),
                "Unexpected char '{ch}' in alphanumeric output"
            );
        }
    }

    #[test]
    fn test_gen_numeric_chars_only() {
        let value = generate_random_value(200, CharsetType::Numeric, None).unwrap();
        for ch in value.chars() {
            assert!(
                ch.is_ascii_digit(),
                "Unexpected char '{ch}' in numeric output"
            );
        }
    }

    #[test]
    fn test_gen_uppercase_chars_only() {
        let value = generate_random_value(200, CharsetType::Uppercase, None).unwrap();
        for ch in value.chars() {
            assert!(
                ch.is_ascii_uppercase(),
                "Unexpected char '{ch}' in uppercase output"
            );
        }
    }

    #[test]
    fn test_gen_lowercase_chars_only() {
        let value = generate_random_value(200, CharsetType::Lowercase, None).unwrap();
        for ch in value.chars() {
            assert!(
                ch.is_ascii_lowercase(),
                "Unexpected char '{ch}' in lowercase output"
            );
        }
    }

    #[test]
    fn test_gen_hex_chars_only() {
        let value = generate_random_value(200, CharsetType::Hex, None).unwrap();
        let valid = "0123456789ABCDEF";
        for ch in value.chars() {
            assert!(valid.contains(ch), "Unexpected char '{ch}' in hex output");
        }
    }

    #[test]
    fn test_gen_alphanumeric_symbols_chars_only() {
        let value = generate_random_value(500, CharsetType::AlphanumericSymbols, None).unwrap();
        let valid = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()_+-=[]{}|;:,.<>?";
        for ch in value.chars() {
            assert!(
                valid.contains(ch),
                "Unexpected char '{ch}' in alphanumeric-symbols output"
            );
        }
    }

    #[test]
    fn test_gen_base64_chars_only() {
        let value = generate_random_value(200, CharsetType::Base64, None).unwrap();
        let valid = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for ch in value.chars() {
            assert!(
                valid.contains(ch),
                "Unexpected char '{ch}' in base64 output"
            );
        }
    }
}
