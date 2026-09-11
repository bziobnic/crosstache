//! Rotation-schedule command handlers (`xv schedule ...`).
//!
//! Thin layer over [`crate::schedule`]: resolves the schedule from CLI flags,
//! confirms the consequences of unattended rotation, and reports what the
//! platform scheduler says.

use std::path::{Path, PathBuf};

use crate::cli::commands::ScheduleCommands;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::schedule::drift;
use crate::schedule::install::{
    install_transactional, uninstall_owned, InstallPlan, RealOwnedScheduleStore,
};
use crate::schedule::manifest::{
    self as manifest, ManifestCadence, ManifestExecution, ManifestTarget, ScheduleManifestV1,
};
use crate::schedule::outcome::{self, RunDiagnostic, RunOutcomeV1, RunState, RunSummary};
use crate::schedule::preview::render_install_preview;
use crate::schedule::status;
use crate::schedule::status_render;
use crate::schedule::target::{
    canonical_path_for_manifest, manifest_path_string, resolve_install_target,
    ResolvedScheduleTarget,
};
use crate::schedule::{
    self, Platform, ProcessRunner, RotationSchedule, ScheduleCommand, ScheduleInterval, UnitPaths,
};
use crate::secret::scheduled_rotation::{
    run_due_rotation, DueRotationOptions, DueRotationSummary, SilentObserver,
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
        ScheduleCommands::Status => execute_status().await,
        ScheduleCommands::Uninstall => execute_uninstall().await,
        ScheduleCommands::Run { manifest } => execute_run(&manifest).await,
    }
}

/// The scheduler this process should talk to.
///
/// Always the real one in a release build: the whole switch below is behind
/// `cfg(debug_assertions)`, so a shipped `xv` contains no fake runner and reads
/// no environment variable to choose one.
///
/// In a debug build `XV_SCHEDULE_RUNNER=fake` (or one of its scenario
/// spellings, `fake:installed[,next=<rfc3339>]`) swaps in
/// [`crate::schedule::testing::RecordingRunner`]. That exists because
/// `launchctl`, `systemctl --user` and `schtasks` act on the invoking user's
/// live session under a fixed global job name — `HOME` does not sandbox them —
/// so a CLI test that spawned `xv schedule uninstall` for real would deregister
/// the developer's own rotation schedule.
#[cfg(debug_assertions)]
fn schedule_runner() -> Box<dyn schedule::CommandRunner> {
    use crate::schedule::testing;
    match std::env::var(testing::RUNNER_VAR) {
        Ok(value) if testing::selects_fake(&value) => {
            Box::new(testing::RecordingRunner::from_env())
        }
        _ => Box::new(ProcessRunner),
    }
}

#[cfg(not(debug_assertions))]
fn schedule_runner() -> Box<dyn schedule::CommandRunner> {
    Box::new(ProcessRunner)
}

/// Home directory used for both the unit location and the scheduled process's
/// `HOME`.
fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| {
        CrosstacheError::config("could not determine the home directory".to_string())
    })
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
/// `state_paths` is the *resolved* state directory the manifest is being
/// written to. Everything rooted in the state directory comes from it: the
/// default log destination (`<state root>/xv/rotate.log`) and
/// `pinned_state_home`, the variable the unit has to carry so a scheduled run
/// that inherits none of the installing shell's environment resolves the same
/// root rather than looking somewhere else for its manifest.
///
/// Deliberately one resolver. The default log path used to read
/// `XDG_STATE_HOME`/`$HOME/.local/state` itself, which agrees with
/// [`crate::schedule::manifest::resolve`] on Unix and disagrees with it on
/// Windows — where `XDG_STATE_HOME` is not an input at all — so an installing
/// shell with that variable set produced a unit whose log lived under it and
/// whose `--manifest` argument lived under `%LOCALAPPDATA%`.
fn build_schedule(
    interval: ScheduleInterval,
    command: ScheduleCommand,
    log_file: Option<String>,
    state_paths: &manifest::ScheduleStatePaths,
) -> Result<RotationSchedule> {
    let home = home_dir()?;
    // One spelling of the executable everywhere: the manifest, the preview's
    // `# command:` line, and the installed unit must not disagree.
    let binary = recorded_binary_path()?;
    let log_path = match log_file {
        Some(raw) => resolve_log_path(&raw)?,
        None => state_paths.default_log_path(),
    };

    Ok(RotationSchedule {
        interval,
        command,
        binary,
        log_path,
        home,
        state_home: state_paths.pinned_state_home(),
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
        &state_paths,
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

    // The manifest is the target; the unit is only a pointer to it. Both are
    // published by one transaction so a failure cannot leave a job without a
    // manifest to read, or — on reinstall — destroy the schedule the user
    // already had. `installed_at` is stamped here rather than in
    // `build_manifest` because the preview must stay deterministic (it renders
    // the placeholder) and because the recorded time should be the moment the
    // target was actually pinned.
    let now = chrono::Utc::now();
    let manifest_bytes = stamp_and_serialize_manifest(manifest_v1, now)?;
    let plan = InstallPlan::new(platform, schedule.clone(), paths.clone(), manifest_bytes)?;
    // Stages 1 and 2 — resolving the target and rendering the manifest and
    // units — mutate nothing, which is why they (and the confirmation prompt)
    // deliberately run before the lock is taken: a person deciding at a prompt
    // must not hold an exclusive lock while they think.
    //
    // Opening the store takes the exclusive `install.lock` and holds it until
    // the transaction ends, so a second installer cannot interleave with this
    // one.
    let mut store = RealOwnedScheduleStore::open(&state_paths)?;
    let report = install_transactional(&plan, &mut store, schedule_runner().as_ref(), now)?;
    let manifest_path = report.manifest_path.clone();

    output::success(&format!(
        "{} a {} rotation schedule: {}.",
        if report.replaced_prior_schedule {
            "Reinstalled"
        } else {
            "Installed"
        },
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

/// Stamp `installed_at`, validate, and serialize the manifest's exact bytes.
///
/// `now` is a parameter so the whole sequence is testable end to end. What
/// makes it worth extracting is the ordering: validation runs *before*
/// serialization, so a manifest this build would refuse to load never reaches
/// the install transaction — the alternative is an installed job that fails
/// every night on a file only a reinstall can fix.
fn stamp_and_serialize_manifest(
    mut manifest_v1: ScheduleManifestV1,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<u8>> {
    // Seconds precision and a `Z` suffix: the schema requires UTC, and the
    // stamp is read by people and compared for drift, not used as a clock.
    manifest_v1.installed_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    manifest::validate_v1(&manifest_v1)?;
    Ok(manifest::serialize_manifest(&manifest_v1))
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
        // Everything else — not a symlink, or the metadata call itself failed
        // (NotFound, permission denied, an I/O error) — falls through on
        // purpose. This check exists only to reject a symlink; it is not the
        // existence or readability check. A path that cannot be stat'ed still
        // has to pass the equality below, and `load_manifest` then opens the
        // owned path no-follow and surfaces the real error there, where the
        // message can name reinstall.
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
/// Ordering is the security property: the path check, the bounded,
/// symlink-refusing manifest load, and the full drift validation all happen
/// before anything that could construct a backend, so a malformed, oversized,
/// symlinked, foreign or drifted manifest cannot reach a provider — let alone
/// mutate a secret.
///
/// **No target-selection input comes from the environment.** Nothing on this
/// path reads `XV_BACKEND` or `XV_ENV`, nothing reads or writes the ambient
/// context file — the sweep passes `track_context_usage: false`, precisely
/// because a usage bump would change the very bytes `context_digest` pins —
/// and the directory the scheduler happened to start the process in does not
/// select anything: the run sets the process cwd to the recorded
/// `working_directory` before the sweep, so ambient-cwd helpers see the
/// directory the manifest names. Every selection input comes out of the
/// manifest.
///
/// Two environment reads remain, and neither selects a target: the state root
/// (`XV_STATE_HOME`/`XDG_STATE_HOME`), which locates the owned manifest and is
/// pinned into the unit at install time, and `XV_NO_PARENT_CONFIG`, which
/// `resolve_record_types` consults when loading custom `[types.*]` schemas
/// from the recorded project file — record *shapes*, not which vault or
/// backend is rotated. (Install refuses when that variable would change
/// project discovery; see `drift`.)
async fn execute_run(supplied_manifest: &Path) -> Result<()> {
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

    // The lock comes before validation, and after the two checks above.
    //
    // Before validation, because validation reads the user's config, project
    // and context files and probes the vault: two runners doing that at once
    // is the concurrency the lock exists to prevent, and the loser must leave
    // before it touches anything. After the path checks, because those decide
    // whether this process is a runner at all — a `--manifest` pointing
    // somewhere else is a misconfigured unit, and it must fail loudly with
    // its own error rather than be reported as a skipped run.
    let Some(_run_guard) = outcome::RunGuard::try_acquire(&state_paths)? else {
        // One line, exit zero, `last-run.json` untouched: the run that holds
        // the lock owns the current outcome, and a scheduler that treated a
        // benign overlap as a failure would alert every night a sweep ran
        // long.
        eprintln!("schedule rotation skipped: another run is already_running");
        return Ok(());
    };

    // Bounded, no-follow, schema-validated. Any failure names reinstall,
    // because a manifest this process cannot trust is not something a
    // scheduled run may work around.
    //
    // A manifest that does not load produces no outcome: there is no
    // installation to bind a record to, and `manifest_digest` is what makes a
    // record meaningful. The error is the report in that case.
    let (manifest::ScheduleManifest::V1(manifest), manifest_bytes) =
        manifest::load_manifest_with_bytes(&state_paths).map_err(|e| {
            CrosstacheError::config(format!(
                "{e}. Reinstall the schedule with 'xv schedule install' to regenerate it."
            ))
        })?;

    // Resolved *before* the `running` record, not at its use site below.
    // `recorded_binary_path` reads `current_exe()`, which can fail (a deleted
    // or unreadable executable). Every fallible step that cannot produce a
    // terminal outcome has to happen while there is still no record to leave
    // dangling — see the region marker below.
    let binary_path = recorded_binary_path()?;

    // The digest of exactly the bytes just parsed — not a re-serialization,
    // and not a second read — so status can tell a record written by *this*
    // installation from one retained across a reinstall.
    let manifest_digest = crate::config::content_digest(&manifest_bytes);
    let started_at = outcome::now_rfc3339_utc();

    // Written before anything else can fail. A process killed after this
    // point leaves `running`, which is precisely the claim that is true: a
    // run started and did not report back.
    outcome::write_outcome_atomic(
        &state_paths,
        &outcome::RunOutcomeV1::running(&manifest_digest, &started_at),
    )?;

    // -----------------------------------------------------------------
    // [[run-outcome-region:begin]]
    //
    // NO EARLY RETURNS — from the `running` write above to the terminal
    // write below.
    //
    // Nothing in here may use `?` or `return`. A `running` record that is
    // never replaced is indistinguishable from a runner that was killed
    // mid-sweep, so an ordinary error escaping this region would make
    // `xv schedule status` report a phantom interrupted run forever.
    // Every step here either yields a `RunOutcomeDraft` (which always has
    // a terminal state) or, like the outcome write itself, degrades to a
    // warning. `schedule_run_has_no_early_return_between_the_outcome_writes`
    // enforces this on the source text.
    // -----------------------------------------------------------------

    // Recompute the recorded target from the recorded inputs. This reads
    // files and nothing else — no backend is constructed until it returns a
    // non-refusing verdict (design invariant 4).
    let report =
        drift::validate_recorded_target(&manifest, &binary_path, env!("CARGO_PKG_VERSION")).await;

    for warning in &report.warnings {
        output::warn(&warning.detail);
    }

    let draft = if report.is_refused() {
        refused_drift_outcome(&report)
    } else {
        execute_pinned_run(&manifest).await
    };

    let (record, result) = draft.finish(&manifest_digest, &started_at);
    if let Err(error) = outcome::write_outcome_atomic(&state_paths, &record) {
        // The sweep already happened; failing to record it must not rewrite
        // what the process reports about it. Say so loudly and keep the run's
        // own verdict — status will show a stale (or absent) record, which is
        // the honest reading of a state directory this process cannot write.
        output::warn(&format!(
            "could not record the run outcome in '{}': {error}",
            state_paths.last_run_path().display()
        ));
    }
    // [[run-outcome-region:end]]
    // -----------------------------------------------------------------
    result
}

/// What the run did, in the shape `last-run.json` needs.
///
/// The sweep builds this draft and the caller turns it into the persisted
/// [`RunOutcomeV1`], so the decision about *what happened* is separate from the
/// decision about *what is written* — the draft also carries the human-facing
/// error, which the file may not. Everything it contributes to the file is safe
/// to serialize: counts, a fixed state token, and a diagnostic whose code and
/// message come from closed sets (`DueRotationFailureCategory::code`,
/// `DueRotationErrorKind::code`, or the drift fields) — never from a secret
/// name, a vault value, or a provider error body.
#[derive(Debug)]
pub(crate) struct RunOutcomeDraft {
    pub(crate) state: RunState,
    pub(crate) summary: Option<DueRotationSummary>,
    pub(crate) diagnostic: Option<RunDiagnostic>,
    /// The error this outcome exits with. Deliberately not part of the
    /// persisted shape: it is the human-facing CLI error, which may name the
    /// vault and quote a provider message, while `diagnostic` is the redacted
    /// form a result file may keep.
    error: Option<CrosstacheError>,
}

impl RunOutcomeDraft {
    /// Split the draft into the record that goes on disk and the exit
    /// behavior the process takes.
    ///
    /// `exit_code` is the code this very process will exit with, derived from
    /// the very error the caller returns — so a reader of
    /// `last-run.json` and a caller watching `$?` can never disagree. Only
    /// aggregate counts cross into the record; `DueRotationSummary::failures`
    /// (which names secrets) stays behind.
    fn finish(self, manifest_digest: &str, started_at: &str) -> (RunOutcomeV1, Result<()>) {
        let exit_code = self
            .error
            .as_ref()
            .map(CrosstacheError::exit_code)
            .unwrap_or(0);
        let outcome = RunOutcomeV1 {
            schema_version: 1,
            schedule_id: manifest::SCHEDULE_ID.to_string(),
            manifest_digest: manifest_digest.to_string(),
            started_at: started_at.to_string(),
            finished_at: Some(outcome::now_rfc3339_utc()),
            state: self.state,
            exit_code: Some(exit_code),
            summary: self.summary.as_ref().map(|summary| RunSummary {
                policy_managed: summary.policy_managed as u64,
                due: summary.due as u64,
                rotated: summary.rotated as u64,
                failed: summary.failed as u64,
            }),
            diagnostic: self.diagnostic,
        };
        let result = match self.error {
            Some(error) => Err(error),
            None => Ok(()),
        };
        (outcome, result)
    }
}

/// The refusal outcome for a full validation report.
fn refused_drift_outcome(report: &drift::DriftReport) -> RunOutcomeDraft {
    refused_drift_from_reasons(&report.reasons)
}

/// Print drift reasons and build the refusal outcome.
///
/// One stderr line per reason, in manifest-field order, plus a hint naming the
/// only operation that accepts a changed target. The exit is the ordinary
/// configuration-error code (3) via [`CrosstacheError::config`]. Used both for
/// a full [`drift::DriftReport`] and for the single-reason refusals the run
/// itself discovers (a working directory that has since gone, a config file
/// that has since become unreadable, a vault that no longer verifies).
fn refused_drift_from_reasons(reasons: &[drift::DriftReason]) -> RunOutcomeDraft {
    output::error("The recorded rotation target has changed; refusing to rotate.");
    for reason in reasons {
        output::error(&format!("  - {}", reason.detail));
    }
    output::hint("Review the changes, then run 'xv schedule install' to accept the new target.");

    let fields: Vec<&str> = reasons.iter().map(|reason| reason.field).collect();
    RunOutcomeDraft {
        state: RunState::RefusedDrift,
        summary: None,
        // Field names only — the same closed set the manifest schema defines,
        // never a path or a file's contents.
        diagnostic: Some(RunDiagnostic::new(
            "target_drift",
            format!(
                "{} changed; review the recorded target and reinstall",
                fields.join(" and ")
            ),
        )),
        error: Some(CrosstacheError::config(format!(
            "the recorded rotation target has drifted ({} reason(s) reported above); reinstall \
             the schedule with 'xv schedule install' to accept the new target.",
            reasons.len()
        ))),
    }
}

/// Run the sweep the manifest pinned, after drift validation has passed.
///
/// Every input is the recorded one: the process moves to the recorded working
/// directory (helpers reached from rotation — record-type resolution, for one
/// — still read the ambient directory, and the recorded one is the directory
/// the target was resolved in), the configuration is re-read from the recorded
/// path rather than taken from the process config, and only the recorded
/// registry backend is constructed. `runtime_open_existing_local` is set so a
/// local store that has gone missing is an error instead of being invented.
///
/// Returns a [`RunOutcomeDraft`] on **every** path, including the failures
/// before rotation starts: an unattended run has to be able to record what it
/// did (or refused to do), and a bare `Err` here would be the one outcome
/// PR 3's result file could not describe.
async fn execute_pinned_run(manifest: &ScheduleManifestV1) -> RunOutcomeDraft {
    let working_directory = Path::new(&manifest.execution.working_directory);
    if std::env::set_current_dir(working_directory).is_err() {
        // Validation checked this directory moments ago, so this is a race (or
        // a permission change) rather than ordinary drift — but it is the same
        // condition and the same advice, so it is reported the same way.
        return refused_drift_from_reasons(&[drift::DriftReason::missing_at(
            "working_directory",
            &manifest.execution.working_directory,
        )]);
    }

    pin_recorded_environment(&manifest.target);

    run_recorded_sweep(manifest).await
}

/// Pin `XV_ENV` in this process to the recorded environment — or remove it
/// when the manifest recorded none.
///
/// [`prepare_recorded_config`]'s `env_flag` is not sufficient on its own:
/// `project::resolve_env` reads `XV_ENV` *first* and falls back to the flag
/// second, and a scheduler unit cannot unset a variable the user manager
/// already exported into every job it starts (systemd `environment.d`,
/// `launchctl setenv`). An ambient `XV_ENV` would therefore outrank the pinned
/// target, and for a manifest that recorded no environment it would select one
/// installation never approved.
///
/// Mutating the process environment is acceptable *here and nowhere else*:
/// `xv schedule run` is a one-shot process whose whole job is to replay one
/// recorded target, it is still single-threaded when this runs, and nothing
/// after it wants the inherited value. Library code must never do this — it
/// would reach into a caller's process.
fn pin_recorded_environment(target: &ManifestTarget) {
    match target.environment.as_deref() {
        Some(environment) => std::env::set_var("XV_ENV", environment),
        None => std::env::remove_var("XV_ENV"),
    }
}

/// The configuration the pinned sweep runs under: the file read back from the
/// recorded path, with the recorded `.xv.toml` environment replayed onto it.
///
/// Installation resolved `XV_ENV`/`--env` once and wrote the winning name into
/// `target.environment`; the runner replays that name and consults neither
/// ambient source (spec: scheduled-target-manifest, invariant 2). Drift
/// validation already replays it for *its* recomputation, but the sweep itself
/// ran with `env_flag: None` — so any helper reached from rotation that
/// resolves a project profile (`Config::resolve_vault_name`,
/// `resolve_group`, `resolve_record_types`'s project walk) would fall back to
/// the project file's `default_env`, or fail closed when the file defines
/// environments and none is selected.
///
/// `env_flag` is only half the fix. `project::resolve_env` consults `XV_ENV`
/// first and the flag second, and while the rendered units carry no `XV_ENV`
/// of their own they cannot *unset* one a user manager already exported into
/// every job it starts (systemd `environment.d`, `launchctl setenv`). The
/// other half is in [`execute_pinned_run`], which pins `XV_ENV` in the
/// runner's own process before the sweep.
///
/// The profile itself is **not** folded into the config here. The sweep's
/// backend and vault come literally from the manifest (`run_due_rotation`
/// performs no workspace, context, or vault-name resolution), so folding
/// `backend`/`vault` would re-resolve a selection the manifest already pinned.
/// `None` is replayed as `None`: no environment was recorded, so none is
/// selected.
fn prepare_recorded_config(mut config: Config, target: &ManifestTarget) -> Config {
    // A local store that has gone missing is an error, never something this
    // run invents.
    config.runtime_open_existing_local = true;
    config.env_flag = target.environment.clone();
    config
}

/// The pinned sweep, from the recorded working directory.
///
/// Split from [`execute_pinned_run`] so every step after the process-global
/// `set_current_dir` is unit-testable: a test can exercise the config re-read
/// and the vault verification without moving the test runner's own working
/// directory.
async fn run_recorded_sweep(manifest: &ScheduleManifestV1) -> RunOutcomeDraft {
    let config_path = Path::new(&manifest.target.config_path);
    let Ok((file_config, _bytes)) =
        crate::config::settings::load_config_file_at_with_bytes(config_path).await
    else {
        return refused_drift_from_reasons(&[drift::DriftReason::missing_at(
            "config_path",
            &manifest.target.config_path,
        )]);
    };
    let file_config = prepare_recorded_config(file_config, &manifest.target);

    let backend_name = manifest.target.backend_name.as_str();
    let vault = manifest.target.vault.as_str();
    let entry = crate::workspace::WorkspaceEntry {
        alias: manifest.target.workspace_alias.clone().unwrap_or_else(|| {
            // The degenerate workspace's own labelling rule — the same one
            // `select_entry` and drift validation use, so one vault never has
            // two alias spellings.
            crate::workspace::degenerate_alias_for(&file_config, vault)
        }),
        backend: backend_name.to_string(),
        vault: vault.to_string(),
        default: true,
    };

    // The same read-only verification installation performed, against the same
    // target: a vault this process cannot list is not one it may rotate. The
    // provider's error body is deliberately dropped — this reason is written
    // to an unattended log.
    if crate::schedule::target::probe_selected_target(&file_config, &entry)
        .await
        .is_err()
    {
        return refused_drift_from_reasons(&[drift::DriftReason::new(
            "vault",
            format!(
                "vault '{vault}' on '{backend_name}' no longer verifies; review the vault and \
                 reinstall"
            ),
        )]);
    }

    let registry = match crate::backend::BackendRegistry::with_lazy(
        &file_config,
        std::slice::from_ref(&entry.backend),
    ) {
        Ok(registry) => registry,
        Err(error) => {
            // Not drift — the target still resolves, this process just could
            // not build it. Recorded with the same closed-set code the
            // due-rotation service uses for an unusable backend.
            let message = "the recorded backend could not be constructed";
            output::error(&format!(
                "cannot construct the recorded backend '{backend_name}': {error}"
            ));
            return RunOutcomeDraft {
                state: RunState::Failed,
                summary: None,
                diagnostic: Some(RunDiagnostic::new("backend-unavailable", message)),
                error: Some(CrosstacheError::config(format!(
                    "cannot construct the recorded backend '{backend_name}'"
                ))),
            };
        }
    };

    output::step(&format!(
        "Scheduled rotation sweep of '{vault}' on backend '{backend_name}'."
    ));

    match run_due_rotation(
        &file_config,
        &registry,
        backend_name,
        vault,
        &DueRotationOptions::default(),
        &mut SilentObserver,
    )
    .await
    {
        Ok(summary) if summary.failed == 0 => {
            output::success(&format!(
                "Rotated {} of {} due secret(s) in '{vault}'.",
                summary.rotated, summary.due
            ));
            RunOutcomeDraft {
                state: RunState::Success,
                summary: Some(summary),
                diagnostic: None,
                error: None,
            }
        }
        Ok(summary) => {
            // A partial batch must not look like a success — the same rule
            // (and the same configuration-error exit) `xv rotate --due` uses.
            let mut codes: Vec<&'static str> = summary
                .failures
                .iter()
                .map(|failure| failure.code())
                .collect();
            codes.sort_unstable();
            codes.dedup();
            let error = CrosstacheError::config(format!(
                "rotated {} of {} due secret(s) in '{vault}'; {} failed ({})",
                summary.rotated,
                summary.due,
                summary.failed,
                codes.join(", ")
            ));
            output::error(&error.to_string());
            RunOutcomeDraft {
                state: RunState::PartialFailure,
                diagnostic: summary
                    .failures
                    .first()
                    .map(|failure| RunDiagnostic::new(failure.code(), failure.message())),
                summary: Some(summary),
                error: Some(error),
            }
        }
        Err(failure) => {
            // Whole-run failure: the vault was never read, so there is no
            // honest summary to record. The redacted code/message go to the
            // outcome; the source error is what the process exits with.
            let diagnostic = RunDiagnostic::new(failure.code(), failure.message());
            RunOutcomeDraft {
                state: RunState::Failed,
                summary: None,
                diagnostic: Some(diagnostic),
                error: Some(failure.into()),
            }
        }
    }
}

/// Read-only diagnosis of what is installed.
///
/// Collection (`schedule::status::collect_status`) and rendering
/// (`schedule::status_render::render_status`) are separate: the collector
/// probes and reads, the renderer is a pure function of what it found, which
/// is what makes every golden layout testable without a scheduler or a
/// filesystem.
///
/// It contacts no provider — vault verification belongs to install and to the
/// run itself — writes nothing, and never claims a target it cannot read from
/// the manifest.
///
/// **Channel:** the whole block goes to stderr, like every other `xv`
/// diagnostic. `status` is human status chrome, not data: the repository's
/// scripting contract reserves stdout for machine-consumable payloads, and a
/// `[ok]`/`[hint]`-prefixed block is not one. The exit code is the machine-
/// readable part (see [`status_render::status_failure`]): `0` for every state
/// `status` can describe accurately, the configuration-error code `3` for a
/// schedule that would refuse its next run, a manifest that cannot be read,
/// and a scheduler that could not be queried at all.
async fn execute_status() -> Result<()> {
    let platform = Platform::detect()?;
    let home = home_dir()?;
    let unit_paths = UnitPaths::for_platform(platform, &home);
    let state_paths = manifest::resolve_from_process_env()?;

    let report = status::collect_status(
        platform,
        &unit_paths,
        &state_paths,
        schedule_runner().as_ref(),
        &recorded_binary_path()?,
        env!("CARGO_PKG_VERSION"),
    )
    .await?;

    eprintln!(
        "{}",
        status_render::render_status(&report, platform, output::should_use_rich_stderr())
    );

    match status_render::status_failure(&report, platform) {
        Some(message) => Err(CrosstacheError::config(message)),
        None => Ok(()),
    }
}

/// Remove the schedule and the manifest, and nothing else.
///
/// Runs under the same `install.lock` an install takes, so an uninstall cannot
/// interleave with a reinstall. What it may remove is fixed by the design's
/// ownership table; every other file in the state directory — the last
/// outcome, both lock inodes, `recovery/`, the log, anything the user left
/// there — is retained, and so is anything at an owned path that xv did not
/// write.
async fn execute_uninstall() -> Result<()> {
    let platform = Platform::detect()?;
    let home = home_dir()?;
    let unit_paths = UnitPaths::for_platform(platform, &home);
    let state_paths = manifest::resolve_from_process_env()?;

    let report = {
        let mut store = RealOwnedScheduleStore::open(&state_paths)?;
        uninstall_owned(
            platform,
            &unit_paths,
            &mut store,
            schedule_runner().as_ref(),
        )?
        // The store is dropped here, releasing `install.lock`.
    };

    if report.removed_anything() {
        output::success(&format!(
            "Removed the {} rotation schedule.",
            platform.name()
        ));
        if report.removed_manifest {
            output::info(&format!(
                "  Removed:   {}",
                state_paths.manifest_path().display()
            ));
        }
        for unit in &report.removed_units {
            output::info(&format!("  Removed:   {}", unit.display()));
        }
        output::info(
            "  Retained:  the last-run record, both lock files, recovery evidence and the \
             rotation log.",
        );
    } else if report.scheduler_error.is_none() {
        output::info(&format!(
            "No {} rotation schedule was installed; nothing to remove.",
            platform.name()
        ));
    }

    for path in &report.foreign {
        output::warn(&format!(
            "  Retained:  {} (xv did not write it, so it was left alone)",
            path.display()
        ));
    }

    // A foreign unit at an owned path stops uninstall from deregistering, so
    // the job is still registered and will still fire. "Removed the ... rotation
    // schedule." on its own would be a false reading of that, and the one
    // person who can move the file aside is reading this line.
    if let Some(path) = &report.deregistration_blocked_by {
        output::warn(&format!(
            "  Retained:  the scheduler registration was left in place because {} is not managed \
             by xv.",
            path.display()
        ));
    }

    remove_schedule_dir_if_empty(&state_paths);

    if let Some(detail) = report.scheduler_error.clone() {
        // Not absence, and not something to paper over. What is true depends on
        // what actually happened: with the files gone the scheduler may still
        // hold a registration; with nothing removed we simply do not know what
        // is installed.
        let situation = if report.removed_anything() {
            "the rotation schedule's files were removed but the scheduler could not be asked to \
             deregister it"
        } else {
            "nothing was removed and the scheduler could not be asked whether anything is \
             registered"
        };
        return Err(CrosstacheError::config(format!(
            "{situation} ({detail}). Check the scheduler and re-run 'xv schedule uninstall'."
        )));
    }
    Ok(())
}

/// Remove the schedule's own directory once nothing is left in it.
///
/// In practice `install.lock` is retained and keeps it non-empty; this exists
/// so a directory that *is* empty — an install that never got past its lock
/// being cleaned up by hand — does not linger. The parent `schedules/`
/// directory is never touched, and a failure here is not worth an error: the
/// schedule is already gone.
fn remove_schedule_dir_if_empty(paths: &manifest::ScheduleStatePaths) {
    if let Ok(mut entries) = std::fs::read_dir(paths.root()) {
        if entries.next().is_none() {
            let _ = std::fs::remove_dir(paths.root());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `running` record must always be replaceable.
    ///
    /// A `running` outcome that is never replaced is indistinguishable from a
    /// runner that was killed mid-sweep, so `xv schedule status` would report
    /// a phantom interrupted run forever. `execute_run` therefore marks the
    /// span between the two outcome writes as a no-early-return region, and
    /// this test enforces that on the source text: a `?` or a `return` added
    /// there later fails here rather than in the field at 3am.
    ///
    /// A source-text assertion is blunt, but the alternative — injecting a
    /// failure into `current_exe()` or `set_current_dir` — needs seams the
    /// runner deliberately does not have.
    #[test]
    fn schedule_run_has_no_early_return_between_the_outcome_writes() {
        const SOURCE: &str = include_str!("schedule_ops.rs");
        // Assembled rather than written out, so this test's own source does
        // not contain the markers it searches for.
        let start_marker = format!("[[run-outcome-region:{}]]", "begin");
        let end_marker = format!("[[run-outcome-region:{}]]", "end");

        let start = SOURCE
            .find(&start_marker)
            .expect("the region start marker is present in execute_run");
        let end = SOURCE
            .find(&end_marker)
            .expect("the region end marker is present in execute_run");
        assert!(start < end, "the region markers are out of order");
        assert_eq!(
            SOURCE.matches(&start_marker).count(),
            1,
            "the region start marker must appear exactly once"
        );
        assert_eq!(
            SOURCE.matches(&end_marker).count(),
            1,
            "the region end marker must appear exactly once"
        );
        let region = &SOURCE[start..end];
        // Sanity: the region really is the body between the two writes, not
        // an empty slice that would make the scan below vacuous.
        assert!(
            region.contains("drift::validate_recorded_target"),
            "the region does not span the validation/sweep body"
        );

        for (offset, line) in region.lines().enumerate() {
            let code = line.trim();
            if code.starts_with("//") || code.starts_with("///") {
                continue;
            }
            assert!(
                !code.contains('?'),
                "line {offset} of the no-early-return region uses `?`: {code}"
            );
            // `return ` catches a value; the bare forms (`return;`, and a
            // `return` that is the last token before the closing brace of a
            // block) return from a `-> ()` helper just as early, so they are
            // caught too.
            let bare_return = code == "return"
                || code.starts_with("return;")
                || code.starts_with("return }")
                || code.starts_with("return}");
            assert!(
                !code.starts_with("return ") && !bare_return,
                "line {offset} of the no-early-return region returns early: {code}"
            );
        }
    }

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
                vault_selection: "implicit".to_string(),
            },
        }
    }

    /// A config file selecting a local store that was never created, so the
    /// read-only probe fails the way a vanished store does.
    fn unopened_local_config(dir: &Path) -> PathBuf {
        let path = dir.join("xv.conf");
        let store = dir.join("never-opened-store");
        let key = dir.join("never-opened-key.txt");
        // The two paths go in as TOML *literal* strings (single quotes): a
        // Windows temp path is full of backslashes, and in a basic string
        // `\U`/`\n`/`\t` are escape sequences, so the file would not parse at
        // all — the drift run would then stop at `config_path is missing or
        // unreadable` and never reach the vault probe this test is about.
        std::fs::write(
            &path,
            format!(
                "backend = \"local\"\ndebug = false\nsubscription_id = \"\"\ndefault_vault = \"default\"\n\
                 default_resource_group = \"\"\ndefault_location = \"\"\ntenant_id = \"\"\n\
                 output_json = false\nno_color = true\ncache_enabled = false\ncache_ttl_secs = 0\n\
                 clipboard_timeout = 0\n\n[local]\nstore_path = '{}'\nkey_file = '{}'\n\
                 default_vault = \"default\"\n",
                store.display(),
                key.display()
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn the_sweep_config_replays_the_recorded_environment() {
        // Installation resolved `XV_ENV`/`--env` once; the sweep replays that
        // name through `env_flag`, which is what `project::resolve_env` reads
        // for every project-profile lookup rotation can reach. Without it the
        // sweep ran unselected and a project file defining environments (with
        // no `default_env`) failed closed under it.
        let tmp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(tmp.path());
        manifest.target.environment = Some("production".to_string());

        let loaded = Config::default();
        assert_eq!(loaded.env_flag, None, "a config read off disk selects none");

        let prepared = prepare_recorded_config(loaded, &manifest.target);

        assert_eq!(prepared.env_flag.as_deref(), Some("production"));
        // The other half of the preparation is unchanged: a vanished local
        // store is an error, not something this run creates.
        assert!(prepared.runtime_open_existing_local);
    }

    #[test]
    fn no_recorded_environment_selects_none() {
        // A target with no `.xv.toml` environment must not inherit one: the
        // replay clears the field rather than leaving whatever was there.
        let tmp = tempfile::tempdir().unwrap();
        let manifest = valid_manifest(tmp.path());
        assert_eq!(manifest.target.environment, None, "fixture shape changed");

        let loaded = Config {
            env_flag: Some("staging".to_string()),
            ..Config::default()
        };

        let prepared = prepare_recorded_config(loaded, &manifest.target);

        assert_eq!(prepared.env_flag, None);
    }

    /// The runner pins `XV_ENV` in its own process, because `env_flag` alone
    /// loses to an inherited one (`project::resolve_env` reads the variable
    /// first) and a unit cannot unset what a user manager exported.
    #[test]
    fn the_runner_pins_the_recorded_environment_over_an_inherited_one() {
        let _guard = crate::config::project::test_support::XvEnvGuard::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(tmp.path());
        manifest.target.environment = Some("production".to_string());

        std::env::set_var("XV_ENV", "staging");
        pin_recorded_environment(&manifest.target);

        assert_eq!(std::env::var("XV_ENV").ok().as_deref(), Some("production"));
    }

    /// A manifest that recorded no environment must leave the process with
    /// none — an inherited `XV_ENV` would otherwise select a profile that
    /// installation never approved, and can fail the run closed.
    #[test]
    fn no_recorded_environment_removes_an_inherited_one() {
        let _guard = crate::config::project::test_support::XvEnvGuard::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let manifest = valid_manifest(tmp.path());
        assert_eq!(manifest.target.environment, None, "fixture shape changed");

        std::env::set_var("XV_ENV", "staging");
        pin_recorded_environment(&manifest.target);

        assert!(std::env::var("XV_ENV").is_err(), "XV_ENV must be removed");
    }

    #[tokio::test]
    async fn a_vault_that_no_longer_verifies_produces_a_refused_drift_draft() {
        // The "selected vault no longer verifies" row of the drift table: the
        // run must produce an outcome, not a bare error, and the reason must
        // not carry the provider's error body into an unattended log.
        let tmp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(tmp.path());
        manifest.target.config_path = unopened_local_config(tmp.path())
            .to_string_lossy()
            .to_string();

        let draft = run_recorded_sweep(&manifest).await;

        assert_eq!(draft.state, RunState::RefusedDrift);
        let diagnostic = draft.diagnostic.expect("a refusal carries a diagnostic");
        assert_eq!(diagnostic.code, "target_drift");
        assert_eq!(
            diagnostic.message,
            "vault changed; review the recorded target and reinstall"
        );
        let rendered = draft.error.expect("a refusal exits non-zero").to_string();
        for leak in ["never-opened-store", "never-opened-key", "age", "decrypt"] {
            assert!(!rendered.contains(leak), "leaked '{leak}': {rendered}");
        }
    }

    #[tokio::test]
    async fn an_unreadable_recorded_config_produces_a_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = valid_manifest(tmp.path());

        let draft = run_recorded_sweep(&manifest).await;

        assert_eq!(draft.state, RunState::RefusedDrift);
        assert_eq!(
            draft.diagnostic.expect("diagnostic").message,
            "config_path changed; review the recorded target and reinstall"
        );
    }

    #[tokio::test]
    async fn a_working_directory_that_cannot_be_entered_produces_a_draft() {
        // `valid_manifest`'s working directory does not exist, so the chdir
        // fails and the process's own directory is never changed.
        let tmp = tempfile::tempdir().unwrap();
        let manifest = valid_manifest(tmp.path());

        let draft = execute_pinned_run(&manifest).await;

        assert_eq!(draft.state, RunState::RefusedDrift);
        assert_eq!(
            draft.diagnostic.expect("diagnostic").message,
            "working_directory changed; review the recorded target and reinstall"
        );
        assert!(draft.error.is_some());
    }

    /// State paths rooted in a tempdir, through the real resolver.
    fn state_paths_in(dir: &Path) -> manifest::ScheduleStatePaths {
        manifest::test_paths_in(dir)
    }

    /// Stamp, validate and publish in one step — the two halves production
    /// runs through the install transaction, so the ordering assertions below
    /// still test the real sequence.
    fn stamp_and_write_manifest(
        paths: &manifest::ScheduleStatePaths,
        manifest_v1: ScheduleManifestV1,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<PathBuf> {
        let bytes = stamp_and_serialize_manifest(manifest_v1, now)?;
        manifest::write_manifest_atomic(paths, &bytes)?;
        Ok(paths.manifest_path())
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
        let (crate::schedule::manifest::ScheduleManifest::V1(loaded), _) =
            manifest::load_manifest_with_bytes(&paths).expect("loads back");
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
