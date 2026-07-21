//! Blob/file subcommand execution (`xv file`, quick upload/download, cache refresh for file lists).
//!
//! Kept separate from [`crate::cli::commands`] so the command router stays thin.

use crate::backend::file::FileBackend;
use crate::backend::BackendKind;
use crate::blob::models::{
    BlobListItem, FileDownloadRequest, FileInfo, FileListRequest, FileUploadRequest,
};
use crate::cli::file::{FileCommands, SyncDirection};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use crate::utils::output;
use crate::utils::pagination::Pagination;
use crate::utils::progress::{self, MultiProgressContext, NoopReporter, ProgressReporter};
use std::path::{Path, PathBuf};

/// The resolved file backend PAIRED with the workspace default entry's vault:
/// every call runs against `files` (the entry's `Backend::files()`) with
/// `vault` (the entry's vault). This is the file-ops analogue of the secret
/// seam's paired resolution — the vault and its backend never drift apart.
/// It mirrors the vault-agnostic surface the handlers were written against,
/// so routing through the trait needs no handler-body changes.
pub(crate) struct FileOps<'a> {
    files: &'a dyn FileBackend,
    secrets: &'a dyn crate::backend::secret::SecretBackend,
    vault: &'a str,
    /// Registry name of the backend that owns `vault` — the cache-key
    /// identifier (`CacheKey::FileList { backend, .. }`).
    backend_name: &'a str,
    /// Resolved backend kind, for backend-aware user messaging and
    /// Azure-only constraints (e.g. the blob 10-tag cap) in the shared handlers.
    kind: BackendKind,
}

impl<'a> FileOps<'a> {
    fn new(
        files: &'a dyn FileBackend,
        secrets: &'a dyn crate::backend::secret::SecretBackend,
        vault: &'a str,
        backend_name: &'a str,
        kind: BackendKind,
    ) -> Self {
        Self {
            files,
            secrets,
            vault,
            backend_name,
            kind,
        }
    }

    async fn upload_file(
        &self,
        request: FileUploadRequest,
        reporter: &dyn ProgressReporter,
    ) -> Result<FileInfo> {
        self.files
            .upload_file(self.vault, request, Some(reporter))
            .await
            .map_err(CrosstacheError::from)
    }

    async fn download_file(
        &self,
        request: FileDownloadRequest,
        reporter: &dyn ProgressReporter,
    ) -> Result<Vec<u8>> {
        crate::secret::attachments::download_decrypted(
            self.secrets,
            self.files,
            self.vault,
            &request.name,
            Some(reporter),
        )
        .await
    }

    async fn upload_file_encrypted(
        &self,
        request: FileUploadRequest,
        reporter: &dyn ProgressReporter,
    ) -> Result<FileInfo> {
        crate::secret::attachments::upload_encrypted(
            self.secrets,
            self.files,
            self.vault,
            request,
            Some(reporter),
        )
        .await
    }

    async fn list_files(&self, request: FileListRequest) -> Result<Vec<FileInfo>> {
        self.files
            .list_files(self.vault, request)
            .await
            .map_err(CrosstacheError::from)
    }

    async fn list_files_hierarchical(&self, request: FileListRequest) -> Result<Vec<BlobListItem>> {
        self.files
            .list_files_hierarchical(self.vault, request)
            .await
            .map_err(CrosstacheError::from)
    }

    async fn delete_file(&self, name: &str) -> Result<()> {
        self.files
            .delete_file(self.vault, name)
            .await
            .map_err(CrosstacheError::from)
    }

    async fn get_file_info(&self, name: &str) -> Result<FileInfo> {
        self.files
            .get_file_info(self.vault, name)
            .await
            .map_err(CrosstacheError::from)
    }
}

/// Resolve the file backend + vault the CLI file ops target — the workspace
/// default entry's `Backend::files()` and vault, materialized together (the
/// paired `resolve_current_vault` pattern). Errors with an actionable message
/// naming the backend and the missing storage config when the resolved backend
/// has no file storage. Returns `(backend_arc, backend_name, vault)`; the
/// caller wraps `backend.files()` in a [`FileOps`].
async fn resolve_file_backend(
    config: &Config,
) -> Result<(std::sync::Arc<dyn crate::backend::Backend>, String, String)> {
    let (backend, backend_name, vault) =
        crate::cli::vault_ops::resolve_current_vault(config, None).await?;
    if backend.files().is_none() {
        return Err(file_storage_unsupported_error(backend.as_ref()));
    }
    Ok((backend, backend_name, vault))
}

/// Actionable capability-gate error for a backend without file storage.
pub(crate) fn file_storage_unsupported_error(
    backend: &dyn crate::backend::Backend,
) -> CrosstacheError {
    use crate::backend::BackendKind;
    let hint = match backend.kind() {
        BackendKind::Azure => "set a storage account (AZURE_STORAGE_ACCOUNT or [azure].storage_account) and run 'xv init'",
        BackendKind::Aws => "set an S3 bucket ([aws].s3_bucket)",
        BackendKind::Local => "the local backend stores files per vault; this should not happen",
    };
    CrosstacheError::invalid_argument(format!(
        "The {} backend has no file storage configured. To use 'xv file', {hint}.",
        backend.name()
    ))
}

pub(crate) fn progress_threshold_bytes(config: &Config) -> u64 {
    let blob_config = config.get_blob_config();
    (blob_config.progress_threshold_mb as u64) * 1024 * 1024
}

pub(crate) fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

pub(crate) async fn execute_file_command(command: FileCommands, config: Config) -> Result<()> {
    // Resolve the workspace default entry's file backend PAIRED with its vault
    // and dispatch every verb through the `FileBackend` trait — uniform across
    // azure/local/aws (no is_aws fork).
    let (backend, backend_name, vault) = resolve_file_backend(&config).await?;
    let files = backend
        .files()
        .expect("resolve_file_backend guarantees files() is Some");
    let blob_manager = FileOps::new(
        files,
        backend.secrets(),
        &vault,
        &backend_name,
        backend.kind(),
    );

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
            encrypt,
        } => {
            // ponytail: --encrypt is single-file only; extend to multi/recursive when needed
            if encrypt && (recursive || files.len() > 1) {
                return Err(CrosstacheError::invalid_argument(
                    "--encrypt currently supports single-file uploads only",
                ));
            }
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
                    encrypt,
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
            // Keyed by the resolved (backend, vault) — the ONE-identifier convention.
            for recursive in [true, false] {
                cache_manager.invalidate(&crate::cache::CacheKey::FileList {
                    backend: backend_name.clone(),
                    vault_name: vault.clone(),
                    recursive,
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

            execute_file_list(
                &blob_manager,
                prefix,
                group,
                pagination,
                pager,
                recursive,
                names_only,
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
            // Keyed by the resolved (backend, vault) — the ONE-identifier convention.
            for recursive in [true, false] {
                cache_manager.invalidate(&crate::cache::CacheKey::FileList {
                    backend: backend_name.clone(),
                    vault_name: vault.clone(),
                    recursive,
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
            // Sync is not yet recomposed over the file trait; block it on the
            // resolved backend kind rather than probing a specific SDK.
            if backend.kind() == crate::backend::BackendKind::Aws {
                return Err(CrosstacheError::invalid_argument(
                    "`xv file sync` is not yet supported on the AWS backend. \
                     upload/download/list/delete/info are available; use \
                     `xv file upload --recursive` / `xv file download --recursive` meanwhile.",
                ));
            }
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
    let (backend, backend_name, vault) = resolve_file_backend(config).await?;
    let files = backend
        .files()
        .expect("resolve_file_backend guarantees file storage");
    let blob_manager = FileOps::new(
        files,
        backend.secrets(),
        &vault,
        &backend_name,
        backend.kind(),
    );

    execute_file_info(&blob_manager, file_name, config).await
}
pub(crate) async fn refresh_file_list(
    backend: String,
    vault_name: String,
    recursive: bool,
    config: Config,
) -> Result<()> {
    use crate::backend::BackendRegistry;
    use crate::blob::models::{BlobListItem, FileListRequest};
    use crate::cache::{CacheKey, CacheManager};

    let registry = BackendRegistry::with_lazy(&config, std::slice::from_ref(&backend))
        .map_err(|e| CrosstacheError::config(e.to_string()))?;
    let backend_impl = registry
        .materialize(&backend)
        .map_err(|e| CrosstacheError::config(e.to_string()))?;
    // Backends without file storage have nothing to refresh.
    let Some(files) = backend_impl.files() else {
        return Ok(());
    };

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
        let flat = files
            .list_files(&vault_name, list_request)
            .await
            .map_err(CrosstacheError::from)?;
        flat.into_iter().map(BlobListItem::File).collect()
    } else {
        files
            .list_files_hierarchical(&vault_name, list_request)
            .await
            .map_err(CrosstacheError::from)?
    };

    let cache_manager = CacheManager::from_config(&config);
    let cache_key = CacheKey::FileList {
        backend,
        vault_name,
        recursive,
    };
    cache_manager.set(&cache_key, &items);

    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn execute_file_upload(
    blob_manager: &FileOps<'_>,
    file_path: &str,
    name: Option<String>,
    groups: Vec<String>,
    metadata: Vec<(String, String)>,
    tags: Vec<(String, String)>,
    content_type: Option<String>,
    encrypt: bool,
    config: &Config,
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
    let file_size = content.len() as u64;

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

    // The 10-tag cap is an Azure Blob index-tag constraint; only enforce it on
    // Azure so local/aws aren't constrained by a foreign backend's limit.
    if blob_manager.kind == BackendKind::Azure && tags_map.len() > 10 {
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
    let threshold = progress_threshold_bytes(config);
    let tty = is_tty();
    if !tty {
        println!("Uploading file '{file_path}' as '{remote_name}'...");
    }
    let reporter = progress::create_file_reporter(file_size, threshold, tty);
    reporter.set_message(format!("Uploading '{remote_name}'..."));

    let file_info = if encrypt {
        blob_manager
            .upload_file_encrypted(upload_request, reporter.as_ref())
            .await?
    } else {
        blob_manager
            .upload_file(upload_request, reporter.as_ref())
            .await?
    };
    output::success(&format!("Successfully uploaded file '{}'", file_info.name));
    println!("   Size: {} bytes", file_info.size);
    println!("   Content-Type: {}", file_info.content_type);
    if !file_info.groups.is_empty() {
        println!("   Groups: {:?}", file_info.groups);
    }

    Ok(())
}

async fn execute_file_download(
    blob_manager: &FileOps<'_>,
    name: &str,
    output: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    let output_path = resolve_single_download_path(name, output.as_deref())?;
    execute_file_download_to_path(blob_manager, name, output_path, force, config).await
}

async fn execute_file_download_to_path(
    blob_manager: &FileOps<'_>,
    name: &str,
    output_path: String,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::blob::models::FileDownloadRequest;
    use std::fs;
    use std::path::Path;

    // Check if file exists and handle force flag
    if Path::new(&output_path).exists() && !force {
        return Err(CrosstacheError::config(format!(
            "File '{output_path}' already exists. Use --force to overwrite."
        )));
    }

    // Create download request
    let download_request = FileDownloadRequest {
        name: name.to_string(),
    };

    let threshold = progress_threshold_bytes(config);
    let tty = is_tty();
    if !tty {
        println!("Downloading file '{name}' to '{output_path}'...");
    }
    let file_size = if tty {
        blob_manager
            .get_file_info(name)
            .await
            .map(|info| info.size)
            .unwrap_or(0)
    } else {
        0
    };
    let reporter = progress::create_file_reporter(file_size, threshold, tty);
    reporter.set_message(format!("Downloading '{name}'..."));

    let content = blob_manager
        .download_file(download_request, reporter.as_ref())
        .await?;
    // Ensure parent directories exist so blob names with path segments
    // (e.g. "docs/readme.md") succeed when their parents are not yet created.
    if let Some(parent) = Path::new(&output_path).parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| {
                CrosstacheError::config(format!(
                    "Failed to create parent directories for {output_path}: {e}"
                ))
            })?;
        }
    }
    crate::utils::helpers::write_file_no_follow(Path::new(&output_path), &content, force)?;
    output::success(&format!("Successfully downloaded file '{name}'"));

    Ok(())
}

pub(crate) fn resolve_single_download_path(name: &str, output: Option<&str>) -> Result<String> {
    use crate::utils::helpers::safe_join;

    // Determine output path with traversal guard.
    // When --output is an existing directory, place the file inside it.
    // When --output is an explicit file path (caller-resolved), use it directly.
    // When --output is omitted, derive from blob name anchored at CWD.
    match output {
        Some(p) if Path::new(p).is_dir() => {
            safe_join(Path::new(p), name).map(|pb| pb.to_string_lossy().into_owned())
        }
        Some(p) => Ok(p.to_string()),
        None => {
            let cwd = std::env::current_dir().map_err(|e| {
                CrosstacheError::config(format!("Cannot determine current directory: {e}"))
            })?;
            safe_join(&cwd, name).map(|pb| pb.to_string_lossy().into_owned())
        }
    }
}

pub(crate) fn display_file_list_items(
    items: &[crate::blob::models::BlobListItem],
    recursive: bool,
    config: &Config,
) -> Result<String> {
    use crate::blob::models::BlobListItem;
    use crate::utils::format::format_size;
    use crate::utils::format::TableFormatter;
    use serde::Serialize;
    use std::fmt::Write as _;
    use tabled::Tabled;

    let fmt = config.runtime_output_format.resolve_for_stdout();
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );

    let mut output = String::new();

    // One display row set for every non-JSON/YAML format (spec: file list CSV
    // becomes the table's column set + a leading Kind column). JSON/YAML keep
    // the rich BlobListItem serialization as the full-fidelity formats.
    #[derive(Tabled, Serialize)]
    struct FileRow {
        #[tabled(rename = "Kind")]
        kind: String,
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

    if items.is_empty() && human_table_like {
        let formatter = TableFormatter::new(
            fmt,
            config.no_color,
            config.template.clone(),
            config.runtime_columns.clone(),
        );
        formatter.validate_columns::<FileRow>()?;
        output::info(&crate::utils::list_output::empty_state_message(
            "files", None,
        ));
        return Ok(String::new());
    }

    // Build the rows once, before the `match fmt`.
    let rows: Vec<FileRow> = items
        .iter()
        .map(|item| match item {
            BlobListItem::Directory { name, .. } => FileRow {
                kind: "directory".to_string(),
                name: name.clone(),
                size: "<DIR>".to_string(),
                content_type: "-".to_string(),
                modified: "-".to_string(),
                groups: "-".to_string(),
            },
            BlobListItem::File(file) => FileRow {
                kind: "file".to_string(),
                name: file.name.clone(),
                size: format_size(file.size),
                content_type: file.content_type.clone(),
                modified: file.last_modified.format("%Y-%m-%d %H:%M:%S").to_string(),
                groups: file.groups.join(", "),
            },
        })
        .collect();

    match fmt {
        OutputFormat::Json => {
            let json_output = serde_json::to_string_pretty(items).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize items: {e}"))
            })?;
            output.push_str(&json_output);
        }
        OutputFormat::Yaml => {
            let yaml_output = serde_yaml::to_string(items).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize items: {e}"))
            })?;
            output.push_str(&yaml_output);
        }
        OutputFormat::Auto => unreachable!("resolve_for_stdout must not return Auto"),
        _ => {
            // Table / Plain / Raw / Csv / Template share the FileRow set.
            let formatter = TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
                config.runtime_columns.clone(),
            );
            output.push_str(&formatter.format_table(&rows)?);

            if human_table_like {
                let file_count = items
                    .iter()
                    .filter(|i| matches!(i, BlobListItem::File(_)))
                    .count();
                let dir_count = items
                    .iter()
                    .filter(|i| matches!(i, BlobListItem::Directory { .. }))
                    .count();

                output.push('\n');
                let mut count_line = crate::utils::list_output::count_label(
                    file_count, file_count, "file", "files", None, false,
                );
                if !recursive && dir_count > 0 {
                    let _ = write!(
                        count_line,
                        ", {}",
                        crate::utils::list_output::pluralize(dir_count, "directory", "directories")
                    );
                }
                let _ = writeln!(output, "{}", count_line);
            }
        }
    }

    Ok(output.trim_end_matches('\n').to_string())
}

#[allow(clippy::too_many_arguments)]
async fn execute_file_list(
    blob_manager: &FileOps<'_>,
    prefix: Option<String>,
    group: Option<String>,
    pagination: Pagination,
    pager: bool,
    recursive: bool,
    names_only: bool,
    no_cache: bool,
    config: &Config,
) -> Result<()> {
    use crate::blob::models::{BlobListItem, FileListRequest};
    use crate::cache::{CacheKey, CacheManager};
    use crate::utils::pagination::{paginate_slice, pagination_footer_text};

    // `--names-only` always needs the full recursive item set (no directory entries).
    let recursive = recursive || names_only;

    let cache_manager = CacheManager::from_config(config);
    let cache_key = CacheKey::FileList {
        backend: blob_manager.backend_name.to_string(),
        vault_name: blob_manager.vault.to_string(),
        recursive,
    };
    let use_cache = cache_manager.is_enabled() && !no_cache;

    let is_unfiltered = prefix.is_none() && group.is_none();

    if use_cache && is_unfiltered {
        if let Some(cached) = cache_manager.get::<Vec<BlobListItem>>(&cache_key) {
            if names_only {
                for item in &cached {
                    if let BlobListItem::File(file) = item {
                        println!("{}", file.name);
                    }
                }
                return Ok(());
            }

            let page = paginate_slice(&cached, pagination);
            let mut output = display_file_list_items(&page.items, recursive, config)?;
            if !output.is_empty() {
                output.push('\n');
                if let Some(footer) =
                    pagination_footer_text(&page, "item", "items", config.runtime_output_format)
                {
                    output.push_str(&footer);
                }
                crate::utils::pager::print_output(&output, pager)?;
            }
            return Ok(());
        }
    }

    // Create list request
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

    if names_only {
        for item in &items {
            if let BlobListItem::File(file) = item {
                println!("{}", file.name);
            }
        }
        return Ok(());
    }

    let page = paginate_slice(&items, pagination);
    let mut output = display_file_list_items(&page.items, recursive, config)?;
    if !output.is_empty() {
        output.push('\n');
        if let Some(footer) =
            pagination_footer_text(&page, "item", "items", config.runtime_output_format)
        {
            output.push_str(&footer);
        }
        crate::utils::pager::print_output(&output, pager)?;
    }
    Ok(())
}

async fn execute_file_delete(
    blob_manager: &FileOps<'_>,
    name: &str,
    force: bool,
    _config: &Config,
) -> Result<()> {
    // Confirmation unless forced
    if !force {
        use crate::utils::interactive::InteractivePrompt;
        let prompt = InteractivePrompt::new();
        if !prompt.confirm(
            &format!("Are you sure you want to delete file '{name}' from file storage?"),
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
    // Recovery hint depends on the backend's delete semantics.
    match blob_manager.kind {
        BackendKind::Azure => {
            output::hint(
                "Blob soft-delete may allow recovery depending on storage account settings.",
            );
        }
        BackendKind::Aws => {
            output::hint(
                "Bucket versioning may allow recovery depending on your S3 configuration.",
            );
        }
        BackendKind::Local => {}
    }

    Ok(())
}

async fn execute_file_info(blob_manager: &FileOps<'_>, name: &str, config: &Config) -> Result<()> {
    // Get file info
    let file_info = blob_manager.get_file_info(name).await?;
    display_file_info(&file_info, config)
}

/// Render a [`FileInfo`] to stdout (shared by the Azure and AWS executors).
pub(crate) fn display_file_info(
    file_info: &crate::blob::models::FileInfo,
    config: &Config,
) -> Result<()> {
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
pub(crate) struct FileUploadInfo {
    /// Full local file path
    pub(crate) local_path: PathBuf,
    /// Relative path from base directory (for blob name calculation)
    _relative_path: String,
    /// Final blob name (includes prefix and converted path separators)
    pub(crate) blob_name: String,
}

/// Convert a relative path to blob name format (forward slashes, no leading slash).
fn path_to_blob_name(path: &Path, prefix: Option<&str>) -> Result<String> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(s) => components.push(s.to_string_lossy().to_string()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(CrosstacheError::invalid_argument(format!(
                    "upload path '{}' contains '..' and cannot be converted to a blob name",
                    path.display()
                )));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(CrosstacheError::invalid_argument(format!(
                    "upload path '{}' must be relative to the upload root",
                    path.display()
                )));
            }
        }
    }

    let relative_path = components.join("/");
    if relative_path.is_empty() {
        return Err(CrosstacheError::invalid_argument(format!(
            "upload path '{}' does not contain a file name",
            path.display()
        )));
    }

    Ok(if let Some(p) = prefix {
        let p = p.trim_matches('/');
        if p.is_empty() {
            relative_path
        } else {
            format!("{}/{}", p, relative_path)
        }
    } else {
        relative_path
    })
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
pub(crate) fn collect_files_with_structure(
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
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    CrosstacheError::invalid_argument(format!(
                        "upload path '{}' does not contain a file name",
                        path.display()
                    ))
                })?
        } else {
            // Preserve structure with forward slashes
            path_to_blob_name(relative, prefix)?
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
    blob_manager: &FileOps<'_>,
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
    let threshold = progress_threshold_bytes(config);
    let tty = is_tty();
    let mp = MultiProgressContext::new(all_files.len() as u64, threshold, tty);

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

        if !tty {
            if !flatten {
                println!("Uploading: {} → {}", local_path_str, file_info.blob_name);
            } else {
                println!("Uploading: {}", local_path_str);
            }
        }

        // Call blob manager directly (not execute_file_upload) to avoid
        // per-file output that conflicts with MultiProgress rendering.
        let result = {
            use crate::blob::models::FileUploadRequest;
            use std::collections::HashMap;

            let content = std::fs::read(&file_info.local_path).map_err(|e| {
                CrosstacheError::config(format!(
                    "Failed to read {}: {e}",
                    file_info.local_path.display()
                ))
            })?;
            let upload_request = FileUploadRequest {
                name: file_info.blob_name.clone(),
                content,
                content_type: None,
                groups: group.clone(),
                metadata: metadata.iter().cloned().collect::<HashMap<_, _>>(),
                tags: tag.iter().cloned().collect::<HashMap<_, _>>(),
            };
            blob_manager
                .upload_file(upload_request, &NoopReporter)
                .await
        };

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
    blob_manager: &FileOps<'_>,
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
            None,  // content_type is not allowed for multiple files
            false, // --encrypt is single-file only
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

/// Resolve the output directory for a multi-file download.
///
/// Returns an error if `output` names an existing non-directory path (which would
/// cause every file to clobber the same destination). Creates the directory if it
/// doesn't exist yet.
pub(crate) fn resolve_multi_download_dir(output: Option<&str>) -> Result<PathBuf> {
    use std::fs;
    use std::path::Path;
    match output {
        Some(p) => {
            let path = Path::new(p);
            if path.exists() && !path.is_dir() {
                return Err(CrosstacheError::invalid_argument(format!(
                    "--output '{p}' must be a directory when downloading multiple files"
                )));
            }
            if !path.exists() {
                fs::create_dir_all(path).map_err(|e| {
                    CrosstacheError::config(format!("Failed to create output directory '{p}': {e}"))
                })?;
            }
            Ok(path.to_path_buf())
        }
        None => std::env::current_dir().map_err(|e| {
            CrosstacheError::config(format!("Cannot determine current directory: {e}"))
        }),
    }
}

async fn execute_file_download_multiple(
    blob_manager: &FileOps<'_>,
    files: Vec<String>,
    output: Option<String>,
    force: bool,
    continue_on_error: bool,
    config: &Config,
) -> Result<()> {
    use crate::utils::helpers::safe_join;

    let output_dir = resolve_multi_download_dir(output.as_deref())?;

    println!("Downloading {} file(s)...", files.len());

    let mut success_count = 0;
    let mut error_count = 0;

    for file_name in files {
        // Compute a unique per-file output path via traversal guard.
        let per_file_output = match safe_join(&output_dir, &file_name) {
            Ok(p) => p.to_string_lossy().into_owned(),
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
                continue;
            }
        };
        match execute_file_download_to_path(
            blob_manager,
            &file_name,
            per_file_output,
            force,
            config,
        )
        .await
        {
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
    blob_manager: &FileOps<'_>,
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
    eprintln!(
        "Downloading to: {}",
        output_path
            .canonicalize()
            .unwrap_or_else(|_| output_path.to_path_buf())
            .display()
    );

    let mut all_files_to_download = Vec::new();

    // List all blobs matching each prefix
    for prefix in &prefixes {
        let list_request = FileListRequest {
            prefix: Some(prefix.clone()),
            groups: None,
            limit: None,
            delimiter: None,
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
    let threshold = progress_threshold_bytes(config);
    let tty = is_tty();
    let mp = MultiProgressContext::new(all_files_to_download.len() as u64, threshold, tty);

    for file_info in &all_files_to_download {
        let blob_name = &file_info.name;

        // Determine local file path.
        // Security: blob names come from the remote listing and are untrusted.
        // `safe_join` rejects both `..` components and absolute paths (a plain
        // `Path::join` would silently discard the base directory for an
        // absolute blob name like "/etc/cron.d/x"). The check inspects the
        // name before any filesystem call, so there is no TOCTOU window.
        let joined = if flatten {
            // Flatten: use only the final filename component
            let filename = Path::new(blob_name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            crate::utils::helpers::safe_join(output_path, &filename)
        } else {
            // Preserve structure: use full blob path
            crate::utils::helpers::safe_join(output_path, blob_name)
        };
        let local_path = match joined {
            Ok(path) => path,
            Err(e) => {
                output::warn(&format!("Skipping '{blob_name}': {e}"));
                failure_count += 1;
                if continue_on_error {
                    mp.advance_overall(blob_name);
                    continue;
                } else {
                    return Err(CrosstacheError::config(format!(
                        "Unsafe blob name '{blob_name}': {e}"
                    )));
                }
            }
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
                println!("Downloading: {} → {}", blob_name, local_path_str);
            } else {
                println!("Downloading: {}", blob_name);
            }
        }

        // Call blob manager directly (not execute_file_download) to avoid
        // per-file output that conflicts with MultiProgress rendering.
        let result = {
            use crate::blob::models::FileDownloadRequest;

            let download_request = FileDownloadRequest {
                name: blob_name.to_string(),
            };
            blob_manager
                .download_file(download_request, &NoopReporter)
                .await
                .and_then(|content| {
                    crate::utils::helpers::write_file_no_follow(&local_path, &content, force)
                        .map(|_| ())
                })
        };

        match result {
            Ok(_) => {
                success_count += 1;
                mp.log(&format!("Downloaded: {}", blob_name));
                mp.advance_overall(blob_name);
            }
            Err(e) => {
                output::error(&format!("Failed to download '{}': {}", blob_name, e));
                failure_count += 1;
                mp.advance_overall(blob_name);
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }

    mp.finish();

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
    blob_manager: &FileOps<'_>,
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
    blob_manager: &FileOps<'_>,
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

/// Read local file, upload blob, align local mtime to server `last_modified`.
async fn file_sync_perform_upload(
    blob_manager: &FileOps<'_>,
    info: &FileUploadInfo,
    blob_name: &str,
    output_json: bool,
    reporter: &dyn crate::utils::progress::ProgressReporter,
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
    if !output_json && !is_tty() {
        println!("upload: {} → {blob_name}", info.local_path.display());
    }
    let uploaded_info = blob_manager.upload_file(upload_request, reporter).await?;
    sync::set_file_mtime_utc(&info.local_path, uploaded_info.last_modified)?;
    Ok(())
}

/// Download blob to local path (with traversal check, parents, mtime).
async fn file_sync_perform_download(
    blob_manager: &FileOps<'_>,
    base_path: &std::path::Path,
    prefix_ref: Option<&str>,
    blob_name: &str,
    remote_info: &crate::blob::models::FileInfo,
    output_json: bool,
    reporter: &dyn crate::utils::progress::ProgressReporter,
) -> Result<()> {
    use crate::blob::models::FileDownloadRequest;
    use crate::blob::sync;
    let target = sync::local_path_from_blob(base_path, prefix_ref, blob_name)?;
    sync_assert_safe_local_path(base_path, &target, blob_name)?;
    if !output_json && !is_tty() {
        println!("download: {blob_name} → {}", target.display());
    }
    let download_request = FileDownloadRequest {
        name: blob_name.to_string(),
    };
    let content = blob_manager
        .download_file(download_request, reporter)
        .await?;
    let file = crate::utils::helpers::write_file_no_follow(&target, &content, true)?;
    sync::set_file_mtime_utc_on_file(&file, &target, remote_info.last_modified)?;
    Ok(())
}

async fn execute_file_sync(
    blob_manager: &FileOps<'_>,
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
    let threshold = progress_threshold_bytes(config);
    let tty = is_tty() && !dry_run;

    match direction {
        SyncDirection::Up => {
            let mut sorted_names: Vec<String> = local_names.iter().cloned().collect();
            sorted_names.sort();
            let mp = MultiProgressContext::new(sorted_names.len() as u64, threshold, tty);
            for blob_name in &sorted_names {
                let info = local_by_blob.get(blob_name).unwrap();
                let (size, mtime) = *local_meta.get(blob_name).unwrap();
                let need = match remote_by_name.get(blob_name) {
                    None => true,
                    Some(r) => !sync::should_skip_sync_up(size, mtime, r),
                };
                if !need {
                    summary.skipped += 1;
                    if tty && !config.output_json {
                        mp.log(&format!("skip (up to date): {blob_name}"));
                    } else if !config.output_json {
                        println!("skip (up to date): {blob_name}");
                    }
                    mp.advance_overall(blob_name);
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
                    mp.advance_overall(blob_name);
                    continue;
                }
                file_sync_perform_upload(
                    blob_manager,
                    info,
                    blob_name,
                    config.output_json,
                    &NoopReporter,
                )
                .await?;
                summary.uploaded += 1;
                if tty && !config.output_json {
                    mp.log(&format!(
                        "upload: {} → {blob_name}",
                        info.local_path.display()
                    ));
                }
                mp.advance_overall(blob_name);
                mutated = true;
            }
            mp.finish();
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
            let mp = MultiProgressContext::new(remote_names.len() as u64, threshold, tty);
            for blob_name in &remote_names {
                let remote_info = remote_by_name.get(blob_name).unwrap();
                let target = sync::local_path_from_blob(base_path, prefix_ref, blob_name)?;
                sync_assert_safe_local_path(base_path, &target, blob_name)?;

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
                    if tty && !config.output_json {
                        mp.log(&format!("skip (up to date): {blob_name}"));
                    } else if !config.output_json {
                        println!("skip (up to date): {blob_name}");
                    }
                    summary.skipped += 1;
                    mp.advance_overall(blob_name);
                    continue;
                }

                if dry_run {
                    if !config.output_json {
                        println!("download (dry-run): {blob_name} → {}", target.display());
                    }
                    summary.downloaded += 1;
                    mp.advance_overall(blob_name);
                    continue;
                }

                file_sync_perform_download(
                    blob_manager,
                    base_path,
                    prefix_ref,
                    blob_name,
                    remote_info,
                    config.output_json,
                    &NoopReporter,
                )
                .await?;
                summary.downloaded += 1;
                if tty && !config.output_json {
                    mp.log(&format!("download: {blob_name} → {}", target.display()));
                }
                mp.advance_overall(blob_name);
                mutated = true;
            }
            mp.finish();
        }
        SyncDirection::Both => {
            let remote_keys: HashSet<String> = remote_by_name.keys().cloned().collect();
            let all_names: HashSet<String> = local_names.union(&remote_keys).cloned().collect();
            let mut ordered: Vec<String> = all_names.into_iter().collect();
            ordered.sort();
            let mp = MultiProgressContext::new(ordered.len() as u64, threshold, tty);

            for blob_name in &ordered {
                let local_present = local_meta.contains_key(blob_name);
                let remote_present = remote_by_name.contains_key(blob_name);

                match (local_present, remote_present) {
                    (true, false) => {
                        let info = local_by_blob.get(blob_name).unwrap();
                        if dry_run {
                            if !config.output_json {
                                println!(
                                    "upload (dry-run): {} → {blob_name}",
                                    info.local_path.display()
                                );
                            }
                            summary.uploaded += 1;
                            mp.advance_overall(blob_name);
                            continue;
                        }
                        file_sync_perform_upload(
                            blob_manager,
                            info,
                            blob_name,
                            config.output_json,
                            &NoopReporter,
                        )
                        .await?;
                        summary.uploaded += 1;
                        if tty && !config.output_json {
                            mp.log(&format!(
                                "upload: {} → {blob_name}",
                                info.local_path.display()
                            ));
                        }
                        mp.advance_overall(blob_name);
                        mutated = true;
                    }
                    (false, true) => {
                        let remote_info = remote_by_name.get(blob_name).unwrap();
                        let target = sync::local_path_from_blob(base_path, prefix_ref, blob_name)?;
                        sync_assert_safe_local_path(base_path, &target, blob_name)?;
                        if dry_run {
                            if !config.output_json {
                                println!("download (dry-run): {blob_name} → {}", target.display());
                            }
                            summary.downloaded += 1;
                            mp.advance_overall(blob_name);
                            continue;
                        }
                        file_sync_perform_download(
                            blob_manager,
                            base_path,
                            prefix_ref,
                            blob_name,
                            remote_info,
                            config.output_json,
                            &NoopReporter,
                        )
                        .await?;
                        summary.downloaded += 1;
                        if tty && !config.output_json {
                            mp.log(&format!("download: {blob_name} → {}", target.display()));
                        }
                        mp.advance_overall(blob_name);
                        mutated = true;
                    }
                    (true, true) => {
                        let info = local_by_blob.get(blob_name).unwrap();
                        let (size, mtime) = *local_meta.get(blob_name).unwrap();
                        let remote_info = remote_by_name.get(blob_name).unwrap();
                        match sync::resolve_both(size, mtime, remote_info) {
                            BothAction::Skip => {
                                if tty && !config.output_json {
                                    mp.log(&format!("skip: {blob_name}"));
                                } else if !config.output_json {
                                    println!("skip: {blob_name}");
                                }
                                summary.skipped += 1;
                                mp.advance_overall(blob_name);
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
                                    mp.advance_overall(blob_name);
                                    continue;
                                }
                                file_sync_perform_upload(
                                    blob_manager,
                                    info,
                                    blob_name,
                                    config.output_json,
                                    &NoopReporter,
                                )
                                .await?;
                                summary.uploaded += 1;
                                if tty && !config.output_json {
                                    mp.log(&format!(
                                        "upload: {} → {blob_name}",
                                        info.local_path.display()
                                    ));
                                }
                                mp.advance_overall(blob_name);
                                mutated = true;
                            }
                            BothAction::Download => {
                                let target =
                                    sync::local_path_from_blob(base_path, prefix_ref, blob_name)?;
                                sync_assert_safe_local_path(base_path, &target, blob_name)?;
                                if dry_run {
                                    if !config.output_json {
                                        println!(
                                            "download (dry-run): {blob_name} → {}",
                                            target.display()
                                        );
                                    }
                                    summary.downloaded += 1;
                                    mp.advance_overall(blob_name);
                                    continue;
                                }
                                file_sync_perform_download(
                                    blob_manager,
                                    base_path,
                                    prefix_ref,
                                    blob_name,
                                    remote_info,
                                    config.output_json,
                                    &NoopReporter,
                                )
                                .await?;
                                summary.downloaded += 1;
                                if tty && !config.output_json {
                                    mp.log(&format!(
                                        "download: {blob_name} → {}",
                                        target.display()
                                    ));
                                }
                                mp.advance_overall(blob_name);
                                mutated = true;
                            }
                        }
                    }
                    (false, false) => {}
                }
            }
            mp.finish();

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
        for recursive in [true, false] {
            cache_manager.invalidate(&crate::cache::CacheKey::FileList {
                backend: blob_manager.backend_name.to_string(),
                vault_name: blob_manager.vault.to_string(),
                recursive,
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
    let (backend, backend_name, vault) = resolve_file_backend(config).await?;
    let files = backend
        .files()
        .expect("resolve_file_backend guarantees file storage");
    let blob_manager = FileOps::new(
        files,
        backend.secrets(),
        &vault,
        &backend_name,
        backend.kind(),
    );

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
        false, // no --encrypt flag on the quick-upload path
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
    let (backend, backend_name, vault) = resolve_file_backend(config).await?;
    let files = backend
        .files()
        .expect("resolve_file_backend guarantees file storage");
    let blob_manager = FileOps::new(
        files,
        backend.secrets(),
        &vault,
        &backend_name,
        backend.kind(),
    );

    let final_output_path = resolve_single_download_path(name, output.as_deref())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- safe_join integration: traversal and absolute-path rejection ---

    #[test]
    fn test_single_file_rejects_traversal() {
        let base = std::path::Path::new("/tmp/base");
        let err = crate::utils::helpers::safe_join(base, "../escape.txt").unwrap_err();
        assert!(
            err.to_string().contains(".."),
            "error should mention '..': {err}"
        );
    }

    #[test]
    fn test_single_file_rejects_absolute_path() {
        let base = std::path::Path::new("/tmp/base");
        let err = crate::utils::helpers::safe_join(base, "/etc/passwd").unwrap_err();
        assert!(
            err.to_string().contains("absolute"),
            "error should mention 'absolute': {err}"
        );
    }

    #[test]
    fn test_single_file_normal_name_resolves_under_base() {
        let base = std::path::Path::new("/tmp/base");
        let result = crate::utils::helpers::safe_join(base, "docs/readme.md").unwrap();
        assert_eq!(result, std::path::Path::new("/tmp/base/docs/readme.md"));
    }

    #[test]
    fn test_single_download_output_dir_resolves_to_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            resolve_single_download_path("docs/readme.md", Some(dir.path().to_str().unwrap()))
                .unwrap();

        assert_eq!(
            std::path::Path::new(&result),
            &dir.path().join("docs/readme.md")
        );
    }

    // --- resolve_multi_download_dir: directory validation ---

    #[test]
    fn test_multi_download_rejects_file_as_output() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("notadir.txt");
        std::fs::write(&file_path, b"data").unwrap();

        let err = resolve_multi_download_dir(Some(file_path.to_str().unwrap())).unwrap_err();
        assert!(
            err.to_string().contains("must be a directory"),
            "error should say 'must be a directory': {err}"
        );
    }

    #[test]
    fn test_multi_download_creates_and_returns_dir() {
        let parent = tempfile::tempdir().unwrap();
        let new_dir = parent.path().join("downloads");

        assert!(!new_dir.exists());
        let result = resolve_multi_download_dir(Some(new_dir.to_str().unwrap())).unwrap();
        assert!(result.exists() && result.is_dir());
    }

    #[test]
    fn test_multi_download_uses_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_multi_download_dir(Some(dir.path().to_str().unwrap())).unwrap();
        assert_eq!(result, dir.path());
    }

    #[test]
    fn path_to_blob_name_rejects_parent_components() {
        let err = path_to_blob_name(std::path::Path::new("safe/../escape.txt"), None).unwrap_err();
        assert!(err.to_string().contains(".."));
    }

    #[test]
    fn path_to_blob_name_preserves_relative_structure_with_prefix() {
        let name =
            path_to_blob_name(std::path::Path::new("docs/readme.md"), Some("release/")).unwrap();
        assert_eq!(name, "release/docs/readme.md");
    }
}
