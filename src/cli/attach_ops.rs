//! Attachment subcommand execution (`xv attach`, `xv attachments`, `xv detach`).
//!
//! Thin CLI plumbing over [`crate::secret::attachments`]: resolve the target
//! secret's backend + vault, gate on file-storage capability, delegate.

use std::path::Path;
use std::sync::Arc;

use crate::backend::Backend;
use crate::cli::file_ops::file_storage_unsupported_error;
use crate::cli::helpers::{confirm_destructive, resolve_workspace_or_default};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::attachments;
use crate::utils::format::format_size;
use crate::utils::output;
use crate::workspace::TargetMode;

/// Reject attachment names that would escape the `attachments/<secret>/`
/// prefix or produce surprising nested paths.
fn validate_attachment_name(name: &str) -> Result<()> {
    if !attachments::is_valid_path_component(name) {
        return Err(CrosstacheError::invalid_argument(format!(
            "invalid attachment name '{name}': must be a plain file name (no path separators)"
        )));
    }
    Ok(())
}

/// Resolve `(backend, vault, resolved_secret_name)` for an attachment verb and
/// gate on file-storage capability.
async fn resolve(
    secret: &str,
    config: &Config,
    mode: TargetMode,
) -> Result<(Arc<dyn Backend>, String, String)> {
    let (backend, _backend_name, vault, resolved_name) =
        resolve_workspace_or_default(secret, config, mode).await?;
    if backend.files().is_none() {
        return Err(file_storage_unsupported_error(backend.as_ref()));
    }
    // A resolved secret name containing a path separator would break the
    // `attachments/<name>/` prefix's isolation from other secrets' blobs
    // (see delete-cascade path-traversal finding) — reject before any
    // attach/list/get/detach verb can create or address such a blob.
    if !attachments::is_valid_path_component(&resolved_name) {
        return Err(CrosstacheError::invalid_argument(format!(
            "secret name '{resolved_name}' cannot be used with attachments (path separators)"
        )));
    }
    Ok((backend, vault, resolved_name))
}

pub(crate) async fn execute_attach(
    secret: String,
    file: String,
    name: Option<String>,
    config: Config,
) -> Result<()> {
    let (backend, vault, secret_name) = resolve(&secret, &config, TargetMode::Write).await?;

    // Attaching to a missing secret is almost always a typo — fail early.
    if !backend
        .secrets()
        .secret_exists(&vault, &secret_name)
        .await?
    {
        return Err(CrosstacheError::invalid_argument(format!(
            "secret '{secret_name}' not found in vault '{vault}' — create it first with 'xv set'"
        )));
    }

    let path = Path::new(&file);
    if !path.exists() {
        return Err(CrosstacheError::config(format!("File not found: {file}")));
    }
    let attachment_name = match name {
        Some(n) => n,
        None => path
            .file_name()
            .ok_or_else(|| {
                CrosstacheError::invalid_argument(format!(
                    "cannot derive a file name from '{file}'"
                ))
            })?
            .to_string_lossy()
            .to_string(),
    };
    validate_attachment_name(&attachment_name)?;

    let content = std::fs::read(path)
        .map_err(|e| CrosstacheError::config(format!("Failed to read file {file}: {e}")))?;
    let size = content.len() as u64;

    let request = crate::blob::models::FileUploadRequest {
        name: attachments::attachment_blob_name(&secret_name, &attachment_name),
        content,
        content_type: None,
        groups: Vec::new(),
        metadata: std::collections::HashMap::new(),
        tags: std::collections::HashMap::new(),
    };
    let files = backend.files().expect("resolve gated on files()");
    attachments::upload_encrypted(backend.secrets(), files, &vault, request, None).await?;
    output::success(&format!(
        "Attached '{attachment_name}' ({}) to secret '{secret_name}' (encrypted)",
        format_size(size)
    ));
    Ok(())
}

pub(crate) async fn execute_attachments(
    secret: String,
    get: Option<String>,
    output_path: Option<String>,
    config: Config,
) -> Result<()> {
    let (backend, vault, secret_name) = resolve(&secret, &config, TargetMode::Read).await?;
    let files = backend.files().expect("resolve gated on files()");

    if let Some(attachment_name) = get {
        validate_attachment_name(&attachment_name)?;
        let blob_name = attachments::attachment_blob_name(&secret_name, &attachment_name);
        let content =
            attachments::download_decrypted(backend.secrets(), files, &vault, &blob_name, None)
                .await?;
        let out = output_path.unwrap_or_else(|| attachment_name.clone());
        if Path::new(&out).exists() {
            return Err(CrosstacheError::config(format!(
                "File '{out}' already exists — pass --output to choose another path"
            )));
        }
        // `overwrite: false` opens with O_EXCL|O_NOFOLLOW, so a dangling
        // symlink planted at `out` after the exists() check above (or one
        // the check missed) is refused rather than written through.
        crate::utils::helpers::write_file_no_follow(Path::new(&out), &content, false)?;
        output::success(&format!(
            "Downloaded attachment '{attachment_name}' to '{out}' ({})",
            format_size(content.len() as u64)
        ));
        return Ok(());
    }

    let listed = attachments::list_attachments(files, &vault, &secret_name).await?;
    if listed.is_empty() {
        output::info(&format!("No attachments on secret '{secret_name}'"));
        return Ok(());
    }
    let prefix = attachments::attachment_prefix(&secret_name);
    for f in &listed {
        let short = f.name.strip_prefix(&prefix).unwrap_or(&f.name);
        println!(
            "{short}\t{}\t{}",
            format_size(f.size),
            f.last_modified.format("%Y-%m-%d %H:%M")
        );
    }
    println!("{} attachment(s) on '{secret_name}'", listed.len());
    Ok(())
}

pub(crate) async fn execute_detach(
    secret: String,
    name: String,
    force: bool,
    config: Config,
) -> Result<()> {
    let (backend, vault, secret_name) = resolve(&secret, &config, TargetMode::Write).await?;
    validate_attachment_name(&name)?;
    if !confirm_destructive(
        force,
        &format!("Remove attachment '{name}' from secret '{secret_name}'?"),
    )? {
        output::info("Aborted; attachment not removed.");
        return Ok(());
    }
    let files = backend.files().expect("resolve gated on files()");
    files
        .delete_file(
            &vault,
            &attachments::attachment_blob_name(&secret_name, &name),
        )
        .await
        .map_err(CrosstacheError::from)?;
    output::success(&format!("Detached '{name}' from '{secret_name}'"));
    Ok(())
}
