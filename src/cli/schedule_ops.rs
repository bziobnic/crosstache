//! Rotation-schedule command handlers (`xv schedule ...`).
//!
//! Thin layer over [`crate::schedule`]: resolves the schedule from CLI flags,
//! confirms the consequences of unattended rotation, and reports what the
//! platform scheduler says.

use std::path::{Path, PathBuf};

use crate::cli::commands::ScheduleCommands;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::schedule::manifest::{
    self as manifest, ManifestCadence, ManifestExecution, ScheduleManifestV1,
};
use crate::schedule::preview::render_install_preview;
use crate::schedule::target::{
    canonical_path_for_manifest, manifest_path_string, resolve_install_target,
    ResolvedScheduleTarget,
};
use crate::schedule::{
    self, Platform, ProcessRunner, RotationSchedule, ScheduleCommand, ScheduleInterval, UnitPaths,
};
use crate::utils::output;
use crate::workspace::WorkspaceSource;

pub(crate) async fn execute_schedule_command(
    command: ScheduleCommands,
    config: Config,
) -> Result<()> {
    match command {
        ScheduleCommands::Install {
            interval,
            at,
            vault,
            log_file,
            print,
            force,
        } => execute_install(&interval, &at, vault, log_file, print, force, &config).await,
        ScheduleCommands::Status => execute_status(&config).await,
        ScheduleCommands::Uninstall => execute_uninstall().await,
    }
}

/// Home directory used for both the unit location and the scheduled process's
/// `HOME`.
fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| {
        CrosstacheError::config("could not determine the home directory".to_string())
    })
}

/// Default log destination: `$XDG_STATE_HOME/xv/rotate.log`, else
/// `~/.local/state/xv/rotate.log`.
fn default_log_path(home: &Path) -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"))
        .join("xv")
        .join("rotate.log")
}

/// The path of *this* binary, so the unit keeps working when PATH changes or
/// the user's shell init is not sourced.
fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| {
        CrosstacheError::config(format!(
            "could not determine the path to the running xv binary: {e}"
        ))
    })
}

/// The same path with symlinks and `.`/`..` resolved, and — on Windows — the
/// `\\?\` verbatim prefix `std::fs::canonicalize` adds stripped back off.
///
/// The manifest records this form so a later run can compare it against its
/// own executable without two spellings of the same file looking like drift,
/// and the same string reaches the `# command:` line and the `schtasks /TR`
/// value, which a person is expected to read and paste. Routed through
/// [`canonical_path_for_manifest`] so every recorded path is normalized the
/// one way.
fn canonical_exe() -> Result<PathBuf> {
    canonical_path_for_manifest(&current_exe()?)
}

/// The `--vault` value the interim legacy install carries.
///
/// The installed legacy command re-resolves this string at run time through
/// [`crate::cli::helpers::resolve_vault_ref_with_workspace`], which looks it up
/// as an attached workspace alias first and only falls back to a raw vault name
/// on the *active* backend. So in a configured workspace the alias is the value
/// that survives the round trip: handing it the real vault instead would be
/// read as a raw name on the active backend, sweeping the wrong backend for any
/// alias attached to another one. In the degenerate workspace-of-one there is
/// no alias to look up, and the raw vault is exactly what resolution expects.
fn legacy_vault_argument(resolved: &ResolvedScheduleTarget) -> String {
    match resolved.workspace_source {
        WorkspaceSource::Context | WorkspaceSource::ProjectToml => resolved.entry.alias.clone(),
        WorkspaceSource::Degenerate => resolved.target.vault.clone(),
    }
}

/// Build the schedule from flags plus the current process's environment.
fn build_schedule(
    interval: ScheduleInterval,
    command: ScheduleCommand,
    log_file: Option<String>,
) -> Result<RotationSchedule> {
    let home = home_dir()?;
    let binary = current_exe()?;

    Ok(RotationSchedule {
        interval,
        command,
        binary,
        log_path: log_file
            .map(PathBuf::from)
            .unwrap_or_else(|| default_log_path(&home)),
        // Carry the *current* config location into the unit so the scheduled run
        // resolves the same configuration the user just tested against.
        config_home: std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
        home,
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute_install(
    interval: &str,
    at: &str,
    vault: Option<String>,
    log_file: Option<String>,
    print: bool,
    force: bool,
    config: &Config,
) -> Result<()> {
    let interval = ScheduleInterval::from_parts(interval, at)?;
    let platform = Platform::detect()?;

    // Resolve the target ONCE, before anything is rendered or installed: the
    // preview, the unit, and the manifest must all describe the same vault on
    // the same backend, rather than leaving the scheduled run to re-resolve a
    // context that may since have changed.
    let resolved = resolve_target(vault.as_deref(), config).await?;

    if print {
        // Dry run: show exactly what installation would write — the pinned
        // manifest and the units that read it — and write nothing at all.
        // `resolve_from_process_env` only computes paths; it creates nothing.
        let state_paths = manifest::resolve_from_process_env()?;
        let manifest_path = state_paths.manifest_path();
        let schedule = RotationSchedule {
            // The preview and the manifest must agree on one spelling of the
            // executable, and the manifest's is the canonical one.
            binary: canonical_exe()?,
            ..build_schedule(
                interval,
                ScheduleCommand::ManifestRun {
                    manifest: manifest_path.clone(),
                    working_directory: resolved.working_directory.clone(),
                },
                log_file,
            )?
        };
        let paths = UnitPaths::for_platform(platform, &schedule.home);
        let manifest = build_manifest(&schedule, &resolved)?;
        print!(
            "{}",
            render_install_preview(platform, &schedule, &paths, &manifest)
        );
        return Ok(());
    }

    // Until the pinned runner ships, the *installed* command is still the
    // legacy sweep against the resolved real vault. Nothing is written to the
    // schedule state directory, so there is no manifest for a runner to read.
    let schedule = build_schedule(
        interval,
        ScheduleCommand::LegacyRotateDue {
            vault: Some(legacy_vault_argument(&resolved)),
        },
        log_file,
    )?;
    let paths = UnitPaths::for_platform(platform, &schedule.home);

    if !force {
        output::warn(&format!(
            "This installs a {} job that runs unattended, {}:\n    {}\n\n\
             It rotates every secret whose rotation policy is already due, replacing values \
             without asking. Anything still holding an old value keeps using it until it \
             re-reads the secret or restarts, so unless rotation is sequenced with your \
             rollout this will eventually break something while nobody is watching.\n\
             Secrets with no rotation policy are never touched.",
            platform.name(),
            schedule.interval.describe(),
            schedule.command_line(),
        ));
        // Without a terminal there is nobody to confirm to. Say so directly
        // rather than surfacing a generic "not a terminal" I/O failure, which
        // reads like a bug in a provisioning script.
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            return Err(CrosstacheError::InvalidArgument(
                "installing a rotation schedule needs confirmation, but stdin is not a \
                 terminal. Re-run with --force to install unattended, or --print to review \
                 the unit without installing."
                    .to_string(),
            ));
        }
        let prompt = crate::utils::interactive::InteractivePrompt::new();
        if !prompt.confirm("Install this rotation schedule?", false)? {
            output::info("Not installed.");
            return Ok(());
        }
    }

    schedule::install(platform, &schedule, &paths, &ProcessRunner)?;

    output::success(&format!(
        "Installed a {} rotation schedule: {}.",
        platform.name(),
        schedule.interval.describe()
    ));
    output::info(&format!("  Command: {}", schedule.command_line()));
    output::info(&format!("  Log:     {}", schedule.log_path.display()));
    for unit in schedule::unit_paths_for(platform, &paths) {
        output::info(&format!("  Unit:    {}", unit.display()));
    }
    output::hint(
        "A scheduled run has no terminal, so any credential that needs interaction will fail \
         there even though it works for you now. Verify with 'xv rotate --due --force' in a \
         clean shell, then watch the log after the first firing. 'xv schedule status' shows \
         whether the scheduler is happy.",
    );
    Ok(())
}

/// Assemble the manifest installation would write for this schedule and
/// target. `installed_at` is a placeholder: the real value is stamped by the
/// write itself, and the preview renders it as `<set-at-install>`.
fn build_manifest(
    schedule: &RotationSchedule,
    resolved: &ResolvedScheduleTarget,
) -> Result<ScheduleManifestV1> {
    let (kind, hour, minute) = match schedule.interval {
        ScheduleInterval::Hourly { minute } => ("hourly", 0, minute),
        ScheduleInterval::Daily { hour, minute } => ("daily", hour, minute),
        ScheduleInterval::Weekly { hour, minute, .. } => ("weekly", hour, minute),
    };

    Ok(ScheduleManifestV1 {
        schema_version: 1,
        schedule_id: manifest::SCHEDULE_ID.to_string(),
        installed_at: String::new(),
        cadence: ManifestCadence {
            kind: kind.to_string(),
            hour: hour as u8,
            minute: minute as u8,
        },
        execution: ManifestExecution {
            binary_path: manifest_path_string("execution.binary_path", &schedule.binary)?,
            installed_version: env!("CARGO_PKG_VERSION").to_string(),
            working_directory: manifest_path_string(
                "execution.working_directory",
                &resolved.working_directory,
            )?,
            log_path: manifest_path_string("execution.log_path", &schedule.log_path)?,
        },
        target: resolved.target.clone(),
    })
}

/// Resolve the schedule target from the *saved* configuration.
///
/// The `Config` a command handler receives has environment overrides folded
/// in; an unattended run has none of that environment, so the manifest pins
/// the configuration file itself. The file must therefore exist and be
/// readable — an environment-only configuration cannot be replayed at 3am.
async fn resolve_target(vault: Option<&str>, config: &Config) -> Result<ResolvedScheduleTarget> {
    let config_path = Config::get_config_path()?;
    let (file_config, config_bytes) =
        crate::config::settings::load_config_file_at_with_bytes(&config_path)
            .await
            .map_err(|e| {
                CrosstacheError::config(format!(
                    "cannot read the configuration file '{}': {e}. A scheduled run replays a \
                     saved configuration rather than the environment you are typing in, so save \
                     your configuration (for example with 'xv init') before installing a schedule.",
                    config_path.display()
                ))
            })?;

    let cwd = std::env::current_dir().map_err(|e| {
        CrosstacheError::config(format!("could not determine the current directory: {e}"))
    })?;

    resolve_install_target(
        &file_config,
        &config_path,
        &config_bytes,
        &cwd,
        vault,
        config.env_flag.as_deref(),
        // The process config already has `--backend`/`XV_BACKEND` folded in;
        // the resolver refuses if that disagrees with the saved file.
        Some(config.effective_backend_name()),
    )
    .await
}

async fn execute_status(config: &Config) -> Result<()> {
    let platform = Platform::detect()?;
    let home = home_dir()?;
    let paths = UnitPaths::for_platform(platform, &home);

    let status = schedule::status(platform, &paths, &ProcessRunner)?;

    if status.installed {
        output::success(&format!(
            "A {} rotation schedule is installed.",
            platform.name()
        ));
    } else {
        output::info(&format!(
            "No {} rotation schedule is installed.",
            platform.name()
        ));
    }
    output::info(&format!("  {}", status.detail));

    for unit in schedule::unit_paths_for(platform, &paths) {
        output::info(&format!(
            "  Unit:    {} ({})",
            unit.display(),
            if unit.exists() { "present" } else { "absent" }
        ));
    }

    let log = default_log_path(&home);
    output::info(&format!(
        "  Log:     {} ({})",
        log.display(),
        if log.exists() {
            "present"
        } else {
            "not yet written"
        }
    ));

    if !status.installed {
        output::hint("Install one with 'xv schedule install --vault <vault>'.");
        return Ok(());
    }

    // What the schedule will actually act on, so status answers the real
    // question — "will anything rotate tonight?" — not just "is a job present?".
    let vault_hint = if config.default_vault.is_empty() {
        "<resolved from context at run time>".to_string()
    } else {
        config.default_vault.clone()
    };
    output::info(&format!("  Vault:   {vault_hint}"));
    output::hint("Run 'xv rotate --check' to see which secrets the next sweep would rotate.");
    Ok(())
}

async fn execute_uninstall() -> Result<()> {
    let platform = Platform::detect()?;
    let home = home_dir()?;
    let paths = UnitPaths::for_platform(platform, &home);

    if schedule::uninstall(platform, &paths, &ProcessRunner)? {
        output::success(&format!(
            "Removed the {} rotation schedule.",
            platform.name()
        ));
    } else {
        output::info(&format!(
            "No {} rotation schedule was installed; nothing to remove.",
            platform.name()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::manifest::ManifestTarget;
    use crate::workspace::WorkspaceEntry;

    fn resolved(
        source: WorkspaceSource,
        alias: &str,
        backend: &str,
        vault: &str,
    ) -> ResolvedScheduleTarget {
        ResolvedScheduleTarget {
            target: ManifestTarget {
                config_path: "/home/u/.config/xv/xv.conf".to_string(),
                config_digest: format!("sha256:{}", "0".repeat(64)),
                project_path: None,
                project_digest: None,
                environment: None,
                context_path: None,
                context_digest: None,
                workspace_source: "context".to_string(),
                workspace_alias: Some(alias.to_string()),
                backend_name: backend.to_string(),
                backend_kind: "local".to_string(),
                backend_identity: format!("sha256:{}", "1".repeat(64)),
                vault: vault.to_string(),
            },
            entry: WorkspaceEntry {
                alias: alias.to_string(),
                backend: backend.to_string(),
                vault: vault.to_string(),
                default: true,
            },
            workspace_source: source,
            working_directory: PathBuf::from("/home/u/work"),
        }
    }

    /// The bug this guards: run-time re-resolution looks `--vault` up as an
    /// attached alias first and otherwise treats it as a raw vault on the
    /// *active* backend. Carrying the resolved real vault for an alias
    /// attached to a non-active backend would sweep the active backend's
    /// same-named vault instead.
    #[test]
    fn legacy_vault_argument_carries_the_alias_in_a_configured_workspace() {
        let context = resolved(WorkspaceSource::Context, "stage", "local-b", "stage-vault");
        assert_eq!(legacy_vault_argument(&context), "stage");

        let project = resolved(
            WorkspaceSource::ProjectToml,
            "stage",
            "local-b",
            "stage-vault",
        );
        assert_eq!(legacy_vault_argument(&project), "stage");
    }

    /// In the degenerate workspace-of-one there is no alias to look up — the
    /// synthesized alias is a label, not a name resolution accepts — so the
    /// raw vault is the only value that round-trips.
    #[test]
    fn legacy_vault_argument_carries_the_raw_vault_in_the_degenerate_workspace() {
        let degenerate = resolved(
            WorkspaceSource::Degenerate,
            "local:stage-vault",
            "local",
            "stage-vault",
        );
        assert_eq!(legacy_vault_argument(&degenerate), "stage-vault");
    }

    /// `canonical_exe` must go through the manifest's path normalizer, so the
    /// manifest, the `# command:` line and the `schtasks /TR` value never
    /// carry Windows' `\\?\` verbatim prefix.
    #[test]
    fn canonical_exe_is_normalized_for_the_manifest() {
        let exe = canonical_exe().expect("the running test binary canonicalizes");
        let expected =
            canonical_path_for_manifest(&current_exe().expect("current exe")).expect("normalizes");
        assert_eq!(exe, expected);
        // Whatever the platform, the recorded form must be a path the manifest
        // schema accepts.
        manifest::validate_absolute_normalized_path(
            "execution.binary_path",
            exe.to_str().expect("test binary path is UTF-8"),
        )
        .expect("canonical_exe is absolute and normalized");
    }

    #[cfg(windows)]
    #[test]
    fn canonical_exe_strips_the_windows_verbatim_prefix() {
        let exe = canonical_exe().expect("the running test binary canonicalizes");
        assert!(
            !exe.to_string_lossy().starts_with(r"\\?\"),
            "canonical_exe leaked a verbatim prefix: {}",
            exe.display()
        );
    }
}
