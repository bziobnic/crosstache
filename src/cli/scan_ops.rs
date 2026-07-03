//! CLI executors for `xv scan` and its subcommands.

use crate::cli::commands::ScanCommands;
use crate::config::project::ScanConfig;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::scan::engine::{MatchEngine, DEFAULT_MIN_VALUE_LENGTH};
use crate::scan::finding::Finding;
use crate::scan::orchestrator::{fetch_secret_values, scan_paths};
use crate::scan::patterns::{builtin_patterns, BuiltinPattern};
use crate::scan::walker::{build_exclude_set, walk, WalkConfig};
use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_scan_command(
    paths: Vec<PathBuf>,
    staged: bool,
    all: bool,
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

    if scan_disabled_via_env() {
        eprintln!("xv scan: disabled via XV_SCAN_DISABLE; skipping scan.");
        return Ok(());
    }

    if staged {
        return execute_scan_staged(hook, all_vaults, format, &config, registry).await;
    }
    if all {
        return execute_scan_head(hook, all_vaults, format, &config, registry).await;
    }
    execute_scan_paths(paths, hook, all_vaults, format, &config, registry).await
}

/// `XV_SCAN_DISABLE=1` (or `true`, case-insensitive) bypasses scanning
/// entirely — an escape hatch for emergencies, documented in `docs/scan.md`.
/// Checked once, uniformly, across every scan mode (paths, `--staged`,
/// `--all`) so the pre-commit hook and local scans agree.
fn scan_disabled_via_env() -> bool {
    std::env::var("XV_SCAN_DISABLE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Load the effective `[scan]` block from the nearest `.xv.toml`, if any.
///
/// Shared by all three scan entry points (paths, `--staged`, `--all`) so
/// excludes, `min_value_length`, and the `patterns` allowlist are resolved
/// identically regardless of scan mode.
async fn load_scan_config() -> Result<Option<ScanConfig>> {
    let cwd = std::env::current_dir()?;
    match crate::config::project::find_project_config(&cwd).await {
        Ok(Some((_, project))) => Ok(project.scan),
        _ => Ok(None),
    }
}

/// Resolve the effective minimum secret-value length from `[scan].min_value_length`,
/// falling back to [`DEFAULT_MIN_VALUE_LENGTH`] when unset.
fn effective_min_value_length(scan_cfg: Option<&ScanConfig>) -> usize {
    scan_cfg
        .and_then(|s| s.min_value_length)
        .unwrap_or(DEFAULT_MIN_VALUE_LENGTH)
}

/// Resolve the effective exclude globs from `[scan].exclude` (empty when unset).
fn effective_excludes(scan_cfg: Option<&ScanConfig>) -> Vec<String> {
    scan_cfg.map(|s| s.exclude.clone()).unwrap_or_default()
}

/// Filter `builtin_patterns()` by `[scan].patterns` allowlist. An empty
/// allowlist enables all built-ins (per `docs/scan.md`). Unknown names in
/// the allowlist are warned about on stderr rather than silently ignored.
fn effective_patterns(scan_cfg: Option<&ScanConfig>) -> Vec<BuiltinPattern> {
    let all = builtin_patterns();
    let allowlist = match scan_cfg {
        Some(s) if !s.patterns.is_empty() => &s.patterns,
        _ => return all,
    };
    let known: std::collections::HashSet<&str> = all.iter().map(|p| p.name).collect();
    for name in allowlist {
        if !known.contains(name.as_str()) {
            eprintln!(
                "xv scan: warning: unknown pattern name '{name}' in [scan].patterns (ignored)"
            );
        }
    }
    all.into_iter()
        .filter(|p| allowlist.iter().any(|a| a == p.name))
        .collect()
}

/// Resolve the active backend and the set of vaults to scan, then fetch every
/// secret value across them through the backend trait.
///
/// Works on any backend (azure/local/aws). `--all-vaults` requires a backend
/// that can enumerate vaults (`Backend::vaults()`); backends without that
/// capability return a clear capability error instead of silently scanning a
/// single vault.
async fn fetch_scan_secrets(
    all_vaults: bool,
    config: &Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<Vec<crate::scan::engine::SecretRef>> {
    let reg = registry.ok_or_else(|| {
        CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        )
    })?;
    let backend = reg.active_arc();

    let vault_names: Vec<String> = if all_vaults {
        match backend.vaults() {
            Some(vaults) => vaults
                .list_vaults()
                .await
                .map_err(CrosstacheError::from)?
                .into_iter()
                .map(|v| v.name)
                .collect(),
            None => {
                return Err(CrosstacheError::invalid_argument(format!(
                    "--all-vaults is not supported on the {} backend (it cannot enumerate \
                     vaults). Scan a single vault instead.",
                    backend.name()
                )))
            }
        }
    } else {
        vec![config.resolve_vault_name(None).await?]
    };

    let progress = crate::utils::interactive::ProgressIndicator::new("Fetching secret values...");
    let secrets = fetch_secret_values(backend, &vault_names, 10).await?;
    progress.finish_clear();
    Ok(secrets)
}

async fn execute_scan_paths(
    paths: Vec<PathBuf>,
    hook: bool,
    all_vaults: bool,
    format: crate::utils::format::OutputFormat,
    config: &Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    let secrets = fetch_scan_secrets(all_vaults, config, registry).await?;

    let scan_cfg = load_scan_config().await?;
    let patterns = effective_patterns(scan_cfg.as_ref());
    let min_value_length = effective_min_value_length(scan_cfg.as_ref());
    let engine = MatchEngine::new(&secrets, &patterns, min_value_length);

    // Build the path list. Apply [scan].exclude from .xv.toml if found.
    let walk_cfg = WalkConfig {
        extra_excludes: effective_excludes(scan_cfg.as_ref()),
    };
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

    let secrets = fetch_scan_secrets(all_vaults, config, registry).await?;

    let scan_cfg = load_scan_config().await?;
    let patterns = effective_patterns(scan_cfg.as_ref());
    let min_value_length = effective_min_value_length(scan_cfg.as_ref());
    let engine = MatchEngine::new(&secrets, &patterns, min_value_length);

    // Apply the same [scan].exclude + default globs the filesystem walk and
    // `--all` head scan use, so the pre-commit hook (`scan --staged --hook`)
    // doesn't scan target/, node_modules/, or user-excluded staged paths.
    let excludes = build_exclude_set(&effective_excludes(scan_cfg.as_ref()))?;
    let findings = scan_staged(&engine, &excludes)?;

    render_findings(&findings, hook, format)
}

async fn execute_scan_head(
    hook: bool,
    all_vaults: bool,
    format: crate::utils::format::OutputFormat,
    config: &Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    use crate::scan::staged::scan_head;

    let secrets = fetch_scan_secrets(all_vaults, config, registry).await?;

    let scan_cfg = load_scan_config().await?;

    // Apply the same [scan].exclude + default globs the filesystem walk uses,
    // so `scan --all` doesn't scan target/, node_modules/, or user-excluded
    // committed paths that `scan .` would skip.
    let excludes = build_exclude_set(&effective_excludes(scan_cfg.as_ref()))?;

    let patterns = effective_patterns(scan_cfg.as_ref());
    let min_value_length = effective_min_value_length(scan_cfg.as_ref());
    let engine = MatchEngine::new(&secrets, &patterns, min_value_length);
    let findings = scan_head(&engine, &excludes)?;

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
    use std::sync::Mutex;

    /// Guards tests that mutate `XV_SCAN_DISABLE` so they don't race each
    /// other under cargo's default parallel test runner (mirrors the
    /// XV_ENV_LOCK pattern in `config/project.rs`).
    static SCAN_DISABLE_ENV_LOCK: Mutex<()> = Mutex::new(());

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

    // --- Issue #309 Finding 6: [scan].min_value_length / patterns / XV_SCAN_DISABLE ---

    #[test]
    fn effective_min_value_length_falls_back_to_default_when_unset() {
        assert_eq!(effective_min_value_length(None), DEFAULT_MIN_VALUE_LENGTH);
        let cfg = ScanConfig::default();
        assert_eq!(
            effective_min_value_length(Some(&cfg)),
            DEFAULT_MIN_VALUE_LENGTH
        );
    }

    #[test]
    fn effective_min_value_length_uses_configured_value() {
        let cfg = ScanConfig {
            min_value_length: Some(4),
            ..Default::default()
        };
        assert_eq!(effective_min_value_length(Some(&cfg)), 4);
    }

    #[test]
    fn effective_patterns_empty_allowlist_enables_all_builtins() {
        let cfg = ScanConfig::default();
        let patterns = effective_patterns(Some(&cfg));
        assert_eq!(patterns.len(), builtin_patterns().len());
    }

    #[test]
    fn effective_patterns_allowlist_filters_to_named_patterns_only() {
        // [scan].patterns = ["aws-access-key-id"] must enable only that
        // pattern; every other built-in (github, stripe, slack, jwt, ...)
        // must not fire.
        let cfg = ScanConfig {
            patterns: vec!["aws-access-key-id".to_string()],
            ..Default::default()
        };
        let patterns = effective_patterns(Some(&cfg));
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].name, "aws-access-key-id");

        let secrets: Vec<crate::scan::engine::SecretRef> = vec![];
        let engine = MatchEngine::new(&secrets, &patterns, DEFAULT_MIN_VALUE_LENGTH);

        // The enabled pattern still fires.
        let aws_findings =
            engine.scan_text(std::path::Path::new("x"), "AWS_KEY=AKIAIOSFODNN7EXAMPLE\n");
        assert_eq!(aws_findings.len(), 1);

        // A different built-in (github token) must NOT fire — it was
        // filtered out of the allowlist.
        let github_findings = engine.scan_text(
            std::path::Path::new("x"),
            "token=ghp_1234567890abcdefghijklmnopqrstuvwxyz\n",
        );
        assert!(
            github_findings.is_empty(),
            "github-token pattern must not fire when only aws-access-key-id is allowlisted"
        );
    }

    #[test]
    fn effective_patterns_unknown_name_is_dropped_not_matched() {
        // Unknown pattern names in the allowlist are warned about (stderr)
        // rather than silently ignored, but must not cause a panic and must
        // not match anything themselves.
        let cfg = ScanConfig {
            patterns: vec!["not-a-real-pattern".to_string()],
            ..Default::default()
        };
        let patterns = effective_patterns(Some(&cfg));
        assert!(
            patterns.is_empty(),
            "unknown allowlisted name must not resolve to any built-in pattern"
        );
    }

    #[test]
    fn scan_disabled_via_env_true_for_one() {
        let _guard = SCAN_DISABLE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XV_SCAN_DISABLE", "1");
        assert!(scan_disabled_via_env());
        std::env::remove_var("XV_SCAN_DISABLE");
    }

    #[test]
    fn scan_disabled_via_env_false_when_unset() {
        let _guard = SCAN_DISABLE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("XV_SCAN_DISABLE");
        assert!(!scan_disabled_via_env());
    }

    #[test]
    fn scan_disabled_via_env_false_for_other_values() {
        let _guard = SCAN_DISABLE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XV_SCAN_DISABLE", "0");
        assert!(!scan_disabled_via_env());
        std::env::remove_var("XV_SCAN_DISABLE");
    }
}
