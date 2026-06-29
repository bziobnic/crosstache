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

/// When to route long output through an interactive pager.
#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq, Eq, Default)]
pub enum PagerWhen {
    /// Page only when stdout is an interactive terminal (default for `--pager`).
    #[default]
    Auto,
    /// Always attempt to page (still falls back to direct print when not a TTY).
    Always,
    /// Never page; print directly.
    Never,
}

impl PagerWhen {
    /// Map to the boolean the pager plumbing expects. `print_output` already
    /// gates paging on `can_page()`, so Auto and Always both request paging and
    /// only Never disables it outright.
    pub fn wants_pager(self) -> bool {
        !matches!(self, PagerWhen::Never)
    }
}

#[derive(Debug, Clone, clap::ValueEnum, PartialEq, Eq, Default)]
pub enum OnConflict {
    /// Skip secrets that already exist in the target (default)
    #[default]
    Skip,
    /// Overwrite the target value, replacing the metadata
    Replace,
    /// Abort the migration on first conflict
    Fail,
}

/// Determine if options should be hidden based on environment or command line
fn should_hide_options() -> bool {
    // Check if --show-options is present in command line args
    !std::env::args().any(|arg| arg == "--show-options")
}

/// Parse and validate `--min-score` is in [0.0, 1.0].
fn parse_min_score(s: &str) -> std::result::Result<f32, String> {
    let f: f32 = s
        .parse()
        .map_err(|e: std::num::ParseFloatError| format!("not a float: {e}"))?;
    if !(0.0..=1.0).contains(&f) {
        return Err(format!("must be in 0.0..=1.0, got {f}"));
    }
    Ok(f)
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
        git_ref: built_info::GIT_HEAD_REF
            .map(|r| r.strip_prefix("refs/heads/").unwrap_or(r))
            .or(built_info::GIT_VERSION)
            .unwrap_or("unknown"),
    }
}

#[derive(Debug)]
pub struct BuildInfo {
    pub version: &'static str,
    pub git_hash: &'static str,
    pub git_ref: &'static str,
}

#[derive(Parser)]
#[command(name = "xv")]
#[command(
    about = "A comprehensive tool for managing secrets across Azure Key Vault, AWS Secrets Manager, and local age-encrypted storage"
)]
#[command(version = get_version(), author)]
#[command(help_template = get_help_template())]
pub struct Cli {
    /// Enable debug logging
    #[arg(long, global = true, hide = should_hide_options())]
    pub debug: bool,

    /// Output format (default: auto = table on TTY, json for pipes/redirects)
    #[arg(long, global = true, value_enum, default_value = "auto", hide = should_hide_options())]
    pub format: OutputFormat,

    /// Active environment from the resolved .xv.toml (overrides default_env).
    /// Lower priority than the XV_ENV env var.
    #[arg(long, global = true, hide = should_hide_options())]
    pub env: Option<String>,

    /// Secrets backend to use (overrides config file and XV_BACKEND env var).
    /// Valid values: azure, local, aws.
    #[cfg(feature = "aws")]
    #[arg(
        long,
        global = true,
        value_name = "BACKEND",
        env = "XV_BACKEND",
        hide = should_hide_options()
    )]
    pub backend: Option<String>,

    /// Secrets backend to use (overrides config file and XV_BACKEND env var).
    /// Valid values: azure, local (aws unavailable in this build; rebuild with --features aws).
    #[cfg(not(feature = "aws"))]
    #[arg(
        long,
        global = true,
        value_name = "BACKEND",
        env = "XV_BACKEND",
        hide = should_hide_options()
    )]
    pub backend: Option<String>,

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

    /// Override the AWS profile for this invocation (only honored when active backend is aws)
    #[arg(long, global = true, hide = should_hide_options())]
    pub aws_profile: Option<String>,

    /// Override the AWS region for this invocation (only honored when active backend is aws)
    #[arg(long, global = true, hide = should_hide_options())]
    pub region: Option<String>,

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
            CharsetType::Alphanumeric => {
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
            }
            CharsetType::AlphanumericSymbols => {
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()_+-=[]{}|;:,.<>?"
            }
            CharsetType::Hex => "0123456789ABCDEF",
            CharsetType::Base64 => {
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
            }
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

/// Write-time metadata flags shared by the secret-creating commands
/// (`set` and `gen --save`).
///
/// Flattened into both commands with `#[command(flatten)]` so they expose an
/// identical metadata surface and can never drift apart. Both build a single
/// [`crate::secret::manager::SecretRequest`] from these fields through the same
/// backend trait path.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SecretWriteArgs {
    /// Group to assign the secret to (repeatable; e.g. `-g db -g prod`)
    #[arg(short, long)]
    pub group: Vec<String>,
    /// Note to attach to the secret
    #[arg(long)]
    pub note: Option<String>,
    /// Folder path for the secret (e.g., 'app/database', 'config/dev')
    #[arg(long)]
    pub folder: Option<String>,
    /// Set expiration date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS)
    #[arg(long)]
    pub expires: Option<String>,
    /// Set not-before date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS)
    #[arg(long)]
    pub not_before: Option<String>,
    /// Custom tag in key=value format (repeatable; e.g. `--tag owner=team-data`)
    #[arg(long = "tag", value_name = "KEY=VALUE", value_parser = parse_key_val::<String, String>)]
    pub tag: Vec<(String, String)>,
}

impl SecretWriteArgs {
    /// True when the user supplied at least one metadata flag. Used by `gen`
    /// to reject metadata passed without `--save` (nothing to attach it to).
    pub fn has_any(&self) -> bool {
        !self.group.is_empty()
            || self.note.is_some()
            || self.folder.is_some()
            || self.expires.is_some()
            || self.not_before.is_some()
            || !self.tag.is_empty()
    }

    /// The groups as an `Option<Vec<String>>` matching `SecretRequest.groups`
    /// (None when no `--group` was given, so existing groups are untouched).
    pub fn groups_opt(&self) -> Option<Vec<String>> {
        if self.group.is_empty() {
            None
        } else {
            Some(self.group.clone())
        }
    }

    /// Build a [`crate::secret::manager::SecretRequest`] for a single secret
    /// from these write-time flags, parsing the `--expires` / `--not-before`
    /// date strings. Shared by `set` (single-secret path) and `gen --save`
    /// so both produce byte-identical requests from the same flags.
    pub fn to_secret_request(
        &self,
        name: &str,
        value: zeroize::Zeroizing<String>,
    ) -> Result<crate::secret::manager::SecretRequest> {
        use crate::utils::datetime::parse_datetime_or_duration;

        let expires_on = match self.expires.as_deref() {
            Some(s) => Some(parse_datetime_or_duration(s)?),
            None => None,
        };
        let not_before_on = match self.not_before.as_deref() {
            Some(s) => Some(parse_datetime_or_duration(s)?),
            None => None,
        };

        let tags = if self.tag.is_empty() {
            None
        } else {
            Some(
                self.tag
                    .iter()
                    .cloned()
                    .collect::<std::collections::HashMap<String, String>>(),
            )
        };

        Ok(crate::secret::manager::SecretRequest {
            name: name.to_string(),
            value,
            content_type: None,
            enabled: Some(true),
            expires_on,
            not_before: not_before_on,
            tags,
            groups: self.groups_opt(),
            note: self.note.clone(),
            folder: self.folder.clone(),
        })
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
        /// Read value from stdin instead of prompting (only for single secret).
        /// Input bytes are preserved exactly: no trimming or newline stripping
        #[arg(long)]
        stdin: bool,
        /// Trim leading/trailing whitespace from the value read via --stdin
        #[arg(long, requires = "stdin")]
        trim: bool,
        /// Inline value for a single secret (avoid: appears in shell history —
        /// prefer the interactive prompt or --stdin). Only valid with a single
        /// secret name; mutually exclusive with --stdin.
        #[arg(long, conflicts_with = "stdin")]
        value: Option<String>,
        /// Write-time metadata (group/note/folder/expires/not-before)
        #[command(flatten)]
        meta: SecretWriteArgs,
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
    /// Ranked fuzzy search over secrets (alias: search). Non-interactive;
    /// pipe the output through fzf or similar for an interactive picker.
    /// Default search field is the secret name; opt in to other fields
    /// via repeated `--in <field>`.
    #[command(alias = "search")]
    Find {
        /// Pattern to score every secret against. Omit to list all
        /// secrets unranked (score 0); flags still apply.
        pattern: Option<String>,

        /// Search additional fields alongside the name. Repeatable.
        /// Allowed: name, folder, groups, note, tags.
        #[arg(long = "in", value_name = "FIELD", num_args = 1..)]
        in_fields: Vec<String>,

        /// Maximum rows to print (default 50).
        #[arg(long, default_value_t = 50)]
        limit: usize,

        /// Drop matches scoring below this fraction of the top match
        /// (0.0..=1.0). Default 0.3.
        #[arg(
            long,
            default_value_t = 0.3,
            value_parser = parse_min_score,
        )]
        min_score: f32,

        /// Search every vault the caller has list rights on. Slow on
        /// cold cache. Mutually exclusive with vault-resolved context.
        #[arg(long)]
        all_vaults: bool,

        /// Print one name per line, no headers, no ANSI. Pipe-friendly.
        /// Overrides `--format` and disables auto-format-resolution to
        /// JSON when stdout is not a TTY.
        #[arg(long)]
        names_only: bool,
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
        /// Page number to display (requires --page-size)
        #[arg(long)]
        page: Option<usize>,
        /// Number of rows per page
        #[arg(long)]
        page_size: Option<usize>,
        /// Use an interactive pager for output. Optional WHEN is auto (default
        /// when the flag is given), always, or never. e.g. `--pager` or `--pager auto`.
        #[arg(long, value_name = "WHEN", num_args = 0..=1, default_missing_value = "always")]
        pager: Option<PagerWhen>,
        /// Print one name per line, no headers, no ANSI. Pipe-friendly.
        /// Overrides --format and disables auto-format-resolution.
        #[arg(long)]
        names_only: bool,
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
        /// Use the backend's native rotation instead of generating a value
        /// locally. On AWS this calls RotateSecret, which invokes the
        /// secret's configured rotation Lambda; the rotation completes
        /// asynchronously. Errors on backends without native rotation.
        /// Generation flags (--length, --charset) are ignored.
        #[arg(long, conflicts_with_all = ["generator", "show_value"])]
        native: bool,
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
        /// Write-time metadata for --save (group/note/folder/expires/not-before)
        #[command(flatten)]
        meta: SecretWriteArgs,
    },
    /// Run a command with secrets injected as environment variables
    Run {
        /// Filter secrets by group (can be specified multiple times)
        #[arg(short, long)]
        group: Vec<String>,
        /// Inject only these secrets by name (repeatable). When given, the set
        /// of injected secrets is restricted to these names (still subject to
        /// --group). Names are matched against the original secret name.
        #[arg(long)]
        include: Vec<String>,
        /// Exclude these secrets by name (repeatable). Applied after --group and
        /// --include. Matched against the original secret name.
        #[arg(long)]
        exclude: Vec<String>,
        /// Disable masking of secret values in output
        #[arg(long)]
        no_masking: bool,
        /// Inherit the parent process environment (default: child starts with a clean env)
        #[arg(long)]
        inherit_env: bool,
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
        /// Read value from stdin.
        /// Input bytes are preserved exactly: no trimming or newline stripping
        #[arg(long)]
        stdin: bool,
        /// Trim leading/trailing whitespace from the value read via --stdin
        #[arg(long, requires = "stdin")]
        trim: bool,
        /// Tags for the secret in key=value format (repeatable)
        #[arg(short, long, visible_alias = "tag", value_parser = parse_key_val::<String, String>)]
        tags: Vec<(String, String)>,
        /// Groups for the secret (can be specified multiple times)
        #[arg(short, long)]
        group: Vec<String>,
        /// New name for the secret (rename operation)
        #[arg(long)]
        rename: Option<String>,
        /// Note to attach to the secret (omit to leave unchanged; use --clear-note to remove)
        #[arg(long)]
        note: Option<String>,
        /// Folder path for the secret (e.g., 'app/database'; omit to leave unchanged; use --clear-folder to remove)
        #[arg(long)]
        folder: Option<String>,
        /// Replace existing tags instead of merging
        #[arg(long)]
        replace_tags: bool,
        /// Replace existing groups instead of merging
        #[arg(long)]
        replace_groups: bool,
        /// Set expiration date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS; omit to leave unchanged; use --clear-expires to remove)
        #[arg(long)]
        expires: Option<String>,
        /// Set not-before date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS; omit to leave unchanged; use --clear-not-before to remove)
        #[arg(long)]
        not_before: Option<String>,
        /// Clear expiration date
        #[arg(long, conflicts_with = "expires")]
        clear_expires: bool,
        /// Clear not-before date
        #[arg(long, conflicts_with = "not_before")]
        clear_not_before: bool,
        /// Clear the note
        #[arg(long, conflicts_with = "note")]
        clear_note: bool,
        /// Clear the folder
        #[arg(long, conflicts_with = "folder")]
        clear_folder: bool,
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
    /// Manage `.xv.toml` project environments (the `[env.<name>]` blocks).
    /// Activate via `--env <name>`, `XV_ENV=<name>`, or by setting `default_env`.
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
    /// Local backend maintenance commands (only relevant for `backend = "local"`)
    Local {
        #[command(subcommand)]
        command: LocalCommands,
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
    /// Scan files for leaked secret values or known-token patterns.
    Scan {
        /// Paths to scan (default: current directory).
        #[arg(default_value = ".", num_args = 1..)]
        paths: Vec<std::path::PathBuf>,
        /// Scan only files staged for commit (`git diff --cached`).
        #[arg(long)]
        staged: bool,
        /// Scan the full HEAD tree.
        #[arg(long)]
        all: bool,
        /// Pre-commit hook mode: quiet on no findings, exit 50 on findings.
        #[arg(long)]
        hook: bool,
        /// Search every vault you can list.
        #[arg(long)]
        all_vaults: bool,
        #[command(subcommand)]
        command: Option<ScanCommands>,
    },
    /// Migrate secrets between backends
    Migrate {
        /// Source backend (azure, local, aws)
        #[arg(long)]
        from: String,
        /// Target backend (azure, local, aws)
        #[arg(long)]
        to: String,
        /// Only migrate secrets from this vault
        #[arg(long)]
        vault: Option<String>,
        /// Filter secrets by glob pattern (e.g., "db-*", "api-*")
        #[arg(long)]
        filter: Option<String>,
        /// Preview what would be migrated without making changes
        #[arg(long)]
        dry_run: bool,
        /// Behavior when a secret already exists in the target
        #[arg(long, value_enum, default_value_t = OnConflict::Skip)]
        on_conflict: OnConflict,
        /// Ignore migration tags and replace targets unconditionally
        #[arg(long)]
        force_replace: bool,
        /// Concurrent transfers (default 8)
        #[arg(long, default_value = "8")]
        concurrency: usize,
        /// DEPRECATED: use --on-conflict replace instead
        #[arg(long, hide = true)]
        overwrite: bool,
    },
    /// Open the read-only terminal browser. Requires --features tui at build time.
    #[cfg(feature = "tui")]
    Tui,
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
        /// Page number to display (requires --page-size)
        #[arg(long)]
        page: Option<usize>,
        /// Number of rows per page
        #[arg(long)]
        page_size: Option<usize>,
        /// Use an interactive pager for output. Optional WHEN is auto (default
        /// when the flag is given), always, or never. e.g. `--pager` or `--pager auto`.
        #[arg(long, value_name = "WHEN", num_args = 0..=1, default_missing_value = "always")]
        pager: Option<PagerWhen>,
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
        format: crate::utils::format::OutputFormat,
        /// Include service accounts in output
        #[arg(long)]
        all: bool,
        /// Page number to display (requires --page-size)
        #[arg(long)]
        page: Option<usize>,
        /// Number of rows per page
        #[arg(long)]
        page_size: Option<usize>,
        /// Use an interactive pager for output. Optional WHEN is auto (default
        /// when the flag is given), always, or never. e.g. `--pager` or `--pager auto`.
        #[arg(long, value_name = "WHEN", num_args = 0..=1, default_missing_value = "always")]
        pager: Option<PagerWhen>,
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
        /// Page number to display (requires --page-size)
        #[arg(long)]
        page: Option<usize>,
        /// Number of rows per page
        #[arg(long)]
        page_size: Option<usize>,
        /// Use an interactive pager for output. Optional WHEN is auto (default
        /// when the flag is given), always, or never. e.g. `--pager` or `--pager auto`.
        #[arg(long, value_name = "WHEN", num_args = 0..=1, default_missing_value = "always")]
        pager: Option<PagerWhen>,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show current configuration
    Show {
        /// Show the *effective* (resolved) backend/env/vault/RG and the source
        /// of each (CLI flag / env var / `.xv.toml` profile / global config /
        /// built-in default).
        ///
        /// Use this when `xv` picks a backend you didn't expect — backend
        /// resolution layers across `--backend`, `.xv.toml`, `XV_BACKEND`, and
        /// global config, and this flag prints which layer won.
        #[arg(long)]
        resolved: bool,
    },
    /// Set a configuration value
    Set {
        /// Setting name
        key: String,
        /// Setting value
        value: String,
    },
    /// Show configuration file path
    Path,
    /// Open the configuration file in your default editor
    ///
    /// Picks the editor from `$VISUAL`, then `$EDITOR`. When neither is
    /// set, falls back to a sensible platform default (`nano` on
    /// Linux/macOS, `notepad` on Windows). The config file (and its parent
    /// directory) is created if it does not yet exist so the editor always
    /// opens on a real path.
    Edit,
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

/// Maintenance subcommands for the local age-encrypted backend.
#[derive(Subcommand)]
pub enum LocalCommands {
    /// Re-encrypt existing plaintext secret metadata at rest.
    ///
    /// Requires `encrypt_metadata = true` under `[local]` in your config.
    /// Walks every vault and rewrites any plaintext `.meta.json` (including
    /// archived versions and trash) as age ciphertext. Already-encrypted
    /// metadata is left untouched, so the command is safe to re-run.
    EncryptMetadata {
        /// Show what would be re-encrypted without modifying anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Migrate an existing store to opaque on-disk filenames.
    ///
    /// Requires `opaque_filenames = true` under `[local]` in your config.
    /// Renames every secret's active, version, and trash files to keyed-hash
    /// stems, builds the encrypted `.index.age`, and rebuilds any missing index
    /// entries from metadata. Idempotent and safe to re-run.
    Migrate {
        /// Print the rename plan without touching anything on disk.
        #[arg(long)]
        dry_run: bool,
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
    /// List environment profiles in the resolved .xv.toml
    Envs,
    /// Create a new .xv.toml in the current directory
    Init {
        /// Env name to create (default: "dev")
        #[arg(long, default_value = "dev")]
        env: String,
        /// Vault for the env (skips interactive prompt if provided)
        #[arg(long)]
        vault: Option<String>,
        /// Resource group for the env (skips interactive prompt if provided)
        #[arg(long)]
        resource_group: Option<String>,
        /// Backend for the env (azure | aws | local). When omitted, uses the
        /// global config backend for prompts and is not written to `.xv.toml`.
        #[arg(long)]
        backend: Option<String>,
        /// Skip prompts entirely; require --vault and --resource-group for azure
        #[arg(long)]
        non_interactive: bool,
        /// Overwrite an existing .xv.toml
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum EnvCommands {
    /// List `.xv.toml` environments from the resolved project config
    List,
    /// Write `default_env = "<name>"` into the nearest `.xv.toml`
    Use {
        /// Env name defined in `.xv.toml`
        name: String,
    },
    /// Add a new `[env.<name>]` block to the nearest `.xv.toml` (creates the file if absent)
    Create {
        /// Env name
        name: String,
        /// Vault name for this env
        #[arg(long)]
        vault: String,
        /// Resource group for the vault
        #[arg(long)]
        resource_group: String,
        /// Backend to use (azure | local | aws); defaults to azure
        #[arg(long)]
        backend: Option<String>,
        /// Default secret-group filter for this env (NOT the Azure resource group; see --resource-group)
        #[arg(long)]
        group: Option<String>,
        /// Default folder prefix for this env
        #[arg(long)]
        folder: Option<String>,
        /// Also set this env as `default_env`
        #[arg(long)]
        default: bool,
        /// Overwrite an existing `[env.<name>]` block
        #[arg(long)]
        force: bool,
    },
    /// Remove an `[env.<name>]` block from the resolved `.xv.toml`
    Delete {
        /// Env name
        name: String,
        /// Remove without confirmation prompt
        #[arg(short, long)]
        force: bool,
    },
    /// Show the currently-active env (source, backend, vault, resource_group, group, folder)
    Show,
    /// Pull secrets to a file format
    Pull {
        /// Output format: plain (dotenv), json, yaml, csv, table
        #[arg(long = "fmt", default_value = "plain", id = "pull_format")]
        format: crate::utils::format::OutputFormat,
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

#[derive(Subcommand)]
pub enum ScanCommands {
    /// Install a pre-commit hook that runs `xv scan --staged --hook`.
    Install {
        #[arg(long)]
        force: bool,
    },
    /// Remove the xv-managed pre-commit hook.
    Uninstall,
}

impl Cli {
    pub async fn execute(
        self,
        mut config: Config,
        registry: Option<&crate::backend::BackendRegistry>,
    ) -> Result<()> {
        let resolved = self.format.resolve_for_stdout();
        config.runtime_output_format = resolved;
        config.output_json = matches!(resolved, OutputFormat::Json);

        // Wire template string
        config.template = self.template.clone();

        // Warn if --template given without --format template
        if config.template.is_some() && resolved != OutputFormat::Template {
            crate::utils::output::warn("--template flag has no effect without --format template");
        }

        // Apply CLI credential type if specified (CLI flag overrides config/env)
        if let Some(cred_type) = self.credential_type {
            use crate::config::settings::AzureCredentialType;
            use std::str::FromStr;

            config.azure_credential_priority =
                AzureCredentialType::from_str(&cred_type).map_err(CrosstacheError::config)?;
        }

        // Apply --aws-profile CLI flag into config.aws
        if let Some(ref p) = self.aws_profile {
            if let Some(ref mut aws) = config.aws {
                aws.profile = Some(p.clone());
            } else {
                config.aws = Some(crate::config::settings::AwsConfig {
                    profile: Some(p.clone()),
                    ..Default::default()
                });
            }
        }

        // Apply --region CLI flag into config.aws
        if let Some(ref r) = self.region {
            if let Some(ref mut aws) = config.aws {
                aws.region = Some(r.clone());
            } else {
                config.aws = Some(crate::config::settings::AwsConfig {
                    region: Some(r.clone()),
                    ..Default::default()
                });
            }
        }

        match self.command {
            Commands::Set {
                args,
                stdin,
                trim,
                value,
                meta,
            } => {
                crate::cli::secret_ops::execute_secret_set_direct(
                    args, stdin, trim, value, meta, config, registry,
                )
                .await
            }
            Commands::Get { name, raw, version } => {
                crate::cli::secret_ops::execute_secret_get_direct(
                    &name, raw, version, config, registry,
                )
                .await
            }
            Commands::Find {
                pattern,
                in_fields,
                limit,
                min_score,
                all_vaults,
                names_only,
            } => {
                crate::cli::secret_ops::execute_secret_find_direct(
                    pattern,
                    in_fields,
                    limit,
                    min_score,
                    all_vaults,
                    names_only,
                    self.format,
                    config,
                    registry,
                )
                .await
            }
            Commands::List {
                group,
                all,
                expiring,
                expired,
                no_cache,
                page,
                page_size,
                pager,
                names_only,
            } => {
                let pagination = crate::utils::pagination::Pagination::from_args(page, page_size)?;
                let pager = pager.map(PagerWhen::wants_pager).unwrap_or(false);
                crate::cli::secret_ops::execute_secret_list_direct(
                    group, all, expiring, expired, no_cache, pagination, pager, names_only, config,
                    registry,
                )
                .await
            }
            Commands::Delete { name, group, force } => {
                crate::cli::secret_ops::execute_secret_delete_direct(
                    name, group, force, config, registry,
                )
                .await
            }
            Commands::History { name } => {
                crate::cli::secret_ops::execute_secret_history_direct(&name, config, registry).await
            }
            Commands::Rollback {
                name,
                version,
                force,
            } => {
                crate::cli::secret_ops::execute_secret_rollback_direct(
                    &name, &version, force, config, registry,
                )
                .await
            }
            Commands::Rotate {
                name,
                length,
                charset,
                generator,
                native,
                show_value,
                force,
            } => {
                crate::cli::secret_ops::execute_secret_rotate_direct(
                    &name, length, charset, generator, native, show_value, force, config, registry,
                )
                .await
            }
            Commands::Gen {
                length,
                charset,
                save,
                vault,
                raw,
                meta,
            } => {
                crate::cli::system_ops::execute_gen_command(
                    length, charset, save, vault, raw, meta, config, registry,
                )
                .await
            }
            Commands::Run {
                group,
                include,
                exclude,
                no_masking,
                inherit_env,
                command,
            } => {
                crate::cli::secret_ops::execute_secret_run_direct(
                    group,
                    include,
                    exclude,
                    no_masking,
                    inherit_env,
                    command,
                    config,
                    registry,
                )
                .await
            }
            Commands::Inject {
                template,
                out,
                group,
            } => {
                crate::cli::secret_ops::execute_secret_inject_direct(
                    template, out, group, config, registry,
                )
                .await
            }
            Commands::Update {
                name,
                value,
                stdin,
                trim,
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
                clear_note,
                clear_folder,
            } => {
                crate::cli::secret_ops::execute_secret_update_direct(
                    &name,
                    value,
                    stdin,
                    trim,
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
                    clear_note,
                    clear_folder,
                    config,
                    registry,
                )
                .await
            }
            Commands::Diff {
                vault1,
                vault2,
                show_values,
                group,
            } => {
                crate::cli::secret_ops::execute_diff_command(
                    &vault1,
                    &vault2,
                    show_values,
                    group,
                    config,
                    registry,
                )
                .await
            }
            Commands::Copy {
                name,
                from,
                to,
                new_name,
            } => {
                crate::cli::secret_ops::execute_secret_copy_direct(
                    &name, &from, &to, new_name, config, registry,
                )
                .await
            }
            Commands::Move {
                name,
                from,
                to,
                new_name,
                force,
            } => {
                crate::cli::secret_ops::execute_secret_move_direct(
                    &name, &from, &to, new_name, force, config, registry,
                )
                .await
            }
            Commands::Purge { name, force } => {
                crate::cli::secret_ops::execute_secret_purge_direct(&name, force, config, registry)
                    .await
            }
            Commands::Restore { name } => {
                crate::cli::secret_ops::execute_secret_restore_direct(&name, config, registry).await
            }
            Commands::Parse {
                connection_string,
                format,
            } => {
                crate::cli::secret_ops::execute_secret_parse_direct(
                    &connection_string,
                    &format,
                    config,
                    registry,
                )
                .await
            }
            Commands::Share { command } => {
                crate::cli::secret_ops::execute_secret_share_direct(command, config, registry).await
            }
            Commands::Vault { command } => {
                crate::cli::vault_ops::execute_vault_command(command, config, registry).await
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
                crate::cli::system_ops::execute_audit_command(
                    name,
                    vault,
                    days,
                    operation,
                    resource_group,
                    raw,
                    config,
                    registry,
                )
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
                    registry,
                )
                .await
            }
            Commands::Version => crate::cli::system_ops::execute_version_command().await,
            Commands::Completion { shell } => {
                crate::cli::system_ops::execute_completion_command(shell).await
            }
            Commands::Whoami => {
                crate::cli::system_ops::execute_whoami_command(config, registry).await
            }
            // Upgrade does not need Azure config — only talks to GitHub API
            Commands::Upgrade { check, force } => {
                crate::cli::upgrade_ops::execute_upgrade_command(check, force).await
            }
            Commands::Cache { command } => {
                crate::cli::config_ops::execute_cache_command(command, config).await
            }
            Commands::Local { command } => {
                crate::cli::local_ops::execute_local_command(command, config).await
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
            Commands::Scan {
                paths,
                staged,
                all,
                hook,
                all_vaults,
                command,
            } => {
                crate::cli::scan_ops::execute_scan_command(
                    paths,
                    staged,
                    all,
                    hook,
                    all_vaults,
                    command,
                    self.format,
                    config,
                    registry,
                )
                .await
            }
            Commands::Migrate {
                from,
                to,
                vault,
                filter,
                dry_run,
                on_conflict,
                force_replace,
                concurrency,
                overwrite,
            } => {
                crate::cli::migrate_ops::execute_migrate(
                    from,
                    to,
                    vault,
                    filter,
                    dry_run,
                    on_conflict,
                    force_replace,
                    concurrency,
                    overwrite,
                    config,
                )
                .await
            }
            #[cfg(feature = "tui")]
            Commands::Tui => crate::tui::run_tui(config, registry).await,
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

    #[test]
    fn test_update_clear_flags_parse() {
        let cli = Cli::try_parse_from([
            "xv",
            "update",
            "mysecret",
            "--clear-expires",
            "--clear-not-before",
            "--clear-note",
            "--clear-folder",
        ])
        .unwrap();

        match cli.command {
            Commands::Update {
                clear_expires,
                clear_not_before,
                clear_note,
                clear_folder,
                expires,
                not_before,
                note,
                folder,
                ..
            } => {
                assert!(clear_expires);
                assert!(clear_not_before);
                assert!(clear_note);
                assert!(clear_folder);
                assert!(expires.is_none());
                assert!(not_before.is_none());
                assert!(note.is_none());
                assert!(folder.is_none());
            }
            _ => panic!("expected update command"),
        }
    }

    #[test]
    fn test_update_set_and_clear_same_field_conflicts() {
        for (set_flag, set_value, clear_flag) in [
            ("--expires", "2030-01-01", "--clear-expires"),
            ("--not-before", "2030-01-01", "--clear-not-before"),
            ("--note", "some note", "--clear-note"),
            ("--folder", "app/db", "--clear-folder"),
        ] {
            let result =
                Cli::try_parse_from(["xv", "update", "mysecret", set_flag, set_value, clear_flag]);
            assert!(
                result.is_err(),
                "{set_flag} together with {clear_flag} should be rejected"
            );
        }
    }

    #[test]
    fn test_secret_list_pagination_args_parse() {
        let cli =
            Cli::try_parse_from(["xv", "list", "--page-size", "25", "--page", "2", "--pager"])
                .unwrap();

        match cli.command {
            Commands::List {
                page,
                page_size,
                pager,
                ..
            } => {
                assert_eq!(page, Some(2));
                assert_eq!(page_size, Some(25));
                assert_eq!(pager, Some(PagerWhen::Always));
            }
            _ => panic!("Expected List command"),
        }
    }

    #[test]
    fn test_vault_list_pagination_args_parse() {
        let cli = Cli::try_parse_from([
            "xv",
            "vault",
            "list",
            "--page-size",
            "50",
            "--page",
            "3",
            "--pager",
        ])
        .unwrap();

        match cli.command {
            Commands::Vault {
                command:
                    VaultCommands::List {
                        page,
                        page_size,
                        pager,
                        ..
                    },
            } => {
                assert_eq!(page, Some(3));
                assert_eq!(page_size, Some(50));
                assert_eq!(pager, Some(PagerWhen::Always));
            }
            _ => panic!("Expected vault list command"),
        }
    }

    #[test]
    fn test_share_list_pagination_args_parse() {
        let cli = Cli::try_parse_from([
            "xv",
            "share",
            "list",
            "api-key",
            "--page-size",
            "10",
            "--page",
            "2",
            "--pager",
        ])
        .unwrap();

        match cli.command {
            Commands::Share {
                command:
                    ShareCommands::List {
                        secret_name,
                        page,
                        page_size,
                        pager,
                        ..
                    },
            } => {
                assert_eq!(secret_name, "api-key");
                assert_eq!(page, Some(2));
                assert_eq!(page_size, Some(10));
                assert_eq!(pager, Some(PagerWhen::Always));
            }
            _ => panic!("Expected share list command"),
        }
    }

    #[test]
    fn test_vault_share_list_pagination_args_parse() {
        let cli = Cli::try_parse_from([
            "xv",
            "vault",
            "share",
            "list",
            "prod-vault",
            "--page-size",
            "10",
            "--page",
            "2",
            "--pager",
        ])
        .unwrap();

        match cli.command {
            Commands::Vault {
                command:
                    VaultCommands::Share {
                        command:
                            VaultShareCommands::List {
                                vault_name,
                                page,
                                page_size,
                                pager,
                                ..
                            },
                    },
            } => {
                assert_eq!(vault_name, "prod-vault");
                assert_eq!(page, Some(2));
                assert_eq!(page_size, Some(10));
                assert_eq!(pager, Some(PagerWhen::Always));
            }
            _ => panic!("Expected vault share list command"),
        }
    }

    // ── gen command unit tests ───────────────────────────────────────────────

    #[test]
    fn test_migrate_command_parses() {
        let cli = Cli::try_parse_from([
            "xv",
            "migrate",
            "--from",
            "azure",
            "--to",
            "local",
            "--vault",
            "my-vault",
            "--filter",
            "db-*",
            "--dry-run",
            "--overwrite",
        ])
        .unwrap();

        match cli.command {
            Commands::Migrate {
                from,
                to,
                vault,
                filter,
                dry_run,
                on_conflict,
                force_replace,
                concurrency,
                overwrite,
            } => {
                assert_eq!(from, "azure");
                assert_eq!(to, "local");
                assert_eq!(vault, Some("my-vault".to_string()));
                assert_eq!(filter, Some("db-*".to_string()));
                assert!(dry_run);
                assert!(overwrite);
                assert_eq!(on_conflict, OnConflict::Skip);
                assert!(!force_replace);
                assert_eq!(concurrency, 8);
            }
            _ => panic!("Expected Migrate command"),
        }
    }

    #[test]
    fn test_migrate_command_minimal_parses() {
        let cli =
            Cli::try_parse_from(["xv", "migrate", "--from", "local", "--to", "azure"]).unwrap();

        match cli.command {
            Commands::Migrate {
                from,
                to,
                vault,
                filter,
                dry_run,
                on_conflict,
                force_replace,
                concurrency,
                overwrite,
            } => {
                assert_eq!(from, "local");
                assert_eq!(to, "azure");
                assert_eq!(vault, None);
                assert_eq!(filter, None);
                assert!(!dry_run);
                assert!(!overwrite);
                assert_eq!(on_conflict, OnConflict::Skip);
                assert!(!force_replace);
                assert_eq!(concurrency, 8);
            }
            _ => panic!("Expected Migrate command"),
        }
    }

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

    // ── shared SecretWriteArgs (set / gen --save parity) ─────────────────────

    #[test]
    fn test_gen_accepts_group_and_metadata_flags() {
        // gen --save must now accept the same metadata surface as set.
        let cli = Cli::try_parse_from([
            "xv",
            "gen",
            "--save",
            "db-pass",
            "-g",
            "db",
            "-g",
            "prod",
            "--note",
            "n",
            "--folder",
            "app/db",
            "--expires",
            "2030-01-01",
        ])
        .unwrap();
        match cli.command {
            Commands::Gen { save, meta, .. } => {
                assert_eq!(save.as_deref(), Some("db-pass"));
                assert_eq!(meta.group, vec!["db".to_string(), "prod".to_string()]);
                assert_eq!(meta.note.as_deref(), Some("n"));
                assert_eq!(meta.folder.as_deref(), Some("app/db"));
                assert_eq!(meta.expires.as_deref(), Some("2030-01-01"));
                assert!(meta.has_any());
            }
            _ => panic!("Expected Gen command"),
        }
    }

    #[test]
    fn test_set_accepts_group_flag() {
        // set gained --group as the symmetric bonus of the shared struct.
        let cli =
            Cli::try_parse_from(["xv", "set", "api-key", "-g", "web", "--folder", "svc"]).unwrap();
        match cli.command {
            Commands::Set { args, meta, .. } => {
                assert_eq!(args, vec!["api-key".to_string()]);
                assert_eq!(meta.group, vec!["web".to_string()]);
                assert_eq!(meta.folder.as_deref(), Some("svc"));
            }
            _ => panic!("Expected Set command"),
        }
    }

    #[test]
    fn test_secret_write_args_has_any() {
        assert!(!SecretWriteArgs::default().has_any());

        let with_group = SecretWriteArgs {
            group: vec!["x".into()],
            ..Default::default()
        };
        assert!(with_group.has_any());

        let with_note = SecretWriteArgs {
            note: Some("hi".into()),
            ..Default::default()
        };
        assert!(with_note.has_any());
    }

    #[test]
    fn test_secret_write_args_groups_opt() {
        assert_eq!(SecretWriteArgs::default().groups_opt(), None);
        let a = SecretWriteArgs {
            group: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        assert_eq!(a.groups_opt(), Some(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn test_to_secret_request_populates_metadata() {
        let meta = SecretWriteArgs {
            group: vec!["db".into()],
            note: Some("note".into()),
            folder: Some("f".into()),
            expires: None,
            not_before: None,
            tag: vec![("owner".into(), "team-data".into())],
        };
        let req = meta
            .to_secret_request("name", zeroize::Zeroizing::new("val".to_string()))
            .unwrap();
        assert_eq!(req.name, "name");
        assert_eq!(req.value.as_str(), "val");
        assert_eq!(req.groups, Some(vec!["db".to_string()]));
        assert_eq!(req.note.as_deref(), Some("note"));
        assert_eq!(req.folder.as_deref(), Some("f"));
        assert!(req.expires_on.is_none());
        assert!(req.not_before.is_none());
        assert_eq!(req.enabled, Some(true));
        assert_eq!(
            req.tags.as_ref().and_then(|t| t.get("owner")).map(String::as_str),
            Some("team-data")
        );
    }

    #[test]
    fn test_has_any_detects_tag() {
        let with_tag = SecretWriteArgs {
            tag: vec![("k".into(), "v".into())],
            ..Default::default()
        };
        assert!(with_tag.has_any());
    }

    #[test]
    fn test_to_secret_request_rejects_bad_date() {
        let meta = SecretWriteArgs {
            expires: Some("not-a-date".into()),
            ..Default::default()
        };
        let res = meta.to_secret_request("n", zeroize::Zeroizing::new("v".to_string()));
        assert!(res.is_err(), "invalid --expires should be rejected");
    }
}
