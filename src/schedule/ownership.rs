//! What is actually installed, and whether `xv` may claim it.
//!
//! `xv schedule status` has to answer two independent questions before it says
//! anything else, and the design
//! (`docs/superpowers/specs/2026-09-09-scheduled-target-manifest-design.md`,
//! "Status contract") keeps them apart on purpose:
//!
//! 1. **What does the scheduler say?** `installed`, `absent`, or `error`. A
//!    `launchctl` that cannot run, or a `schtasks` that fails for a reason
//!    other than "no such task", is *not* evidence of absence. Collapsing the
//!    two is how a status command ends up telling a user their rotation
//!    schedule is gone when it is running fine.
//! 2. **What is on disk, and did we write it?** `managed`, `legacy-unpinned`,
//!    `orphaned-manifest`, `foreign`, or nothing at all.
//!
//! Ownership is decided from bytes, never from hope: a unit must carry the
//! `Managed by crosstache (xv schedule)` marker to be ours at all (that check
//! lives in [`crate::schedule::install`], which uses the same rules to decide
//! what a reinstall may replace), and its *command* must be one of the two
//! shapes `xv` has ever rendered. Anything else at an owned path is reported
//! and left exactly where it is.
//!
//! Nothing here contacts a secrets provider, and nothing here writes.

use std::path::PathBuf;

use crate::error::Result;
use crate::schedule::install::{classify_owned_manifest, classify_owned_unit, ArtifactState};
use crate::schedule::manifest::ScheduleStatePaths;
use crate::schedule::{
    launchd_domain_target, quote_if_needed, unit_paths_for, CommandOutput, CommandRunner, Platform,
    ScheduleCommand, UnitPaths, SCHTASKS_NAME, SYSTEMD_UNIT,
};

/// What the platform scheduler answered when asked about our entry.
///
/// Four states, because collapsing any two of them lies to somebody: a
/// `launchctl` that will not run is not an absent schedule, and a `systemctl`
/// whose output we could not read is not a registered one either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerState {
    /// The scheduler reports our entry is registered.
    Installed,
    /// The scheduler ran and said it has no such entry.
    Absent,
    /// The scheduler ran, exited successfully, and said something we could not
    /// interpret. Presence is unproven either way.
    Unknown,
    /// The scheduler could not be run, or failed for some reason other than
    /// "no such entry". The string is sanitized: the command name and its exit
    /// status, never the raw output, which on Windows is locale-dependent and
    /// on Unix may quote paths from another user's job.
    Error(String),
}

impl SchedulerState {
    /// Display form for the `Scheduler:` status line.
    pub fn describe(&self) -> String {
        match self {
            Self::Installed => "installed".to_string(),
            Self::Absent => "not registered".to_string(),
            Self::Unknown => "unknown (the scheduler gave no readable answer)".to_string(),
            Self::Error(detail) => format!("error ({detail})"),
        }
    }
}

/// Who owns what is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    /// A manifest and a native entry that runs it. The only state that may be
    /// described as a working pinned schedule.
    Managed,
    /// A native entry `xv` wrote whose target cannot be proven: either the old
    /// `rotate --due --force` sweep, which re-resolved its target at run time,
    /// or a pinned unit whose manifest has since gone. Reported with the
    /// command line actually installed, because that is the only honest thing
    /// we can say about what it will do.
    LegacyUnpinned { command_line: String },
    /// A manifest with no native entry. Reinstall repairs it; uninstall
    /// removes it.
    OrphanedManifest,
    /// Something `xv` did not write sits at a path it owns. Retained, never
    /// removed, never adopted.
    Foreign { paths: Vec<PathBuf> },
    /// No manifest and no native entry.
    Absent,
}

impl Ownership {
    /// The `Ownership:` token from the goldens, or `None` when nothing is
    /// installed (there is no ownership state to report).
    pub fn label(&self) -> Option<&'static str> {
        match self {
            Self::Managed => Some("managed"),
            Self::LegacyUnpinned { .. } => Some("legacy-unpinned"),
            Self::OrphanedManifest => Some("orphaned-manifest"),
            Self::Foreign { .. } => Some("foreign"),
            Self::Absent => None,
        }
    }
}

/// The two independent dimensions, answered together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipReport {
    pub state: Ownership,
    pub scheduler: SchedulerState,
}

/// Inspect the owned paths and the scheduler, and decide what is installed.
///
/// Read-only: it reads at most the owned unit files and `manifest.json`
/// (bounded and without following a final symlink, through
/// [`crate::schedule::install`]'s classifiers) and runs the platform's own
/// query command. It never constructs a backend.
pub fn inspect_ownership(
    platform: Platform,
    unit_paths: &UnitPaths,
    state: &ScheduleStatePaths,
    runner: &dyn CommandRunner,
) -> Result<OwnershipReport> {
    let scheduler = probe_scheduler(platform, runner);

    let mut foreign: Vec<PathBuf> = Vec::new();

    let manifest_path = state.manifest_path();
    let manifest_present = match classify_owned_manifest(&manifest_path)? {
        ArtifactState::Owned(_) => true,
        ArtifactState::Absent => false,
        ArtifactState::Foreign(_) => {
            foreign.push(manifest_path);
            false
        }
    };

    let mut units: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for path in unit_paths_for(platform, unit_paths) {
        match classify_owned_unit(&path) {
            Ok(ArtifactState::Owned(bytes)) => units.push((path, bytes)),
            Ok(ArtifactState::Absent) => {}
            Ok(ArtifactState::Foreign(_)) => foreign.push(path),
            // A unit we cannot open is a unit we cannot prove we wrote, which
            // is the definition of foreign here — and `status` must never
            // abort on one dimension it could not read when it can still
            // answer every other.
            Err(_) => foreign.push(path),
        }
    }

    if !foreign.is_empty() {
        return Ok(OwnershipReport {
            state: Ownership::Foreign { paths: foreign },
            scheduler,
        });
    }

    // Task Scheduler keeps no files of ours, so its registration *is* the
    // artifact. An errored probe there leaves presence unproven; the scheduler
    // dimension carries that, and ownership does not invent a claim from it.
    let native_present = match platform {
        Platform::Schtasks => scheduler == SchedulerState::Installed,
        _ => !units.is_empty(),
    };

    if !native_present {
        return Ok(OwnershipReport {
            state: if manifest_present {
                Ownership::OrphanedManifest
            } else {
                Ownership::Absent
            },
            scheduler,
        });
    }

    let state = match read_installed_command(platform, &units, &scheduler, runner) {
        CommandReading::Found(command_line) => match classify_command_line(&command_line) {
            // The pinned runner with the manifest it needs: the only managed
            // state.
            CommandShape::ManifestRun if manifest_present => Ownership::Managed,
            // A pinned unit whose manifest is gone will refuse itself at 3am,
            // and we cannot prove what it was aimed at either — the same claim
            // `legacy-unpinned` makes, with the same repair (reinstall).
            CommandShape::ManifestRun | CommandShape::Legacy => {
                Ownership::LegacyUnpinned { command_line }
            }
            CommandShape::Unrecognized => Ownership::Foreign {
                paths: units.into_iter().map(|(path, _)| path).collect(),
            },
        },
        // A unit carrying our marker whose command we cannot even read is not
        // a unit we wrote in any version.
        CommandReading::Unreadable => Ownership::Foreign {
            paths: units.into_iter().map(|(path, _)| path).collect(),
        },
        // Task Scheduler answered "registered" but the detailed query failed.
        // Fall back to the artifact we *can* read.
        CommandReading::NotQueried => {
            if manifest_present {
                Ownership::Managed
            } else {
                Ownership::LegacyUnpinned {
                    command_line: String::new(),
                }
            }
        }
    };

    Ok(OwnershipReport { state, scheduler })
}

// ---------------------------------------------------------------------------
// Scheduler probe
// ---------------------------------------------------------------------------

/// Ask the platform scheduler whether our entry exists, distinguishing "it
/// said no" from "it could not answer".
pub fn probe_scheduler(platform: Platform, runner: &dyn CommandRunner) -> SchedulerState {
    match platform {
        Platform::Launchd => {
            let target = launchd_domain_target();
            match runner.run("launchctl", &["print", &target]) {
                Err(_) => SchedulerState::Error("launchctl could not be run".to_string()),
                Ok(out) if out.ok() => SchedulerState::Installed,
                Ok(out) => classify_failure("launchctl print", &out),
            }
        }
        Platform::Systemd => {
            // `show` exits 0 even for a unit that does not exist, so the
            // properties are the real answer and a non-zero exit is a genuine
            // failure of the command itself.
            let timer = format!("{SYSTEMD_UNIT}.timer");
            match runner.run(
                "systemctl",
                &[
                    "--user",
                    "show",
                    &timer,
                    "--property=LoadState",
                    "--property=ActiveState",
                    "--property=UnitFileState",
                ],
            ) {
                Err(_) => SchedulerState::Error("systemctl could not be run".to_string()),
                Ok(out) if !out.ok() => classify_failure("systemctl --user show", &out),
                Ok(out) => {
                    let properties =
                        crate::schedule::install::parse_systemd_properties(&out.stdout);
                    if crate::schedule::install::systemd_is_registered(&properties) {
                        SchedulerState::Installed
                    } else if properties.contains_key("LoadState") {
                        SchedulerState::Absent
                    } else {
                        // `show` succeeded but printed nothing we recognize —
                        // not the same as it saying the timer does not exist.
                        SchedulerState::Unknown
                    }
                }
            }
        }
        Platform::Schtasks => match runner.run("schtasks", &["/Query", "/TN", SCHTASKS_NAME]) {
            Err(_) => SchedulerState::Error("schtasks could not be run".to_string()),
            Ok(out) if out.ok() => SchedulerState::Installed,
            Ok(out) => classify_failure("schtasks /Query", &out),
        },
    }
}

/// A non-zero exit is absence only when the scheduler said so in the words it
/// uses for "no such entry"; anything else is an error we must not launder
/// into "not installed".
///
/// An unreachable user bus is deliberately an *error* here even though
/// deregistration converges on absent for it: a `systemctl` that could not talk
/// to the user manager did not tell us whether a timer is enabled, and status
/// exists to say what is true rather than what is convenient.
fn classify_failure(what: &str, out: &CommandOutput) -> SchedulerState {
    if says_user_bus_unavailable(out) {
        SchedulerState::Error(format!(
            "{what} failed (exit {}): user bus unavailable",
            out.status
        ))
    } else if says_absent(out) {
        SchedulerState::Absent
    } else {
        SchedulerState::Error(format!("{what} failed (exit {})", out.status))
    }
}

/// Whether the failure was "there is no user service manager to ask".
///
/// For *uninstall* this is convergence — no user manager, no registered user
/// timer, nothing to remove — so
/// [`crate::schedule::unregister_native_reporting`] treats it as absence. For
/// `status` it is a question that went unanswered.
pub(crate) fn says_user_bus_unavailable(out: &CommandOutput) -> bool {
    format!("{} {}", out.stdout, out.stderr)
        .to_ascii_lowercase()
        .contains("failed to connect to bus")
}

/// The phrases each scheduler uses for "there is no such entry".
///
/// Kept deliberately small and lowercase-matched. `launchctl print` answers
/// 113 / "Could not find service", `launchctl bootout` answers 3 / "No such
/// process", `systemctl --user disable` answers "does not exist", and
/// `schtasks` answers "cannot find the file specified" / "does not exist".
pub(crate) fn says_absent(out: &CommandOutput) -> bool {
    const ABSENT_PHRASES: [&str; 6] = [
        "could not find service",
        "no such process",
        "no such file or directory",
        "does not exist",
        "cannot find the file specified",
        "not loaded",
    ];
    let haystack = format!("{} {}", out.stdout, out.stderr).to_ascii_lowercase();
    ABSENT_PHRASES
        .iter()
        .any(|phrase| haystack.contains(phrase))
}

// ---------------------------------------------------------------------------
// What the installed entry actually runs
// ---------------------------------------------------------------------------

/// The command line an owned entry invokes, when it can be read.
enum CommandReading {
    Found(String),
    /// A unit file is present and marked as ours, but carries no command we
    /// can parse.
    Unreadable,
    /// Task Scheduler holds the command and would not tell us.
    NotQueried,
}

/// The two command shapes `xv` has ever installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandShape {
    /// `rotate --due --force [--vault V]` — the pre-manifest sweep.
    Legacy,
    /// `schedule run --manifest <path>` — the pinned runner.
    ManifestRun,
    Unrecognized,
}

fn read_installed_command(
    platform: Platform,
    units: &[(PathBuf, Vec<u8>)],
    scheduler: &SchedulerState,
    runner: &dyn CommandRunner,
) -> CommandReading {
    match platform {
        Platform::Launchd => match units.first() {
            Some((_, bytes)) => match plist_program_arguments(&String::from_utf8_lossy(bytes)) {
                Some(argv) => CommandReading::Found(join_argv(&argv)),
                None => CommandReading::Unreadable,
            },
            None => CommandReading::Unreadable,
        },
        Platform::Systemd => {
            // Only the service unit carries a command; the timer never does.
            let service = units
                .iter()
                .find_map(|(_, bytes)| systemd_exec_start(&String::from_utf8_lossy(bytes)));
            match service {
                Some(argv) => CommandReading::Found(join_argv(&argv)),
                None => CommandReading::Unreadable,
            }
        }
        Platform::Schtasks => {
            if *scheduler != SchedulerState::Installed {
                return CommandReading::NotQueried;
            }
            match runner.run(
                "schtasks",
                &["/Query", "/TN", SCHTASKS_NAME, "/V", "/FO", "LIST"],
            ) {
                Ok(out) if out.ok() => match schtasks_task_to_run(&out.stdout) {
                    Some(command) => CommandReading::Found(command),
                    None => CommandReading::NotQueried,
                },
                _ => CommandReading::NotQueried,
            }
        }
    }
}

/// Which of the shapes a command line is.
///
/// The legacy arms are matched against [`ScheduleCommand::LegacyRotateDue`]'s
/// own arguments rather than a hand-written string, so the recognizer cannot
/// drift away from the shape the renderer produced.
fn classify_command_line(command_line: &str) -> CommandShape {
    let legacy_no_vault = ScheduleCommand::LegacyRotateDue { vault: None }
        .args()
        .join(" ");
    if command_line.contains(&legacy_no_vault) {
        return CommandShape::Legacy;
    }
    let pinned_prefix = ["schedule", "run", "--manifest"].join(" ");
    if command_line.contains(&pinned_prefix) {
        return CommandShape::ManifestRun;
    }
    CommandShape::Unrecognized
}

/// What `status` may honestly say about the target of an unpinned entry.
///
/// [`Ownership::LegacyUnpinned`] covers two different situations and they need
/// two different sentences. The pre-manifest `rotate --due --force` command
/// never recorded a target at all. A `schedule run --manifest` command did —
/// the manifest is simply gone, and saying "the legacy unit does not record
/// backend or account identity" about it would be false: it recorded one, at a
/// path we can name.
pub fn unverified_target_note(command_line: &str) -> String {
    match classify_command_line(command_line) {
        CommandShape::ManifestRun => match manifest_argument(command_line) {
            Some(path) => format!("unverified (the recorded manifest {path} is missing)"),
            None => "unverified (the recorded manifest is missing)".to_string(),
        },
        _ => "unverified (the legacy unit does not record backend or account identity)".to_string(),
    }
}

/// The value after `--manifest` in a command line, if there is one.
pub(crate) fn manifest_argument(command_line: &str) -> Option<String> {
    let after = command_line.split_once("--manifest")?.1.trim_start();
    let value = if let Some(quoted) = after.strip_prefix('"') {
        quoted.split_once('"').map(|(value, _)| value)?
    } else {
        after.split_whitespace().next()?
    };
    (!value.is_empty()).then(|| value.to_string())
}

fn join_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| quote_if_needed(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `ProgramArguments` from a launchd plist, in order.
pub(crate) fn plist_program_arguments(text: &str) -> Option<Vec<String>> {
    let after = text.split_once("<key>ProgramArguments</key>")?.1;
    let body = after.split_once("<array>")?.1.split_once("</array>")?.0;
    let mut argv = Vec::new();
    let mut rest = body;
    while let Some((_, tail)) = rest.split_once("<string>") {
        let (value, remainder) = tail.split_once("</string>")?;
        argv.push(xml_unescape(value.trim()));
        rest = remainder;
    }
    if argv.is_empty() {
        None
    } else {
        Some(argv)
    }
}

pub(crate) fn xml_unescape(s: &str) -> String {
    // `&amp;` last: unescaping it first would turn `&amp;lt;` into `<`.
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `ExecStart=` from a systemd service unit, tokenized.
pub(crate) fn systemd_exec_start(text: &str) -> Option<Vec<String>> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("ExecStart="))?;
    let argv = split_exec_args(line.trim_start_matches("ExecStart="));
    if argv.is_empty() {
        None
    } else {
        Some(argv)
    }
}

/// systemd's `ExecStart` splitting, enough of it: whitespace-separated words,
/// double quotes grouping, backslash escaping inside them. Older `xv` versions
/// and newer ones both render quoted arguments, but a hand-edited unit may not.
fn split_exec_args(value: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut started = false;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_quotes => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    argv.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        argv.push(current);
    }
    argv
}

/// The `Task To Run:` value from `schtasks /Query /V /FO LIST`.
pub(crate) fn schtasks_task_to_run(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| line.to_ascii_lowercase().starts_with("task to run:"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::manifest::test_paths_in;
    use crate::schedule::{
        fixture_abs, render, RotationSchedule, ScheduleInterval, UnitFile, UnitPaths,
    };
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    /// A scheduler that answers from a script, and records what it was asked.
    #[derive(Debug, Default)]
    struct FakeRunner {
        answers: HashMap<String, CommandOutput>,
        default: Option<CommandOutput>,
        spawn_fails: bool,
        calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new() -> Self {
            Self::default()
        }

        fn answering(mut self, contains: &str, status: i32, stdout: &str, stderr: &str) -> Self {
            self.answers.insert(
                contains.to_string(),
                CommandOutput {
                    status,
                    stdout: stdout.to_string(),
                    stderr: stderr.to_string(),
                },
            );
            self
        }

        fn otherwise(mut self, status: i32, stdout: &str, stderr: &str) -> Self {
            self.default = Some(CommandOutput {
                status,
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            });
            self
        }

        fn spawn_failure() -> Self {
            Self {
                spawn_fails: true,
                ..Self::default()
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
            let line = format!("{program} {}", args.join(" "));
            self.calls.lock().unwrap().push(line.clone());
            if self.spawn_fails {
                return Err(crate::error::CrosstacheError::config(format!(
                    "failed to run {program}"
                )));
            }
            for (needle, answer) in &self.answers {
                if line.contains(needle) {
                    return Ok(answer.clone());
                }
            }
            Ok(self.default.clone().unwrap_or_else(|| CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            }))
        }
    }

    /// A runner that reports our entry as registered on every platform.
    fn registered() -> FakeRunner {
        FakeRunner::new()
            .answering(
                "systemctl --user show",
                0,
                "LoadState=loaded\nActiveState=active\nUnitFileState=enabled\n",
                "",
            )
            .otherwise(0, "", "")
    }

    /// A runner that reports absence in each scheduler's own words.
    fn not_registered() -> FakeRunner {
        FakeRunner::new()
            .answering(
                "systemctl --user show",
                0,
                "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n",
                "",
            )
            .answering("launchctl print", 113, "", "Could not find service \"x\"")
            .answering(
                "schtasks /Query",
                1,
                "",
                "ERROR: The system cannot find the file specified.",
            )
            .otherwise(0, "", "")
    }

    fn schedule_with(command: ScheduleCommand) -> RotationSchedule {
        RotationSchedule {
            interval: ScheduleInterval::Daily {
                hour: 3,
                minute: 30,
            },
            command,
            binary: PathBuf::from(fixture_abs("/home/alice/bin/xv")),
            log_path: PathBuf::from(fixture_abs("/home/alice/.local/state/xv/rotate.log")),
            home: PathBuf::from(fixture_abs("/home/alice")),
            state_home: None,
        }
    }

    /// A legacy schedule whose paths are spelled the way a **Unix** host wrote
    /// them, deliberately not through [`fixture_abs`].
    ///
    /// launchd plists and systemd units exist only on Unix, so their bytes are
    /// always Unix-shaped whatever host is reading them back. Running the
    /// Windows spelling through this fixture tests nothing real and cannot
    /// round-trip: `C:\home\alice\bin\xv` inside a quoted `ExecStart=` comes
    /// back out of systemd's backslash-escape rules as `C:homealicebinxv`.
    fn legacy_unix_schedule() -> RotationSchedule {
        RotationSchedule {
            interval: ScheduleInterval::Daily {
                hour: 3,
                minute: 30,
            },
            command: ScheduleCommand::LegacyRotateDue {
                vault: Some("payments-production".to_string()),
            },
            binary: PathBuf::from(LEGACY_UNIX_BINARY),
            log_path: PathBuf::from("/home/alice/.local/state/xv/rotate.log"),
            home: PathBuf::from("/home/alice"),
            state_home: None,
        }
    }

    const LEGACY_UNIX_BINARY: &str = "/home/alice/bin/xv";

    fn pinned_schedule(manifest: &Path) -> RotationSchedule {
        schedule_with(ScheduleCommand::ManifestRun {
            manifest: manifest.to_path_buf(),
            working_directory: PathBuf::from(fixture_abs("/home/alice/work")),
        })
    }

    /// Write the rendered units for `schedule` into `unit_dir`.
    fn seed_units(platform: Platform, schedule: &RotationSchedule, unit_dir: &UnitPaths) {
        std::fs::create_dir_all(&unit_dir.dir).unwrap();
        for UnitFile { path, contents } in render(platform, schedule, unit_dir) {
            std::fs::write(path, contents).unwrap();
        }
    }

    fn seed_manifest(state: &ScheduleStatePaths) {
        std::fs::create_dir_all(state.root()).unwrap();
        std::fs::write(
            state.manifest_path(),
            b"{\n  \"schedule_id\": \"rotation-default\"\n}\n",
        )
        .unwrap();
    }

    /// A temp dir, its unit paths and its state paths, for one platform.
    struct Fixture {
        _tmp: tempfile::TempDir,
        units: UnitPaths,
        state: ScheduleStatePaths,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let units = UnitPaths {
            dir: tmp.path().join("units"),
        };
        let state = test_paths_in(&tmp.path().join("state"));
        std::fs::create_dir_all(&units.dir).unwrap();
        Fixture {
            _tmp: tmp,
            units,
            state,
        }
    }

    fn platforms() -> [Platform; 3] {
        [Platform::Launchd, Platform::Systemd, Platform::Schtasks]
    }

    #[test]
    fn nothing_installed_is_absent_on_every_platform() {
        for platform in platforms() {
            let f = fixture();
            let report =
                inspect_ownership(platform, &f.units, &f.state, &not_registered()).unwrap();
            assert_eq!(report.state, Ownership::Absent, "{platform:?}");
            assert_eq!(report.scheduler, SchedulerState::Absent, "{platform:?}");
            assert_eq!(report.state.label(), None);
        }
    }

    #[test]
    fn a_pinned_unit_with_its_manifest_is_managed() {
        for platform in platforms() {
            let f = fixture();
            seed_manifest(&f.state);
            seed_units(
                platform,
                &pinned_schedule(&f.state.manifest_path()),
                &f.units,
            );
            let report = inspect_ownership(platform, &f.units, &f.state, &registered()).unwrap();
            assert_eq!(report.state, Ownership::Managed, "{platform:?}");
            assert_eq!(report.state.label(), Some("managed"));
            assert_eq!(report.scheduler, SchedulerState::Installed);
        }
    }

    #[test]
    fn a_legacy_unit_without_a_manifest_is_legacy_unpinned_with_its_real_command() {
        for platform in [Platform::Launchd, Platform::Systemd] {
            let f = fixture();
            seed_units(platform, &legacy_unix_schedule(), &f.units);
            let report = inspect_ownership(platform, &f.units, &f.state, &registered()).unwrap();
            let Ownership::LegacyUnpinned { command_line } = report.state else {
                panic!(
                    "{platform:?}: expected legacy-unpinned, got {:?}",
                    report.state
                );
            };
            // The golden's `Command:` line: the actual argv, including the
            // vault the legacy sweep would have swept.
            assert!(
                command_line.ends_with("rotate --due --force --vault payments-production"),
                "{platform:?}: {command_line}"
            );
            assert!(
                command_line.starts_with(LEGACY_UNIX_BINARY),
                "{platform:?}: {command_line}"
            );
        }
    }

    #[test]
    fn a_legacy_scheduled_task_is_read_from_the_scheduler() {
        let f = fixture();
        let runner = registered().answering(
            "/V /FO LIST",
            0,
            "TaskName:      \\crosstache-xv-rotate\r\n\
             Task To Run:   C:\\bin\\xv.exe rotate --due --force --vault payments\r\n\
             Status:        Ready\r\n",
            "",
        );
        let report = inspect_ownership(Platform::Schtasks, &f.units, &f.state, &runner).unwrap();
        assert_eq!(
            report.state,
            Ownership::LegacyUnpinned {
                command_line: "C:\\bin\\xv.exe rotate --due --force --vault payments".to_string()
            }
        );
    }

    #[test]
    fn a_manifest_without_a_unit_is_an_orphaned_manifest() {
        for platform in platforms() {
            let f = fixture();
            seed_manifest(&f.state);
            let report =
                inspect_ownership(platform, &f.units, &f.state, &not_registered()).unwrap();
            assert_eq!(report.state, Ownership::OrphanedManifest, "{platform:?}");
            assert_eq!(report.state.label(), Some("orphaned-manifest"));
        }
    }

    #[test]
    fn a_pinned_unit_whose_manifest_is_gone_is_not_claimed_as_managed() {
        let f = fixture();
        seed_units(
            Platform::Systemd,
            &pinned_schedule(&f.state.manifest_path()),
            &f.units,
        );
        let report =
            inspect_ownership(Platform::Systemd, &f.units, &f.state, &registered()).unwrap();
        assert!(
            matches!(report.state, Ownership::LegacyUnpinned { .. }),
            "{:?}",
            report.state
        );
    }

    #[test]
    fn an_unmarked_file_at_an_owned_path_is_foreign_and_never_adopted() {
        let f = fixture();
        let path = f.units.dir.join(format!("{SYSTEMD_UNIT}.service"));
        std::fs::write(&path, "[Service]\nExecStart=/usr/bin/true\n").unwrap();
        let report =
            inspect_ownership(Platform::Systemd, &f.units, &f.state, &registered()).unwrap();
        assert_eq!(report.state, Ownership::Foreign { paths: vec![path] });
        assert_eq!(report.state.label(), Some("foreign"));
    }

    #[test]
    fn a_marked_unit_running_something_else_is_foreign() {
        let f = fixture();
        let path = f.units.dir.join(format!("{SYSTEMD_UNIT}.service"));
        std::fs::write(
            &path,
            "# Managed by crosstache (xv schedule).\n[Service]\nExecStart=/usr/bin/curl evil\n",
        )
        .unwrap();
        let report =
            inspect_ownership(Platform::Systemd, &f.units, &f.state, &registered()).unwrap();
        assert_eq!(report.state, Ownership::Foreign { paths: vec![path] });
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_an_owned_path_is_foreign() {
        let f = fixture();
        let target = f.units.dir.join("elsewhere");
        std::fs::write(&target, "# Managed by crosstache (xv schedule).\n").unwrap();
        let path = f.units.dir.join(format!("{SYSTEMD_UNIT}.service"));
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let report =
            inspect_ownership(Platform::Systemd, &f.units, &f.state, &registered()).unwrap();
        assert_eq!(report.state, Ownership::Foreign { paths: vec![path] });
    }

    #[test]
    fn a_manifest_for_another_schedule_is_foreign() {
        let f = fixture();
        std::fs::create_dir_all(f.state.root()).unwrap();
        std::fs::write(
            f.state.manifest_path(),
            b"{\"schedule_id\":\"someone-elses\"}",
        )
        .unwrap();
        let report =
            inspect_ownership(Platform::Systemd, &f.units, &f.state, &not_registered()).unwrap();
        assert_eq!(
            report.state,
            Ownership::Foreign {
                paths: vec![f.state.manifest_path()]
            }
        );
    }

    #[test]
    fn a_failing_scheduler_command_is_an_error_not_absence() {
        // Each scheduler failing for a reason that is *not* "no such entry".
        let cases = [
            (
                Platform::Launchd,
                FakeRunner::new().otherwise(74, "", "Bad request."),
                "launchctl print failed (exit 74)",
            ),
            (
                Platform::Systemd,
                FakeRunner::new().otherwise(1, "", "Access denied"),
                "systemctl --user show failed (exit 1)",
            ),
            (
                Platform::Schtasks,
                FakeRunner::new().otherwise(1, "", "ERROR: Access is denied."),
                "schtasks /Query failed (exit 1)",
            ),
        ];
        for (platform, runner, expected) in cases {
            let f = fixture();
            let report = inspect_ownership(platform, &f.units, &f.state, &runner).unwrap();
            assert_eq!(
                report.scheduler,
                SchedulerState::Error(expected.to_string()),
                "{platform:?}"
            );
            assert_ne!(report.scheduler, SchedulerState::Absent, "{platform:?}");
        }
    }

    #[test]
    fn a_scheduler_that_cannot_be_run_is_an_error() {
        for platform in platforms() {
            let f = fixture();
            let report =
                inspect_ownership(platform, &f.units, &f.state, &FakeRunner::spawn_failure())
                    .unwrap();
            assert!(
                matches!(report.scheduler, SchedulerState::Error(ref d) if d.contains("could not be run")),
                "{platform:?}: {:?}",
                report.scheduler
            );
        }
    }

    #[test]
    fn a_scheduler_error_never_leaks_raw_output() {
        let f = fixture();
        let runner = FakeRunner::new().otherwise(
            74,
            "com.apple.secret /Users/alice/private-token",
            "domain gui/501 is unavailable",
        );
        let report = inspect_ownership(Platform::Launchd, &f.units, &f.state, &runner).unwrap();
        let SchedulerState::Error(detail) = report.scheduler else {
            panic!("expected an error");
        };
        assert_eq!(detail, "launchctl print failed (exit 74)");
    }

    #[test]
    fn a_host_without_a_user_service_manager_is_an_error_not_absence() {
        // `systemctl --user` that could not reach the user manager did not say
        // the timer is absent; it said nothing at all. Status must not turn
        // that into "no schedule is installed".
        let f = fixture();
        let runner = FakeRunner::new().otherwise(1, "", "Failed to connect to bus: No such file");
        let report = inspect_ownership(Platform::Systemd, &f.units, &f.state, &runner).unwrap();
        assert_eq!(
            report.scheduler,
            SchedulerState::Error(
                "systemctl --user show failed (exit 1): user bus unavailable".to_string()
            )
        );
    }

    #[test]
    fn the_unverified_target_note_names_a_missing_manifest() {
        // Two situations under one ownership label, and only one of them may
        // claim the unit recorded no target.
        assert_eq!(
            unverified_target_note("/bin/xv rotate --due --force --vault v"),
            "unverified (the legacy unit does not record backend or account identity)"
        );
        assert_eq!(
            unverified_target_note("/bin/xv schedule run --manifest /s/manifest.json"),
            "unverified (the recorded manifest /s/manifest.json is missing)"
        );
        assert_eq!(
            unverified_target_note("\"/o p/xv\" schedule run --manifest \"/s p/manifest.json\""),
            "unverified (the recorded manifest /s p/manifest.json is missing)"
        );
        // An entry whose command the scheduler would not report.
        assert_eq!(
            unverified_target_note(""),
            "unverified (the legacy unit does not record backend or account identity)"
        );
    }

    #[test]
    fn recognized_absence_phrases_are_absence() {
        for (status, stderr) in [
            (113, "Could not find service \"com.crosstache.xv-rotate\""),
            (3, "No such process"),
            (1, "ERROR: The system cannot find the file specified."),
        ] {
            let out = CommandOutput {
                status,
                stdout: String::new(),
                stderr: stderr.to_string(),
            };
            assert!(says_absent(&out), "{stderr}");
        }
        assert!(!says_absent(&CommandOutput {
            status: 74,
            stdout: String::new(),
            stderr: "Bad request.".to_string(),
        }));
    }

    #[test]
    fn command_shapes_are_recognized_from_rendered_units() {
        assert_eq!(
            classify_command_line("/usr/bin/xv rotate --due --force"),
            CommandShape::Legacy
        );
        assert_eq!(
            classify_command_line("/usr/bin/xv rotate --due --force --vault v"),
            CommandShape::Legacy
        );
        assert_eq!(
            classify_command_line("/usr/bin/xv schedule run --manifest /s/manifest.json"),
            CommandShape::ManifestRun
        );
        assert_eq!(
            classify_command_line("/usr/bin/xv list"),
            CommandShape::Unrecognized
        );
    }

    #[test]
    fn exec_start_tokenizing_survives_quoting_and_spaces() {
        assert_eq!(
            systemd_exec_start("ExecStart=\"/opt/x v/xv\" \"rotate\" \"--due\"\n").unwrap(),
            vec!["/opt/x v/xv", "rotate", "--due"]
        );
        assert_eq!(
            systemd_exec_start("ExecStart=/opt/xv rotate --due\n").unwrap(),
            vec!["/opt/xv", "rotate", "--due"]
        );
        assert!(systemd_exec_start("[Service]\nType=oneshot\n").is_none());
    }

    #[test]
    fn plist_arguments_are_unescaped() {
        let plist = "<key>ProgramArguments</key>\n<array>\n<string>/bin/xv</string>\n\
                     <string>--vault</string>\n<string>a&amp;b</string>\n</array>\n";
        assert_eq!(
            plist_program_arguments(plist).unwrap(),
            vec!["/bin/xv", "--vault", "a&b"]
        );
        assert!(plist_program_arguments("<plist/>").is_none());
    }
}
