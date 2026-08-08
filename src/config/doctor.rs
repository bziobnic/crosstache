use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::DocumentMut;

use crate::{
    config::Config,
    error::{CrosstacheError, Result},
    utils::helpers::read_file_no_follow,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorCheckStatus {
    Ok,
    Fixed,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub status: DoctorCheckStatus,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub path: PathBuf,
    pub checks: Vec<DoctorCheck>,
    pub repairs: Vec<String>,
    pub backup_path: Option<PathBuf>,
    pub unresolved: Vec<String>,
}

impl DoctorReport {
    pub fn is_healthy(&self) -> bool {
        self.unresolved.is_empty()
    }

    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            checks: Vec::new(),
            repairs: Vec::new(),
            backup_path: None,
            unresolved: Vec::new(),
        }
    }

    fn with_check(mut self, status: DoctorCheckStatus, message: impl Into<String>) -> Self {
        self.checks.push(DoctorCheck {
            status,
            message: message.into(),
        });
        self
    }

    fn with_unresolved(self, message: impl Into<String>) -> Self {
        let message = message.into();
        let mut report = self.with_check(DoctorCheckStatus::Error, message.clone());
        report.unresolved.push(message);
        report
    }
}

enum ParsedConfig {
    Toml(DocumentMut),
    Json(Value),
}

pub async fn diagnose_and_repair(path: &Path) -> Result<DoctorReport> {
    let report = DoctorReport::new(path);
    let original = match std::fs::symlink_metadata(path) {
        Ok(_) => read_file_no_follow(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(report.with_check(
                DoctorCheckStatus::Ok,
                format!("Configuration file '{}' does not exist.", path.display()),
            ));
        }
        Err(error) => {
            return Err(CrosstacheError::config(format!(
                "Failed to inspect config file '{}': {error}",
                path.display()
            )));
        }
    };

    let text = std::str::from_utf8(&original).map_err(|error| {
        CrosstacheError::config(format!(
            "Configuration file '{}' is not UTF-8: {error}",
            path.display()
        ))
    })?;
    let parsed = match text.parse::<DocumentMut>() {
        Ok(document) => ParsedConfig::Toml(document),
        Err(toml_error) => match serde_json::from_str::<Value>(text) {
            Ok(value) => ParsedConfig::Json(value),
            Err(_) => {
                let location = toml_error
                    .span()
                    .map(|span| format!(" at byte {}", span.start))
                    .unwrap_or_default();
                return Ok(report.with_unresolved(format!(
                    "TOML syntax error in '{}'{}.",
                    path.display(),
                    location
                )));
            }
        },
    };

    let schema_format = match parsed {
        ParsedConfig::Toml(document) => toml::from_str::<Config>(&document.to_string())
            .err()
            .map(|_| "TOML"),
        ParsedConfig::Json(value) => serde_json::from_value::<Config>(value)
            .err()
            .map(|_| "JSON"),
    };

    match schema_format {
        None => Ok(report.with_check(
            DoctorCheckStatus::Ok,
            format!("Configuration file '{}' is valid.", path.display()),
        )),
        Some(format_name) => Ok(report.with_unresolved(format!(
            "{format_name} schema error in '{}'.",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    use super::{diagnose_and_repair, DoctorReport};

    fn assert_diagnostics_exclude(report: &DoctorReport, sensitive_value: &str) {
        for message in report
            .checks
            .iter()
            .map(|check| &check.message)
            .chain(report.unresolved.iter())
        {
            assert!(
                !message.contains(sensitive_value),
                "diagnostic disclosed sensitive value: {message}"
            );
        }
    }

    #[tokio::test]
    async fn missing_config_is_healthy_and_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let report = diagnose_and_repair(&path).await.unwrap();
        assert!(report.is_healthy());
        assert!(report.backup_path.is_none());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn current_toml_is_healthy_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let config = Config {
            backend: Some("local".into()),
            ..Config::default()
        };
        let bytes = toml::to_string_pretty(&config).unwrap().into_bytes();
        std::fs::write(&path, &bytes).unwrap();
        let report = diagnose_and_repair(&path).await.unwrap();
        assert!(report.is_healthy());
        assert!(report.backup_path.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[tokio::test]
    async fn malformed_toml_reports_location_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let bytes = b"debug = [\n";
        std::fs::write(&path, bytes).unwrap();
        let report = diagnose_and_repair(&path).await.unwrap();
        assert!(!report.is_healthy());
        assert!(report.unresolved.join("\n").contains("syntax"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(report.backup_path.is_none());
    }

    #[tokio::test]
    async fn malformed_toml_diagnostics_do_not_disclose_source_values() {
        const SENTINEL: &str = "doctor-private-malformed-toml-value";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        std::fs::write(&path, format!("backend = [\"{SENTINEL}\"\n")).unwrap();

        let report = diagnose_and_repair(&path).await.unwrap();

        assert!(!report.is_healthy());
        assert_diagnostics_exclude(&report, SENTINEL);
    }

    #[tokio::test]
    async fn toml_schema_diagnostics_do_not_disclose_invalid_values() {
        const SENTINEL: &str = "doctor-private-toml-schema-value";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        std::fs::write(&path, format!("debug = \"{SENTINEL}\"\n")).unwrap();

        let report = diagnose_and_repair(&path).await.unwrap();

        assert!(!report.is_healthy());
        assert_diagnostics_exclude(&report, SENTINEL);
    }

    #[tokio::test]
    async fn json_schema_diagnostics_do_not_disclose_invalid_values() {
        const SENTINEL: &str = "doctor-private-json-schema-value";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        std::fs::write(&path, format!(r#"{{"debug":"{SENTINEL}"}}"#)).unwrap();

        let report = diagnose_and_repair(&path).await.unwrap();

        assert!(!report.is_healthy());
        assert_diagnostics_exclude(&report, SENTINEL);
    }

    #[tokio::test]
    async fn valid_complete_json_is_accepted_without_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let config = Config {
            backend: Some("local".into()),
            ..Config::default()
        };
        let bytes = serde_json::to_vec_pretty(&config).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let report = diagnose_and_repair(&path).await.unwrap();
        assert!(report.is_healthy());
        assert!(report.backup_path.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
