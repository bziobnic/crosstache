//! CLI executors for `xv scan` and its subcommands.

use crate::cli::commands::ScanCommands;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::scan::engine::MatchEngine;
use crate::scan::finding::Finding;
use crate::scan::orchestrator::{fetch_secret_values, scan_paths};
use crate::scan::patterns::builtin_patterns;
use crate::scan::walker::{walk, WalkConfig};
use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_scan_command(
    paths: Vec<PathBuf>,
    staged: bool,
    _all: bool,
    hook: bool,
    all_vaults: bool,
    command: Option<ScanCommands>,
    format: crate::utils::format::OutputFormat,
    config: Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    if let Some(cmd) = command {
        return match cmd {
            ScanCommands::Install { force } => execute_scan_install(force, &config).await,
            ScanCommands::Uninstall => execute_scan_uninstall(&config).await,
        };
    }
    if staged {
        return execute_scan_staged(hook, all_vaults, format, &config, registry).await;
    }
    execute_scan_paths(paths, hook, all_vaults, format, &config, registry).await
}

async fn execute_scan_paths(
    paths: Vec<PathBuf>,
    hook: bool,
    all_vaults: bool,
    format: crate::utils::format::OutputFormat,
    config: &Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    use crate::secret::manager::SecretManager;

    let auth_provider = crate::cli::helpers::get_azure_auth_provider(registry, config)?;
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Pick which vaults to fetch from.
    let vault_names: Vec<String> = if all_vaults {
        let auth = crate::cli::helpers::get_azure_auth_provider(registry, config)?;
        let vault_manager = crate::vault::manager::VaultManager::new(
            auth,
            config.subscription_id.clone(),
            config.no_color,
        )?;
        vault_manager
            .vault_ops()
            .list_vaults(Some(&config.subscription_id), None)
            .await?
            .into_iter()
            .map(|v| v.name)
            .collect()
    } else {
        vec![config.resolve_vault_name(None).await?]
    };

    let progress = crate::utils::interactive::ProgressIndicator::new("Fetching secret values...");
    let secrets = fetch_secret_values(&secret_manager, &vault_names, 10).await?;
    progress.finish_clear();

    let patterns = builtin_patterns();
    let engine = MatchEngine::new(&secrets, &patterns);

    // Build the path list. Apply [scan].exclude from .xv.toml if found.
    let mut walk_cfg = WalkConfig::default();
    if let Ok(Some((_, project))) =
        crate::config::project::find_project_config(&std::env::current_dir()?).await
    {
        if let Some(scan) = &project.scan {
            walk_cfg.extra_excludes = scan.exclude.clone();
        }
    }
    let path_refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    let walked = walk(&path_refs, &walk_cfg)?;
    let outcome = scan_paths(&walked, &engine)?;

    // Fail loud in hook/CI mode if any file could not be scanned — an
    // unscanned file could conceal a leak, so silence is unacceptable there.
    if outcome.skipped_count() > 0 {
        if hook {
            return Err(crate::scan::orchestrator::skipped_files_error(&outcome));
        }
        for (p, sz) in &outcome.skipped_too_large {
            eprintln!(
                "xv scan: skipped {} (too large: {sz} bytes, cap {})",
                p.display(),
                crate::scan::orchestrator::MAX_SCAN_FILE_SIZE
            );
        }
        for p in &outcome.skipped_unreadable {
            eprintln!("xv scan: skipped {} (unreadable)", p.display());
        }
    }

    render_findings(&outcome.findings, hook, format)
}

async fn execute_scan_staged(
    hook: bool,
    all_vaults: bool,
    format: crate::utils::format::OutputFormat,
    config: &Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    use crate::scan::staged::scan_staged;
    use crate::secret::manager::SecretManager;

    let auth_provider = crate::cli::helpers::get_azure_auth_provider(registry, config)?;
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    let vault_names: Vec<String> = if all_vaults {
        let auth = crate::cli::helpers::get_azure_auth_provider(registry, config)?;
        let vault_manager = crate::vault::manager::VaultManager::new(
            auth,
            config.subscription_id.clone(),
            config.no_color,
        )?;
        vault_manager
            .vault_ops()
            .list_vaults(Some(&config.subscription_id), None)
            .await?
            .into_iter()
            .map(|v| v.name)
            .collect()
    } else {
        vec![config.resolve_vault_name(None).await?]
    };

    let progress = crate::utils::interactive::ProgressIndicator::new("Fetching secret values...");
    let secrets = fetch_secret_values(&secret_manager, &vault_names, 10).await?;
    progress.finish_clear();

    let patterns = builtin_patterns();
    let engine = MatchEngine::new(&secrets, &patterns);
    let findings = scan_staged(&engine)?;

    render_findings(&findings, hook, format)
}

async fn execute_scan_install(force: bool, _config: &Config) -> Result<()> {
    use crate::scan::installer::{install, HookInstallStatus};
    match install(force)? {
        HookInstallStatus::Installed(path) => {
            crate::utils::output::success(&format!(
                "Installed pre-commit hook at {}",
                path.display()
            ));
        }
        HookInstallStatus::AlreadyInstalled(path) => {
            crate::utils::output::info(&format!("Hook already installed at {}", path.display()));
        }
    }
    Ok(())
}

async fn execute_scan_uninstall(_config: &Config) -> Result<()> {
    use crate::scan::installer::{uninstall, HookUninstallStatus};
    match uninstall()? {
        HookUninstallStatus::Removed(path) => {
            crate::utils::output::success(&format!(
                "Removed pre-commit hook at {}",
                path.display()
            ));
        }
        HookUninstallStatus::NotPresent => {
            crate::utils::output::info("No pre-commit hook to remove");
        }
    }
    Ok(())
}

fn render_findings(
    findings: &[Finding],
    hook: bool,
    format: crate::utils::format::OutputFormat,
) -> Result<()> {
    use crate::utils::format::OutputFormat;
    let resolved = format.resolve_for_stdout();

    if matches!(resolved, OutputFormat::Json | OutputFormat::Yaml) {
        let rendered = match resolved {
            OutputFormat::Json => serde_json::to_string_pretty(findings).unwrap_or_default(),
            OutputFormat::Yaml => serde_yaml::to_string(findings).unwrap_or_default(),
            _ => unreachable!(),
        };
        println!("{rendered}");
    } else {
        for f in findings {
            let secret = f.secret_name.as_deref().unwrap_or("(no secret)");
            let vault = f.vault.as_deref().unwrap_or("");
            eprintln!(
                "{}:{}:{}: matches {} (kind={:?}, severity={:?}{})",
                f.file.display(),
                f.line,
                f.col,
                secret,
                f.kind,
                f.severity,
                if vault.is_empty() {
                    String::new()
                } else {
                    format!(", vault={vault}")
                }
            );
        }
    }

    if !findings.is_empty() {
        return Err(CrosstacheError::scan_leak_detected(findings.len()));
    }
    if !hook {
        eprintln!("xv scan: no findings.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::finding::{FindingKind, Severity};
    use crate::utils::format::OutputFormat;

    #[test]
    fn render_no_findings_returns_ok() {
        let result = render_findings(&[], true, OutputFormat::Plain);
        assert!(result.is_ok());
    }

    #[test]
    fn render_findings_returns_scan_leak_detected() {
        let f = Finding {
            file: std::path::PathBuf::from("x"),
            line: 1,
            col: 1,
            secret_name: Some("S".to_string()),
            vault: Some("v".to_string()),
            kind: FindingKind::SecretValue,
            severity: Severity::Critical,
        };
        let result = render_findings(&[f], true, OutputFormat::Json);
        match result {
            Err(crate::error::CrosstacheError::ScanLeakDetected { count }) => {
                assert_eq!(count, 1);
            }
            other => panic!("expected ScanLeakDetected, got {other:?}"),
        }
    }
}
