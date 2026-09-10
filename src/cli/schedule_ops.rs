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

/// The canonical working directory installation resolves everything against.
fn install_cwd() -> Result<PathBuf> {
    let cwd = std::env::current_dir().map_err(|e| {
        CrosstacheError::config(format!("could not determine the current directory: {e}"))
    })?;
    canonical_path_for_manifest(&cwd)
}

/// The path of the invoked binary, in the spelling the manifest records.
///
/// Deliberately **not** `std::fs::canonicalize`: that resolves symlinks, so a
/// package-manager shim — `/opt/homebrew/bin/xv`, `~/.local/bin/xv`, a Nix or
/// asdf shim — would be recorded as the versioned store path it happens to
/// point at today. The design treats a binary at the *same* path reporting a
/// new version as a warning that the run is still allowed to proceed
/// (`docs/superpowers/specs/2026-09-09-scheduled-target-manifest-design.md`,
/// the "in-place upgrade" row of the drift table); recording the versioned
/// target instead would turn every ordinary `brew upgrade` into
/// missing-binary drift and refuse the sweep.
///
/// So the recorded form is `std::env::current_exe()`, made absolute against
/// the canonical install cwd only if it came back relative, lexically
/// normalized, with Windows' `\\?\` verbatim prefix stripped. That is a path
/// `manifest::validate_absolute_normalized_path` accepts, and it is the same
/// string that reaches the `# command:` line, the unit `ExecStart`, and the
/// `schtasks /TR` value a person is expected to read and paste.
///
/// Note for Linux: `current_exe()` there reads `/proc/self/exe`, which the
/// kernel has *already* resolved through symlinks. Nothing here can un-resolve
/// that — this function only guarantees xv adds no resolution of its own.
fn recorded_binary_path() -> Result<PathBuf> {
    let exe = current_exe()?;
    let base = if exe.is_absolute() {
        PathBuf::new()
    } else {
        install_cwd()?
    };
    Ok(crate::utils::helpers::lexically_normalize_from(&base, &exe))
}

/// True when `metadata` describes a Windows reparse point.
///
/// Windows has more redirection primitives than `is_symlink()` reports. A
/// *directory junction* (`mklink /J`, creatable without elevation) is a
/// reparse point that `Path::exists` follows but that `FileType::is_symlink`
/// does not flag, so a junction planted anywhere along the log path would
/// silently redirect an unattended write. `FILE_ATTRIBUTE_REPARSE_POINT`
/// covers junctions, mount points, and symlinks alike.
#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Non-Windows platforms have no reparse points; `is_symlink()` is the whole
/// story there.
#[cfg(not(windows))]
fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// A path made up solely of a prefix and/or the root (`/`, `C:\`, `\\?\C:\`,
/// a UNC share root). Those cannot themselves be redirected and are not
/// meaningfully inspectable, so the ancestor walk skips them.
fn is_root_or_prefix(path: &Path) -> bool {
    use std::path::Component;
    path.components()
        .all(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
}

/// Refuse the log destination if *any* existing directory component on the way
/// to it is a symlink or a Windows reparse point.
///
/// Checking only the nearest existing ancestor is not enough: `exists()` walks
/// straight through an intermediate symlink, so `/tmp/link/sub/rotate.log`
/// would report `/tmp/link/sub` as a perfectly ordinary directory while the
/// write still lands wherever `link` points. Every component from the leaf's
/// parent up to (but excluding) the root is inspected with `symlink_metadata`,
/// which never follows.
///
/// Fail closed: a component that exists but cannot be inspected is a refusal,
/// not a pass. Only `NotFound` — the components below the nearest existing
/// ancestor, which the caller has already accounted for — is ignored.
fn reject_redirected_ancestors(resolved: &Path) -> Result<()> {
    for ancestor in resolved.ancestors().skip(1) {
        if is_root_or_prefix(ancestor) {
            continue;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                let symlink = metadata.file_type().is_symlink();
                if symlink || is_reparse_point(&metadata) {
                    let kind = if symlink {
                        "symlink"
                    } else {
                        "reparse point (junction or mount point)"
                    };
                    return Err(CrosstacheError::config(format!(
                        "the log destination '{}' is reached through the {kind} '{}'. A \
                         scheduled run writes there unattended, so pin a real directory \
                         instead.",
                        resolved.display(),
                        ancestor.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(CrosstacheError::config(format!(
                    "could not inspect '{}' on the way to the log destination '{}': {error}. \
                     Refusing to install a schedule that writes through a directory xv cannot \
                     verify.",
                    ancestor.display(),
                    resolved.display()
                )));
            }
        }
    }
    Ok(())
}

/// Resolve a `--log-file` value into the absolute, normalized form the
/// manifest schema requires.
///
/// `--log-file` is a raw user string and may be relative, in which case an
/// unnormalized copy would land in `execution.log_path` and be rejected by
/// `validate_v1` at write time — after the preview had already shown it. It is
/// resolved against the canonical install cwd, lexically normalized, and the
/// verbatim prefix stripped.
///
/// The file itself need not exist yet, but per the design its nearest existing
/// ancestor must resolve without symlinks — and that is enforced over *every*
/// existing component of the path, not just the nearest one, because
/// `exists()` walks straight through an intermediate link. A log destination
/// reached through a symlink (or a Windows junction) lets whoever controls the
/// link redirect an unattended root-less write somewhere the user never chose.
fn resolve_log_path(log_file: &str) -> Result<PathBuf> {
    let raw = PathBuf::from(log_file);
    let base = if raw.is_absolute() {
        PathBuf::new()
    } else {
        install_cwd()?
    };
    let resolved = crate::utils::helpers::lexically_normalize_from(&base, &raw);

    if !resolved
        .ancestors()
        .skip(1)
        .any(|candidate| candidate.exists())
    {
        return Err(CrosstacheError::config(format!(
            "the log destination '{}' has no existing parent directory. Create it before \
             installing a schedule, so a failed unattended run has somewhere to report.",
            resolved.display()
        )));
    }
    reject_redirected_ancestors(&resolved)?;
    Ok(resolved)
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
    // One spelling of the executable everywhere: the manifest, the preview's
    // `# command:` line, and the installed unit must not disagree.
    let binary = recorded_binary_path()?;
    let log_path = match log_file {
        Some(raw) => resolve_log_path(&raw)?,
        None => default_log_path(&home),
    };

    Ok(RotationSchedule {
        interval,
        command,
        binary,
        log_path,
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
        // `build_schedule` already records the executable and the log path in
        // the manifest's spelling, so the preview, the manifest and the unit
        // all read the same strings.
        let schedule = build_schedule(
            interval,
            ScheduleCommand::ManifestRun {
                manifest: manifest_path.clone(),
                working_directory: resolved.working_directory.clone(),
            },
            log_file,
        )?;
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

    /// The recorded executable must be a path the manifest schema accepts on
    /// every platform.
    #[test]
    fn recorded_binary_path_is_absolute_and_normalized() {
        let exe = recorded_binary_path().expect("the running test binary resolves");
        manifest::validate_absolute_normalized_path(
            "execution.binary_path",
            exe.to_str().expect("test binary path is UTF-8"),
        )
        .expect("recorded_binary_path is absolute and normalized");
    }

    #[cfg(windows)]
    #[test]
    fn recorded_binary_path_strips_the_windows_verbatim_prefix() {
        let exe = recorded_binary_path().expect("the running test binary resolves");
        assert!(
            !exe.to_string_lossy().starts_with(r"\\?\"),
            "recorded_binary_path leaked a verbatim prefix: {}",
            exe.display()
        );
    }

    /// The bug this guards: `std::fs::canonicalize` follows symlinks, so a
    /// package-manager shim (`/opt/homebrew/bin/xv` → a versioned Cellar path)
    /// would be recorded as the *versioned* path. The design allows a
    /// same-path version bump with a warning; recording the version-bearing
    /// target instead makes every upgrade read as missing-binary drift.
    ///
    /// Asserted against the shared normalizer rather than `recorded_binary_path`
    /// itself, because a test cannot re-exec the suite through a symlink — and
    /// on Linux `current_exe()` is `/proc/self/exe`, already OS-resolved.
    #[cfg(unix)]
    #[test]
    fn the_recorded_path_shaping_does_not_resolve_a_symlinked_binary() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("xv-0.39.0");
        std::fs::write(&real, b"#!/bin/sh\n").unwrap();
        let link = dir.path().join("xv");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let recorded = crate::utils::helpers::lexically_normalize_from(&PathBuf::new(), &link);
        assert_eq!(recorded, link, "the link path must be recorded verbatim");
        assert_ne!(recorded, real);
        // And the contrast: canonicalization — what this deliberately does not
        // do — would have swapped in the versioned name.
        assert_eq!(
            canonical_path_for_manifest(&link).unwrap(),
            canonical_path_for_manifest(&real).unwrap()
        );
    }

    /// A relative `--log-file` must land in the manifest as an absolute,
    /// normalized path; leaving it raw made `validate_v1` reject the manifest
    /// the preview had just shown.
    #[test]
    fn a_relative_log_file_is_resolved_against_the_install_cwd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("logs")).unwrap();
        let cwd = canonical_path_for_manifest(dir.path()).unwrap();

        let resolved = crate::utils::helpers::lexically_normalize_from(
            &cwd,
            Path::new("./logs/../logs/x.log"),
        );
        assert_eq!(resolved, cwd.join("logs").join("x.log"));
        manifest::validate_absolute_normalized_path(
            "execution.log_path",
            resolved.to_str().unwrap(),
        )
        .expect("a resolved relative log path satisfies the schema");
    }

    /// An absolute `--log-file` is left where the user put it, and a missing
    /// leaf file is fine — only the nearest existing ancestor is checked.
    #[test]
    fn an_absolute_log_file_survives_resolution_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let target = canonical_path_for_manifest(dir.path())
            .unwrap()
            .join("rotate.log");
        assert_eq!(resolve_log_path(target.to_str().unwrap()).unwrap(), target);
    }

    /// A log destination reached through a symlinked directory is refused:
    /// whoever controls the link would otherwise redirect an unattended write.
    ///
    /// The temp root is canonicalized first: on macOS `tempfile` hands back a
    /// path under `/var`, which is itself a symlink to `/private/var`, and the
    /// ancestor walk would refuse for that unrelated reason.
    #[cfg(unix)]
    #[test]
    fn a_log_path_under_a_symlinked_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = root.join("linked");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let error = resolve_log_path(link.join("rotate.log").to_str().unwrap()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("symlink"), "{message}");
        assert!(
            message.contains(link.to_str().unwrap()),
            "the offending component must be named: {message}"
        );
    }

    /// The bug this guards: checking only the *nearest existing* ancestor
    /// misses a symlink further up. `exists()` walks straight through
    /// `linked/`, so `linked/sub` looks like an ordinary directory while the
    /// unattended write still lands wherever `linked` points.
    #[cfg(unix)]
    #[test]
    fn a_log_path_under_an_intermediate_symlinked_component_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let real = root.join("real");
        std::fs::create_dir_all(real.join("sub")).unwrap();
        let link = root.join("linked");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let nested = link.join("sub").join("rotate.log");
        // The nearest existing ancestor is not itself a symlink...
        assert!(nested.parent().unwrap().exists());
        assert!(!std::fs::symlink_metadata(nested.parent().unwrap())
            .unwrap()
            .file_type()
            .is_symlink());
        // ...but the path still reaches it through one, so it is refused.
        let error = resolve_log_path(nested.to_str().unwrap()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("symlink"), "{message}");
        assert!(
            message.contains(link.to_str().unwrap()),
            "the offending component must be named: {message}"
        );
    }

    /// The walk must not become a blanket refusal: a plainly nested real
    /// directory whose leaf file does not exist yet is still accepted.
    #[test]
    fn a_plain_nested_log_path_with_a_missing_leaf_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        // `canonical_path_for_manifest` both resolves macOS's `/var` symlink
        // and strips Windows' verbatim prefix, which is the exact spelling
        // `resolve_log_path` returns.
        let root = canonical_path_for_manifest(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();

        let target = root.join("a").join("b").join("rotate.log");
        assert!(!target.exists());
        assert_eq!(resolve_log_path(target.to_str().unwrap()).unwrap(), target);
    }

    /// A Windows directory *junction* is a reparse point that `exists()`
    /// follows but `is_symlink()` does not report, so it needs the
    /// `FILE_ATTRIBUTE_REPARSE_POINT` check rather than the symlink check.
    /// `mklink /J` works without elevation, unlike `mklink /D`.
    #[cfg(windows)]
    #[test]
    fn a_log_path_under_a_directory_junction_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let junction = root.join("linked");

        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&real)
            .status();
        match status {
            Ok(status) if status.success() => {}
            // No `cmd`, or the filesystem refused the junction: nothing to
            // assert about a reparse point that does not exist.
            _ => return,
        }
        assert!(
            is_reparse_point(&std::fs::symlink_metadata(&junction).unwrap()),
            "mklink /J must produce a reparse point"
        );

        let error = resolve_log_path(junction.join("rotate.log").to_str().unwrap()).unwrap_err();
        let message = error.to_string();
        // Rust's std reports a junction as a symlink on some Windows versions
        // and only as a bare reparse point on others; either wording proves the
        // walk refused it.
        assert!(
            message.contains("reparse point") || message.contains("symlink"),
            "{message}"
        );
        assert!(message.contains("linked"), "{message}");
    }
}
