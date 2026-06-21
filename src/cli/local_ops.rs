//! Local backend maintenance command handlers (`xv local ...`).

use crate::backend::local::LocalBackend;
use crate::cli::commands::LocalCommands;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::output;

pub(crate) async fn execute_local_command(command: LocalCommands, config: Config) -> Result<()> {
    match command {
        LocalCommands::EncryptMetadata { dry_run } => {
            execute_encrypt_metadata(dry_run, config).await
        }
        LocalCommands::Migrate { dry_run } => execute_migrate(dry_run, config).await,
    }
}

async fn execute_encrypt_metadata(dry_run: bool, config: Config) -> Result<()> {
    // This command only makes sense for the local backend.
    if config.effective_backend_name() != "local" {
        return Err(CrosstacheError::config(format!(
            "`xv local encrypt-metadata` only applies to the local backend, but the active \
             backend is '{}'. Set backend = \"local\" (or pass --backend local) to use it.",
            config.effective_backend_name()
        )));
    }

    let backend = LocalBackend::new(config.local.as_ref())
        .map_err(|e| CrosstacheError::config(format!("failed to open local backend: {e}")))?;

    if !backend.encrypt_metadata_enabled() {
        output::warn(
            "Metadata encryption is not enabled. New writes will still store metadata as \
             plaintext.\n  Set `encrypt_metadata = true` under [local] in your config first, \
             then re-run this command\n  so existing secrets and all future writes are \
             encrypted consistently.",
        );
        return Err(CrosstacheError::config(
            "encrypt_metadata is false under [local]; enable it before migrating".to_string(),
        ));
    }

    if dry_run {
        let (would_convert, already) = backend
            .reencrypt_all_metadata(true)
            .map_err(|e| CrosstacheError::config(format!("scan failed: {e}")))?;
        output::info(&format!(
            "Dry run: {would_convert} plaintext metadata file(s) would be encrypted; \
             {already} already encrypted (left as-is)."
        ));
        return Ok(());
    }

    let (converted, already) = backend
        .reencrypt_all_metadata(false)
        .map_err(|e| CrosstacheError::config(format!("re-encryption failed: {e}")))?;

    if converted == 0 {
        output::success(&format!(
            "Nothing to do: all {already} metadata file(s) are already encrypted."
        ));
    } else {
        output::success(&format!(
            "Encrypted {converted} metadata file(s) at rest ({already} already encrypted). \
             Secret names remain visible as on-disk filenames."
        ));
    }
    Ok(())
}

async fn execute_migrate(dry_run: bool, config: Config) -> Result<()> {
    // This command only makes sense for the local backend.
    if config.effective_backend_name() != "local" {
        return Err(CrosstacheError::config(format!(
            "`xv local migrate` only applies to the local backend, but the active backend is \
             '{}'. Set backend = \"local\" (or pass --backend local) to use it.",
            config.effective_backend_name()
        )));
    }

    let backend = LocalBackend::new(config.local.as_ref())
        .map_err(|e| CrosstacheError::config(format!("failed to open local backend: {e}")))?;

    if !backend.opaque_filenames_enabled() {
        output::warn(
            "Opaque filenames are not enabled. Secret names are still stored as on-disk \
             filenames.\n  Set `opaque_filenames = true` under [local] in your config first, \
             then re-run this command\n  so existing secrets and all future writes use opaque \
             stems.",
        );
        return Err(CrosstacheError::config(
            "opaque_filenames is false under [local]; enable it before migrating".to_string(),
        ));
    }

    let report = backend
        .migrate_all(dry_run)
        .map_err(|e| CrosstacheError::config(format!("migration failed: {e}")))?;

    if dry_run {
        if report.total() == 0 {
            output::info("Dry run: nothing to migrate; the store is already opaque.");
        } else {
            output::info(&format!(
                "Dry run: {} change(s) would be made:",
                report.total()
            ));
            for line in &report.plan {
                output::info(&format!("  {line}"));
            }
        }
        return Ok(());
    }

    if report.total() == 0 {
        output::success("Nothing to do: the store is already using opaque filenames.");
    } else {
        output::success(&format!(
            "Migrated to opaque filenames: {} secret(s), {} trash entry(ies); {} index \
             entry(ies) rebuilt. Directory listings no longer reveal secret names.",
            report.migrated, report.trash_migrated, report.recovered
        ));
    }
    Ok(())
}
