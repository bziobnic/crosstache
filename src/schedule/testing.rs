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

use crate::error::Result;
use crate::schedule::{CommandOutput, CommandRunner};

/// The variable that selects this runner.
pub const RUNNER_VAR: &str = "XV_SCHEDULE_RUNNER";
/// The variable naming the file invocations are appended to.
pub const RUNNER_LOG_VAR: &str = "XV_SCHEDULE_RUNNER_LOG";
/// The only accepted value of [`RUNNER_VAR`].
pub const FAKE: &str = "fake";

/// Records what it was asked to run and answers "nothing is registered".
#[derive(Debug, Default)]
pub struct RecordingRunner {
    log: Option<PathBuf>,
}

impl RecordingRunner {
    /// Read the log destination from the environment. A missing or empty
    /// value means "answer, but record nothing".
    pub fn from_env() -> Self {
        Self {
            log: std::env::var(RUNNER_LOG_VAR)
                .ok()
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
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
        let runner = RecordingRunner { log: None };
        assert!(!runner.run("schtasks", &["/Query"]).unwrap().ok());
    }
}
