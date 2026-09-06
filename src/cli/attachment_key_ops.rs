//! Attachment-key observations and explicit offline lifecycle operations.

use clap::Subcommand;
use serde::Serialize;

use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::attachment_key::AttachmentKeyId;
use crate::secret::{
    attachment_backup, attachment_backup_codec as codec, attachment_inventory,
    attachment_lifecycle, attachment_restore,
};
use crate::utils::format::{sanitize_control_chars, OutputFormat};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

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
    /// Export verified keys and a current-file manifest encrypted to an independent age recipient
    Export {
        #[arg(long)]
        vault: Option<String>,
        #[arg(long)]
        recipient: String,
        #[arg(long)]
        output: PathBuf,
        /// Acknowledge all source writers and older clients are stopped
        #[arg(long, required = true)]
        offline: bool,
    },
    /// Preview recovery from an encrypted key bundle; file payloads must be restored separately
    Restore {
        #[arg(long)]
        vault: Option<String>,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        identity_file: PathBuf,
        /// Explicitly permit repairing a malformed pointer after all keys and files verify
        #[arg(long)]
        repair_pointer: bool,
        #[command(flatten)]
        action: ApplyOptions,
    },
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
        | AttachmentKeyCommands::Restore { action, .. }
            if action.apply && !action.offline =>
        {
            return Err(CrosstacheError::invalid_argument(
                "Applying requires --offline and stopped writers.",
            ));
        }
        _ => {}
    }
    // Validate recovery inputs before credentials or provider access. Private
    // state remains in these non-Debug locals and never reaches report rendering.
    let export_recipient = match &command {
        AttachmentKeyCommands::Export {
            recipient,
            output,
            offline,
            ..
        } => {
            if !offline {
                return Err(CrosstacheError::invalid_argument(
                    "Export requires --offline and stopped writers.",
                ));
            }
            if output.as_os_str().is_empty() || output.file_name().is_none() {
                return Err(CrosstacheError::invalid_argument(
                    "Choose a new output file for the encrypted backup.",
                ));
            }
            match std::fs::symlink_metadata(output) {
                Ok(_) => {
                    return Err(CrosstacheError::conflict(
                        "Backup output already exists; choose a new path.",
                    ))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            Some(recipient.parse::<age::x25519::Recipient>().map_err(|_| {
                CrosstacheError::invalid_argument("Expected an X25519 age recovery recipient.")
            })?)
        }
        _ => None,
    };
    let restore_bundle = match &command {
        AttachmentKeyCommands::Restore {
            input,
            identity_file,
            ..
        } => {
            let identity = read_recovery_identity(identity_file)?;
            let bytes = read_bounded(input, codec::MAX_BUNDLE_BYTES)?;
            Some(codec::decrypt(&bytes, &identity)?)
        }
        _ => None,
    };
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
        | AttachmentKeyCommands::Recover { vault, .. }
        | AttachmentKeyCommands::Export { vault, .. }
        | AttachmentKeyCommands::Restore { vault, .. } => vault.as_deref(),
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
        AttachmentKeyCommands::Export { output, .. } => {
            let files = backend.files().ok_or_else(|| {
                crate::cli::file_ops::file_storage_unsupported_error(backend.as_ref())
            })?;
            let bundle = attachment_backup::collect(
                backend.attachment_keys().as_ref(),
                files,
                &backend_name,
                &vault,
            )
            .await?;
            let encrypted = codec::encrypt(
                &bundle,
                export_recipient
                    .as_ref()
                    .expect("validated export recipient"),
            )?;
            write_backup(&output, &encrypted)?;
            #[derive(Serialize)]
            struct ExportReport<'a> {
                schema_version: u32,
                operation: &'static str,
                outcome: &'static str,
                scope: &'static str,
                identities: usize,
                files: usize,
                active_key_id: &'a str,
                legacy_key_id: Option<&'a str>,
                key_ids: Vec<&'a str>,
                verified_files: Vec<&'a str>,
                source_references: &'a [codec::SourceRef],
            }
            render(
                &Envelope {
                    backend: &backend_name,
                    vault: &vault,
                    report: ExportReport {
                        schema_version: 1,
                        operation: "export",
                        outcome: "exported",
                        scope: "visible_current_files",
                        identities: bundle.identities.len(),
                        files: bundle.files.len(),
                        active_key_id: &bundle.active_key_id,
                        legacy_key_id: bundle.legacy_key_id.as_deref(),
                        key_ids: bundle
                            .identities
                            .iter()
                            .map(|r| r.key_id.as_str())
                            .collect(),
                        verified_files: bundle.files.iter().map(|f| f.name.as_str()).collect(),
                        source_references: &bundle.references,
                    },
                },
                format,
            )
        }
        AttachmentKeyCommands::Restore {
            action,
            repair_pointer,
            ..
        } => {
            let files = backend.files().ok_or_else(|| {
                crate::cli::file_ops::file_storage_unsupported_error(backend.as_ref())
            })?;
            let report = attachment_restore::restore(
                backend.attachment_keys().as_ref(),
                files,
                &vault,
                restore_bundle.as_ref().expect("validated recovery bundle"),
                action.apply,
                repair_pointer,
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

fn read_bounded(path: &Path, max: usize) -> Result<Zeroizing<Vec<u8>>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(CrosstacheError::invalid_argument(
            "Recovery input exceeds its size limit.",
        ));
    }
    Ok(bytes)
}

fn read_recovery_identity(path: &Path) -> Result<age::x25519::Identity> {
    let bytes = read_bounded(path, 64 * 1024)?;
    let invalid = || {
        CrosstacheError::invalid_argument(
            "Recovery identity file must contain exactly one X25519 age private key.",
        )
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'));
    let identity = lines
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    if lines.next().is_some() {
        return Err(invalid());
    }
    Ok(identity)
}

fn write_backup(path: &Path, encrypted: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(encrypted)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|e| {
        if e.error.kind() == std::io::ErrorKind::AlreadyExists {
            CrosstacheError::conflict("Backup output already exists; choose a new path.")
        } else {
            CrosstacheError::from(e.error)
        }
    })?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
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

#[cfg(test)]
mod backup_cli_tests {
    use super::{read_recovery_identity, write_backup};
    use crate::cli::commands::Cli;
    use clap::Parser;

    #[test]
    fn backup_restore_cli_requires_explicit_offline_writes() {
        let export = [
            "xv",
            "attachment-key",
            "export",
            "--recipient",
            "age1test",
            "--output",
            "keys.age",
        ];
        assert!(Cli::try_parse_from(export).is_err());
        assert!(Cli::try_parse_from(export.into_iter().chain(["--offline"])).is_ok());
        let restore = [
            "xv",
            "attachment-key",
            "restore",
            "--input",
            "keys.age",
            "--identity-file",
            "key.txt",
        ];
        assert!(Cli::try_parse_from(restore).is_ok());
        assert!(Cli::try_parse_from(restore.into_iter().chain(["--apply"])).is_err());
        assert!(Cli::try_parse_from(restore.into_iter().chain([
            "--apply",
            "--offline",
            "--repair-pointer"
        ]))
        .is_ok());
    }

    #[test]
    fn backup_file_io_never_overwrites_or_echoes_private_input() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("bundle.age");
        write_backup(&out, b"ciphertext").unwrap();
        assert!(write_backup(&out, b"replacement").is_err());
        assert_eq!(std::fs::read(&out).unwrap(), b"ciphertext");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temporary ciphertext file is removed on error"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let secret = dir.path().join("invalid-key");
        std::fs::write(&secret, "PRIVATE-INVALID-IDENTITY").unwrap();
        let error = read_recovery_identity(&secret).err().unwrap();
        assert!(!format!("{error:?}").contains("PRIVATE-INVALID-IDENTITY"));
        std::fs::write(&secret, vec![b'x'; 64 * 1024 + 1]).unwrap();
        assert!(read_recovery_identity(&secret).is_err());
    }
}
