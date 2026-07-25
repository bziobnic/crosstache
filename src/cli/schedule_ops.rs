//! Rotation-schedule command handlers (`xv schedule ...`).
//!
//! Thin layer over [`crate::schedule`]: resolves the schedule from CLI flags,
//! confirms the consequences of unattended rotation, and reports what the
//! platform scheduler says.

use std::path::{Path, PathBuf};

use crate::cli::commands::ScheduleCommands;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::schedule::{
    self, Platform, ProcessRunner, RotationSchedule, ScheduleInterval, UnitPaths,
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

/// Build the schedule from flags plus the current process's environment.
fn build_schedule(
    interval: ScheduleInterval,
    vault: Option<String>,
    log_file: Option<String>,
) -> Result<RotationSchedule> {
    let home = home_dir()?;

    // The absolute path of *this* binary, so the unit keeps working when PATH
    // changes or the user's shell init is not sourced.
    let binary = std::env::current_exe().map_err(|e| {
        CrosstacheError::config(format!(
            "could not determine the path to the running xv binary: {e}"
        ))
    })?;

    Ok(RotationSchedule {
        interval,
        vault,
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

    // Default to the vault the user is actually working in, rather than leaving
    // the scheduled run to re-resolve a context that may since have changed.
    let vault = match vault {
        Some(v) => Some(v),
        None => Some(config.default_vault.clone()).filter(|v| !v.is_empty()),
    };

    let schedule = build_schedule(interval, vault.clone(), log_file)?;
    let paths = UnitPaths::for_platform(platform, &schedule.home);

    if print {
        // Dry run: show exactly what would be installed, write nothing.
        println!("# scheduler: {}", platform.name());
        println!("# schedule:  {}", schedule.interval.describe());
        println!("# command:   {}", schedule.command_line());
        println!("# log:       {}", schedule.log_path.display());
        for unit in schedule::render(platform, &schedule, &paths) {
            println!("\n# --- {} ---", unit.path.display());
            print!("{}", unit.contents);
        }
        if platform == Platform::Schtasks {
            println!(
                "\n# --- schtasks invocation ---\nschtasks {}",
                schedule::schtasks_create_args(&schedule).join(" ")
            );
        }
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
        if vault.is_none() {
            output::warn(
                "No --vault was given and no default_vault is configured, so the scheduled run \
                 will resolve whatever vault its context points at when it fires. Pass --vault to \
                 pin it.",
            );
        }
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
