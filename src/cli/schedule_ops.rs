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
        ScheduleCommands::Run { manifest } => execute_run(&manifest),
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

/// Build the schedule from flags plus the current process's environment.
///
/// `state_home` is `ScheduleStatePaths::pinned_state_home` for the paths the
/// manifest is being written to: the unit has to carry whichever variable
/// picked that root, or the scheduled run will look somewhere else for it.
fn build_schedule(
    interval: ScheduleInterval,
    command: ScheduleCommand,
    log_file: Option<String>,
    state_home: Option<(&'static str, PathBuf)>,
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
        home,
        state_home,
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

    // `resolve_from_process_env` only computes paths; it creates nothing, so
    // this is safe on the `--print` path too.
    let state_paths = manifest::resolve_from_process_env()?;
    let schedule = build_schedule(
        interval,
        ScheduleCommand::ManifestRun {
            manifest: state_paths.manifest_path(),
            working_directory: resolved.working_directory.clone(),
        },
        log_file,
        state_paths.pinned_state_home(),
    )?;
    let paths = UnitPaths::for_platform(platform, &schedule.home);
    let manifest_v1 = build_manifest(&schedule, &resolved)?;

    if print {
        // Dry run: show exactly what installation would write — the pinned
        // manifest and the units that read it — and write nothing at all.
        print!(
            "{}",
            render_install_preview(platform, &schedule, &paths, &manifest_v1)
        );
        return Ok(());
    }

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

    // The manifest is the target; the unit is only a pointer to it. Publish it
    // before the scheduler can ever fire, so a job that exists always has a
    // manifest to read. `installed_at` is stamped here rather than in
    // `build_manifest` because the preview must stay deterministic — it renders
    // the placeholder — and because the recorded time should be the moment the
    // target was actually pinned.
    //
    // This is deliberately non-transactional: a failure after the write leaves
    // a manifest with no job, which `xv schedule status` reports and a reinstall
    // replaces. Task 4 of this series wraps the sequence in the spec's
    // six-stage install transaction with rollback.
    let manifest_path = stamp_and_write_manifest(&state_paths, manifest_v1, chrono::Utc::now())?;

    schedule::install(platform, &schedule, &paths, &ProcessRunner)?;

    output::success(&format!(
        "Installed a {} rotation schedule: {}.",
        platform.name(),
        schedule.interval.describe()
    ));
    output::info(&format!("  Command:  {}", schedule.command_line()));
    output::info(&format!("  Manifest: {}", manifest_path.display()));
    output::info(&format!("  Log:      {}", schedule.log_path.display()));
    for unit in schedule::unit_paths_for(platform, &paths) {
        output::info(&format!("  Unit:     {}", unit.display()));
    }
    output::hint(
        "A scheduled run has no terminal, so any credential that needs interaction will fail \
         there even though it works for you now. Verify with 'xv rotate --due --force' in a \
         clean shell, then watch the log after the first firing. 'xv schedule status' shows \
         whether the scheduler is happy.",
    );
    Ok(())
}

/// Stamp `installed_at`, validate, and atomically publish `manifest.json`.
///
/// `now` is a parameter so the whole sequence is testable end to end. What
/// makes it worth extracting is the ordering: validation runs *before*
/// serialization, so a manifest this build would refuse to load never reaches
/// the disk — the alternative is an installed job that fails every night on a
/// file only a reinstall can fix.
///
/// Returns the path written, which is also what the success output names.
fn stamp_and_write_manifest(
    paths: &manifest::ScheduleStatePaths,
    mut manifest_v1: ScheduleManifestV1,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<PathBuf> {
    // Seconds precision and a `Z` suffix: the schema requires UTC, and the
    // stamp is read by people and compared for drift, not used as a clock.
    manifest_v1.installed_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    manifest::validate_v1(&manifest_v1)?;
    manifest::write_manifest_atomic(paths, &manifest::serialize_manifest(&manifest_v1))?;
    Ok(paths.manifest_path())
}

/// Assemble the manifest installation would write for this schedule and
/// target. `installed_at` is left empty: [`execute_install`] stamps it just
/// before writing, and the preview renders the empty value as
/// `<set-at-install>`. Keeping it out of here is what lets the preview be
/// deterministic and byte-comparable across runs.
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

/// Accept only the manifest path this user's own installation owns.
///
/// The scheduler passes `--manifest` verbatim, so whoever can influence that
/// argument — a hand-edited unit, a foreign job reusing our subcommand — picks
/// what gets rotated. Comparing against the owned location makes the flag a
/// consistency check rather than a target-selection input, which is what
/// invariant 2 requires of everything outside the manifest.
///
/// Both sides are compared in the same canonical form so `/tmp/...` and
/// `/private/tmp/...` on macOS are not mistaken for different files. The path
/// need not exist yet: a missing manifest is a separate, more useful error than
/// "wrong path", and it is reported as one.
fn check_owned_manifest_path(supplied: &Path, owned: &Path) -> Result<()> {
    if !supplied.is_absolute() {
        return Err(CrosstacheError::config(format!(
            "'xv schedule run --manifest' requires an absolute path: '{}'. This command is \
             scheduler plumbing; the installed job passes the right path itself.",
            supplied.display()
        )));
    }
    // A symlink here would let the checked path and the read path be two
    // different files. `load_manifest` refuses one too; refusing it before the
    // comparison keeps the equality below honest.
    match std::fs::symlink_metadata(supplied) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(CrosstacheError::config(format!(
                "Refusing symlinked schedule manifest '{}'; reinstall the schedule with \
                 'xv schedule install' to regenerate it.",
                supplied.display()
            )));
        }
        _ => {}
    }
    if comparable_form(supplied) != comparable_form(owned) {
        return Err(CrosstacheError::config(format!(
            "'xv schedule run --manifest {}' does not name this user's schedule manifest \
             ('{}'). Reinstall the schedule with 'xv schedule install' so its job points at the \
             owned manifest.",
            supplied.display(),
            owned.display()
        )));
    }
    Ok(())
}

/// The path with its *directory* canonicalized, leaving the file name alone so
/// a not-yet-existing manifest still compares correctly.
fn comparable_form(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    let base = canonical_path_for_manifest(parent).unwrap_or_else(|_| parent.to_path_buf());
    match path.file_name() {
        Some(name) => base.join(name),
        None => base,
    }
}

/// `xv schedule run --manifest <path>` — the pinned scheduled sweep.
///
/// Ordering is the security property: the path check and the bounded,
/// symlink-refusing manifest load both happen before anything that could
/// construct a backend, so a malformed, oversized, symlinked or foreign
/// manifest cannot reach a provider — let alone mutate a secret. Nothing here
/// touches `XV_BACKEND`, `XV_ENV` or the context file.
///
/// The rotation itself is not implemented yet. Returning an error rather than
/// `Ok(())` is deliberate: a scheduler wired to this build must record a
/// failure, not a silent no-op that looks like a clean nightly sweep.
fn execute_run(supplied_manifest: &Path) -> Result<()> {
    let state_paths = manifest::resolve_from_process_env()?;
    let owned = state_paths.manifest_path();
    check_owned_manifest_path(supplied_manifest, &owned)?;

    if !owned.exists() {
        return Err(CrosstacheError::config(format!(
            "the pinned schedule manifest '{}' is missing; reinstall the schedule with \
             'xv schedule install'.",
            owned.display()
        )));
    }

    // Bounded, no-follow, schema-validated. Any failure names reinstall,
    // because a manifest this process cannot trust is not something a
    // scheduled run may work around.
    manifest::load_manifest(&state_paths).map_err(|e| {
        CrosstacheError::config(format!(
            "{e}. Reinstall the schedule with 'xv schedule install' to regenerate it."
        ))
    })?;

    Err(CrosstacheError::config(
        "the scheduled rotation runner is not implemented in this build; reinstall the schedule \
         after upgrading xv ('xv schedule install').",
    ))
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

    /// A real owned path under a tempdir, so canonicalization has something to
    /// resolve on hosts where the temp root is itself a symlink (macOS).
    fn owned_in(dir: &Path) -> PathBuf {
        let root = dir.join("xv").join("schedules").join("rotation-default");
        std::fs::create_dir_all(&root).unwrap();
        root.join("manifest.json")
    }

    /// A manifest whose every field passes `validate_v1`, rooted at `dir` so
    /// the paths are absolute and normalized on the host running the test.
    fn valid_manifest(dir: &Path) -> ScheduleManifestV1 {
        let p = |name: &str| dir.join(name).to_string_lossy().to_string();
        ScheduleManifestV1 {
            schema_version: 1,
            schedule_id: manifest::SCHEDULE_ID.to_string(),
            installed_at: String::new(),
            cadence: ManifestCadence {
                kind: "daily".to_string(),
                hour: 3,
                minute: 0,
            },
            execution: ManifestExecution {
                binary_path: p("xv"),
                installed_version: "0.39.0".to_string(),
                working_directory: p("work"),
                log_path: p("rotate.log"),
            },
            target: crate::schedule::manifest::ManifestTarget {
                config_path: p("xv.conf"),
                config_digest: format!("sha256:{}", "0".repeat(64)),
                project_path: None,
                project_digest: None,
                environment: None,
                context_path: None,
                context_digest: None,
                workspace_source: "degenerate".to_string(),
                workspace_alias: None,
                backend_name: "local".to_string(),
                backend_kind: "local".to_string(),
                backend_identity: format!("sha256:{}", "1".repeat(64)),
                vault: "default".to_string(),
            },
        }
    }

    /// State paths rooted in a tempdir, through the real resolver.
    fn state_paths_in(dir: &Path) -> manifest::ScheduleStatePaths {
        manifest::resolve(&manifest::ScheduleEnv {
            xv_state_home: Some(dir.to_string_lossy().to_string()),
            ..Default::default()
        })
        .expect("an explicit override always resolves")
    }

    #[test]
    fn stamp_and_write_manifest_publishes_a_manifest_the_runner_can_load() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = state_paths_in(tmp.path());
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-10T15:04:05.987Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let written =
            stamp_and_write_manifest(&paths, valid_manifest(tmp.path()), now).expect("writes");
        assert_eq!(written, paths.manifest_path());

        // The whole point of the write is that the runner can read it back.
        let crate::schedule::manifest::ScheduleManifest::V1(loaded) =
            manifest::load_manifest(&paths).expect("loads back");
        // Seconds precision, UTC, `Z` — sub-second noise would make two
        // manifests written in the same second compare unequal for no reason.
        assert_eq!(loaded.installed_at, "2026-09-10T15:04:05Z");
        assert_eq!(loaded.target.vault, "default");
    }

    #[cfg(unix)]
    #[test]
    fn the_written_manifest_is_owner_private_inside_an_owner_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let paths = state_paths_in(tmp.path());
        let written =
            stamp_and_write_manifest(&paths, valid_manifest(tmp.path()), chrono::Utc::now())
                .expect("writes");

        let file = std::fs::metadata(&written).unwrap().permissions().mode() & 0o777;
        assert_eq!(file, 0o600, "manifest mode {file:o}");
        let dir = std::fs::metadata(paths.root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir, 0o700, "state directory mode {dir:o}");
    }

    #[test]
    fn an_invalid_manifest_is_refused_before_anything_is_written() {
        // Writing first and validating later would leave an installed job
        // pointing at a file this same build refuses to load — a failure only
        // a reinstall can clear, discovered at 3am.
        let tmp = tempfile::tempdir().unwrap();
        let paths = state_paths_in(tmp.path());
        let mut invalid = valid_manifest(tmp.path());
        invalid.execution.working_directory = "relative/work".to_string();

        let err = stamp_and_write_manifest(&paths, invalid, chrono::Utc::now())
            .expect_err("an invalid manifest is refused");
        assert!(err.to_string().contains("absolute path"), "{err}");
        assert!(
            !paths.manifest_path().exists(),
            "a refused write left a file"
        );
    }

    #[test]
    fn the_owned_manifest_path_is_accepted_even_before_it_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let owned = owned_in(tmp.path());
        // Nothing written yet: "missing" is a separate, more useful error than
        // "wrong path", so the check itself must not depend on existence.
        check_owned_manifest_path(&owned, &owned).expect("the owned path is accepted");
        std::fs::write(&owned, b"{}").unwrap();
        check_owned_manifest_path(&owned, &owned).expect("still accepted once written");
    }

    #[test]
    fn a_relative_manifest_path_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let owned = owned_in(tmp.path());
        let err = check_owned_manifest_path(Path::new("manifest.json"), &owned)
            .expect_err("relative paths are refused");
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    #[test]
    fn a_foreign_absolute_manifest_path_is_refused() {
        // Whoever picks the manifest picks what gets rotated, so a path
        // outside the owned location is refused even when it parses fine.
        let tmp = tempfile::tempdir().unwrap();
        let owned = owned_in(tmp.path());
        let foreign = tmp.path().join("elsewhere.json");
        std::fs::write(&foreign, b"{}").unwrap();
        let err =
            check_owned_manifest_path(&foreign, &owned).expect_err("a foreign path is refused");
        assert!(err.to_string().contains("does not name"), "{err}");
        assert!(err.to_string().contains("Reinstall"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_manifest_path_is_refused() {
        // Otherwise the path this check compares and the file the loader reads
        // could be two different things.
        let tmp = tempfile::tempdir().unwrap();
        let owned = owned_in(tmp.path());
        let real = tmp.path().join("real.json");
        std::fs::write(&real, b"{}").unwrap();
        std::os::unix::fs::symlink(&real, &owned).unwrap();
        let err = check_owned_manifest_path(&owned, &owned).expect_err("a symlink is refused");
        assert!(err.to_string().contains("symlink"), "{err}");
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
