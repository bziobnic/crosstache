//! `xv file` execution against the AWS backend (S3-backed file storage).
//!
//! Mirrors the Azure flow in [`crate::cli::file_ops`] but routes through
//! [`AwsFileBackend`]: files live under `<vault>/files/<name>` in the
//! configured bucket, downloads stream to disk behind the 5 GiB guard, and
//! uploads stream from disk (multipart above the part-size threshold).

use std::collections::HashMap;
use std::path::Path;

use crate::backend::aws::files::{self, AwsFileBackend, FileUploadSpec};
use crate::backend::file::FileBackend;
use crate::blob::models::{BlobListItem, FileInfo, FileListRequest};
use crate::cli::file::FileCommands;
use crate::cli::file_ops::{
    collect_files_with_structure, display_file_info, display_file_list_items, is_tty,
    progress_threshold_bytes, resolve_multi_download_dir, resolve_single_download_path,
};
use crate::config::settings::AwsConfig;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::helpers::safe_join;
use crate::utils::output;
use crate::utils::progress::{self, MultiProgressContext, NoopReporter, ProgressReporter};

// ---------------------------------------------------------------------------
// Backend detection & construction
// ---------------------------------------------------------------------------

/// True when the active backend (top-level or named) is AWS.
pub(crate) fn is_aws_backend_active(config: &Config) -> bool {
    use crate::backend::BackendKind;
    use crate::config::settings::NamedBackendEntry;

    let name = config.effective_backend_name();
    if let Some(entry) = config.named_backends.get(name) {
        return matches!(entry, NamedBackendEntry::Aws(_));
    }
    matches!(name.parse::<BackendKind>(), Ok(BackendKind::Aws))
}

/// The `AwsConfig` for the active backend (named entry first, then `[aws]`).
fn aws_config_for(config: &Config) -> Result<&AwsConfig> {
    use crate::config::settings::NamedBackendEntry;

    let name = config.effective_backend_name();
    if let Some(NamedBackendEntry::Aws(cfg)) = config.named_backends.get(name) {
        return Ok(cfg);
    }
    config.aws.as_ref().ok_or_else(|| {
        CrosstacheError::config("[aws] config block is required when backend = \"aws\"")
    })
}

/// Build an [`AwsFileBackend`] from config: resolves the bucket (clear setup
/// hint when unset), loads SDK credentials, and applies the blob transfer
/// settings (`chunk_size_mb`, `max_concurrent_uploads`).
async fn create_aws_file_backend(config: &Config) -> Result<AwsFileBackend> {
    let aws_cfg = aws_config_for(config)?;
    let bucket = files::resolve_bucket(aws_cfg)?;
    let sdk_config = crate::backend::aws::auth::load_sdk_config(aws_cfg, None, None).await?;
    let client = crate::backend::aws::auth::build_s3_client(aws_cfg, &sdk_config);
    let blob_config = config.get_blob_config();
    Ok(AwsFileBackend::new(client, bucket).with_transfer_config(
        blob_config.chunk_size_mb,
        blob_config.max_concurrent_uploads,
    ))
}

/// Resolve the vault whose file prefix is targeted: the usual chain
/// (context, default_vault), then `[aws].default_vault`.
async fn resolve_aws_file_vault(config: &Config) -> Result<String> {
    // Phase 3 (file-ops routing): file ops route through the workspace default entry
    if let Ok(vault) = config.resolve_vault_name(None).await {
        return Ok(vault);
    }
    if let Ok(aws_cfg) = aws_config_for(config) {
        if let Some(vault) = aws_cfg.default_vault.as_deref() {
            if !vault.trim().is_empty() {
                return Ok(vault.trim().to_string());
            }
        }
    }
    Err(CrosstacheError::config(
        "No vault specified for file operations. Set a context with 'xv context use', \
         configure default_vault, or set [aws].default_vault.",
    ))
}

/// Drop the cached file listings for a vault after a mutation.
fn invalidate_file_cache(config: &Config, vault: &str) {
    let cache_manager = crate::cache::CacheManager::from_config(config);
    for recursive in [true, false] {
        cache_manager.invalidate(&crate::cache::CacheKey::FileList {
            vault_name: vault.to_string(),
            recursive,
        });
    }
}

// ---------------------------------------------------------------------------
// Command dispatch
// ---------------------------------------------------------------------------

pub(crate) async fn execute_file_command_aws(command: FileCommands, config: Config) -> Result<()> {
    let backend = create_aws_file_backend(&config).await?;
    let vault = resolve_aws_file_vault(&config).await?;

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
            if recursive {
                if name.is_some() || content_type.is_some() {
                    return Err(CrosstacheError::invalid_argument(
                        "--name and --content-type cannot be used with --recursive",
                    ));
                }
                execute_upload_recursive(
                    &backend,
                    &vault,
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
                execute_upload_single(
                    &backend,
                    &vault,
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
                if name.is_some() || content_type.is_some() {
                    return Err(CrosstacheError::invalid_argument(
                        "--name and --content-type can only be used when uploading a single file",
                    ));
                }
                execute_upload_multiple(
                    &backend,
                    &vault,
                    files,
                    group,
                    metadata,
                    tag,
                    continue_on_error,
                    &config,
                )
                .await?;
            }
            invalidate_file_cache(&config, &vault);
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
            if recursive {
                if rename.is_some() {
                    return Err(CrosstacheError::invalid_argument(
                        "--rename cannot be used with --recursive",
                    ));
                }
                execute_download_recursive(
                    &backend,
                    &vault,
                    files,
                    output,
                    force,
                    flatten,
                    continue_on_error,
                    &config,
                )
                .await?;
            } else {
                if rename.is_some() && files.len() > 1 {
                    return Err(CrosstacheError::invalid_argument(
                        "--rename can only be used when downloading a single file",
                    ));
                }
                if files.len() == 1 {
                    let output_path = if let Some(new_name) = rename {
                        Some(new_name)
                    } else {
                        output
                    };
                    let resolved = resolve_single_download_path(&files[0], output_path.as_deref())?;
                    execute_download_to_path(&backend, &vault, &files[0], resolved, force, &config)
                        .await?;
                    output::success(&format!("Successfully downloaded file '{}'", files[0]));
                } else {
                    execute_download_multiple(
                        &backend,
                        &vault,
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
            page,
            page_size,
            pager,
            recursive,
            names_only,
            no_cache,
        } => {
            use crate::utils::pagination::Pagination;

            let pager = pager
                .map(crate::cli::commands::PagerWhen::wants_pager)
                .unwrap_or(false);

            if limit.is_some() && page_size.is_some() {
                return Err(CrosstacheError::invalid_argument(
                    "--limit cannot be combined with --page-size; use --page-size instead",
                ));
            }
            if limit.is_some() && page.is_some() {
                return Err(CrosstacheError::invalid_argument(
                    "--limit shows the first page only and cannot be combined with --page",
                ));
            }
            let pagination = if limit.is_some() {
                Pagination::first_page_with_size(limit)?
            } else {
                Pagination::from_args(page, page_size)?
            };

            execute_list(
                &backend, &vault, prefix, group, pagination, pager, recursive, names_only,
                no_cache, &config,
            )
            .await?;
        }
        FileCommands::Delete {
            files,
            force,
            continue_on_error,
        } => {
            execute_delete(&backend, &vault, files, force, continue_on_error).await?;
            invalidate_file_cache(&config, &vault);
        }
        FileCommands::Info { name } => {
            let file_info = backend.get_file_info(&vault, &name).await?;
            display_file_info(&file_info, &config)?;
        }
        FileCommands::Sync { .. } => {
            return Err(CrosstacheError::invalid_argument(
                "`xv file sync` is not yet supported on the AWS backend. \
                 upload/download/list/delete/info are available; use \
                 `xv file upload --recursive` / `xv file download --recursive` meanwhile.",
            ));
        }
    }

    Ok(())
}

/// `xv info <file>` when the resource is a file.
pub(crate) async fn execute_file_info_aws(file_name: &str, config: &Config) -> Result<()> {
    let backend = create_aws_file_backend(config).await?;
    let vault = resolve_aws_file_vault(config).await?;
    let file_info = backend.get_file_info(&vault, file_name).await?;
    display_file_info(&file_info, config)
}

/// Background cache refresh for file listings.
pub(crate) async fn refresh_file_list_aws(
    vault_name: String,
    recursive: bool,
    config: Config,
) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};

    let backend = create_aws_file_backend(&config).await?;
    let list_request = FileListRequest {
        prefix: None,
        groups: None,
        limit: None,
        delimiter: if recursive {
            None
        } else {
            Some("/".to_string())
        },
    };

    let items: Vec<BlobListItem> = if recursive {
        let files = backend.list_files(&vault_name, list_request).await?;
        files.into_iter().map(BlobListItem::File).collect()
    } else {
        backend
            .list_files_hierarchical(&vault_name, list_request)
            .await?
    };

    let cache_manager = CacheManager::from_config(&config);
    cache_manager.set(
        &CacheKey::FileList {
            vault_name,
            recursive,
        },
        &items,
    );
    Ok(())
}

/// Quick upload (`xv up`).
pub(crate) async fn execute_file_upload_quick_aws(
    file_path: &str,
    name: Option<String>,
    groups: Option<String>,
    metadata: Vec<String>,
    config: &Config,
) -> Result<()> {
    let backend = create_aws_file_backend(config).await?;
    let vault = resolve_aws_file_vault(config).await?;

    let groups_vec: Vec<String> = groups
        .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let metadata_pairs: Vec<(String, String)> = metadata
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

    execute_upload_single(
        &backend,
        &vault,
        file_path,
        name,
        groups_vec,
        metadata_pairs,
        Vec::new(),
        None,
        config,
    )
    .await?;
    invalidate_file_cache(config, &vault);
    Ok(())
}

/// Quick download (`xv down`).
pub(crate) async fn execute_file_download_quick_aws(
    name: &str,
    output: Option<String>,
    open: bool,
    config: &Config,
) -> Result<()> {
    let backend = create_aws_file_backend(config).await?;
    let vault = resolve_aws_file_vault(config).await?;

    let final_output_path = resolve_single_download_path(name, output.as_deref())?;
    execute_download_to_path(
        &backend,
        &vault,
        name,
        final_output_path.clone(),
        false,
        config,
    )
    .await?;
    output::success(&format!("Successfully downloaded file '{name}'"));

    if open {
        match std::fs::canonicalize(&final_output_path) {
            Ok(path) => {
                if let Err(e) = opener::open(&path) {
                    eprintln!("Warning: could not open file '{}': {}", path.display(), e);
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: could not resolve path '{}': {}",
                    final_output_path, e
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

/// Stream one local file to S3 (multipart above the part-size threshold).
#[allow(clippy::too_many_arguments)]
async fn upload_local_file(
    backend: &AwsFileBackend,
    vault: &str,
    local_path: &Path,
    remote_name: &str,
    content_type: Option<String>,
    groups: Vec<String>,
    metadata: HashMap<String, String>,
    tags: HashMap<String, String>,
    reporter: &dyn ProgressReporter,
) -> Result<FileInfo> {
    let meta = tokio::fs::metadata(local_path).await.map_err(|e| {
        CrosstacheError::config(format!("Failed to read file {}: {e}", local_path.display()))
    })?;
    let mut file = tokio::fs::File::open(local_path).await.map_err(|e| {
        CrosstacheError::config(format!("Failed to read file {}: {e}", local_path.display()))
    })?;
    let spec = FileUploadSpec {
        name: remote_name.to_string(),
        content_type,
        groups,
        metadata,
        tags,
    };
    let info = backend
        .upload_file_streaming(vault, spec, &mut file, meta.len(), reporter)
        .await?;
    Ok(info)
}

#[allow(clippy::too_many_arguments)]
async fn execute_upload_single(
    backend: &AwsFileBackend,
    vault: &str,
    file_path: &str,
    name: Option<String>,
    groups: Vec<String>,
    metadata: Vec<(String, String)>,
    tags: Vec<(String, String)>,
    content_type: Option<String>,
    config: &Config,
) -> Result<()> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(CrosstacheError::config(format!(
            "File not found: {file_path}"
        )));
    }

    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if file_size == 0 {
        output::warn(&format!(
            "File '{file_path}' is empty (0 bytes). Uploading anyway."
        ));
    }

    let remote_name = name.unwrap_or_else(|| {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });

    let metadata_map: HashMap<String, String> = metadata.into_iter().collect();
    let tags_map: HashMap<String, String> = tags.into_iter().collect();

    let threshold = progress_threshold_bytes(config);
    let tty = is_tty();
    if !tty {
        println!("Uploading file '{file_path}' as '{remote_name}'...");
    }
    let reporter = progress::create_file_reporter(file_size, threshold, tty);
    reporter.set_message(format!("Uploading '{remote_name}'..."));

    let file_info = upload_local_file(
        backend,
        vault,
        path,
        &remote_name,
        content_type,
        groups,
        metadata_map,
        tags_map,
        reporter.as_ref(),
    )
    .await?;

    output::success(&format!("Successfully uploaded file '{}'", file_info.name));
    println!("   Size: {} bytes", file_info.size);
    println!("   Content-Type: {}", file_info.content_type);
    if !file_info.groups.is_empty() {
        println!("   Groups: {:?}", file_info.groups);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_upload_multiple(
    backend: &AwsFileBackend,
    vault: &str,
    files: Vec<String>,
    groups: Vec<String>,
    metadata: Vec<(String, String)>,
    tags: Vec<(String, String)>,
    continue_on_error: bool,
    config: &Config,
) -> Result<()> {
    println!("Uploading {} file(s)...", files.len());

    let mut success_count = 0;
    let mut error_count = 0;

    for file_path in files {
        match execute_upload_single(
            backend,
            vault,
            &file_path,
            None,
            groups.clone(),
            metadata.clone(),
            tags.clone(),
            None,
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
        return Err(CrosstacheError::unknown(format!(
            "{error_count} file(s) failed to upload"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_upload_recursive(
    backend: &AwsFileBackend,
    vault: &str,
    paths: Vec<String>,
    groups: Vec<String>,
    metadata: Vec<(String, String)>,
    tags: Vec<(String, String)>,
    continue_on_error: bool,
    flatten: bool,
    prefix: Option<String>,
    config: &Config,
) -> Result<()> {
    let mut all_files = Vec::new();
    for path_str in &paths {
        let path = Path::new(path_str);
        if !path.exists() {
            if continue_on_error {
                output::error(&format!("Path not found: {path_str}"));
                continue;
            }
            return Err(CrosstacheError::config(format!(
                "Path not found: {path_str}"
            )));
        }
        // Use the parent directory as base path so the top-level folder name
        // is preserved in remote paths (e.g. docs/api/users.md).
        let base_path = path.parent().unwrap_or(path);
        all_files.extend(collect_files_with_structure(
            path,
            base_path,
            prefix.as_deref(),
            flatten,
        )?);
    }

    if all_files.is_empty() {
        output::info("No files found to upload");
        return Ok(());
    }

    println!("Found {} file(s) to upload", all_files.len());

    let mut success_count = 0;
    let mut failure_count = 0;
    let threshold = progress_threshold_bytes(config);
    let tty = is_tty();
    let mp = MultiProgressContext::new(all_files.len() as u64, threshold, tty);

    let metadata_map: HashMap<String, String> = metadata.into_iter().collect();
    let tags_map: HashMap<String, String> = tags.into_iter().collect();

    for file_info in &all_files {
        let local_path_str = file_info.local_path.to_string_lossy();
        if !tty {
            if !flatten {
                println!("Uploading: {} → {}", local_path_str, file_info.blob_name);
            } else {
                println!("Uploading: {}", local_path_str);
            }
        }

        let result = upload_local_file(
            backend,
            vault,
            &file_info.local_path,
            &file_info.blob_name,
            None,
            groups.clone(),
            metadata_map.clone(),
            tags_map.clone(),
            &NoopReporter,
        )
        .await;

        match result {
            Ok(_) => {
                success_count += 1;
                mp.log(&format!("Uploaded: {}", file_info.blob_name));
                mp.advance_overall(&file_info.blob_name);
            }
            Err(e) => {
                output::error(&format!("Failed to upload '{}': {}", local_path_str, e));
                failure_count += 1;
                mp.advance_overall(&file_info.blob_name);
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }
    mp.finish();

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
        if continue_on_error {
            return Err(CrosstacheError::unknown(format!(
                "{failure_count} file(s) failed to upload"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

/// Stream one file to `output_path`. Downloads into a temporary sibling and
/// renames on success so a failed transfer never leaves a partial file.
async fn execute_download_to_path(
    backend: &AwsFileBackend,
    vault: &str,
    name: &str,
    output_path: String,
    force: bool,
    config: &Config,
) -> Result<()> {
    if Path::new(&output_path).exists() && !force {
        return Err(CrosstacheError::config(format!(
            "File '{output_path}' already exists. Use --force to overwrite."
        )));
    }

    // Ensure parent directories exist so names with path segments
    // (e.g. "docs/readme.md") succeed.
    if let Some(parent) = Path::new(&output_path).parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CrosstacheError::config(format!(
                    "Failed to create parent directories for {output_path}: {e}"
                ))
            })?;
        }
    }

    let threshold = progress_threshold_bytes(config);
    let tty = is_tty();
    if !tty {
        println!("Downloading file '{name}' to '{output_path}'...");
    }
    let file_size = if tty {
        backend
            .get_file_info(vault, name)
            .await
            .map(|info| info.size)
            .unwrap_or(0)
    } else {
        0
    };
    let reporter = progress::create_file_reporter(file_size, threshold, tty);
    reporter.set_message(format!("Downloading '{name}'..."));

    let tmp_path = format!("{output_path}.xv-partial");
    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(|e| CrosstacheError::config(format!("Failed to write file {tmp_path}: {e}")))?;

    match backend
        .download_file_to_writer(vault, name, &mut file, reporter.as_ref())
        .await
    {
        Ok(_) => {
            drop(file);
            tokio::fs::rename(&tmp_path, &output_path)
                .await
                .map_err(|e| {
                    CrosstacheError::config(format!("Failed to write file {output_path}: {e}"))
                })?;
            Ok(())
        }
        Err(e) => {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp_path).await;
            Err(e.into())
        }
    }
}

async fn execute_download_multiple(
    backend: &AwsFileBackend,
    vault: &str,
    files: Vec<String>,
    output: Option<String>,
    force: bool,
    continue_on_error: bool,
    config: &Config,
) -> Result<()> {
    let output_dir = resolve_multi_download_dir(output.as_deref())?;

    println!("Downloading {} file(s)...", files.len());

    let mut success_count = 0;
    let mut error_count = 0;

    for file_name in files {
        // Per-file output path with traversal guard.
        let result = match safe_join(&output_dir, &file_name) {
            Ok(p) => {
                execute_download_to_path(
                    backend,
                    vault,
                    &file_name,
                    p.to_string_lossy().into_owned(),
                    force,
                    config,
                )
                .await
            }
            Err(e) => Err(e),
        };
        match result {
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
        return Err(CrosstacheError::unknown(format!(
            "{error_count} file(s) failed to download"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_download_recursive(
    backend: &AwsFileBackend,
    vault: &str,
    prefixes: Vec<String>,
    output: Option<String>,
    force: bool,
    flatten: bool,
    continue_on_error: bool,
    config: &Config,
) -> Result<()> {
    let output_dir = output.unwrap_or_else(|| ".".to_string());
    let output_path = Path::new(&output_dir);
    if !output_path.exists() {
        std::fs::create_dir_all(output_path).map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to create output directory {}: {}",
                output_dir, e
            ))
        })?;
    }
    eprintln!(
        "Downloading to: {}",
        output_path
            .canonicalize()
            .unwrap_or_else(|_| output_path.to_path_buf())
            .display()
    );

    let mut all_files: Vec<FileInfo> = Vec::new();
    for prefix in &prefixes {
        let files = backend
            .list_files(
                vault,
                FileListRequest {
                    prefix: Some(prefix.clone()),
                    groups: None,
                    limit: None,
                    delimiter: None,
                },
            )
            .await?;
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
        all_files.extend(files);
    }

    if all_files.is_empty() {
        output::info("No files found to download");
        return Ok(());
    }

    println!("Found {} file(s) to download", all_files.len());

    let mut success_count = 0;
    let mut failure_count = 0;
    let threshold = progress_threshold_bytes(config);
    let tty = is_tty();
    let mp = MultiProgressContext::new(all_files.len() as u64, threshold, tty);

    for file_info in &all_files {
        let remote_name = &file_info.name;

        // Security: remote names are untrusted; `safe_join` rejects `..`
        // components and absolute paths before any filesystem call.
        let joined = if flatten {
            let filename = Path::new(remote_name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            safe_join(output_path, &filename)
        } else {
            safe_join(output_path, remote_name)
        };
        let local_path = match joined {
            Ok(path) => path,
            Err(e) => {
                output::warn(&format!("Skipping '{remote_name}': {e}"));
                failure_count += 1;
                if continue_on_error {
                    mp.advance_overall(remote_name);
                    continue;
                }
                return Err(CrosstacheError::config(format!(
                    "Unsafe file name '{remote_name}': {e}"
                )));
            }
        };
        let local_path_str = local_path.to_string_lossy().to_string();

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

        if !tty {
            if !flatten {
                println!("Downloading: {} → {}", remote_name, local_path_str);
            } else {
                println!("Downloading: {}", remote_name);
            }
        }

        // Force is pre-checked above; pass true to skip the redundant check.
        let result = execute_download_to_path(
            backend,
            vault,
            remote_name,
            local_path_str.clone(),
            true,
            config,
        )
        .await;

        match result {
            Ok(_) => {
                success_count += 1;
                mp.log(&format!("Downloaded: {}", remote_name));
                mp.advance_overall(remote_name);
            }
            Err(e) => {
                output::error(&format!("Failed to download '{}': {}", remote_name, e));
                failure_count += 1;
                mp.advance_overall(remote_name);
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }
    mp.finish();

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
        if continue_on_error {
            return Err(CrosstacheError::unknown(format!(
                "{} file(s) failed to download",
                failure_count
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn execute_list(
    backend: &AwsFileBackend,
    vault: &str,
    prefix: Option<String>,
    group: Option<String>,
    pagination: crate::utils::pagination::Pagination,
    pager: bool,
    recursive: bool,
    names_only: bool,
    no_cache: bool,
    config: &Config,
) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};
    use crate::utils::pagination::{paginate_slice, pagination_footer_text};

    // `--names-only` always needs the full recursive item set (no directory entries).
    let recursive = recursive || names_only;

    let cache_manager = CacheManager::from_config(config);
    let cache_key = CacheKey::FileList {
        vault_name: vault.to_string(),
        recursive,
    };
    let use_cache = cache_manager.is_enabled() && !no_cache;
    let is_unfiltered = prefix.is_none() && group.is_none();

    let cached: Option<Vec<BlobListItem>> = if use_cache && is_unfiltered {
        cache_manager.get::<Vec<BlobListItem>>(&cache_key)
    } else {
        None
    };

    let items = match cached {
        Some(items) => items,
        None => {
            let list_request = FileListRequest {
                prefix: prefix.clone(),
                groups: group.map(|g| vec![g]),
                limit: None,
                delimiter: if recursive {
                    None
                } else {
                    Some("/".to_string())
                },
            };
            let fetched = if recursive {
                backend
                    .list_files(vault, list_request)
                    .await?
                    .into_iter()
                    .map(BlobListItem::File)
                    .collect::<Vec<_>>()
            } else {
                backend.list_files_hierarchical(vault, list_request).await?
            };
            if use_cache && is_unfiltered {
                cache_manager.set(&cache_key, &fetched);
            }
            fetched
        }
    };

    if names_only {
        for item in &items {
            if let BlobListItem::File(file) = item {
                println!("{}", file.name);
            }
        }
        return Ok(());
    }

    let page = paginate_slice(&items, pagination);
    let mut rendered = display_file_list_items(&page.items, recursive, config)?;
    if !rendered.is_empty() {
        rendered.push('\n');
        if let Some(footer) =
            pagination_footer_text(&page, "item", "items", config.runtime_output_format)
        {
            rendered.push_str(&footer);
        }
        crate::utils::pager::print_output(&rendered, pager)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

async fn execute_delete(
    backend: &AwsFileBackend,
    vault: &str,
    files: Vec<String>,
    force: bool,
    continue_on_error: bool,
) -> Result<()> {
    use crate::utils::interactive::InteractivePrompt;

    if !force {
        let prompt = InteractivePrompt::new();
        let confirmed = if files.len() == 1 {
            prompt.confirm(
                &format!(
                    "Are you sure you want to delete file '{}' from S3 storage?",
                    files[0]
                ),
                false,
            )?
        } else {
            println!("You are about to delete {} files:", files.len());
            for (i, file) in files.iter().enumerate() {
                if i < 5 {
                    println!("  - {file}");
                } else if i == 5 {
                    println!("  ... and {} more", files.len() - 5);
                    break;
                }
            }
            prompt.confirm("Are you sure you want to delete these files?", false)?
        };
        if !confirmed {
            println!("Delete operation cancelled.");
            return Ok(());
        }
    }

    let multiple = files.len() > 1;
    if multiple {
        println!("Deleting {} file(s)...", files.len());
    }

    let mut success_count = 0;
    let mut error_count = 0;

    for file_name in &files {
        if !multiple {
            println!("Deleting file '{file_name}'...");
        }
        match backend.delete_file(vault, file_name).await {
            Ok(_) => {
                if multiple {
                    println!(
                        "  {}",
                        output::format_line(
                            output::Level::Success,
                            file_name,
                            output::should_use_rich_stdout()
                        )
                    );
                } else {
                    output::success(&format!("Successfully deleted file '{file_name}'"));
                    output::hint(
                        "Bucket versioning may allow recovery depending on bucket settings.",
                    );
                }
                success_count += 1;
            }
            Err(e) => {
                let e: CrosstacheError = e.into();
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

    if multiple {
        println!("\nDelete completed: {success_count} succeeded, {error_count} failed");
    }
    if error_count > 0 && !continue_on_error {
        return Err(CrosstacheError::unknown(format!(
            "{error_count} file(s) failed to delete"
        )));
    }
    Ok(())
}
