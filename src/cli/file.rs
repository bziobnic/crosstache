//! File/blob CLI command definitions.

use clap::Subcommand;

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
        /// Maximum number of results
        #[arg(long)]
        limit: Option<usize>,
        /// List all files recursively (show all nested files instead of directory structure)
        #[arg(short, long)]
        recursive: bool,
        /// Bypass the local cache and fetch fresh data
        #[arg(long)]
        no_cache: bool,
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

/// Parse a single key-value pair.
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
