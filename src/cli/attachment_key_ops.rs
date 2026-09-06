//! Read-only attachment key observations, scoped to the current backend/vault.

use clap::Subcommand;
use serde::Serialize;

use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::attachment_inventory;
use crate::utils::format::{sanitize_control_chars, OutputFormat};

#[derive(Debug, Subcommand)]
pub enum AttachmentKeyCommands {
    /// Inspect the current pointer and validate its active identity; never initializes keys
    Status {
        /// Workspace alias or literal vault on the effective backend
        #[arg(long)]
        vault: Option<String>,
    },
    /// Inventory all file metadata references; does not verify ciphertext or retirement safety
    Inventory {
        /// Workspace alias or literal vault on the effective backend
        #[arg(long)]
        vault: Option<String>,
    },
}

#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    backend: &'a str,
    vault: &'a str,
    report: T,
}

pub(crate) async fn execute(command: AttachmentKeyCommands, config: Config) -> Result<()> {
    let format = config.runtime_output_format.resolve_for_stdout();
    if matches!(format, OutputFormat::Csv | OutputFormat::Template) {
        return Err(CrosstacheError::invalid_argument(
            "attachment-key reports support JSON, YAML, or human-readable output; use --format json",
        ));
    }
    let vault_override = match &command {
        AttachmentKeyCommands::Status { vault } | AttachmentKeyCommands::Inventory { vault } => {
            vault.as_deref()
        }
    };
    let (backend, backend_name, vault) = match vault_override {
        None => crate::cli::vault_ops::resolve_current_vault(&config, None).await?,
        Some(raw) => {
            let ws = crate::workspace::resolve_configured_workspace(&config).await?;
            let (name, vault) =
                crate::cli::helpers::vault_ref_cache_identity(raw, ws.as_ref(), &config);
            let registry =
                crate::backend::BackendRegistry::with_lazy(&config, std::slice::from_ref(&name))?;
            let backend = registry.materialize(&name)?;
            (backend, name, vault)
        }
    };
    match command {
        AttachmentKeyCommands::Status { .. } => {
            let report =
                attachment_inventory::key_status(backend.attachment_keys().as_ref(), &vault)
                    .await?;
            render(
                &Envelope {
                    backend: &backend_name,
                    vault: &vault,
                    report,
                },
                format,
            )
        }
        AttachmentKeyCommands::Inventory { .. } => {
            let files = backend.files().ok_or_else(|| {
                crate::cli::file_ops::file_storage_unsupported_error(backend.as_ref())
            })?;
            let report = attachment_inventory::file_inventory(files, &vault).await?;
            render(
                &Envelope {
                    backend: &backend_name,
                    vault: &vault,
                    report,
                },
                format,
            )
        }
    }
}

fn render<T: Serialize>(report: &T, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(report)?),
        OutputFormat::Yaml => print!("{}", serde_yaml::to_string(report)?),
        _ => {
            // YAML preserves the nested report structure in a readable form;
            // its quoted scalars plus control escaping prevent terminal injection.
            print!(
                "{}",
                sanitize_control_chars(&serde_yaml::to_string(report)?)
            );
        }
    }
    Ok(())
}
