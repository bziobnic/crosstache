//! Read-only attachment transfer previews.

use crate::backend::BackendRegistry;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};

#[derive(Debug, clap::Args)]
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
}

pub(crate) async fn execute(
    options: TransferOptions,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use crate::secret::attachment_transfer::{
        self, TransferEndpoint, TransferIntent, TransferOperation,
    };
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
    };
    let plan = attachment_transfer::plan(source.as_ref(), destination.as_ref(), intent).await?;
    println!("{}", serde_json::to_string_pretty(&plan.preview())?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cli::{Cli, Commands};
    use clap::Parser;

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
    fn transfer_preview_requires_both_endpoints_and_has_no_apply_switch() {
        assert!(Cli::try_parse_from(["xv", "transfer", "cert", "--from", "work"]).is_err());
        assert!(Cli::try_parse_from([
            "xv", "transfer", "cert", "--from", "work", "--to", "stage", "--apply",
        ])
        .is_err());
    }
}
