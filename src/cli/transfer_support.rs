//! Shared generic-command transfer policy. Detection and identity checks are read-only.
use crate::backend::{Backend, BackendError};
use crate::error::{CrosstacheError, Result};

#[derive(Debug, Clone, Default, clap::Args)]
pub struct AttachmentTransferOptions {
    /// Include attachments using the recoverable transfer engine
    #[arg(long)]
    pub with_attachments: bool,
    /// Assert that other writers to both endpoints have stopped
    #[arg(long)]
    pub offline: bool,
    /// Expected destination active attachment key ID
    #[arg(long)]
    pub to_key_id: Option<String>,
    /// Directory for durable transfer recovery records
    #[arg(long)]
    pub recovery_dir: Option<std::path::PathBuf>,
}

/// Physical identities protect even unattached, feature-disabled moves. A failed
/// namespace lookup must not grant permission for a potentially aliased overwrite.
pub(crate) async fn reject_self_target(
    source: &dyn Backend,
    source_vault: &str,
    source_name: &str,
    destination: &dyn Backend,
    destination_vault: &str,
    destination_name: &str,
) -> Result<()> {
    // Local secret reads acquire an existing-directory lock, so an absent vault
    // otherwise becomes an opaque lock error before attachment preflight.
    if destination.kind() == crate::backend::BackendKind::Local {
        if let Some(vaults) = destination.vaults() {
            match vaults.get_vault(destination_vault, None).await {
                Ok(_) => {},
                Err(BackendError::VaultNotFound { .. }) => return Err(CrosstacheError::config(format!(
                    "destination vault '{destination_vault}' must already exist; create it first, and for attachments run xv attachment-key initialize --vault {destination_vault} --apply --offline, then supply --to-key-id"
                ))),
                Err(error) => return Err(error.into()),
            }
        }
    }
    let source_props = source
        .secrets()
        .get_secret(source_vault, source_name, false)
        .await?;
    if source_props.name != source_name && source_props.original_name != source_name {
        return Err(CrosstacheError::conflict(
            "attachment transfer requires the exact stored source name; aliases are refused",
        ));
    }
    let destination_props = match destination
        .secrets()
        .get_secret(destination_vault, destination_name, false)
        .await
    {
        Ok(props) => Some(props),
        Err(BackendError::NotFound { .. } | BackendError::VaultNotFound { .. }) => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(props) = &destination_props {
        if props.name != destination_name && props.original_name != destination_name {
            return Err(CrosstacheError::conflict("attachment transfer requires the exact stored destination name; aliases are refused"));
        }
    }
    let same_name = source_name == destination_name
        || destination_props
            .as_ref()
            .is_some_and(|p| p.name == source_props.name);
    if same_name {
        let source_namespace = source
            .transfer_secret_physical_namespace(source_vault)
            .await?;
        let destination_namespace = destination
            .transfer_secret_physical_namespace(destination_vault)
            .await?;
        if source_namespace == destination_namespace {
            return Err(CrosstacheError::conflict(
                "source and destination identify the same physical secret; no changes were written",
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "file-ops")]
pub(crate) fn folder_override(folder: &str) -> Result<String> {
    if folder == "/" {
        return Ok(folder.into());
    }
    let folder = folder.trim().trim_end_matches('/');
    crate::utils::helpers::validate_folder_path(folder)?;
    if folder.chars().any(char::is_control)
        || folder
            .split('/')
            .any(|p| p == "." || p == ".." || p.trim() != p)
    {
        return Err(CrosstacheError::invalid_argument(
            "destination folder must use canonical folder components",
        ));
    }
    Ok(folder.into())
}

#[cfg(feature = "file-ops")]
pub(crate) async fn run_attached(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: crate::secret::attachment_transfer::TransferIntent,
    options: &AttachmentTransferOptions,
    dry_run: bool,
) -> Result<()> {
    use crate::secret::attachment_transfer_execution::{self as execution, RecoveryStore};
    let preview = execution::preflight(source, destination, intent.clone()).await?;
    if dry_run {
        println!("{}", serde_json::to_string_pretty(&preview)?);
        return Ok(());
    }
    if !options.offline {
        return Err(CrosstacheError::invalid_argument("attachment execution requires --offline after stopping other writers; use --dry-run to preview"));
    }
    let recovery = RecoveryStore::new(match &options.recovery_dir {
        Some(path) => path.clone(),
        None => RecoveryStore::default_path()?,
    });
    let report = execution::apply(source, destination, intent, options.offline, &recovery).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
