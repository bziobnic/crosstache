//! A scheduler stand-in for tests that run the real `xv` binary.
//!
//! **Debug builds only.** The whole module is behind `cfg(debug_assertions)`,
//! and so is the code in `src/cli/schedule_ops.rs` that consults its
//! environment variables, so a release binary neither contains this runner nor
//! reads `XV_SCHEDULE_RUNNER`.
//!
//! ## Why it exists
//!
//! `launchctl`, `systemctl --user` and `schtasks` act on the invoking user's
//! live session. `HOME` does not sandbox them, and the job name `xv` uses is a
//! fixed global label. So a CLI test that shells out to `xv schedule uninstall`
//! runs `launchctl bootout gui/<uid>/com.crosstache.xv-rotate` against the
//! developer's own machine and would deregister a rotation schedule they
//! actually rely on. The unit tests avoid this with a fake `CommandRunner`, but
//! an integration test spawns a separate process and cannot inject one — hence
//! an environment switch the binary itself honors.
//!
//! ## Contract
//!
//! - `XV_SCHEDULE_RUNNER=fake` swaps this in for [`ProcessRunner`].
//! - `XV_SCHEDULE_RUNNER=fake:installed` answers every *query* the way a
//!   scheduler with our job registered would, and
//!   `fake:installed,next=2026-09-10T03:00:00Z` additionally reports that
//!   instant as the next fire time, in the platform's own rendering. That is
//!   what lets a CLI test reach the healthy `status` goldens — which need a
//!   scheduler that says "installed" and a next run — without registering
//!   anything anywhere.
//! - `XV_SCHEDULE_RUNNER=fake:registered` answers nothing at all until it is
//!   asked to *register* something, and from then on answers every
//!   verification query out of what it was actually asked to register — the
//!   plist it was handed to `bootstrap`, the service unit behind the timer it
//!   was asked to `enable`, the `/Create` arguments it was given. That is what
//!   lets one CLI test drive the real install transaction (which verifies the
//!   scheduler's own answer in stage 6) to a successful end, without
//!   registering anything anywhere. It is deliberately *not* `installed`: a
//!   scheduler that has not been asked yet says "no such job", so the
//!   transaction's prior-state probe still sees a first install. The
//!   registration is remembered beside `XV_SCHEDULE_RUNNER_LOG` when one is
//!   set, so a later `xv schedule status` in the same test sees the same job,
//!   and a deregistration forgets it again.
//! - `XV_SCHEDULE_RUNNER=fake:error` answers every command with a failure that
//!   is not the platform's "no such job" shape, so a caller's
//!   `SchedulerState::Error` path is exercised instead of its absence path.
//! - `XV_SCHEDULE_RUNNER_LOG=<path>` appends one `program arg arg…` line per
//!   invocation, so a test can still prove the right commands were issued with
//!   the right arguments.
//! - Registration commands answer success; every *query* and every
//!   deregistration answers in the platform's own "there is no such job"
//!   shape. A full `xv schedule install` under the bare `fake` runner
//!   therefore fails its verification step, which is correct — nothing was
//!   really registered. A test that needs a successful install asks for
//!   `fake:registered` instead.
//!
//! [`ProcessRunner`]: super::ProcessRunner

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::schedule::{CommandOutput, CommandRunner, Platform, UnitPaths};

/// The variable that selects this runner.
pub const RUNNER_VAR: &str = "XV_SCHEDULE_RUNNER";
/// The variable naming the file invocations are appended to.
pub const RUNNER_LOG_VAR: &str = "XV_SCHEDULE_RUNNER_LOG";
/// The bare value of [`RUNNER_VAR`]; also the prefix of every scenario
/// spelling (`fake:installed`, `fake:installed,next=<rfc3339>`).
pub const FAKE: &str = "fake";

/// Whether `value` selects this runner at all.
pub fn selects_fake(value: &str) -> bool {
    value == FAKE || value.starts_with("fake:")
}

/// Records what it was asked to run and answers from a canned scenario.
#[derive(Debug, Default)]
pub struct RecordingRunner {
    log: Option<PathBuf>,
    /// Answer queries as "our job is registered" instead of "no such job".
    installed: bool,
    /// The next fire time to report, as an RFC 3339 UTC instant. Only
    /// meaningful together with `installed`.
    next: Option<DateTime<Utc>>,
    /// Answer every query with a failure that is *not* the platform's "no such
    /// job" shape, so the caller's `SchedulerState::Error` path is exercised
    /// rather than its absence path.
    failing: bool,
    /// Answer verification queries out of what this runner was actually asked
    /// to register, so the real install transaction can reach the end of stage
    /// 6. Nothing is answered until a registration arrives.
    registering: bool,
    /// What registration this runner has been handed, if any. Interior
    /// mutability because [`CommandRunner::run`] takes `&self` and the
    /// registration is observed during the same process that issued it.
    registered: Mutex<Option<Registration>>,
}

/// What a `fake:registered` runner was asked to register, in the platform's own
/// terms. Every field is something the scheduler was *handed* — the answers are
/// read back out of it rather than invented.
///
/// Serialized beside the invocation log so it outlives the process that
/// registered it: a real scheduler still knows about the job when the *next*
/// `xv schedule status` asks, and a lifecycle test that installs in one process
/// and queries in another needs the same continuity. Deregistration removes it,
/// exactly as it removes the real thing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum Registration {
    /// The plist `launchctl bootstrap` was pointed at.
    Launchd { plist: PathBuf },
    /// A timer was enabled; the service unit beside it carries the command.
    Systemd,
    /// The full `schtasks /Create` argument vector.
    Schtasks { args: Vec<String> },
}

impl RecordingRunner {
    /// Read the scenario and the log destination from the environment. A
    /// missing or empty log value means "answer, but record nothing".
    pub fn from_env() -> Self {
        let mut runner = Self::from_spec(&std::env::var(RUNNER_VAR).unwrap_or_default());
        runner.log = std::env::var(RUNNER_LOG_VAR)
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        runner
    }

    /// Parse `fake`, `fake:installed`, `fake:installed,next=<rfc3339>`,
    /// `fake:registered`, `fake:error`.
    ///
    /// Unknown words are ignored rather than rejected: this is a test switch a
    /// release build never reads, and a typo that silently falls back to the
    /// "nothing is registered" answer fails the test that set it, loudly, at
    /// the assertion rather than in a panic here.
    fn from_spec(spec: &str) -> Self {
        let mut runner = Self::default();
        let Some((_, options)) = spec.split_once(':') else {
            return runner;
        };
        for option in options.split(',') {
            match option.split_once('=') {
                Some(("next", value)) => {
                    runner.next = DateTime::parse_from_rfc3339(value)
                        .ok()
                        .map(|parsed| parsed.with_timezone(&Utc));
                }
                _ if option == "installed" => runner.installed = true,
                _ if option == "registered" => runner.registering = true,
                _ if option == "error" => runner.failing = true,
                _ => {}
            }
        }
        runner
    }

    /// The next fire time in launchd's spelling, when one was configured.
    fn launchd_next(&self) -> String {
        self.next.map_or_else(String::new, |next| {
            format!(
                "\n\tnext fire date = {}",
                next.format("%Y-%m-%d %H:%M:%S +0000")
            )
        })
    }

    /// The next fire time in systemd's `--timestamp=utc` spelling.
    fn systemd_next(&self) -> String {
        self.next.map_or_else(
            || "n/a".to_string(),
            |next| next.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        )
    }

    /// A command failure the absence detector must not read as absence.
    ///
    /// Deliberately free of every phrase in `ownership::says_absent`: the point
    /// of the scenario is that a scheduler which *would not answer* is not a
    /// scheduler that said "no".
    fn failing_answer(&self, program: &str) -> Option<(i32, String, String)> {
        self.failing.then(|| {
            (
                5,
                String::new(),
                format!("{program}: operation not permitted"),
            )
        })
    }

    /// The registered answers, when the scenario says our job exists.
    fn installed_answer(&self, program: &str, joined: &str) -> Option<(i32, String, String)> {
        if !self.installed {
            return None;
        }
        match program {
            "launchctl" if joined.starts_with("print") => Some((
                0,
                format!(
                    "com.crosstache.xv-rotate = {{\n\tstate = waiting{}\n}}",
                    self.launchd_next()
                ),
                String::new(),
            )),
            "systemctl" if joined.contains("show") => {
                // One `show` asks for the registration properties and another
                // for the next elapse; answer whichever was asked for.
                let mut stdout = String::new();
                if joined.contains("LoadState") {
                    stdout
                        .push_str("LoadState=loaded\nActiveState=active\nUnitFileState=enabled\n");
                }
                if joined.contains("NextElapseUSecRealtime") {
                    stdout.push_str(&format!("NextElapseUSecRealtime={}\n", self.systemd_next()));
                }
                Some((0, stdout, String::new()))
            }
            // Task Scheduler's registration *is* the artifact, so a query that
            // succeeds is the whole answer. The detailed `/V /FO LIST` form
            // deliberately reports no next run and no `Task To Run`: this fake
            // renders no task, so claiming one would be an invention.
            "schtasks" if joined.contains("/Query") => Some((
                0,
                "TaskName: \\crosstache-xv-rotate\nNext Run Time: N/A\n".to_string(),
                String::new(),
            )),
            _ => None,
        }
    }

    /// Where a registration is remembered across processes: beside the
    /// invocation log, which a lifecycle test already shares between steps.
    fn registration_file(&self) -> Option<PathBuf> {
        self.log
            .as_ref()
            .map(|log| PathBuf::from(format!("{}.registration", log.display())))
    }

    /// Remember what this runner was asked to register, so the verification
    /// queries that follow — in this process or a later one — can be answered
    /// out of it.
    fn capture(&self, program: &str, args: &[&str]) {
        if !self.registering {
            return;
        }
        let mut slot = self.registered.lock().expect("registration slot");
        match program {
            // `bootstrap <domain> <plist>`: the plist is the whole
            // registration, and it is on disk, so the answers come from it.
            "launchctl" => {
                if args.first() == Some(&"bootstrap") {
                    if let Some(plist) = args.get(2) {
                        *slot = Some(Registration::Launchd {
                            plist: PathBuf::from(plist),
                        });
                    }
                }
            }
            // systemd is handed a unit *name*; the command lives in the service
            // file beside the timer, which is where `show ExecStart` reads it.
            "systemctl" => *slot = Some(Registration::Systemd),
            "schtasks" => {
                *slot = Some(Registration::Schtasks {
                    args: args.iter().map(|arg| (*arg).to_string()).collect(),
                })
            }
            _ => {}
        }
        if let (Some(path), Some(registration)) = (self.registration_file(), slot.as_ref()) {
            if let Ok(body) = serde_json::to_vec(registration) {
                let _ = std::fs::write(path, body);
            }
        }
    }

    /// Forget the registration, the way a scheduler that accepted a
    /// deregistration would.
    fn forget(&self) {
        if !self.registering {
            return;
        }
        *self.registered.lock().expect("registration slot") = None;
        if let Some(path) = self.registration_file() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The registration this runner knows about: the one it made itself, or the
    /// one an earlier process recorded beside the log.
    fn current_registration(&self) -> Option<Registration> {
        if !self.registering {
            return None;
        }
        if let Some(registration) = self.registered.lock().expect("registration slot").clone() {
            return Some(registration);
        }
        let path = self.registration_file()?;
        let body = std::fs::read(path).ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// The answers a scheduler that really accepted this registration would
    /// give. `None` until something was registered — an unasked scheduler says
    /// "no such job", which is what the absence answers below are for.
    fn registered_answer(&self, program: &str, joined: &str) -> Option<(i32, String, String)> {
        if !self.registering {
            return None;
        }
        match (program, &self.current_registration()?) {
            ("launchctl", Registration::Launchd { plist }) if joined.starts_with("print") => {
                Some((
                    0,
                    launchd_print_output(plist, &self.launchd_next()),
                    String::new(),
                ))
            }
            ("systemctl", Registration::Systemd) if joined.contains("show") => {
                let mut stdout = String::new();
                if joined.contains("LoadState") {
                    stdout
                        .push_str("LoadState=loaded\nActiveState=active\nUnitFileState=enabled\n");
                }
                if joined.contains("ExecStart") {
                    stdout.push_str(&systemd_exec_start_property());
                }
                if joined.contains("NextElapseUSecRealtime") {
                    stdout.push_str(&format!("NextElapseUSecRealtime={}\n", self.systemd_next()));
                }
                Some((0, stdout, String::new()))
            }
            ("schtasks", Registration::Schtasks { args })
                if joined.contains("/Query") && joined.contains("/XML") =>
            {
                Some((0, schtasks_query_xml(args), String::new()))
            }
            ("schtasks", Registration::Schtasks { args }) if joined.contains("/Query") => {
                Some((0, schtasks_query_list(args), String::new()))
            }
            _ => None,
        }
    }

    fn record(&self, line: &str) {
        let Some(path) = &self.log else {
            return;
        };
        // Best effort: a test that cares asserts on the file's contents, and a
        // fake scheduler has no business failing a command over its own log.
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// `launchctl print` for a job registered from `plist`, with its real program
/// and arguments — read back out of the very file `bootstrap` was handed.
fn launchd_print_output(plist: &Path, next: &str) -> String {
    let contents = std::fs::read_to_string(plist).unwrap_or_default();
    let args = plist_program_arguments(&contents);
    let program = args.first().cloned().unwrap_or_default();
    let arguments: String = args.iter().map(|arg| format!("\t\t{arg}\n")).collect();
    format!(
        "{label} = {{\n\tactive count = 0\n\tstate = waiting\n\tprogram = {program}\n\targuments =          {{\n{arguments}\t}}{next}\n}}",
        label = crate::schedule::LAUNCHD_LABEL,
    )
}

/// The `<string>` values of a plist's `ProgramArguments` array, unescaped.
fn plist_program_arguments(plist: &str) -> Vec<String> {
    let Some(after_key) = plist.split_once("<key>ProgramArguments</key>") else {
        return Vec::new();
    };
    let Some(array) = after_key.1.split_once("<array>") else {
        return Vec::new();
    };
    let Some(body) = array.1.split_once("</array>") else {
        return Vec::new();
    };
    body.0
        .split("<string>")
        .skip(1)
        .filter_map(|chunk| chunk.split_once("</string>"))
        .map(|(value, _)| xml_unescape(value.trim()))
        .collect()
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `systemctl --user show xv-rotate.service --property=ExecStart`, rendered from
/// the service unit the timer was enabled against.
///
/// The unit directory is derived from `HOME` exactly the way installation
/// derived it, so this reads the file the install under test just wrote.
fn systemd_exec_start_property() -> String {
    let Some(home) = dirs::home_dir() else {
        return String::new();
    };
    let service = UnitPaths::for_platform(Platform::Systemd, &home).systemd_service();
    let contents = std::fs::read_to_string(service).unwrap_or_default();
    let exec = contents
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .unwrap_or_default();
    // The renderer quotes every argument; systemd reports the split argv.
    let argv: Vec<String> = exec
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    let path = argv.first().cloned().unwrap_or_default();
    format!(
        "ExecStart={{ path={path} ; argv[]={} ; ignore_errors=no }}\n",
        argv.join(" ")
    )
}

/// The value after `flag` in a `schtasks` argument vector.
fn schtasks_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg.eq_ignore_ascii_case(flag))
        .and_then(|at| args.get(at + 1))
        .cloned()
}

/// `schtasks /Query /XML` for the task these `/Create` arguments registered.
fn schtasks_query_xml(args: &[String]) -> String {
    let kind = schtasks_value(args, "/SC")
        .unwrap_or_default()
        .to_uppercase();
    let start = schtasks_value(args, "/ST").unwrap_or_else(|| "00:00".to_string());
    let boundary = format!("2026-01-01T{start}:00");
    let trigger = match kind.as_str() {
        "HOURLY" => format!(
            "    <TimeTrigger>\n      <StartBoundary>{boundary}</StartBoundary>\n                   <Enabled>true</Enabled>\n      <Repetition>\n        <Interval>PT1H</Interval>\n                     <Duration>P1D</Duration>\n      </Repetition>\n    </TimeTrigger>"
        ),
        "WEEKLY" => format!(
            "    <CalendarTrigger>\n      <StartBoundary>{boundary}</StartBoundary>\n                   <Enabled>true</Enabled>\n      <ScheduleByWeek>\n        <DaysOfWeek>\n          <{day}/>\n                     </DaysOfWeek>\n        <WeeksInterval>1</WeeksInterval>\n      </ScheduleByWeek>\n                 </CalendarTrigger>",
            day = schtasks_xml_day(schtasks_value(args, "/D").unwrap_or_default().as_str()),
        ),
        _ => format!(
            "    <CalendarTrigger>\n      <StartBoundary>{boundary}</StartBoundary>\n                   <Enabled>true</Enabled>\n      <ScheduleByDay>\n        <DaysInterval>1</DaysInterval>\n                   </ScheduleByDay>\n    </CalendarTrigger>"
        ),
    };
    let run = schtasks_value(args, "/TR").unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.2\"          xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <Triggers>\n{trigger}\n           </Triggers>\n  <Actions Context=\"Author\">\n    <Exec>\n      <Command>cmd</Command>\n               <Arguments>{}</Arguments>\n    </Exec>\n  </Actions>\n</Task>\n",
        crate::schedule::xml_escape(&run)
    )
}

/// Task XML's element name for a `schtasks /D` day abbreviation.
fn schtasks_xml_day(abbreviation: &str) -> &'static str {
    match abbreviation.to_uppercase().as_str() {
        "MON" => "Monday",
        "TUE" => "Tuesday",
        "WED" => "Wednesday",
        "THU" => "Thursday",
        "FRI" => "Friday",
        "SAT" => "Saturday",
        _ => "Sunday",
    }
}

/// `schtasks /Query /V /FO LIST` for the task these `/Create` arguments
/// registered: the command line and the log path come straight from `/TR`.
fn schtasks_query_list(args: &[String]) -> String {
    format!(
        "\nFolder: \\\nTaskName:                             \\{name}\nNext Run Time:                                 N/A\nStatus:                               Ready\nTask To Run:                                   {run}\nStart In:                             N/A\n",
        name = crate::schedule::SCHTASKS_NAME,
        run = schtasks_value(args, "/TR").unwrap_or_default(),
    )
}

/// Whether these arguments ask a scheduler to *deregister* something.
///
/// launchd's `bootout` also runs immediately before a `bootstrap`, so this is
/// checked (and the registration forgotten) before the registration is
/// captured, never after.
fn is_deregistration(joined: &str) -> bool {
    joined.starts_with("bootout") || joined.contains("disable --now") || joined.contains("/Delete")
}

/// Whether these arguments ask a scheduler to *register* something.
fn is_registration(joined: &str) -> bool {
    joined.contains("bootstrap")
        || joined.contains("enable --now")
        || joined.contains("daemon-reload")
        || joined.contains("/Create")
}

impl CommandRunner for RecordingRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
        let joined = args.join(" ");
        self.record(&format!("{program} {joined}"));

        if is_deregistration(&joined) {
            // A scheduler that really had the job reports a removal; one that
            // never had it falls through to the absence answers below.
            let had_one = self.current_registration().is_some();
            self.forget();
            if had_one {
                return Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
        }
        if is_registration(&joined) {
            self.capture(program, args);
        }

        if let Some((status, stdout, stderr)) = self
            .failing_answer(program)
            .or_else(|| self.registered_answer(program, &joined))
            .or_else(|| self.installed_answer(program, &joined))
        {
            return Ok(CommandOutput {
                status,
                stdout,
                stderr,
            });
        }

        if is_registration(&joined) {
            return Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            });
        }

        // Each platform's own words for "no such job", so the caller's
        // absence detection is exercised rather than bypassed.
        let (status, stdout, stderr) = match program {
            "launchctl" if joined.starts_with("print") => (
                113,
                String::new(),
                "Could not find service \"com.crosstache.xv-rotate\" in domain for user gui"
                    .to_string(),
            ),
            "launchctl" => (
                3,
                String::new(),
                "Boot-out failed: 3: No such process".to_string(),
            ),
            "systemctl" if joined.contains("show") => (
                0,
                "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n".to_string(),
                String::new(),
            ),
            "systemctl" => (
                1,
                String::new(),
                "Failed to disable unit: Unit file xv-rotate.timer does not exist.".to_string(),
            ),
            _ => (
                1,
                String::new(),
                "ERROR: The system cannot find the file specified.".to_string(),
            ),
        };
        Ok(CommandOutput {
            status,
            stdout,
            stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::ownership::says_absent;

    use crate::schedule::install::verify_schtasks_cadence;
    use crate::schedule::{
        render, schtasks_create_args, RotationSchedule, ScheduleCommand, ScheduleInterval,
        UnitPaths,
    };

    /// A schedule whose every path is under `home`, so a test can write the
    /// units it renders into a tempdir.
    fn a_schedule(home: &std::path::Path) -> RotationSchedule {
        RotationSchedule {
            interval: ScheduleInterval::Daily {
                hour: 3,
                minute: 30,
            },
            command: ScheduleCommand::ManifestRun {
                manifest: home.join("state/xv/schedules/rotation-default/manifest.json"),
                working_directory: home.to_path_buf(),
            },
            binary: home.join("bin/xv"),
            log_path: home.join("rotate.log"),
            home: home.to_path_buf(),
            state_home: None,
        }
    }

    fn registering_runner(log: Option<PathBuf>) -> RecordingRunner {
        RecordingRunner {
            log,
            registering: true,
            ..RecordingRunner::default()
        }
    }

    #[test]
    fn a_registering_runner_says_no_such_job_until_something_is_registered() {
        let runner = registering_runner(None);
        let out = runner
            .run("launchctl", &["print", "gui/501/com.crosstache.xv-rotate"])
            .unwrap();
        assert!(
            !out.ok(),
            "an unasked scheduler must not claim a job: {out:?}"
        );
        assert!(says_absent(&out), "{out:?}");
    }

    #[test]
    fn launchd_verification_reads_back_the_plist_it_was_handed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let schedule = a_schedule(home);
        let paths = UnitPaths::for_platform(Platform::Launchd, home);
        std::fs::create_dir_all(&paths.dir).unwrap();
        for unit in render(Platform::Launchd, &schedule, &paths) {
            std::fs::write(&unit.path, &unit.contents).unwrap();
        }

        let runner = registering_runner(None);
        let plist = paths.launchd_plist();
        assert!(runner
            .run(
                "launchctl",
                &["bootstrap", "gui/501", &plist.to_string_lossy()]
            )
            .unwrap()
            .ok());

        let out = runner
            .run("launchctl", &["print", "gui/501/com.crosstache.xv-rotate"])
            .unwrap();
        assert!(out.ok(), "{out:?}");
        // Exactly what stage 6 of the install transaction requires.
        assert!(
            out.stdout
                .contains(&schedule.binary.to_string_lossy().to_string()),
            "{}",
            out.stdout
        );
        let ScheduleCommand::ManifestRun { manifest, .. } = &schedule.command else {
            unreachable!("the fixture pins a manifest run")
        };
        assert!(
            out.stdout.contains(&manifest.to_string_lossy().to_string()),
            "{}",
            out.stdout
        );
    }

    #[test]
    fn schtasks_verification_answers_out_of_the_create_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        let schedule = a_schedule(tmp.path());
        let args = schtasks_create_args(&schedule);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let runner = registering_runner(None);
        assert!(runner.run("schtasks", &refs).unwrap().ok());

        let xml = runner
            .run(
                "schtasks",
                &["/Query", "/TN", "crosstache-xv-rotate", "/XML"],
            )
            .unwrap();
        assert!(xml.ok(), "{xml:?}");
        // The real check, not a rephrasing of it.
        verify_schtasks_cadence(&xml.stdout, schedule.interval).expect("the cadence must verify");

        let list = runner
            .run(
                "schtasks",
                &["/Query", "/TN", "crosstache-xv-rotate", "/V", "/FO", "LIST"],
            )
            .unwrap();
        assert!(list.ok(), "{list:?}");
        assert!(
            list.stdout.contains(&schedule.command_line()),
            "{}",
            list.stdout
        );
        assert!(
            list.stdout
                .contains(&schedule.log_path.to_string_lossy().to_string()),
            "{}",
            list.stdout
        );
    }

    #[test]
    fn systemd_verification_reports_the_loaded_timer_and_the_services_exec_start() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let schedule = a_schedule(home);
        let paths = UnitPaths::for_platform(Platform::Systemd, home);
        std::fs::create_dir_all(&paths.dir).unwrap();
        for unit in render(Platform::Systemd, &schedule, &paths) {
            std::fs::write(&unit.path, &unit.contents).unwrap();
        }

        let runner = registering_runner(None);
        assert!(runner
            .run("systemctl", &["--user", "daemon-reload"])
            .unwrap()
            .ok());

        let properties = runner
            .run(
                "systemctl",
                &[
                    "--user",
                    "show",
                    "xv-rotate.timer",
                    "--property=LoadState",
                    "--property=ActiveState",
                    "--property=UnitFileState",
                ],
            )
            .unwrap();
        assert!(
            properties.stdout.contains("LoadState=loaded"),
            "{properties:?}"
        );
        assert!(
            properties.stdout.contains("ActiveState=active"),
            "{properties:?}"
        );

        // `systemd_exec_start_property` reads the service unit under the *real*
        // `HOME`, which this test does not own, so the ExecStart rendering is
        // exercised where the fixture home is the home: the CLI round-trip test
        // covers the wired path.
        let exec = runner
            .run(
                "systemctl",
                &[
                    "--user",
                    "show",
                    "xv-rotate.service",
                    "--property=ExecStart",
                ],
            )
            .unwrap();
        assert!(exec.ok(), "{exec:?}");
    }

    #[test]
    fn a_deregistration_makes_the_fake_forget_and_the_memory_outlives_the_process() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("calls.log");
        let plist = tmp.path().join("com.crosstache.xv-rotate.plist");
        std::fs::write(&plist, "<plist></plist>\n").unwrap();

        let installer = registering_runner(Some(log.clone()));
        assert!(installer
            .run(
                "launchctl",
                &["bootstrap", "gui/501", &plist.to_string_lossy()]
            )
            .unwrap()
            .ok());

        // A *separate* runner — the next process in a lifecycle test — sees the
        // same job.
        let later = registering_runner(Some(log.clone()));
        assert!(later
            .run("launchctl", &["print", "gui/501/com.crosstache.xv-rotate"])
            .unwrap()
            .ok());

        // Deregistration removes it, and the process after that sees absence.
        assert!(later
            .run(
                "launchctl",
                &["bootout", "gui/501/com.crosstache.xv-rotate"]
            )
            .unwrap()
            .ok());
        let after = registering_runner(Some(log));
        let out = after
            .run("launchctl", &["print", "gui/501/com.crosstache.xv-rotate"])
            .unwrap();
        assert!(!out.ok(), "{out:?}");
        assert!(says_absent(&out), "{out:?}");
    }

    #[test]
    fn every_query_and_deregistration_answers_absent() {
        let runner = RecordingRunner::default();
        for (program, args) in [
            (
                "launchctl",
                vec!["print", "gui/501/com.crosstache.xv-rotate"],
            ),
            (
                "launchctl",
                vec!["bootout", "gui/501/com.crosstache.xv-rotate"],
            ),
            (
                "systemctl",
                vec!["--user", "disable", "--now", "xv-rotate.timer"],
            ),
            ("schtasks", vec!["/Query", "/TN", "crosstache-xv-rotate"]),
            (
                "schtasks",
                vec!["/Delete", "/TN", "crosstache-xv-rotate", "/F"],
            ),
        ] {
            let out = runner.run(program, &args).unwrap();
            assert!(!out.ok(), "{program} {args:?} must not claim success");
            assert!(
                says_absent(&out),
                "{program} {args:?} must answer in the platform's absence words: {out:?}"
            );
        }
        // `systemctl show` exits 0 and says it in the properties instead.
        let out = runner
            .run(
                "systemctl",
                &["--user", "show", "xv-rotate.timer", "--property=LoadState"],
            )
            .unwrap();
        assert!(out.ok());
        assert!(out.stdout.contains("LoadState=not-found"));
    }

    #[test]
    fn registrations_succeed_and_every_call_is_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("calls.log");
        let runner = RecordingRunner {
            log: Some(log.clone()),
            ..RecordingRunner::default()
        };
        assert!(runner
            .run("systemctl", &["--user", "daemon-reload"])
            .unwrap()
            .ok());
        assert!(!runner
            .run(
                "systemctl",
                &["--user", "disable", "--now", "xv-rotate.timer"]
            )
            .unwrap()
            .ok());

        let recorded = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            recorded,
            "systemctl --user daemon-reload\n\
             systemctl --user disable --now xv-rotate.timer\n"
        );
    }

    #[test]
    fn recording_is_optional() {
        // No log configured: still answers, writes nothing.
        let runner = RecordingRunner {
            log: None,
            ..RecordingRunner::default()
        };
        assert!(!runner.run("schtasks", &["/Query"]).unwrap().ok());
    }
}
