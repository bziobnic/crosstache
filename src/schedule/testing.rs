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
//! - `XV_SCHEDULE_RUNNER=fake:error` answers every command with a failure that
//!   is not the platform's "no such job" shape, so a caller's
//!   `SchedulerState::Error` path is exercised instead of its absence path.
//! - `XV_SCHEDULE_RUNNER_LOG=<path>` appends one `program arg arg…` line per
//!   invocation, so a test can still prove the right commands were issued with
//!   the right arguments.
//! - Registration commands answer success; every *query* and every
//!   deregistration answers in the platform's own "there is no such job"
//!   shape. A full `xv schedule install` under this runner therefore fails its
//!   verification step, which is correct — nothing was really registered. A
//!   test that needs a successful install has to teach this runner to answer
//!   the verification queries too.
//!
//! [`ProcessRunner`]: super::ProcessRunner

use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::schedule::{CommandOutput, CommandRunner};

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
    /// `fake:error`.
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

        if let Some((status, stdout, stderr)) = self
            .failing_answer(program)
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
