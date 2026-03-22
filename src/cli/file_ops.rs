//! Blob/file subcommand execution (`xv file`, quick upload/download, cache refresh for file lists).
//!
//! Kept separate from [`crate::cli::commands`] so the command router stays thin.

use crate::blob::manager::{create_blob_manager, BlobManager};
use crate::cli::file::{FileCommands, SyncDirection};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use crate::utils::output;
use std::path::{Path, PathBuf};

pub(crate) async fn execute_file_command(command: FileCommands, config: Config) -> Result<()> {
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
                    continue_on_error,
                    &config,
                )
                .await?;
            }
            // Invalidate the file list cache (both recursive and hierarchical) after any upload
            let cache_manager = crate::cache::CacheManager::from_config(&config);
            if let Ok(vault_name) = config.resolve_vault_name(None).await {
                cache_manager.invalidate(&crate::cache::CacheKey::FileList {
                    vault_name: vault_name.clone(),
                    recursive: true,
                });
                cache_manager.invalidate(&crate::cache::CacheKey::FileList {
                    vault_name,
                    recursive: false,
                });
            }
        }
        FileCommands::Download {
            files,
            output,
            rename,
            recursive,
            flatten,
            force,
            continue_on_error,
        } => {
            // Handle recursive download
            if recursive {
                // Validate that --rename is not used with --recursive
                if rename.is_some() {
                    return Err(CrosstacheError::invalid_argument(
                        "--rename cannot be used with --recursive",
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
                    execute_file_download(&blob_manager, &files[0], output_path, force, &config)
                        .await?;
                } else {
                    execute_file_download_multiple(
                        &blob_manager,
                        files,
                        output,
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
            limit,
            recursive,
            no_cache,
        } => {
            execute_file_list(
                &blob_manager,
                prefix,
                group,
                limit,
                recursive,
                no_cache,
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
            // Invalidate the file list cache (both recursive and hierarchical) after any delete
            let cache_manager = crate::cache::CacheManager::from_config(&config);
            if let Ok(vault_name) = config.resolve_vault_name(None).await {
                cache_manager.invalidate(&crate::cache::CacheKey::FileList {
                    vault_name: vault_name.clone(),
                    recursive: true,
                });
                cache_manager.invalidate(&crate::cache::CacheKey::FileList {
                    vault_name,
                    recursive: false,
                });
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

/// `xv info` when resource type is file/blob.
pub(crate) async fn execute_file_info_from_root(file_name: &str, config: &Config) -> Result<()> {
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
pub(crate) async fn refresh_file_list(vault_name: String, recursive: bool, config: Config) -> Result<()> {
    use crate::blob::models::{BlobListItem, FileListRequest};
    use crate::cache::{CacheKey, CacheManager};

    let blob_manager = create_blob_manager(&config)?;
    let list_request = FileListRequest {
        prefix: None,
        groups: None,
        limit: None,
        delimiter: if recursive {
            None
        } else {
            Some("/".to_string())
        },
        recursive,
    };

    let items: Vec<BlobListItem> = if recursive {
        let files = blob_manager.list_files(list_request).await?;
        files.into_iter().map(BlobListItem::File).collect()
    } else {
        blob_manager.list_files_hierarchical(list_request).await?
    };

    let cache_manager = CacheManager::from_config(&config);
    let cache_key = CacheKey::FileList {
        vault_name,
        recursive,
    };
    cache_manager.set(&cache_key, &items);

    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn execute_file_upload(
    blob_manager: &BlobManager,
    file_path: &str,
    name: Option<String>,
    groups: Vec<String>,
    metadata: Vec<(String, String)>,
    tags: Vec<(String, String)>,
    content_type: Option<String>,
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

    if content.is_empty() {
        output::warn(&format!(
            "File '{file_path}' is empty (0 bytes). Uploading anyway."
        ));
    }

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

    // Azure Blob Storage supports a maximum of 10 tags per blob
    if tags_map.len() > 10 {
        return Err(CrosstacheError::invalid_argument(format!(
            "Too many tags ({}) — Azure Blob Storage allows a maximum of 10 tags per blob. Remove {} tag(s).",
            tags_map.len(),
            tags_map.len() - 10
        )));
    }

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

    let file_info = blob_manager.upload_file(upload_request).await?;
    output::success(&format!("Successfully uploaded file '{}'", file_info.name));
    println!("   Size: {} bytes", file_info.size);
    println!("   Content-Type: {}", file_info.content_type);
    if !file_info.groups.is_empty() {
        println!("   Groups: {:?}", file_info.groups);
    }

    Ok(())
}

async fn execute_file_download(
    blob_manager: &BlobManager,
    name: &str,
    output: Option<String>,
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
        stream: false,
    };

    println!("Downloading file '{name}' to '{output_path}'...");

    let content = blob_manager.download_file(download_request).await?;
    fs::write(&output_path, content)
        .map_err(|e| CrosstacheError::config(format!("Failed to write file {output_path}: {e}")))?;
    output::success(&format!("Successfully downloaded file '{name}'"));

    Ok(())
}

fn display_file_list_items(
    items: &[crate::blob::models::BlobListItem],
    recursive: bool,
    config: &Config,
) -> Result<()> {
    use crate::blob::manager::format_size;
    use crate::blob::models::BlobListItem;
    use crate::utils::format::TableFormatter;
    use serde::Serialize;
    use tabled::Tabled;

    if items.is_empty() {
        output::info("No files found");
        return Ok(());
    }

    let fmt = config.runtime_output_format.resolve_for_stdout();

    // Rows for `--format csv`: machine-oriented fields (not `format_size()` / joined strings).
    #[derive(Tabled, Serialize)]
    struct FileListCsvRow {
        #[tabled(rename = "type")]
        kind: String,
        name: String,
        size: u64,
        #[tabled(rename = "content_type")]
        content_type: String,
        #[tabled(rename = "last_modified")]
        last_modified: String,
        etag: String,
        groups: String,
        #[tabled(rename = "full_path")]
        full_path: String,
        #[tabled(rename = "metadata")]
        metadata: String,
        #[tabled(rename = "tags")]
        tags: String,
    }

    #[derive(Tabled, Serialize)]
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

    match fmt {
        OutputFormat::Json => {
            let json_output = serde_json::to_string_pretty(items).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize items: {e}"))
            })?;
            println!("{json_output}");
        }
        OutputFormat::Yaml => {
            let yaml_output = serde_yaml::to_string(items).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize items: {e}"))
            })?;
            println!("{yaml_output}");
        }
        OutputFormat::Csv => {
            let csv_rows: Vec<FileListCsvRow> = items
                .iter()
                .map(|item| match item {
                    BlobListItem::Directory { name, full_path } => FileListCsvRow {
                        kind: "directory".to_string(),
                        name: name.clone(),
                        size: 0,
                        content_type: String::new(),
                        last_modified: String::new(),
                        etag: String::new(),
                        groups: "[]".to_string(),
                        full_path: full_path.clone(),
                        metadata: "{}".to_string(),
                        tags: "{}".to_string(),
                    },
                    BlobListItem::File(file) => FileListCsvRow {
                        kind: "file".to_string(),
                        name: file.name.clone(),
                        size: file.size,
                        content_type: file.content_type.clone(),
                        last_modified: file.last_modified.to_rfc3339(),
                        etag: file.etag.clone(),
                        groups: serde_json::to_string(&file.groups).unwrap_or_else(|_| "[]".into()),
                        full_path: String::new(),
                        metadata: serde_json::to_string(&file.metadata)
                            .unwrap_or_else(|_| "{}".into()),
                        tags: serde_json::to_string(&file.tags).unwrap_or_else(|_| "{}".into()),
                    },
                })
                .collect();
            let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
            println!("{}", formatter.format_table(&csv_rows)?);
        }
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw => {
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

            let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
            println!("{}", formatter.format_table(&display_items)?);

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
        OutputFormat::Template => {
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
            let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
            println!("{}", formatter.format_table(&display_items)?);
        }
        OutputFormat::Auto => unreachable!("resolve_for_stdout must not return Auto"),
    }

    Ok(())
}

async fn execute_file_list(
    blob_manager: &BlobManager,
    prefix: Option<String>,
    group: Option<String>,
    limit: Option<usize>,
    recursive: bool,
    no_cache: bool,
    config: &Config,
) -> Result<()> {
    use crate::blob::models::{BlobListItem, FileListRequest};
    use crate::cache::{CacheKey, CacheManager};

    let cache_manager = CacheManager::from_config(config);
    let vault_name = config.resolve_vault_name(None).await.unwrap_or_default();
    let cache_key = CacheKey::FileList {
        vault_name,
        recursive,
    };
    let use_cache = cache_manager.is_enabled() && !no_cache;

    let is_unfiltered = prefix.is_none() && group.is_none() && limit.is_none();

    if use_cache && is_unfiltered {
        if let Some(cached) = cache_manager.get::<Vec<BlobListItem>>(&cache_key) {
            return display_file_list_items(&cached, recursive, config);
        }
    }

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

    if use_cache && is_unfiltered {
        cache_manager.set(&cache_key, &items);
    }

    display_file_list_items(&items, recursive, config)
}

async fn execute_file_delete(
    blob_manager: &BlobManager,
    name: &str,
    force: bool,
    _config: &Config,
) -> Result<()> {
    // Confirmation unless forced
    if !force {
        use crate::utils::interactive::InteractivePrompt;
        let prompt = InteractivePrompt::new();
        if !prompt.confirm(
            &format!("Are you sure you want to delete file '{name}' from blob storage?"),
            false,
        )? {
            println!("Delete operation cancelled.");
            return Ok(());
        }
    }

    // Delete file
    println!("Deleting file '{name}'...");
    blob_manager.delete_file(name).await?;
    output::success(&format!("Successfully deleted file '{name}'"));
    output::hint("Blob soft-delete may allow recovery depending on storage account settings.");

    Ok(())
}

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

#[allow(clippy::too_many_arguments)]
async fn execute_file_upload_recursive(
    blob_manager: &BlobManager,
    paths: Vec<String>,
    group: Vec<String>,
    metadata: Vec<(String, String)>,
    tag: Vec<(String, String)>,
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
                output::error(&format!("Path not found: {path_str}"));
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
        output::info("No files found to upload");
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
                output::error(&error_msg);
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
            config,
        )
        .await;

        match result {
            Ok(_) => {
                success_count += 1;
            }
            Err(e) => {
                output::error(&format!("Failed to upload '{}': {}", local_path_str, e));
                failure_count += 1;
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }

    // Print summary
    println!();
    output::info("Upload Summary:");
    println!(
        "  {}",
        output::format_line(
            output::Level::Success,
            &format!("Successful: {success_count}"),
            output::should_use_rich_stdout()
        )
    );
    if failure_count > 0 {
        println!(
            "  {}",
            output::format_line(
                output::Level::Error,
                &format!("Failed: {failure_count}"),
                output::should_use_rich_stdout()
            )
        );
    }

    if failure_count > 0 && continue_on_error {
        return Err(CrosstacheError::azure_api(format!(
            "{failure_count} file(s) failed to upload"
        )));
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_file_upload_multiple(
    blob_manager: &BlobManager,
    files: Vec<String>,
    group: Vec<String>,
    metadata: Vec<(String, String)>,
    tag: Vec<(String, String)>,
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
            config,
        )
        .await
        {
            Ok(_) => {
                println!(
                    "  {}",
                    output::format_line(
                        output::Level::Success,
                        &file_path,
                        output::should_use_rich_stdout()
                    )
                );
                success_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "  {}",
                    output::format_line(
                        output::Level::Error,
                        &format!("{file_path}: {e}"),
                        output::should_use_rich_stderr(),
                    )
                );
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

async fn execute_file_download_multiple(
    blob_manager: &BlobManager,
    files: Vec<String>,
    output: Option<String>,
    force: bool,
    continue_on_error: bool,
    config: &Config,
) -> Result<()> {
    println!("Downloading {} file(s)...", files.len());

    let mut success_count = 0;
    let mut error_count = 0;

    for file_name in files {
        match execute_file_download(blob_manager, &file_name, output.clone(), force, config).await {
            Ok(_) => {
                println!(
                    "  {}",
                    output::format_line(
                        output::Level::Success,
                        &file_name,
                        output::should_use_rich_stdout()
                    )
                );
                success_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "  {}",
                    output::format_line(
                        output::Level::Error,
                        &format!("{file_name}: {e}"),
                        output::should_use_rich_stderr(),
                    )
                );
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
            eprintln!(
                "{}",
                output::format_line(
                    output::Level::Warn,
                    &format!("No files found matching prefix: {}", prefix),
                    output::should_use_rich_stderr(),
                )
            );
            continue;
        }

        all_files_to_download.extend(files);
    }

    if all_files_to_download.is_empty() {
        output::info("No files found to download");
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
                output::warn(&format!(
                    "Skipping '{}': path traversal detected in blob name",
                    blob_name
                ));
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
            output::warn(&format!(
                "File already exists: {} (use --force to overwrite)",
                local_path_str
            ));
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
            force,
            config,
        )
        .await;

        match result {
            Ok(_) => {
                success_count += 1;
            }
            Err(e) => {
                output::error(&format!("Failed to download '{}': {}", blob_name, e));
                failure_count += 1;
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }

    // Print summary
    println!();
    output::info("Download Summary:");
    println!(
        "  {}",
        output::format_line(
            output::Level::Success,
            &format!("Successful: {}", success_count),
            output::should_use_rich_stdout()
        )
    );
    if failure_count > 0 {
        println!(
            "  {}",
            output::format_line(
                output::Level::Error,
                &format!("Failed: {}", failure_count),
                output::should_use_rich_stdout()
            )
        );
    }

    if failure_count > 0 && continue_on_error {
        return Err(CrosstacheError::azure_api(format!(
            "{} file(s) failed to download",
            failure_count
        )));
    }

    Ok(())
}

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

        use crate::utils::interactive::InteractivePrompt;
        let prompt = InteractivePrompt::new();
        if !prompt.confirm("Are you sure you want to delete these files?", false)? {
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
                println!(
                    "  {}",
                    output::format_line(
                        output::Level::Success,
                        &file_name,
                        output::should_use_rich_stdout()
                    )
                );
                success_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "  {}",
                    output::format_line(
                        output::Level::Error,
                        &format!("{file_name}: {e}"),
                        output::should_use_rich_stderr(),
                    )
                );
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

fn sync_assert_safe_local_path(
    base: &std::path::Path,
    target: &std::path::Path,
    blob_name: &str,
) -> Result<()> {
    use std::path::Component;

    let canonical_base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let mut resolved = canonical_base.clone();
    for component in target.strip_prefix(base).unwrap_or(target).components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(c) => {
                resolved.push(c);
            }
            _ => {}
        }
    }
    if !resolved.starts_with(&canonical_base) {
        return Err(CrosstacheError::config(format!(
            "Path traversal detected in blob name: {blob_name}"
        )));
    }
    Ok(())
}

#[derive(Default, serde::Serialize)]
struct FileSyncSummary {
    uploaded: usize,
    downloaded: usize,
    deleted: usize,
    skipped: usize,
    dry_run: bool,
}

#[allow(clippy::too_many_arguments)]
async fn file_sync_delete_remote_not_local(
    blob_manager: &BlobManager,
    direction: &SyncDirection,
    prefix_ref: Option<&str>,
    remote_map: &std::collections::HashMap<String, crate::blob::models::FileInfo>,
    local_set: &std::collections::HashSet<String>,
    dry_run: bool,
    delete_requested: bool,
    quiet_stdout: bool,
    summary: &mut FileSyncSummary,
    mutated: &mut bool,
) -> Result<()> {
    use crate::blob::sync;

    if !delete_requested {
        return Ok(());
    }
    if matches!(direction, SyncDirection::Down) {
        output::warn(
            "`--delete` applies to remote files not present locally and is ignored for sync down; use sync up or both.",
        );
        return Ok(());
    }

    let scope = prefix_ref
        .map(|p| {
            let t = p.trim_matches('/');
            format!("{t}/")
        })
        .or_else(|| sync::common_directory_prefix(local_set))
        .filter(|s| !s.is_empty());

    let Some(scope_prefix) = scope else {
        output::warn(
            "`--delete` skipped: set `--prefix` or sync a directory tree with a shared path prefix (e.g. docs/...).",
        );
        return Ok(());
    };

    let mut to_delete: Vec<String> = remote_map
        .keys()
        .filter(|name| name.starts_with(&scope_prefix) && !local_set.contains(*name))
        .cloned()
        .collect();
    to_delete.sort();

    if to_delete.is_empty() {
        return Ok(());
    }

    if dry_run {
        if !quiet_stdout {
            for n in &to_delete {
                println!("delete (dry-run): {n}");
            }
        }
        summary.deleted += to_delete.len();
        return Ok(());
    }

    use crate::utils::interactive::InteractivePrompt;
    let prompt = InteractivePrompt::new();
    if !prompt.confirm(
        &format!(
            "Delete {} remote file(s) under '{}' that are not present locally?",
            to_delete.len(),
            scope_prefix.trim_end_matches('/')
        ),
        false,
    )? {
        output::info("Delete cancelled.");
        return Ok(());
    }

    for n in to_delete {
        if !quiet_stdout {
            println!("Deleting remote: {n}");
        }
        blob_manager.delete_file(&n).await?;
        summary.deleted += 1;
        *mutated = true;
    }
    Ok(())
}

/// Ensure parent directories exist for a sync download target.
fn file_sync_ensure_parent_dirs(target: &std::path::Path) -> Result<()> {
    use std::fs;
    if let Some(parent) = target.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| {
                CrosstacheError::config(format!(
                    "Failed to create directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
    }
    Ok(())
}

/// Read local file, upload blob, align local mtime to server `last_modified`.
async fn file_sync_perform_upload(
    blob_manager: &BlobManager,
    info: &FileUploadInfo,
    blob_name: &str,
    output_json: bool,
) -> Result<()> {
    use crate::blob::models::FileUploadRequest;
    use crate::blob::sync;
    use std::collections::HashMap;
    use std::fs;
    let content = fs::read(&info.local_path).map_err(|e| {
        CrosstacheError::config(format!("Failed to read {}: {e}", info.local_path.display()))
    })?;
    let upload_request = FileUploadRequest {
        name: blob_name.to_string(),
        content,
        content_type: None,
        groups: vec![],
        metadata: HashMap::new(),
        tags: HashMap::new(),
    };
    if !output_json {
        println!("upload: {} → {blob_name}", info.local_path.display());
    }
    let uploaded_info = blob_manager.upload_file(upload_request).await?;
    sync::set_file_mtime_utc(&info.local_path, uploaded_info.last_modified)?;
    Ok(())
}

/// Download blob to local path (with traversal check, parents, mtime).
async fn file_sync_perform_download(
    blob_manager: &BlobManager,
    base_path: &std::path::Path,
    prefix_ref: Option<&str>,
    blob_name: &str,
    remote_info: &crate::blob::models::FileInfo,
    output_json: bool,
) -> Result<()> {
    use crate::blob::models::FileDownloadRequest;
    use crate::blob::sync;
    use std::fs;
    let target = sync::local_path_from_blob(base_path, prefix_ref, blob_name);
    sync_assert_safe_local_path(base_path, &target, blob_name)?;
    file_sync_ensure_parent_dirs(&target)?;
    if !output_json {
        println!("download: {blob_name} → {}", target.display());
    }
    let download_request = FileDownloadRequest {
        name: blob_name.to_string(),
        output_path: Some(target.display().to_string()),
        stream: false,
    };
    let content = blob_manager.download_file(download_request).await?;
    fs::write(&target, content).map_err(|e| {
        CrosstacheError::config(format!("Failed to write {}: {e}", target.display()))
    })?;
    sync::set_file_mtime_utc(&target, remote_info.last_modified)?;
    Ok(())
}

async fn execute_file_sync(
    blob_manager: &BlobManager,
    local_path: &str,
    prefix: Option<String>,
    direction: &SyncDirection,
    dry_run: bool,
    delete: bool,
    config: &Config,
) -> Result<()> {
    use crate::blob::models::FileListRequest;
    use crate::blob::sync::{self, BothAction};
    use chrono::{DateTime, Utc};
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::Path;

    let path = Path::new(local_path);
    if !path.exists() {
        return Err(CrosstacheError::config(format!(
            "Path not found: {local_path}"
        )));
    }

    if delete && matches!(direction, SyncDirection::Down) {
        output::warn(
            "`--delete` applies to remote files not present locally and is ignored for sync down; use sync up or both.",
        );
    }

    let base_path = path.parent().unwrap_or(path);
    let prefix_ref = prefix.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let local_files = collect_files_with_structure(path, base_path, prefix_ref, false)?;

    if local_files.is_empty() && !config.output_json {
        output::info("No local files found to sync");
    }

    let mut local_by_blob: HashMap<String, FileUploadInfo> = HashMap::new();
    let mut local_meta: HashMap<String, (u64, DateTime<Utc>)> = HashMap::new();

    for info in &local_files {
        let meta = fs::metadata(&info.local_path).map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to read metadata for {}: {e}",
                info.local_path.display()
            ))
        })?;
        let size = meta.len();
        let mtime_utc: DateTime<Utc> = meta
            .modified()
            .map_err(|e| {
                CrosstacheError::config(format!(
                    "Failed to read mtime for {}: {e}",
                    info.local_path.display()
                ))
            })
            .map(Into::into)?;
        local_by_blob.insert(info.blob_name.clone(), info.clone());
        local_meta.insert(info.blob_name.clone(), (size, mtime_utc));
    }

    let list_prefix = prefix_ref.map(|p| p.to_string());

    let list_request = FileListRequest {
        prefix: list_prefix.clone(),
        groups: None,
        limit: None,
        delimiter: None,
        recursive: true,
    };
    let remote_list = blob_manager.list_files(list_request).await?;
    let mut remote_by_name: HashMap<String, crate::blob::models::FileInfo> = HashMap::new();
    for f in remote_list {
        remote_by_name.insert(f.name.clone(), f);
    }

    let local_names: HashSet<String> = local_by_blob.keys().cloned().collect();

    let mut summary = FileSyncSummary {
        dry_run,
        ..Default::default()
    };
    let mut mutated = false;

    match direction {
        SyncDirection::Up => {
            let mut sorted_names: Vec<String> = local_names.iter().cloned().collect();
            sorted_names.sort();
            for blob_name in sorted_names {
                let info = local_by_blob.get(&blob_name).unwrap();
                let (size, mtime) = *local_meta.get(&blob_name).unwrap();
                let need = match remote_by_name.get(&blob_name) {
                    None => true,
                    Some(r) => !sync::should_skip_sync_up(size, mtime, r),
                };
                if !need {
                    summary.skipped += 1;
                    if !config.output_json {
                        println!("skip (up to date): {blob_name}");
                    }
                    continue;
                }
                if dry_run {
                    if !config.output_json {
                        println!(
                            "upload (dry-run): {} → {blob_name}",
                            info.local_path.display()
                        );
                    }
                    summary.uploaded += 1;
                    continue;
                }
                file_sync_perform_upload(blob_manager, info, &blob_name, config.output_json)
                    .await?;
                summary.uploaded += 1;
                mutated = true;
            }
            file_sync_delete_remote_not_local(
                blob_manager,
                direction,
                prefix_ref,
                &remote_by_name,
                &local_names,
                dry_run,
                delete,
                config.output_json,
                &mut summary,
                &mut mutated,
            )
            .await?;
        }
        SyncDirection::Down => {
            let mut remote_names: Vec<String> = remote_by_name.keys().cloned().collect();
            remote_names.sort();
            for blob_name in remote_names {
                let remote_info = remote_by_name.get(&blob_name).unwrap();
                let target = sync::local_path_from_blob(base_path, prefix_ref, &blob_name);
                sync_assert_safe_local_path(base_path, &target, &blob_name)?;

                let need = if !target.exists() {
                    true
                } else {
                    let meta = fs::metadata(&target).map_err(|e| {
                        CrosstacheError::config(format!(
                            "Failed to read metadata for {}: {e}",
                            target.display()
                        ))
                    })?;
                    let size = meta.len();
                    let mtime_utc: DateTime<Utc> = meta
                        .modified()
                        .map_err(|e| {
                            CrosstacheError::config(format!(
                                "Failed to read mtime for {}: {e}",
                                target.display()
                            ))
                        })
                        .map(Into::into)?;
                    !sync::is_unchanged(size, mtime_utc, remote_info)
                };

                if !need {
                    if !config.output_json {
                        println!("skip (up to date): {blob_name}");
                    }
                    summary.skipped += 1;
                    continue;
                }

                if dry_run {
                    if !config.output_json {
                        println!("download (dry-run): {blob_name} → {}", target.display());
                    }
                    summary.downloaded += 1;
                    continue;
                }

                file_sync_perform_download(
                    blob_manager,
                    base_path,
                    prefix_ref,
                    &blob_name,
                    remote_info,
                    config.output_json,
                )
                .await?;
                summary.downloaded += 1;
                mutated = true;
            }
        }
        SyncDirection::Both => {
            let remote_keys: HashSet<String> = remote_by_name.keys().cloned().collect();
            let all_names: HashSet<String> = local_names.union(&remote_keys).cloned().collect();
            let mut ordered: Vec<String> = all_names.into_iter().collect();
            ordered.sort();

            for blob_name in ordered {
                let local_present = local_meta.contains_key(&blob_name);
                let remote_present = remote_by_name.contains_key(&blob_name);

                match (local_present, remote_present) {
                    (true, false) => {
                        let info = local_by_blob.get(&blob_name).unwrap();
                        if dry_run {
                            if !config.output_json {
                                println!(
                                    "upload (dry-run): {} → {blob_name}",
                                    info.local_path.display()
                                );
                            }
                            summary.uploaded += 1;
                            continue;
                        }
                        file_sync_perform_upload(
                            blob_manager,
                            info,
                            &blob_name,
                            config.output_json,
                        )
                        .await?;
                        summary.uploaded += 1;
                        mutated = true;
                    }
                    (false, true) => {
                        let remote_info = remote_by_name.get(&blob_name).unwrap();
                        let target = sync::local_path_from_blob(base_path, prefix_ref, &blob_name);
                        sync_assert_safe_local_path(base_path, &target, &blob_name)?;
                        if dry_run {
                            if !config.output_json {
                                println!("download (dry-run): {blob_name} → {}", target.display());
                            }
                            summary.downloaded += 1;
                            continue;
                        }
                        file_sync_perform_download(
                            blob_manager,
                            base_path,
                            prefix_ref,
                            &blob_name,
                            remote_info,
                            config.output_json,
                        )
                        .await?;
                        summary.downloaded += 1;
                        mutated = true;
                    }
                    (true, true) => {
                        let info = local_by_blob.get(&blob_name).unwrap();
                        let (size, mtime) = *local_meta.get(&blob_name).unwrap();
                        let remote_info = remote_by_name.get(&blob_name).unwrap();
                        match sync::resolve_both(size, mtime, remote_info) {
                            BothAction::Skip => {
                                if !config.output_json {
                                    println!("skip: {blob_name}");
                                }
                                summary.skipped += 1;
                            }
                            BothAction::Upload => {
                                if dry_run {
                                    if !config.output_json {
                                        println!(
                                            "upload (dry-run): {} → {blob_name}",
                                            info.local_path.display()
                                        );
                                    }
                                    summary.uploaded += 1;
                                    continue;
                                }
                                file_sync_perform_upload(
                                    blob_manager,
                                    info,
                                    &blob_name,
                                    config.output_json,
                                )
                                .await?;
                                summary.uploaded += 1;
                                mutated = true;
                            }
                            BothAction::Download => {
                                let target =
                                    sync::local_path_from_blob(base_path, prefix_ref, &blob_name);
                                sync_assert_safe_local_path(base_path, &target, &blob_name)?;
                                if dry_run {
                                    if !config.output_json {
                                        println!(
                                            "download (dry-run): {blob_name} → {}",
                                            target.display()
                                        );
                                    }
                                    summary.downloaded += 1;
                                    continue;
                                }
                                file_sync_perform_download(
                                    blob_manager,
                                    base_path,
                                    prefix_ref,
                                    &blob_name,
                                    remote_info,
                                    config.output_json,
                                )
                                .await?;
                                summary.downloaded += 1;
                                mutated = true;
                            }
                        }
                    }
                    (false, false) => {}
                }
            }

            if delete {
                let local_names_after: HashSet<String> = {
                    let path = Path::new(local_path);
                    let rescanned =
                        collect_files_with_structure(path, base_path, prefix_ref, false)?;
                    rescanned.into_iter().map(|i| i.blob_name).collect()
                };

                let remote_after = blob_manager
                    .list_files(FileListRequest {
                        prefix: list_prefix.clone(),
                        groups: None,
                        limit: None,
                        delimiter: None,
                        recursive: true,
                    })
                    .await?;
                let mut remote_map_after: HashMap<String, crate::blob::models::FileInfo> =
                    HashMap::new();
                for f in remote_after {
                    remote_map_after.insert(f.name.clone(), f);
                }

                file_sync_delete_remote_not_local(
                    blob_manager,
                    direction,
                    prefix_ref,
                    &remote_map_after,
                    &local_names_after,
                    dry_run,
                    delete,
                    config.output_json,
                    &mut summary,
                    &mut mutated,
                )
                .await?;
            }
        }
    }

    if mutated && !dry_run {
        let cache_manager = crate::cache::CacheManager::from_config(config);
        if let Ok(vault_name) = config.resolve_vault_name(None).await {
            cache_manager.invalidate(&crate::cache::CacheKey::FileList {
                vault_name: vault_name.clone(),
                recursive: true,
            });
            cache_manager.invalidate(&crate::cache::CacheKey::FileList {
                vault_name,
                recursive: false,
            });
        }
    }

    if config.output_json {
        let json_output = serde_json::to_string_pretty(&summary).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize sync summary: {e}"))
        })?;
        println!("{json_output}");
    } else {
        println!();
        output::info("Sync summary:");
        println!(
            "  {}",
            output::format_line(
                output::Level::Info,
                &format!("Uploaded: {}", summary.uploaded),
                output::should_use_rich_stdout()
            )
        );
        println!(
            "  {}",
            output::format_line(
                output::Level::Info,
                &format!("Downloaded: {}", summary.downloaded),
                output::should_use_rich_stdout()
            )
        );
        println!(
            "  {}",
            output::format_line(
                output::Level::Info,
                &format!("Deleted (remote): {}", summary.deleted),
                output::should_use_rich_stdout()
            )
        );
        println!(
            "  {}",
            output::format_line(
                output::Level::Info,
                &format!("Skipped: {}", summary.skipped),
                output::should_use_rich_stdout()
            )
        );
        if dry_run {
            output::hint("Dry run: no changes were applied.");
        }
    }

    Ok(())
}
/// Quick file upload command (alias for file upload)
pub(crate) async fn execute_file_upload_quick(
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
        config,
    )
    .await
}

/// Quick file download command (alias for file download)
pub(crate) async fn execute_file_download_quick(
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
        false, // force
        config,
    )
    .await?;

    // Handle --open flag: open the downloaded file with the system's default application
    if open {
        let final_output_path = output_path.unwrap_or_else(|| name.to_string());
        match std::fs::canonicalize(&final_output_path) {
            Ok(path) => {
                if let Err(e) = opener::open(&path) {
                    eprintln!("Warning: could not open file '{}': {}", path.display(), e);
                }
            }
            Err(e) => {
                eprintln!("Warning: could not resolve path '{}': {}", final_output_path, e);
            }
        }
    }

    Ok(())
}
