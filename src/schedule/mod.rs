//! Rotation scheduling via the operating system's scheduler.
//!
//! `xv rotate --due` performs a sweep; something has to invoke it on a cadence.
//! This module makes `xv` install and manage that trigger itself, so automatic
//! rotation is a first-party feature rather than a documented cron line the user
//! assembles by hand.
//!
//! ## Why the OS scheduler and not a daemon
//!
//! A resident `xv` process would need to survive reboots, log rotation, sleep and
//! wake, and its own crashes — reimplementing, worse, what launchd, systemd, and
//! Task Scheduler already do correctly. It would also mean a long-lived process
//! holding decryption credentials, which is precisely the thing a CLI secrets
//! manager avoids. So `xv` writes a native unit and manages its lifecycle:
//!
//! | Platform | Mechanism | Unit |
//! |----------|-----------|------|
//! | macOS | launchd user agent | `~/Library/LaunchAgents/com.crosstache.xv-rotate.plist` |
//! | Linux | systemd **user** timer | `~/.config/systemd/user/xv-rotate.{service,timer}` |
//! | Windows | Task Scheduler | task `crosstache-xv-rotate` |
//!
//! All three are **per-user**, never system-wide: rotation runs as the user whose
//! credentials and config it needs, and uninstalling never requires root.
//!
//! ## What ends up in the unit
//!
//! Only an absolute binary path, the `rotate --due --force` arguments, a log
//! path, and the `HOME`/`XDG_CONFIG_HOME` pair. **No credentials and no secret
//! values** — the scheduled run authenticates exactly as an interactive one does.
//!
//! The env pair matters more than it looks: a scheduled process does not inherit
//! the interactive shell's environment, and a schedule that resolves a different
//! config than the user tested against is the classic way this feature silently
//! rotates the wrong vault, or nothing at all.
//!
//! ## The honest limitation
//!
//! A scheduled run has no terminal, so any credential needing interaction fails.
//! Azure CLI tokens work while the refresh token is valid and the keyring is
//! unlocked; managed identity and the local backend work unconditionally.
//! [`install`] surfaces this at install time rather than letting it appear as a
//! silent 3 a.m. failure weeks later.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{CrosstacheError, Result};

/// launchd job label and systemd/Task Scheduler unit name.
const LAUNCHD_LABEL: &str = "com.crosstache.xv-rotate";
const SYSTEMD_UNIT: &str = "xv-rotate";
const SCHTASKS_NAME: &str = "crosstache-xv-rotate";

// ---------------------------------------------------------------------------
// Command runner seam
// ---------------------------------------------------------------------------

/// Result of running a scheduler CLI command.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// Process exit code (`-1` when the process was signalled).
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    /// Whether the command reported success.
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

/// Runs external scheduler commands (`launchctl`, `systemctl`, `schtasks`).
///
/// A trait so install/uninstall/status logic — argument order, error mapping,
/// idempotence — is testable without registering real jobs on the developer's
/// machine, which no test should ever do.
pub trait CommandRunner: Send + Sync + fmt::Debug {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput>;
}

/// Real runner over `std::process::Command`.
#[derive(Debug, Default)]
pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|e| {
                CrosstacheError::config(format!(
                    "failed to run {program}: {e}. It must be on PATH to manage the rotation \
                     schedule."
                ))
            })?;
        Ok(CommandOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Interval
// ---------------------------------------------------------------------------

/// How often the rotation sweep runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleInterval {
    /// Every hour, at `minute` past.
    Hourly { minute: u32 },
    /// Every day at `hour:minute`.
    Daily { hour: u32, minute: u32 },
    /// Every week on `weekday` (0 = Sunday) at `hour:minute`.
    Weekly {
        weekday: u32,
        hour: u32,
        minute: u32,
    },
}

impl ScheduleInterval {
    /// Build from the CLI's `--interval` plus `--at HH:MM`.
    pub fn from_parts(interval: &str, at: &str) -> Result<Self> {
        let (hour, minute) = parse_hhmm(at)?;
        match interval.to_lowercase().as_str() {
            "hourly" => Ok(Self::Hourly { minute }),
            "daily" => Ok(Self::Daily { hour, minute }),
            // Sunday, matching both cron's and launchd's `0`.
            "weekly" => Ok(Self::Weekly {
                weekday: 0,
                hour,
                minute,
            }),
            other => Err(CrosstacheError::InvalidArgument(format!(
                "unknown schedule interval '{other}'. Valid values: hourly, daily, weekly."
            ))),
        }
    }

    /// Human description for confirmation prompts and `status`.
    pub fn describe(&self) -> String {
        match self {
            Self::Hourly { minute } => format!("every hour at :{minute:02}"),
            Self::Daily { hour, minute } => format!("daily at {hour:02}:{minute:02}"),
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => format!(
                "weekly on {} at {hour:02}:{minute:02}",
                weekday_name(*weekday)
            ),
        }
    }
}

fn weekday_name(d: u32) -> &'static str {
    match d {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        _ => "Saturday",
    }
}

/// Parse `HH:MM` in 24-hour form.
fn parse_hhmm(at: &str) -> Result<(u32, u32)> {
    let invalid = || {
        CrosstacheError::InvalidArgument(format!(
            "invalid time '{at}'. Use 24-hour HH:MM, e.g. 03:00."
        ))
    };
    let (h, m) = at.trim().split_once(':').ok_or_else(invalid)?;
    // Reject `3:0`: a schedule silently landing an hour or an hour-and-a-half
    // off is worse than a rejected argument.
    if h.len() != 2 || m.len() != 2 {
        return Err(invalid());
    }
    let hour: u32 = h.parse().map_err(|_| invalid())?;
    let minute: u32 = m.parse().map_err(|_| invalid())?;
    if hour > 23 || minute > 59 {
        return Err(invalid());
    }
    Ok((hour, minute))
}

// ---------------------------------------------------------------------------
// The schedule
// ---------------------------------------------------------------------------

/// Everything needed to render and install a rotation schedule.
#[derive(Debug, Clone)]
pub struct RotationSchedule {
    pub interval: ScheduleInterval,
    /// Vault to sweep. `None` leaves the scheduled run to resolve the config
    /// default, which is a common source of surprise — the CLI warns.
    pub vault: Option<String>,
    /// Absolute path to the `xv` binary the unit invokes.
    pub binary: PathBuf,
    /// File the scheduled run's output is appended to.
    pub log_path: PathBuf,
    /// `HOME` for the scheduled process.
    pub home: PathBuf,
    /// `XDG_CONFIG_HOME`, when the current process has one.
    pub config_home: Option<PathBuf>,
}

impl RotationSchedule {
    /// The arguments the scheduler invokes: an unattended due-rotation sweep.
    ///
    /// `--force` is required: there is no terminal to confirm at. `--due` alone
    /// keeps the blast radius to secrets that already carry a policy and are
    /// already past it.
    pub fn command_args(&self) -> Vec<String> {
        let mut args = vec![
            "rotate".to_string(),
            "--due".to_string(),
            "--force".to_string(),
        ];
        if let Some(vault) = &self.vault {
            args.push("--vault".to_string());
            args.push(vault.clone());
        }
        args
    }

    /// The full command line, for display and for Task Scheduler's `/TR`.
    pub fn command_line(&self) -> String {
        let mut parts = vec![quote_if_needed(&self.binary.to_string_lossy())];
        parts.extend(self.command_args().iter().map(|a| quote_if_needed(a)));
        parts.join(" ")
    }

    /// Environment pairs the unit must set.
    fn env_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = vec![("HOME".to_string(), self.home.to_string_lossy().to_string())];
        if let Some(config_home) = &self.config_home {
            pairs.push((
                "XDG_CONFIG_HOME".to_string(),
                config_home.to_string_lossy().to_string(),
            ));
        }
        pairs
    }
}

fn quote_if_needed(s: &str) -> String {
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Platforms
// ---------------------------------------------------------------------------

/// Which native scheduler is in use.
///
/// Every variant is matched by the rendering and lifecycle code on all
/// platforms, but each is only *constructed* by [`Platform::detect`] on its own
/// `target_os` (plus by tests, which exercise all three everywhere). Without
/// the exemption, dead-code analysis on any single platform flags the other
/// two variants as never constructed — exactly what CI's Linux clippy run does
/// to `Launchd`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Launchd,
    Systemd,
    Schtasks,
}

impl Platform {
    /// Detect the platform's scheduler.
    ///
    /// Returns a diagnostic error, not a silent no-op, when the host has none we
    /// manage — a Linux box without systemd is a real configuration, and the
    /// right answer there is a cron line the user owns, not a scheduler `xv`
    /// pretends to have installed.
    pub fn detect() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self::Launchd)
        }
        #[cfg(target_os = "windows")]
        {
            Ok(Self::Schtasks)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if Path::new("/run/systemd/system").exists() {
                Ok(Self::Systemd)
            } else {
                Err(CrosstacheError::config(
                    "no supported scheduler found: this looks like a Linux host without systemd \
                     running (/run/systemd/system is absent), so 'xv schedule' has nothing to \
                     manage.\n  Add a cron entry instead — 'xv schedule install --print' prints \
                     the exact command to run:\n    0 3 * * *  <command>"
                        .to_string(),
                ))
            }
        }
        #[cfg(not(any(unix, target_os = "windows")))]
        {
            Err(CrosstacheError::config(
                "scheduling is not supported on this platform.".to_string(),
            ))
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd user timer",
            Self::Schtasks => "Task Scheduler",
        }
    }
}

/// A file the scheduler needs on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFile {
    pub path: PathBuf,
    pub contents: String,
}

/// Where unit files live. Overridable so tests can render into a tempdir.
#[derive(Debug, Clone)]
pub struct UnitPaths {
    /// `~/Library/LaunchAgents` on macOS, `~/.config/systemd/user` on Linux.
    pub dir: PathBuf,
}

impl UnitPaths {
    /// The platform's default unit directory under `home`.
    pub fn for_platform(platform: Platform, home: &Path) -> Self {
        let dir = match platform {
            Platform::Launchd => home.join("Library/LaunchAgents"),
            Platform::Systemd => home.join(".config/systemd/user"),
            // Task Scheduler keeps its own registry; nothing is written by us.
            Platform::Schtasks => home.to_path_buf(),
        };
        Self { dir }
    }

    fn launchd_plist(&self) -> PathBuf {
        self.dir.join(format!("{LAUNCHD_LABEL}.plist"))
    }

    fn systemd_service(&self) -> PathBuf {
        self.dir.join(format!("{SYSTEMD_UNIT}.service"))
    }

    fn systemd_timer(&self) -> PathBuf {
        self.dir.join(format!("{SYSTEMD_UNIT}.timer"))
    }
}

// ---------------------------------------------------------------------------
// Rendering (pure)
// ---------------------------------------------------------------------------

/// Render the unit file(s) for `schedule`. Pure — writes nothing.
pub fn render(platform: Platform, schedule: &RotationSchedule, paths: &UnitPaths) -> Vec<UnitFile> {
    match platform {
        Platform::Launchd => vec![UnitFile {
            path: paths.launchd_plist(),
            contents: render_launchd(schedule),
        }],
        Platform::Systemd => vec![
            UnitFile {
                path: paths.systemd_service(),
                contents: render_systemd_service(schedule),
            },
            UnitFile {
                path: paths.systemd_timer(),
                contents: render_systemd_timer(schedule),
            },
        ],
        // Task Scheduler is configured entirely through `schtasks` arguments.
        Platform::Schtasks => Vec::new(),
    }
}

/// Escape text for an XML text node.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn render_launchd(schedule: &RotationSchedule) -> String {
    let mut args = String::new();
    args.push_str(&format!(
        "        <string>{}</string>\n",
        xml_escape(&schedule.binary.to_string_lossy())
    ));
    for arg in schedule.command_args() {
        args.push_str(&format!("        <string>{}</string>\n", xml_escape(&arg)));
    }

    let mut env = String::new();
    for (key, value) in schedule.env_pairs() {
        env.push_str(&format!(
            "        <key>{}</key>\n        <string>{}</string>\n",
            xml_escape(&key),
            xml_escape(&value)
        ));
    }

    let calendar = match schedule.interval {
        ScheduleInterval::Hourly { minute } => {
            format!("        <key>Minute</key>\n        <integer>{minute}</integer>\n")
        }
        ScheduleInterval::Daily { hour, minute } => format!(
            "        <key>Hour</key>\n        <integer>{hour}</integer>\n        \
             <key>Minute</key>\n        <integer>{minute}</integer>\n"
        ),
        ScheduleInterval::Weekly {
            weekday,
            hour,
            minute,
        } => format!(
            "        <key>Weekday</key>\n        <integer>{weekday}</integer>\n        \
             <key>Hour</key>\n        <integer>{hour}</integer>\n        \
             <key>Minute</key>\n        <integer>{minute}</integer>\n"
        ),
    };

    let log = xml_escape(&schedule.log_path.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Managed by crosstache (xv schedule). Edits are overwritten on reinstall. -->
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>EnvironmentVariables</key>
    <dict>
{env}    </dict>
    <key>StartCalendarInterval</key>
    <dict>
{calendar}    </dict>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>RunAtLoad</key>
    <false/>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#
    )
}

fn render_systemd_service(schedule: &RotationSchedule) -> String {
    let mut env = String::new();
    for (key, value) in schedule.env_pairs() {
        env.push_str(&format!("Environment=\"{key}={value}\"\n"));
    }
    // systemd splits ExecStart on whitespace; quote each argument so a vault
    // name containing a space cannot become two arguments.
    let exec = std::iter::once(schedule.binary.to_string_lossy().to_string())
        .chain(schedule.command_args())
        .map(|a| format!("\"{}\"", a.replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" ");
    let log = schedule.log_path.display();
    format!(
        "# Managed by crosstache (xv schedule). Edits are overwritten on reinstall.\n\
         [Unit]\n\
         Description=crosstache due-secret rotation sweep\n\
         Documentation=https://github.com/bziobnic/crosstache/blob/main/docs/rotation.md\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={exec}\n\
         {env}\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n"
    )
}

fn render_systemd_timer(schedule: &RotationSchedule) -> String {
    let on_calendar = match schedule.interval {
        ScheduleInterval::Hourly { minute } => format!("*-*-* *:{minute:02}:00"),
        ScheduleInterval::Daily { hour, minute } => format!("*-*-* {hour:02}:{minute:02}:00"),
        ScheduleInterval::Weekly {
            weekday,
            hour,
            minute,
        } => format!(
            "{} *-*-* {hour:02}:{minute:02}:00",
            systemd_weekday(weekday)
        ),
    };
    format!(
        "# Managed by crosstache (xv schedule). Edits are overwritten on reinstall.\n\
         [Unit]\n\
         Description=crosstache due-secret rotation schedule\n\
         \n\
         [Timer]\n\
         OnCalendar={on_calendar}\n\
         # Run a missed sweep once the machine is back, rather than skipping a day.\n\
         Persistent=true\n\
         # Spread load and avoid every host rotating at the same instant.\n\
         RandomizedDelaySec=300\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    )
}

fn systemd_weekday(d: u32) -> &'static str {
    match d {
        0 => "Sun",
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        _ => "Sat",
    }
}

/// `schtasks /Create` arguments for `schedule`.
pub fn schtasks_create_args(schedule: &RotationSchedule) -> Vec<String> {
    let (sc, st, extra) = match schedule.interval {
        ScheduleInterval::Hourly { minute } => (
            "HOURLY".to_string(),
            format!("00:{minute:02}"),
            Vec::<String>::new(),
        ),
        ScheduleInterval::Daily { hour, minute } => (
            "DAILY".to_string(),
            format!("{hour:02}:{minute:02}"),
            Vec::new(),
        ),
        ScheduleInterval::Weekly {
            weekday,
            hour,
            minute,
        } => (
            "WEEKLY".to_string(),
            format!("{hour:02}:{minute:02}"),
            vec!["/D".to_string(), schtasks_weekday(weekday).to_string()],
        ),
    };

    // Output is redirected through cmd.exe so failures land in the log file, the
    // same way launchd's StandardOutPath and systemd's append: do.
    let command = format!(
        "cmd /c {} >> \"{}\" 2>&1",
        schedule.command_line(),
        schedule.log_path.display()
    );

    let mut args = vec![
        "/Create".to_string(),
        "/TN".to_string(),
        SCHTASKS_NAME.to_string(),
        "/TR".to_string(),
        command,
        "/SC".to_string(),
        sc,
        "/ST".to_string(),
        st,
        // Overwrite an existing task so reinstall is idempotent.
        "/F".to_string(),
    ];
    args.extend(extra);
    args
}

fn schtasks_weekday(d: u32) -> &'static str {
    match d {
        0 => "SUN",
        1 => "MON",
        2 => "TUE",
        3 => "WED",
        4 => "THU",
        5 => "FRI",
        _ => "SAT",
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Whether a schedule is currently registered, and what the OS says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleStatus {
    pub installed: bool,
    /// The scheduler's own description, for display.
    pub detail: String,
}

/// Install (or reinstall) the schedule. Idempotent.
pub fn install(
    platform: Platform,
    schedule: &RotationSchedule,
    paths: &UnitPaths,
    runner: &dyn CommandRunner,
) -> Result<()> {
    // Ensure the log directory exists before the scheduler tries to append; a
    // missing directory makes launchd fail the job with no visible reason.
    if let Some(parent) = schedule.log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CrosstacheError::config(format!(
                "failed to create the log directory {}: {e}",
                parent.display()
            ))
        })?;
    }

    for unit in render(platform, schedule, paths) {
        if let Some(parent) = unit.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CrosstacheError::config(format!("failed to create {}: {e}", parent.display()))
            })?;
        }
        std::fs::write(&unit.path, &unit.contents).map_err(|e| {
            CrosstacheError::config(format!("failed to write {}: {e}", unit.path.display()))
        })?;
    }

    match platform {
        Platform::Launchd => {
            let plist = paths.launchd_plist();
            let target = launchd_domain_target();
            // Remove any previous registration first so reinstall is not an
            // "already bootstrapped" error.
            let _ = runner.run("launchctl", &["bootout", &target]);
            let domain = launchd_domain();
            expect_ok(
                runner.run(
                    "launchctl",
                    &["bootstrap", &domain, &plist.to_string_lossy()],
                )?,
                "launchctl bootstrap",
            )
        }
        Platform::Systemd => {
            expect_ok(
                runner.run("systemctl", &["--user", "daemon-reload"])?,
                "systemctl --user daemon-reload",
            )?;
            expect_ok(
                runner.run(
                    "systemctl",
                    &[
                        "--user",
                        "enable",
                        "--now",
                        &format!("{SYSTEMD_UNIT}.timer"),
                    ],
                )?,
                "systemctl --user enable --now",
            )
        }
        Platform::Schtasks => {
            let args = schtasks_create_args(schedule);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            expect_ok(runner.run("schtasks", &refs)?, "schtasks /Create")
        }
    }
}

/// Report whether the schedule is registered.
pub fn status(
    platform: Platform,
    paths: &UnitPaths,
    runner: &dyn CommandRunner,
) -> Result<ScheduleStatus> {
    match platform {
        Platform::Launchd => {
            let out = runner.run("launchctl", &["print", &launchd_domain_target()])?;
            Ok(ScheduleStatus {
                installed: out.ok(),
                detail: if out.ok() {
                    summarize_launchd(&out.stdout)
                } else {
                    format!(
                        "not registered with launchd (plist {} {})",
                        paths.launchd_plist().display(),
                        if paths.launchd_plist().exists() {
                            "exists but is not loaded"
                        } else {
                            "does not exist"
                        }
                    )
                },
            })
        }
        Platform::Systemd => {
            let timer = format!("{SYSTEMD_UNIT}.timer");
            let active = runner.run("systemctl", &["--user", "is-active", &timer])?;
            let listed = runner.run("systemctl", &["--user", "list-timers", "--all", &timer])?;
            Ok(ScheduleStatus {
                installed: active.stdout.trim() == "active",
                detail: if active.stdout.trim() == "active" {
                    listed.stdout.trim().to_string()
                } else {
                    format!("timer {timer} is {}", active.stdout.trim())
                },
            })
        }
        Platform::Schtasks => {
            let out = runner.run("schtasks", &["/Query", "/TN", SCHTASKS_NAME])?;
            Ok(ScheduleStatus {
                installed: out.ok(),
                detail: if out.ok() {
                    out.stdout.trim().to_string()
                } else {
                    format!("task {SCHTASKS_NAME} is not registered")
                },
            })
        }
    }
}

/// Remove the schedule. Succeeds when nothing was installed.
pub fn uninstall(
    platform: Platform,
    paths: &UnitPaths,
    runner: &dyn CommandRunner,
) -> Result<bool> {
    let mut removed = false;

    match platform {
        Platform::Launchd => {
            // A missing job is not an error here: uninstall must converge on
            // "absent" whatever the starting state.
            if runner
                .run("launchctl", &["bootout", &launchd_domain_target()])?
                .ok()
            {
                removed = true;
            }
        }
        Platform::Systemd => {
            let timer = format!("{SYSTEMD_UNIT}.timer");
            if runner
                .run("systemctl", &["--user", "disable", "--now", &timer])?
                .ok()
            {
                removed = true;
            }
        }
        Platform::Schtasks => {
            if runner
                .run("schtasks", &["/Delete", "/TN", SCHTASKS_NAME, "/F"])?
                .ok()
            {
                removed = true;
            }
        }
    }

    for unit in unit_paths_for(platform, paths) {
        if unit.exists() {
            std::fs::remove_file(&unit).map_err(|e| {
                CrosstacheError::config(format!("failed to remove {}: {e}", unit.display()))
            })?;
            removed = true;
        }
    }

    if platform == Platform::Systemd {
        // Reload so the removed units leave systemd's view too.
        let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
    }

    Ok(removed)
}

/// Unit files this platform owns, whether or not they exist.
pub fn unit_paths_for(platform: Platform, paths: &UnitPaths) -> Vec<PathBuf> {
    match platform {
        Platform::Launchd => vec![paths.launchd_plist()],
        Platform::Systemd => vec![paths.systemd_service(), paths.systemd_timer()],
        Platform::Schtasks => Vec::new(),
    }
}

fn expect_ok(out: CommandOutput, what: &str) -> Result<()> {
    if out.ok() {
        return Ok(());
    }
    let detail = if out.stderr.trim().is_empty() {
        out.stdout.trim().to_string()
    } else {
        out.stderr.trim().to_string()
    };
    Err(CrosstacheError::config(format!(
        "{what} failed (exit {}): {detail}",
        out.status
    )))
}

/// launchd's per-user domain, e.g. `gui/501`.
fn launchd_domain() -> String {
    format!("gui/{}", current_uid())
}

/// launchd's service target, e.g. `gui/501/com.crosstache.xv-rotate`.
fn launchd_domain_target() -> String {
    format!("{}/{LAUNCHD_LABEL}", launchd_domain())
}

fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getuid is always safe; it reads the calling process's real UID
        // and cannot fail.
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Pull the interesting lines out of `launchctl print` output.
fn summarize_launchd(stdout: &str) -> String {
    let mut wanted = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("state =")
            || trimmed.starts_with("last exit code =")
            || trimmed.starts_with("runs =")
        {
            wanted.push(trimmed.to_string());
        }
    }
    if wanted.is_empty() {
        "registered with launchd".to_string()
    } else {
        wanted.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule() -> RotationSchedule {
        RotationSchedule {
            interval: ScheduleInterval::Daily {
                hour: 3,
                minute: 30,
            },
            vault: Some("prod-kv".into()),
            binary: PathBuf::from("/usr/local/bin/xv"),
            log_path: PathBuf::from("/home/u/.local/state/xv/rotate.log"),
            home: PathBuf::from("/home/u"),
            config_home: Some(PathBuf::from("/home/u/.config")),
        }
    }

    fn paths() -> UnitPaths {
        UnitPaths {
            dir: PathBuf::from("/home/u/units"),
        }
    }

    /// Runner that records calls and returns scripted results.
    #[derive(Debug, Default)]
    struct FakeRunner {
        calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
        /// Exit code returned for any call whose args contain this substring.
        fail_containing: Option<String>,
    }

    impl FakeRunner {
        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.lock().unwrap().clone()
        }
        fn programs(&self) -> Vec<String> {
            self.calls().into_iter().map(|(p, _)| p).collect()
        }
        fn flat(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .map(|(p, a)| format!("{p} {}", a.join(" ")))
                .collect()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
            let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
            self.calls
                .lock()
                .unwrap()
                .push((program.to_string(), args.clone()));
            let joined = args.join(" ");
            let fail = self
                .fail_containing
                .as_ref()
                .is_some_and(|needle| joined.contains(needle));
            Ok(CommandOutput {
                status: if fail { 1 } else { 0 },
                stdout: if program == "systemctl" && joined.contains("is-active") {
                    "active\n".to_string()
                } else {
                    String::new()
                },
                stderr: if fail {
                    "boom".to_string()
                } else {
                    String::new()
                },
            })
        }
    }

    // -- interval parsing ---------------------------------------------------

    #[test]
    fn parses_intervals_and_times() {
        assert_eq!(
            ScheduleInterval::from_parts("daily", "03:00").unwrap(),
            ScheduleInterval::Daily { hour: 3, minute: 0 }
        );
        assert_eq!(
            ScheduleInterval::from_parts("hourly", "00:15").unwrap(),
            ScheduleInterval::Hourly { minute: 15 }
        );
        assert_eq!(
            ScheduleInterval::from_parts("weekly", "23:59").unwrap(),
            ScheduleInterval::Weekly {
                weekday: 0,
                hour: 23,
                minute: 59
            }
        );
    }

    #[test]
    fn rejects_malformed_or_out_of_range_times() {
        // `3:0` is rejected rather than guessed: silently landing an hour off is
        // worse than a rejected argument.
        for bad in [
            "3:0", "0300", "24:00", "03:60", "", ":", "aa:bb", "03:00:00",
        ] {
            assert!(
                ScheduleInterval::from_parts("daily", bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(ScheduleInterval::from_parts("fortnightly", "03:00").is_err());
    }

    #[test]
    fn describes_intervals_readably() {
        assert_eq!(
            ScheduleInterval::Daily { hour: 3, minute: 5 }.describe(),
            "daily at 03:05"
        );
        assert_eq!(
            ScheduleInterval::Hourly { minute: 7 }.describe(),
            "every hour at :07"
        );
        assert_eq!(
            ScheduleInterval::Weekly {
                weekday: 0,
                hour: 3,
                minute: 0
            }
            .describe(),
            "weekly on Sunday at 03:00"
        );
    }

    // -- command construction ----------------------------------------------

    #[test]
    fn scheduled_command_is_an_unattended_due_sweep() {
        let s = schedule();
        assert_eq!(
            s.command_args(),
            vec!["rotate", "--due", "--force", "--vault", "prod-kv"]
        );
        // --due bounds the blast radius to policy-bearing, already-due secrets;
        // --force is required because there is no terminal to confirm at.
        assert!(s.command_args().contains(&"--due".to_string()));
        assert!(!s.command_args().contains(&"--every".to_string()));
    }

    #[test]
    fn command_omits_vault_when_unset() {
        let mut s = schedule();
        s.vault = None;
        assert_eq!(s.command_args(), vec!["rotate", "--due", "--force"]);
    }

    #[test]
    fn command_line_quotes_paths_with_spaces() {
        let mut s = schedule();
        s.binary = PathBuf::from("/Applications/My Tools/xv");
        assert!(
            s.command_line()
                .starts_with("\"/Applications/My Tools/xv\""),
            "{}",
            s.command_line()
        );
    }

    // -- launchd ------------------------------------------------------------

    #[test]
    fn launchd_plist_is_well_formed_and_complete() {
        let units = render(Platform::Launchd, &schedule(), &paths());
        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].path,
            PathBuf::from("/home/u/units/com.crosstache.xv-rotate.plist")
        );
        let body = &units[0].contents;

        assert!(body.starts_with("<?xml version=\"1.0\""), "{body}");
        assert!(body.contains("<string>com.crosstache.xv-rotate</string>"));
        assert!(body.contains("<string>/usr/local/bin/xv</string>"));
        assert!(body.contains("<string>--due</string>"));
        assert!(body.contains("<string>prod-kv</string>"));
        // Calendar interval, not a naive StartInterval.
        assert!(body.contains("StartCalendarInterval"));
        assert!(body.contains("<key>Hour</key>\n        <integer>3</integer>"));
        assert!(body.contains("<key>Minute</key>\n        <integer>30</integer>"));
        // Environment so the scheduled run resolves the same config.
        assert!(body.contains("<key>HOME</key>"));
        assert!(body.contains("<key>XDG_CONFIG_HOME</key>"));
        // Output must be capturable or a 3am failure is invisible.
        assert!(body.contains("StandardOutPath"));
        assert!(body.contains("/home/u/.local/state/xv/rotate.log"));
        // Must not fire on load — installing is not rotating.
        assert!(body.contains("<key>RunAtLoad</key>\n    <false/>"));
        // Tags balance.
        assert_eq!(
            body.matches("<dict>").count(),
            body.matches("</dict>").count()
        );
        assert_eq!(
            body.matches("<array>").count(),
            body.matches("</array>").count()
        );
    }

    #[test]
    fn launchd_plist_escapes_xml_metacharacters() {
        // A vault name with an ampersand would otherwise produce invalid XML and
        // a job launchd silently refuses to load.
        let mut s = schedule();
        s.vault = Some("prod&stage<\"'>".into());
        let body = render(Platform::Launchd, &s, &paths())[0].contents.clone();
        assert!(
            body.contains("prod&amp;stage&lt;&quot;&apos;&gt;"),
            "{body}"
        );
        assert!(!body.contains("prod&stage"), "{body}");
    }

    #[test]
    fn launchd_hourly_and_weekly_calendars() {
        let mut s = schedule();
        s.interval = ScheduleInterval::Hourly { minute: 15 };
        let body = render(Platform::Launchd, &s, &paths())[0].contents.clone();
        assert!(body.contains("<key>Minute</key>"));
        assert!(!body.contains("<key>Hour</key>"), "hourly pins no hour");

        s.interval = ScheduleInterval::Weekly {
            weekday: 0,
            hour: 4,
            minute: 0,
        };
        let body = render(Platform::Launchd, &s, &paths())[0].contents.clone();
        assert!(body.contains("<key>Weekday</key>\n        <integer>0</integer>"));
    }

    // -- systemd ------------------------------------------------------------

    #[test]
    fn systemd_renders_a_service_and_a_timer() {
        let units = render(Platform::Systemd, &schedule(), &paths());
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].path.file_name().unwrap(), "xv-rotate.service");
        assert_eq!(units[1].path.file_name().unwrap(), "xv-rotate.timer");

        let service = &units[0].contents;
        assert!(service.contains("Type=oneshot"));
        assert!(service.contains("\"/usr/local/bin/xv\" \"rotate\" \"--due\" \"--force\""));
        assert!(service.contains("Environment=\"HOME=/home/u\""));
        assert!(service.contains("Environment=\"XDG_CONFIG_HOME=/home/u/.config\""));
        assert!(service.contains("StandardOutput=append:/home/u/.local/state/xv/rotate.log"));

        let timer = &units[1].contents;
        assert!(timer.contains("OnCalendar=*-*-* 03:30:00"), "{timer}");
        // A missed sweep should run once the machine is back, not be skipped.
        assert!(timer.contains("Persistent=true"));
        assert!(timer.contains("WantedBy=timers.target"));
    }

    #[test]
    fn systemd_quotes_arguments_so_a_spaced_vault_stays_one_argument() {
        let mut s = schedule();
        s.vault = Some("my vault".into());
        let service = render(Platform::Systemd, &s, &paths())[0].contents.clone();
        assert!(service.contains("\"my vault\""), "{service}");
    }

    #[test]
    fn systemd_calendars_for_each_interval() {
        let mut s = schedule();
        s.interval = ScheduleInterval::Hourly { minute: 5 };
        assert!(render(Platform::Systemd, &s, &paths())[1]
            .contents
            .contains("OnCalendar=*-*-* *:05:00"));

        s.interval = ScheduleInterval::Weekly {
            weekday: 3,
            hour: 6,
            minute: 0,
        };
        assert!(render(Platform::Systemd, &s, &paths())[1]
            .contents
            .contains("OnCalendar=Wed *-*-* 06:00:00"));
    }

    // -- schtasks -----------------------------------------------------------

    #[test]
    fn schtasks_arguments_are_complete_and_idempotent() {
        let args = schtasks_create_args(&schedule());
        let joined = args.join(" ");
        assert!(joined.contains("/Create"));
        assert!(joined.contains("/TN crosstache-xv-rotate"));
        assert!(joined.contains("/SC DAILY"));
        assert!(joined.contains("/ST 03:30"));
        // /F overwrites, so reinstall does not fail on an existing task.
        assert!(args.contains(&"/F".to_string()));
        // Output redirected so a failure is diagnosable.
        assert!(joined.contains("rotate.log"));
        assert!(joined.contains("2>&1"));
        // Task Scheduler writes no unit files of ours.
        assert!(render(Platform::Schtasks, &schedule(), &paths()).is_empty());
    }

    #[test]
    fn schtasks_weekly_passes_the_day() {
        let mut s = schedule();
        s.interval = ScheduleInterval::Weekly {
            weekday: 5,
            hour: 2,
            minute: 0,
        };
        let joined = schtasks_create_args(&s).join(" ");
        assert!(joined.contains("/SC WEEKLY"), "{joined}");
        assert!(joined.contains("/D FRI"), "{joined}");
    }

    // -- lifecycle ----------------------------------------------------------

    #[test]
    fn install_writes_units_then_registers_with_launchd() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = schedule();
        s.log_path = tmp.path().join("state/rotate.log");
        let p = UnitPaths {
            dir: tmp.path().join("LaunchAgents"),
        };
        let runner = FakeRunner::default();

        install(Platform::Launchd, &s, &p, &runner).unwrap();

        assert!(p.dir.join("com.crosstache.xv-rotate.plist").exists());
        // Log directory pre-created; launchd fails silently without it.
        assert!(tmp.path().join("state").is_dir());

        let flat = runner.flat();
        // bootout before bootstrap makes reinstall idempotent.
        assert!(flat[0].contains("bootout"), "{flat:?}");
        assert!(flat[1].contains("bootstrap"), "{flat:?}");
        assert!(
            flat[1].contains("com.crosstache.xv-rotate.plist"),
            "{flat:?}"
        );
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = schedule();
        s.log_path = tmp.path().join("state/rotate.log");
        let p = UnitPaths {
            dir: tmp.path().join("units"),
        };
        let runner = FakeRunner::default();
        install(Platform::Systemd, &s, &p, &runner).unwrap();
        install(Platform::Systemd, &s, &p, &runner).unwrap();
        assert!(p.dir.join("xv-rotate.timer").exists());
        assert_eq!(
            std::fs::read_dir(&p.dir).unwrap().count(),
            2,
            "reinstall must not accumulate unit files"
        );
    }

    #[test]
    fn install_reports_scheduler_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = schedule();
        s.log_path = tmp.path().join("rotate.log");
        let p = UnitPaths {
            dir: tmp.path().join("units"),
        };
        let runner = FakeRunner {
            fail_containing: Some("enable".to_string()),
            ..Default::default()
        };
        let err = install(Platform::Systemd, &s, &p, &runner).expect_err("must surface failure");
        let msg = err.to_string();
        assert!(msg.contains("systemctl --user enable --now"), "{msg}");
        assert!(msg.contains("boom"), "{msg}");
    }

    #[test]
    fn systemd_install_reloads_before_enabling() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = schedule();
        s.log_path = tmp.path().join("rotate.log");
        let p = UnitPaths {
            dir: tmp.path().join("units"),
        };
        let runner = FakeRunner::default();
        install(Platform::Systemd, &s, &p, &runner).unwrap();
        let flat = runner.flat();
        // Enabling before a reload would act on a stale unit view.
        assert!(flat[0].contains("daemon-reload"), "{flat:?}");
        assert!(flat[1].contains("enable --now xv-rotate.timer"), "{flat:?}");
    }

    #[test]
    fn uninstall_removes_units_and_deregisters() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = schedule();
        s.log_path = tmp.path().join("rotate.log");
        let p = UnitPaths {
            dir: tmp.path().join("units"),
        };
        let runner = FakeRunner::default();
        install(Platform::Systemd, &s, &p, &runner).unwrap();

        let removed = uninstall(Platform::Systemd, &p, &runner).unwrap();
        assert!(removed);
        assert!(!p.dir.join("xv-rotate.timer").exists());
        assert!(!p.dir.join("xv-rotate.service").exists());
        let flat = runner.flat();
        assert!(flat.iter().any(|c| c.contains("disable --now")), "{flat:?}");
    }

    #[test]
    fn uninstall_converges_when_nothing_is_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let p = UnitPaths {
            dir: tmp.path().join("units"),
        };
        // Every scheduler call fails, as it would with no job registered.
        let runner = FakeRunner {
            fail_containing: Some(String::new()),
            ..Default::default()
        };
        let removed = uninstall(Platform::Launchd, &p, &runner).unwrap();
        assert!(!removed, "nothing was there to remove");
    }

    #[test]
    fn status_reports_installed_for_each_platform() {
        let tmp = tempfile::tempdir().unwrap();
        let p = UnitPaths {
            dir: tmp.path().to_path_buf(),
        };
        let runner = FakeRunner::default();

        assert!(status(Platform::Launchd, &p, &runner).unwrap().installed);
        assert!(status(Platform::Systemd, &p, &runner).unwrap().installed);
        assert!(status(Platform::Schtasks, &p, &runner).unwrap().installed);
        assert_eq!(runner.programs().len(), 4, "systemd needs two queries");
    }

    #[test]
    fn status_reports_absent_when_the_scheduler_says_no() {
        let tmp = tempfile::tempdir().unwrap();
        let p = UnitPaths {
            dir: tmp.path().to_path_buf(),
        };
        let runner = FakeRunner {
            fail_containing: Some(String::new()),
            ..Default::default()
        };
        let s = status(Platform::Launchd, &p, &runner).unwrap();
        assert!(!s.installed);
        assert!(s.detail.contains("does not exist"), "{}", s.detail);
    }

    #[test]
    fn unit_paths_match_platform_conventions() {
        let home = Path::new("/home/u");
        assert_eq!(
            UnitPaths::for_platform(Platform::Launchd, home).dir,
            PathBuf::from("/home/u/Library/LaunchAgents")
        );
        assert_eq!(
            UnitPaths::for_platform(Platform::Systemd, home).dir,
            PathBuf::from("/home/u/.config/systemd/user")
        );
        assert!(unit_paths_for(Platform::Schtasks, &paths()).is_empty());
    }

    #[test]
    fn launchd_targets_the_per_user_gui_domain() {
        // A system domain would need root and would run rotation as the wrong
        // user, without access to their credentials or config.
        assert!(launchd_domain().starts_with("gui/"));
        assert!(launchd_domain_target().ends_with("/com.crosstache.xv-rotate"));
    }

    #[test]
    fn summarize_launchd_extracts_the_useful_lines() {
        let out = "\tstate = running\n\tlast exit code = 0\n\truns = 12\n\tnoise = x\n";
        let summary = summarize_launchd(out);
        assert!(summary.contains("state = running"));
        assert!(summary.contains("last exit code = 0"));
        assert!(!summary.contains("noise"));
        assert_eq!(
            summarize_launchd("nothing useful"),
            "registered with launchd"
        );
    }

    #[test]
    fn no_unit_ever_contains_a_credential_shaped_value() {
        // Guard against a future change threading auth into the unit: the
        // scheduled run must authenticate the same way an interactive one does.
        for platform in [Platform::Launchd, Platform::Systemd] {
            for unit in render(platform, &schedule(), &paths()) {
                let body = unit.contents.to_lowercase();
                for forbidden in [
                    "client_secret",
                    "password",
                    "age-secret-key",
                    "azure_client_secret",
                    "aws_secret_access_key",
                    "bearer ",
                ] {
                    assert!(
                        !body.contains(forbidden),
                        "{platform:?} unit contains {forbidden:?}"
                    );
                }
            }
        }
    }
}
