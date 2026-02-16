//! CLI commands and argument parsing
//!
//! This module defines the command-line interface structure using clap,
//! including all commands, subcommands, and their arguments.

use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use crate::vault::{VaultCreateRequest, VaultManager};
#[cfg(feature = "file-ops")]
use crate::blob::manager::{BlobManager, create_blob_manager};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

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
        git_branch: built_info::GIT_HEAD_REF.map(|r| r.strip_prefix("refs/heads/").unwrap_or(r)).unwrap_or("unknown"),
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

#[derive(Subcommand)]
pub enum Commands {
    /// Set a secret in the current vault context
    Set {
        /// Secret name
        name: String,
        /// Read value from stdin instead of prompting
        #[arg(long)]
        stdin: bool,
        /// Note to attach to the secret
        #[arg(long)]
        note: Option<String>,
        /// Folder path for the secret (e.g., 'app/database', 'config/dev')
        #[arg(long)]
        folder: Option<String>,
    },
    /// Get a secret from the current vault context
    Get {
        /// Secret name
        name: String,
        /// Raw output (print value instead of copying to clipboard)
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
    },
    /// Delete a secret from the current vault context (alias: rm)
    #[command(alias = "rm")]
    Delete {
        /// Secret name
        name: String,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
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

impl Cli {
    pub async fn execute(self, mut config: Config) -> Result<()> {
        // Apply CLI credential type if specified (CLI flag overrides config/env)
        if let Some(cred_type) = self.credential_type {
            use crate::config::settings::AzureCredentialType;
            use std::str::FromStr;
            
            config.azure_credential_priority = AzureCredentialType::from_str(&cred_type)
                .map_err(CrosstacheError::config)?;
        }
        
        match self.command {
            Commands::Set {
                name,
                stdin,
                note,
                folder,
            } => execute_secret_set_direct(&name, stdin, note, folder, config).await,
            Commands::Get { name, raw } => execute_secret_get_direct(&name, raw, config).await,
            Commands::List { group, all } => execute_secret_list_direct(group, all, config).await,
            Commands::Delete { name, force } => {
                execute_secret_delete_direct(&name, force, config).await
            }
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
                    config,
                )
                .await
            }
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
            Commands::Init => execute_init_command(config).await,
            Commands::Info {
                resource,
                resource_type,
                resource_group,
                subscription,
            } => execute_info_command(resource, resource_type, resource_group, subscription, config).await,
            Commands::Version => execute_version_command().await,
            #[cfg(feature = "file-ops")]
            Commands::Upload { file_path, name, groups, metadata } => {
                execute_file_upload_quick(&file_path, name, groups, metadata, &config).await
            },
            #[cfg(feature = "file-ops")]
            Commands::Download { name, output, open } => {
                execute_file_download_quick(&name, output, open, &config).await
            },
        }
    }
}

#[cfg(feature = "file-ops")]
async fn execute_file_command(command: FileCommands, config: Config) -> Result<()> {

    // Create blob manager
    let blob_manager = create_blob_manager(&config).map_err(|e| {
        if e.to_string().contains("No storage account configured") {
            CrosstacheError::config("No blob storage configured. Run 'xv init' to set up blob storage.")
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
                        "--name and --content-type cannot be used with --recursive"
                    ));
                }
                // Validate that --prefix is not used with --name
                if prefix.is_some() && name.is_some() {
                    return Err(CrosstacheError::invalid_argument(
                        "--prefix cannot be used with --name"
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
                        "--name and --content-type can only be used when uploading a single file"
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
                        "--rename and --stream cannot be used with --recursive"
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
                        "--rename can only be used when downloading a single file"
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
                    execute_file_download(&blob_manager, &files[0], output_path, stream, force, &config).await?;
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
            execute_file_list(&blob_manager, prefix, group, metadata, limit, recursive, &config).await?;
        }
        FileCommands::Delete { files, force, continue_on_error } => {
            // Handle single vs multiple file delete
            if files.len() == 1 {
                execute_file_delete(&blob_manager, &files[0], force, &config).await?;
            } else {
                execute_file_delete_multiple(&blob_manager, files, force, continue_on_error, &config).await?;
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
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use std::sync::Arc;

    // Create authentication provider with credential priority from config
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?
    );

    // Create vault manager
    let vault_manager = VaultManager::new(
        auth_provider,
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
        VaultCommands::List { resource_group, format } => {
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
            execute_vault_share(&vault_manager, command, &config).await?;
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
    name: &str,
    stdin: bool,
    note: Option<String>,
    folder: Option<String>,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_set(&secret_manager, name, None, stdin, note, folder, &config).await
}

async fn execute_secret_get_direct(name: &str, raw: bool, config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_get(&secret_manager, name, None, raw, &config).await
}

async fn execute_secret_list_direct(
    group: Option<String>,
    all: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_list(&secret_manager, None, group, all, &config).await
}

async fn execute_secret_delete_direct(name: &str, force: bool, config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_delete(&secret_manager, name, None, force, &config).await
}

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
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );

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
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );

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
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_restore(&secret_manager, name, None, &config).await
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
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_parse(&secret_manager, connection_string, format, &config).await
}

async fn execute_secret_share_direct(command: ShareCommands, config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_share(&secret_manager, command, &config).await
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
    let detected_type = ResourceDetector::detect_resource_type(
        &resource,
        resource_type,
        resource_group.is_some(),
    );
    
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
        ResourceType::Secret => {
            execute_secret_info_from_root(&resource, &config).await
        }
        #[cfg(feature = "file-ops")]
        ResourceType::File => {
            execute_file_info_from_root(&resource, &config).await
        }
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
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone()
        )?
    );
    
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
async fn execute_secret_info_from_root(
    secret_name: &str,
    config: &Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;
    
    // Check if we have a vault context
    let vault_name = if !config.default_vault.is_empty() {
        &config.default_vault
    } else {
        return Err(CrosstacheError::config("No vault context set. Use 'xv context set <vault>' to set a default vault"));
    };
    
    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone()
        )?
    );
    
    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);
    
    // Get secret info
    let secret_info = secret_manager.get_secret_info(vault_name, secret_name).await?;
    
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
async fn execute_file_info_from_root(
    file_name: &str,
    config: &Config,
) -> Result<()> {
    // Create blob manager
    let blob_manager = create_blob_manager(config).map_err(|e| {
        if e.to_string().contains("No storage account configured") {
            CrosstacheError::config("No blob storage configured. Run 'xv init' to set up blob storage.")
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
            use std::str::FromStr;
            use crate::config::settings::AzureCredentialType;
            config.azure_credential_priority = AzureCredentialType::from_str(value).map_err(|e| {
                CrosstacheError::config(e)
            })?;
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
                CrosstacheError::config(format!("Invalid value for blob_max_concurrent_uploads: {value}"))
            })?;
            let mut blob_config = config.get_blob_config();
            blob_config.max_concurrent_uploads = max_uploads;
            config.set_blob_config(blob_config);
        }
        _ => {
            return Err(CrosstacheError::config(format!(
                "Unknown configuration key: {key}. Available keys: debug, subscription_id, default_vault, default_resource_group, default_location, tenant_id, function_app_url, cache_ttl, output_json, no_color, azure_credential_priority, storage_account, storage_container, storage_endpoint, blob_chunk_size_mb, blob_max_concurrent_uploads"
            )));
        }
    }

    config.save().await?;
    println!("✅ Configuration updated: {key} = {value}");

    Ok(())
}

async fn execute_secret_set(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    stdin: bool,
    note: Option<String>,
    folder: Option<String>,
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

    // Create secret request with note and/or folder if provided
    let secret_request = if note.is_some() || folder.is_some() {
        Some(crate::secret::manager::SecretRequest {
            name: name.to_string(),
            value: value.clone(),
            content_type: None,
            enabled: Some(true),
            expires_on: None,
            not_before: None,
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
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use clipboard::{ClipboardContext, ClipboardProvider};

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get the secret
    let secret = secret_manager
        .get_secret_safe(&vault_name, name, true, true)
        .await?;

    if raw {
        // Raw output - print the value
        if let Some(value) = secret.value {
            print!("{value}");
        }
    } else {
        // Default behavior - copy to clipboard
        if let Some(ref value) = secret.value {
            match ClipboardContext::new() {
                Ok(mut ctx) => match ctx.set_contents(value.clone()) {
                    Ok(_) => {
                        println!("✅ Secret '{name}' copied to clipboard");
                    }
                    Err(e) => {
                        eprintln!("⚠️  Failed to copy to clipboard: {e}");
                        eprintln!("Secret value: {value}");
                    }
                },
                Err(e) => {
                    eprintln!("⚠️  Failed to access clipboard: {e}");
                    eprintln!("Secret value: {value}");
                }
            }
        } else {
            println!("⚠️  Secret '{name}' has no value");
        }
    }

    Ok(())
}

async fn execute_secret_list(
    secret_manager: &crate::secret::manager::SecretManager,
    vault: Option<String>,
    group: Option<String>,
    show_all: bool,
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

    secret_manager
        .list_secrets_formatted(
            &vault_name,
            group.as_deref(),
            output_format,
            false,
            show_all,
        )
        .await?;

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
        Some(v)
    } else if stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        let trimmed = buffer.trim().to_string();
        if trimmed.is_empty() {
            return Err(CrosstacheError::config("Secret value cannot be empty"));
        }
        Some(trimmed)
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
    {
        return Err(CrosstacheError::invalid_argument(
            "No updates specified. Use 'secret update' to modify metadata (groups, tags, folder, note) or rename secrets. Use 'secret set' to update secret values."
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

    // Create update request with enhanced parameters
    let update_request = SecretUpdateRequest {
        name: name.to_string(),
        new_name: rename.clone(),
        value: new_value.clone(),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
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
    secret_manager: &crate::secret::manager::SecretManager,
    command: ShareCommands,
    config: &Config,
) -> Result<()> {
    let _ = secret_manager;
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(None).await?;

    match command {
        ShareCommands::Grant {
            secret_name,
            user,
            level,
        } => {
            eprintln!(
                "Secret sharing (grant {level} access to '{secret_name}' for '{user}' in vault '{vault_name}') is not yet implemented."
            );
        }
        ShareCommands::Revoke { secret_name, user } => {
            eprintln!(
                "Secret sharing (revoke access to '{secret_name}' for '{user}' in vault '{vault_name}') is not yet implemented."
            );
        }
        ShareCommands::List { secret_name } => {
            eprintln!(
                "Secret sharing (list permissions for '{secret_name}' in vault '{vault_name}') is not yet implemented."
            );
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
    use std::fs::File;
    use std::io::Write;
    use std::sync::Arc;

    let _resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Create secret manager to get secrets from vault
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );
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
                                secret_data
                                    .insert("value".to_string(), serde_json::Value::String(value));
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
                                env_lines.push(format!("{env_name}={value}"));
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
                                txt_lines.push(format!("  Value: {value}"));
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
            let mut file = File::create(&file_path).map_err(|e| {
                CrosstacheError::unknown(format!("Failed to create output file: {e}"))
            })?;
            file.write_all(export_data.as_bytes()).map_err(|e| {
                CrosstacheError::unknown(format!("Failed to write to output file: {e}"))
            })?;
            println!("Exported {} secrets to {}", secrets.len(), file_path);
        }
        None => {
            println!("{export_data}");
        }
    }

    Ok(())
}

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
            io::stdin().read_to_string(&mut buffer).map_err(|e| {
                CrosstacheError::unknown(format!("Failed to read from stdin: {e}"))
            })?;
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
                    value: value.to_string(),
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
                        value: value.to_string(),
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
        DefaultAzureCredentialProvider::with_credential_priority(config.azure_credential_priority.clone())
            .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?    );
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

    println!(
        "Import completed: {imported_count} imported, {skipped_count} skipped"
    );

    Ok(())
}

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

    // Vault update requires proper implementation in vault manager
    let _ = (vault_manager, update_request);
    eprintln!("Vault update for '{name}' in resource group '{resource_group}' is not yet implemented.");

    Ok(())
}

async fn execute_vault_share(
    vault_manager: &VaultManager,
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
                    &user,
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

            vault_manager
                .revoke_vault_access(&vault_name, &resource_group, &user, Some(&user))
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
    println!(
        "✅ Cleared vault context for '{vault_name}' ({scope} scope)"
    );

    Ok(())
}

// File operation functions
#[cfg(feature = "file-ops")]
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
        return Err(CrosstacheError::config(format!("File not found: {file_path}")));
    }

    // Read file content
    let content = fs::read(file_path).map_err(|e| {
        CrosstacheError::config(format!("Failed to read file {file_path}: {e}"))
    })?;

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
    use crate::blob::models::{BlobListItem, FileListRequest};
    use crate::blob::manager::format_size;
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    // Create list request
    let list_request = FileListRequest {
        prefix: prefix.clone(),
        groups: group.map(|g| vec![g]),
        limit,
        delimiter: if recursive { None } else { Some("/".to_string()) },
        recursive,
    };

    // Get items based on recursive flag
    let items = if recursive {
        // Old behavior: flat list of all files
        let files = blob_manager.list_files(list_request).await?;
        files.into_iter().map(BlobListItem::File).collect::<Vec<_>>()
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
        let file_count = items.iter().filter(|i| matches!(i, BlobListItem::File(_))).count();
        let dir_count = items.iter().filter(|i| matches!(i, BlobListItem::Directory { .. })).count();

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
async fn execute_file_info(
    blob_manager: &BlobManager,
    name: &str,
    config: &Config,
) -> Result<()> {
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
        println!("  Last Modified: {}", file_info.last_modified.format("%Y-%m-%d %H:%M:%S UTC"));
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
    relative_path: String,
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
fn collect_files_recursive(path: &Path) -> Result<Vec<PathBuf>> {
    use std::fs;
    
    let mut files = Vec::new();
    
    if path.is_file() {
        files.push(path.to_path_buf());
    } else if path.is_dir() {
        let entries = fs::read_dir(path).map_err(|e| {
            CrosstacheError::config(format!("Failed to read directory {}: {}", path.display(), e))
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
                files.extend(collect_files_recursive(&entry_path)?);
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
        let relative = path.strip_prefix(base_path)
            .unwrap_or(path);

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
            relative_path: relative.to_string_lossy().to_string(),
            blob_name,
        });
    } else if path.is_dir() {
        let entries = fs::read_dir(path).map_err(|e| {
            CrosstacheError::config(format!("Failed to read directory {}: {}", path.display(), e))
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
            files.extend(collect_files_with_structure(&entry_path, base_path, prefix, flatten)?);
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
                return Err(CrosstacheError::config(format!("Path not found: {path_str}")));
            }
        }
        // Canonicalize the base path for consistent relative path calculation
        let base_path = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };

        let files = collect_files_with_structure(
            path,
            base_path,
            prefix.as_deref(),
            flatten,
        )?;
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
            &local_path_str.to_string(),
            Some(file_info.blob_name.clone()), // Use the calculated blob name
            group.clone(),
            metadata.clone(),
            tag.clone(),
            None, // No content type override for batch uploads
            progress,
            config,
        ).await;

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
        ).await {
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
        return Err(CrosstacheError::azure_api(
            format!("{error_count} file(s) failed to upload")
        ));
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
        ).await {
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
        return Err(CrosstacheError::azure_api(
            format!("{error_count} file(s) failed to download")
        ));
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
    use std::path::Path;
    use std::fs;

    // Determine output directory (default to current directory)
    let output_dir = output.unwrap_or_else(|| ".".to_string());
    let output_path = Path::new(&output_dir);

    // Create output directory if it doesn't exist
    if !output_path.exists() {
        fs::create_dir_all(output_path).map_err(|e| {
            CrosstacheError::config(format!("Failed to create output directory {}: {}", output_dir, e))
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
            eprintln!("⚠️  File already exists: {} (use --force to overwrite)", local_path_str);
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
        let confirm = rpassword::prompt_password(
            "Are you sure you want to delete these files? (y/N): "
        )?;
        
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
        return Err(CrosstacheError::azure_api(
            format!("{error_count} file(s) failed to delete")
        ));
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
    let _ = (blob_manager, local_path, prefix, direction, dry_run, delete, config);
    eprintln!("File sync is not yet implemented.");
    
    Ok(())
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
            CrosstacheError::config("No blob storage configured. Run 'xv init' to set up blob storage.")
        } else {
            e
        }
    })?;

    // Convert parameters to match FileCommands::Upload format
    let groups_vec = groups.map(|g| g.split(',').map(|s| s.trim().to_string()).collect()).unwrap_or_default();
    let metadata_map = metadata.into_iter().filter_map(|m| {
        let parts: Vec<&str> = m.splitn(2, '=').collect();
        if parts.len() == 2 {
            Some((parts[0].trim().to_string(), parts[1].trim().to_string()))
        } else {
            None
        }
    }).collect();

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
    ).await
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
            CrosstacheError::config("No blob storage configured. Run 'xv init' to set up blob storage.")
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
    ).await?;

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
