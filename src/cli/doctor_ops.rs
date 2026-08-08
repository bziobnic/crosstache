use crate::config::doctor::{diagnose_and_repair, DoctorCheckStatus};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};

pub(crate) async fn execute_doctor_command() -> Result<()> {
    let path = Config::get_config_path()?;
    let report = diagnose_and_repair(&path).await?;

    println!("Configuration: {}", report.path.display());
    for check in &report.checks {
        let status = match check.status {
            DoctorCheckStatus::Ok => "ok",
            DoctorCheckStatus::Fixed => "fixed",
            DoctorCheckStatus::Error => "error",
        };
        println!("{status}: {}", check.message);
        if matches!(check.status, DoctorCheckStatus::Error)
            && (check.message.contains("syntax error") || check.message.contains("schema error"))
        {
            println!(
                "action: Edit the indicated field or section in '{}', then run `xv doctor` again.",
                report.path.display()
            );
        }
    }
    if report
        .checks
        .iter()
        .any(|check| check.message.contains("does not exist"))
    {
        println!("ok: Configuration defaults are usable; no file was created.");
    } else if report.repairs.is_empty() && report.unresolved.is_empty() {
        println!("ok: Configuration is healthy.");
    }
    if let Some(backup_path) = &report.backup_path {
        println!("Backup: {}", backup_path.display());
    }

    if report.unresolved.is_empty() {
        Ok(())
    } else {
        Err(CrosstacheError::config(
            "doctor found unresolved configuration errors; review the errors above",
        ))
    }
}
