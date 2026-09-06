//! Attachment-key observations and explicit offline lifecycle operations.

use clap::Subcommand;
use serde::Serialize;

use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::attachment_key::AttachmentKeyId;
use crate::secret::{attachment_inventory, attachment_lifecycle};
use crate::utils::format::{sanitize_control_chars, OutputFormat};

#[derive(Debug, clap::Args)]
pub struct ApplyOptions {
    /// Apply the previewed operation (requires all writers/old clients to be stopped)
    #[arg(long, requires = "offline")]
    apply: bool,
    /// Acknowledge that all writers and older clients are stopped for this vault
    #[arg(long)]
    offline: bool,
}

#[derive(Debug, Subcommand)]
pub enum AttachmentKeyCommands {
    /// List visible marked retained-key records without reading private values
    Keys {
        #[arg(long)]
        vault: Option<String>,
    },
    /// Preview V1-to-V2 upgrade preserving the original identity and old attachment reads
    Upgrade {
        #[arg(long)]
        vault: Option<String>,
        #[command(flatten)]
        action: ApplyOptions,
    },
    /// Preview pointer repair using existing retained keys; never generates a key
    Recover {
        #[arg(long)]
        vault: Option<String>,
        #[arg(long)]
        key_id: String,
        /// Explicit original legacy fallback key (required unless --no-legacy)
        #[arg(
            long,
            required_unless_present = "no_legacy",
            conflicts_with = "no_legacy"
        )]
        legacy_key_id: Option<String>,
        /// Explicitly declare this ring has no legacy fallback
        #[arg(long, conflicts_with = "legacy_key_id")]
        no_legacy: bool,
        #[command(flatten)]
        action: ApplyOptions,
    },
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
    match &command {
        AttachmentKeyCommands::Upgrade { action, .. }
        | AttachmentKeyCommands::Recover { action, .. }
            if action.apply && !action.offline =>
        {
            return Err(CrosstacheError::invalid_argument(
                "Applying requires --offline and stopped writers.",
            ));
        }
        _ => {}
    }
    // Validate caller-supplied identifiers before any provider access.
    let recovery_ids = if let AttachmentKeyCommands::Recover {
        key_id,
        legacy_key_id,
        no_legacy,
        ..
    } = &command
    {
        if legacy_key_id.is_some() == *no_legacy {
            return Err(CrosstacheError::invalid_argument(
                "Choose --legacy-key-id or --no-legacy explicitly.",
            ));
        }
        Some((
            parse_id(key_id)?,
            legacy_key_id.as_deref().map(parse_id).transpose()?,
        ))
    } else {
        None
    };
    let vault_override = match &command {
        AttachmentKeyCommands::Status { vault }
        | AttachmentKeyCommands::Inventory { vault }
        | AttachmentKeyCommands::Keys { vault }
        | AttachmentKeyCommands::Upgrade { vault, .. }
        | AttachmentKeyCommands::Recover { vault, .. } => vault.as_deref(),
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
        AttachmentKeyCommands::Keys { .. } => {
            #[derive(Serialize)]
            struct RetainedReport {
                schema_version: u32,
                observation: &'static str,
                keys: Vec<crate::backend::attachment_keys::RetainedKeySummary>,
            }
            let report = RetainedReport {
                schema_version: 1,
                observation: "visible_retained_records",
                keys: backend.attachment_keys().list_retained_keys(&vault).await?,
            };
            render(
                &Envelope {
                    backend: &backend_name,
                    vault: &vault,
                    report,
                },
                format,
            )
        }
        AttachmentKeyCommands::Upgrade { action, .. } => {
            let report = attachment_lifecycle::upgrade(
                backend.attachment_keys().as_ref(),
                &vault,
                action.apply,
            )
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
        AttachmentKeyCommands::Recover { action, .. } => {
            let (active, legacy) = recovery_ids.expect("recover identifiers were validated");
            let report = attachment_lifecycle::recover(
                backend.attachment_keys().as_ref(),
                &vault,
                &active,
                legacy.as_ref(),
                action.apply,
            )
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

fn parse_id(raw: &str) -> Result<AttachmentKeyId> {
    AttachmentKeyId::parse(raw).ok_or_else(|| {
        CrosstacheError::invalid_argument(
            "Expected an attachment key ID: ak1- followed by 64 lowercase hexadecimal digits.",
        )
    })
}
