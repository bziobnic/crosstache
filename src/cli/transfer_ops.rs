//! Attachment transfer previews, offline execution, and recovery.

use crate::backend::BackendRegistry;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};

#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("transfer_execution").args(["apply", "resume"])))]
pub struct TransferOptions {
    /// Source secret name
    pub name: String,
    /// Source vault or workspace alias
    #[arg(long)]
    pub from: String,
    /// Destination vault or workspace alias
    #[arg(long)]
    pub to: String,
    /// Destination secret name (defaults to the source name)
    #[arg(long)]
    pub new_name: Option<String>,
    /// Plan a move, removing the source only after verified transfer
    #[arg(long = "move")]
    pub move_source: bool,
    /// Expected destination active attachment key ID for cross-vault transfers
    #[arg(long)]
    pub to_key_id: Option<String>,
    /// Destination folder override; / explicitly clears to root
    #[arg(long)]
    pub to_folder: Option<String>,
    /// Apply the transfer after verifying source and destination
    #[arg(long, requires = "offline")]
    pub apply: bool,
    /// Assert that other writers to both endpoints have stopped
    #[arg(long, requires = "transfer_execution")]
    pub offline: bool,
    /// Resume the saved transfer with this operation ID and the same intent
    #[arg(long, requires = "offline")]
    pub resume: Option<String>,
    /// Recovery directory (overrides XV_TRANSFER_RECOVERY_DIR and the safe default)
    #[arg(long)]
    pub recovery_dir: Option<std::path::PathBuf>,
}

pub(crate) async fn execute(
    options: TransferOptions,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use crate::secret::attachment_transfer::{TransferEndpoint, TransferIntent, TransferOperation};
    use crate::secret::attachment_transfer_execution::{self as execution, RecoveryStore};
    let rebuilt;
    let registry = match registry {
        Some(registry) => registry,
        None => {
            rebuilt = BackendRegistry::from_config(&config)
                .map_err(|error| CrosstacheError::config(error.to_string()))?;
            &rebuilt
        }
    };
    let (workspace, workspace_registry) =
        super::helpers::resolve_workspace_and_registry(&config).await?;
    let (source, source_backend, source_vault) = super::helpers::resolve_vault_ref_with_workspace(
        &options.from,
        workspace.as_ref(),
        workspace_registry.as_ref(),
        registry,
        &config,
    )
    .await?;
    let (destination, destination_backend, destination_vault) =
        super::helpers::resolve_vault_ref_with_workspace(
            &options.to,
            workspace.as_ref(),
            workspace_registry.as_ref(),
            registry,
            &config,
        )
        .await?;
    let intent = TransferIntent {
        source: TransferEndpoint {
            identity: source_backend,
            vault: source_vault,
        },
        destination: TransferEndpoint {
            identity: destination_backend,
            vault: destination_vault,
        },
        destination_name: options.new_name.unwrap_or_else(|| options.name.clone()),
        source_name: options.name,
        operation: if options.move_source {
            TransferOperation::Move
        } else {
            TransferOperation::Copy
        },
        destination_key_id: options.to_key_id,
        destination_folder: options
            .to_folder
            .as_deref()
            .map(super::transfer_support::folder_override)
            .transpose()?,
    };
    if options.apply || options.resume.is_some() {
        let root = match options.recovery_dir {
            Some(path) => path,
            None => RecoveryStore::default_path()?,
        };
        let recovery = RecoveryStore::new(root);
        let report = if let Some(id) = options.resume {
            execution::resume(
                source.as_ref(),
                destination.as_ref(),
                intent,
                &id,
                options.offline,
                &recovery,
            )
            .await?
        } else {
            execution::apply(
                source.as_ref(),
                destination.as_ref(),
                intent,
                options.offline,
                &recovery,
            )
            .await?
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let preview = execution::preview(source.as_ref(), destination.as_ref(), intent).await?;
        println!("{}", serde_json::to_string_pretty(&preview)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cli::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn generic_transfer_options_and_saved_folder_parse() {
        for verb in ["copy", "move"] {
            assert!(Cli::try_parse_from([
                "xv",
                verb,
                "cert",
                "--from",
                "a",
                "--to",
                "b",
                "--with-attachments",
                "--offline",
                "--to-key-id",
                "key",
                "--recovery-dir",
                "recovery"
            ])
            .is_ok());
        }
        assert!(Cli::try_parse_from([
            "xv",
            "mv",
            "a:cert",
            "b:folder/",
            "--with-attachments",
            "--dry-run",
            "--to-key-id",
            "key"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "xv",
            "migrate",
            "--from",
            "local:a",
            "--to",
            "local:b",
            "--with-attachments",
            "--offline",
            "--to-key-id",
            "key"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "xv",
            "transfer",
            "cert",
            "--from",
            "a",
            "--to",
            "b",
            "--to-folder",
            "/"
        ])
        .is_ok());
    }

    #[test]
    fn transfer_preview_parses_explicit_endpoints_and_binding() {
        let cli = Cli::try_parse_from([
            "xv",
            "transfer",
            "cert",
            "--from",
            "work",
            "--to",
            "stage",
            "--new-name",
            "certificate",
            "--move",
            "--to-key-id",
            "expected",
        ])
        .unwrap();
        let Commands::Transfer { options } = cli.command else {
            panic!("wrong command")
        };
        assert_eq!(options.name, "cert");
        assert_eq!(options.new_name.as_deref(), Some("certificate"));
        assert_eq!(options.to_key_id.as_deref(), Some("expected"));
        assert!(options.move_source);
    }

    #[test]
    fn transfer_apply_requires_explicit_offline_and_resume_uses_existing_intent() {
        let base = [
            "xv",
            "transfer",
            "cert",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "renamed",
            "--move",
        ];
        let mut args = base.to_vec();
        args.push("--apply");
        assert!(Cli::try_parse_from(&args).is_err());
        args.push("--offline");
        assert!(Cli::try_parse_from(&args).is_ok());
        let mut resume = base.to_vec();
        resume.extend([
            "--resume",
            "d38ee960-1d83-4db8-9b48-6a4dc395e606",
            "--offline",
        ]);
        assert!(Cli::try_parse_from(&resume).is_ok());
        let mut invalid = base.to_vec();
        invalid.push("--offline");
        assert!(Cli::try_parse_from(&invalid).is_err());
    }

    #[test]
    fn transfer_preview_requires_both_endpoints_and_apply_requires_offline() {
        assert!(Cli::try_parse_from(["xv", "transfer", "cert", "--from", "work"]).is_err());
        assert!(Cli::try_parse_from([
            "xv", "transfer", "cert", "--from", "work", "--to", "stage", "--apply",
        ])
        .is_err());
    }
}
