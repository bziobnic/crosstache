use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;
use toml_edit::{DocumentMut, TableLike};

use crate::{
    config::{apply_environment_overrides, BlobConfig, Config},
    error::{CrosstacheError, Result},
    utils::helpers::{atomic_replace_with_private_backup_no_follow_async, read_file_no_follow},
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

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "debug",
    "subscription_id",
    "default_vault",
    "default_resource_group",
    "default_location",
    "tenant_id",
    "output_json",
    "no_color",
];

const REQUIRED_BLOB_CONFIG: &[&str] = &[
    "storage_account",
    "container_name",
    "enable_large_file_support",
    "chunk_size_mb",
    "max_concurrent_uploads",
];

const LEGACY_KEY_MIGRATIONS: &[(&str, &str)] = &[];

fn serialized_defaults<T: serde::Serialize>(defaults: T) -> serde_json::Map<String, Value> {
    serde_json::to_value(defaults)
        .expect("configuration defaults must serialize")
        .as_object()
        .expect("configuration defaults must serialize as objects")
        .clone()
}

fn default_to_toml_value(default: &Value) -> toml_edit::Item {
    match default {
        Value::Bool(default) => toml_edit::value(*default),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                toml_edit::value(value)
            } else if let Some(value) = value.as_u64() {
                toml_edit::value(i64::try_from(value).expect("default integer must fit in TOML"))
            } else if let Some(value) = value.as_f64() {
                toml_edit::value(value)
            } else {
                unreachable!("configuration defaults only contain valid JSON numbers")
            }
        }
        Value::String(default) => toml_edit::value(default.clone()),
        _ => unreachable!("repaired configuration defaults must be TOML scalars"),
    }
}

fn repair_toml_fields(
    table: &mut dyn TableLike,
    defaults: &serde_json::Map<String, Value>,
    fields: &[&str],
    prefix: &str,
    repairs: &mut Vec<String>,
) {
    for &field in fields {
        if !table.contains_key(field) {
            let default = defaults
                .get(field)
                .expect("required repair field must have a serialized default");
            table.insert(field, default_to_toml_value(default));
            repairs.push(format!("{prefix}{field}"));
        }
    }
}

fn repair_toml(document: &mut DocumentMut) -> Vec<String> {
    let config_defaults = serialized_defaults(Config::default());
    let blob_defaults = serialized_defaults(BlobConfig::default());
    let mut repairs = Vec::new();

    repair_toml_fields(
        document.as_table_mut(),
        &config_defaults,
        REQUIRED_TOP_LEVEL,
        "",
        &mut repairs,
    );
    if let Some(table) = document
        .get_mut("blob_config")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        repair_toml_fields(
            table,
            &blob_defaults,
            REQUIRED_BLOB_CONFIG,
            "blob_config.",
            &mut repairs,
        );
    }

    for &(legacy_key, current_key) in LEGACY_KEY_MIGRATIONS {
        let _ = (legacy_key, current_key);
    }

    repairs
}

fn repair_json_fields(
    object: &mut serde_json::Map<String, Value>,
    defaults: &serde_json::Map<String, Value>,
    fields: &[&str],
    prefix: &str,
    repairs: &mut Vec<String>,
) {
    for &field in fields {
        if !object.contains_key(field) {
            object.insert(
                field.to_string(),
                defaults
                    .get(field)
                    .expect("required repair field must have a serialized default")
                    .clone(),
            );
            repairs.push(format!("{prefix}{field}"));
        }
    }
}

fn repair_json(value: &mut Value) -> Vec<String> {
    let config_defaults = serialized_defaults(Config::default());
    let blob_defaults = serialized_defaults(BlobConfig::default());
    let Some(object) = value.as_object_mut() else {
        return Vec::new();
    };
    let mut repairs = Vec::new();

    repair_json_fields(
        object,
        &config_defaults,
        REQUIRED_TOP_LEVEL,
        "",
        &mut repairs,
    );
    if let Some(blob_object) = object.get_mut("blob_config").and_then(Value::as_object_mut) {
        repair_json_fields(
            blob_object,
            &blob_defaults,
            REQUIRED_BLOB_CONFIG,
            "blob_config.",
            &mut repairs,
        );
    }

    for &(legacy_key, current_key) in LEGACY_KEY_MIGRATIONS {
        let _ = (legacy_key, current_key);
    }

    repairs
}

fn toml_field_at(text: &str, byte_offset: usize) -> Option<&str> {
    let line_start = text[..byte_offset.min(text.len())]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let line = text[line_start..].split('\n').next()?.trim();
    let field = line.split_once('=')?.0.trim();
    (!field.is_empty()
        && field.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        }))
    .then_some(field)
}

fn expected_type(message: &str) -> &'static str {
    if message.contains("expected a boolean") {
        "boolean"
    } else if message.contains("expected a string") {
        "string"
    } else if message.contains("expected an integer") {
        "integer"
    } else if message.contains("expected a positive integer") {
        "positive integer"
    } else if message.contains("expected a sequence") {
        "sequence"
    } else if message.contains("expected a map") {
        "map"
    } else {
        "valid configuration value"
    }
}

fn toml_schema_diagnostic(text: &str, error: &toml::de::Error) -> String {
    let span = error.span();
    let field = span
        .as_ref()
        .and_then(|span| toml_field_at(text, span.start))
        .unwrap_or("configuration field");
    let location = span
        .map(|span| format!(" at byte {}", span.start))
        .unwrap_or_default();
    format!(
        "TOML schema error for '{field}'{location}: expected {}.",
        expected_type(error.message())
    )
}

fn json_byte_offset(text: &str, line: usize, column: usize) -> Option<usize> {
    if line == 0 || column == 0 {
        return None;
    }

    let line_start = if line == 1 {
        0
    } else {
        text.match_indices('\n').nth(line - 2)?.0 + 1
    };
    let line_end = text[line_start..]
        .find('\n')
        .map(|index| line_start + index)
        .unwrap_or(text.len());
    let line = &text[line_start..line_end];
    let column_offset = line
        .char_indices()
        .nth(column - 1)
        .map(|(index, _)| index)
        .unwrap_or(line.len());
    Some(line_start + column_offset)
}

fn json_field_at(text: &str, byte_offset: usize) -> Option<&str> {
    let mut in_string = false;
    let mut escaped = false;
    let mut last_structural_colon = None;

    for (index, character) in text.char_indices() {
        if index >= byte_offset {
            break;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
        } else if character == ':' {
            last_structural_colon = Some(index);
        }
    }

    let prefix = text[..last_structural_colon?].trim_end();
    let key_without_closing_quote = prefix.strip_suffix('"')?;
    let key_start = key_without_closing_quote.rfind('"')?;
    let key = &key_without_closing_quote[key_start + 1..];
    (key.chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-'))
    .then_some(key)
}

fn json_schema_diagnostic(
    text: &str,
    error: &serde_json::Error,
    preserve_source_location: bool,
) -> String {
    let byte_offset = json_byte_offset(text, error.line(), error.column());
    let location = if preserve_source_location && byte_offset.is_some() {
        format!(" at line {} column {}", error.line(), error.column())
    } else {
        String::new()
    };
    let field = byte_offset
        .and_then(|byte_offset| json_field_at(text, byte_offset))
        .unwrap_or("configuration field");
    format!(
        "JSON schema error for '{field}'{location}: expected {}.",
        expected_type(&error.to_string())
    )
}

fn backup_path(path: &Path, now: DateTime<Utc>) -> PathBuf {
    let file_name = path
        .file_name()
        .expect("configuration path must name a file")
        .to_string_lossy();
    path.with_file_name(format!(
        "{file_name}.backup-{}",
        now.format("%Y%m%dT%H%M%SZ")
    ))
}

async fn persist_repair(
    path: &Path,
    original: &[u8],
    repaired: &[u8],
    now: DateTime<Utc>,
) -> Result<(PathBuf, Vec<u8>)> {
    let backup = backup_path(path, now);
    let backup_name = backup.file_name().ok_or_else(|| {
        CrosstacheError::invalid_argument("Configuration backup path must name a file")
    })?;
    let verified =
        atomic_replace_with_private_backup_no_follow_async(path, backup_name, original, repaired)
            .await?;
    Ok((backup, verified))
}

pub async fn diagnose_and_repair(path: &Path) -> Result<DoctorReport> {
    diagnose_and_repair_with(path, apply_environment_overrides).await
}

async fn diagnose_and_repair_with(
    path: &Path,
    apply_overrides: impl FnOnce(&mut Config),
) -> Result<DoctorReport> {
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

    let (repairs, candidate, schema_error) = match parsed {
        ParsedConfig::Toml(mut document) => {
            let repairs = repair_toml(&mut document);
            let candidate = document.to_string();
            let error = toml::from_str::<Config>(&candidate)
                .err()
                .map(|error| toml_schema_diagnostic(&candidate, &error));
            (repairs, candidate, error)
        }
        ParsedConfig::Json(mut value) => {
            let repairs = repair_json(&mut value);
            let preserve_source_location = repairs.is_empty();
            let candidate = if preserve_source_location {
                text.to_string()
            } else {
                serde_json::to_string(&value).expect("repaired JSON configuration must serialize")
            };
            let error = serde_json::from_str::<Config>(&candidate)
                .err()
                .map(|error| json_schema_diagnostic(&candidate, &error, preserve_source_location));
            (repairs, candidate, error)
        }
    };
    let mut report = report;
    report.repairs = repairs;

    match schema_error {
        None => {
            let persisted = if !report.repairs.is_empty() {
                let (backup, written) =
                    persist_repair(path, &original, candidate.as_bytes(), Utc::now()).await?;
                let written = std::str::from_utf8(&written).map_err(|error| {
                    CrosstacheError::config(format!(
                        "Repaired configuration file '{}' is not UTF-8: {error}",
                        path.display()
                    ))
                })?;
                deserialize_verified_config(path, written)?;
                report.backup_path = Some(backup);
                for repair in &report.repairs {
                    report.checks.push(DoctorCheck {
                        status: DoctorCheckStatus::Fixed,
                        message: format!("Restored missing configuration field '{repair}'."),
                    });
                }
                written.as_bytes().to_vec()
            } else {
                original
            };
            let persisted = std::str::from_utf8(&persisted).map_err(|error| {
                CrosstacheError::config(format!(
                    "Configuration file '{}' is not UTF-8 after repair: {error}",
                    path.display()
                ))
            })?;
            let mut effective = deserialize_verified_config(path, persisted)?;
            apply_overrides(&mut effective);
            if effective.validate().is_err() {
                add_semantic_unresolved(&mut report, &effective);
            }

            let status = if report.is_healthy() {
                DoctorCheckStatus::Ok
            } else {
                DoctorCheckStatus::Error
            };
            let message = if report.is_healthy() {
                format!("Configuration file '{}' is valid.", path.display())
            } else {
                format!(
                    "Configuration file '{}' requires manual configuration.",
                    path.display()
                )
            };
            Ok(report.with_check(status, message))
        }
        Some(error) => Ok(report.with_unresolved(error)),
    }
}

fn deserialize_verified_config(path: &Path, text: &str) -> Result<Config> {
    toml::from_str::<Config>(text)
        .or_else(|_| serde_json::from_str::<Config>(text))
        .map_err(|_| {
            CrosstacheError::config(format!(
                "Configuration file '{}' could not be verified after repair",
                path.display()
            ))
        })
}

fn add_semantic_unresolved(report: &mut DoctorReport, config: &Config) {
    match config.effective_backend_name() {
        "azure" => {
            if config.subscription_id.is_empty() {
                let message =
                    "Azure subscription_id is required. Run `xv config set subscription_id <id>`.";
                report.checks.push(DoctorCheck {
                    status: DoctorCheckStatus::Error,
                    message: message.to_string(),
                });
                report.unresolved.push(message.to_string());
            }
            if config.tenant_id.is_empty() {
                let message = "Azure tenant_id is required. Run `xv config set tenant_id <id>`.";
                report.checks.push(DoctorCheck {
                    status: DoctorCheckStatus::Error,
                    message: message.to_string(),
                });
                report.unresolved.push(message.to_string());
            }
        }
        "aws" => match &config.aws {
            None => {
                let message = "AWS backend requires an [aws] block. Add [aws] settings and run `xv doctor` again.";
                report.checks.push(DoctorCheck {
                    status: DoctorCheckStatus::Error,
                    message: message.to_string(),
                });
                report.unresolved.push(message.to_string());
            }
            Some(aws) if aws.region.is_none() => {
                let message = "AWS region is required. Set `[aws].region` or `AWS_REGION`, then run `xv doctor` again.";
                report.checks.push(DoctorCheck {
                    status: DoctorCheckStatus::Error,
                    message: message.to_string(),
                });
                report.unresolved.push(message.to_string());
            }
            Some(_) => {}
        },
        _ => {
            let message = "Configuration validation failed. Review the selected backend settings and run `xv doctor` again.";
            report.checks.push(DoctorCheck {
                status: DoctorCheckStatus::Error,
                message: message.to_string(),
            });
            report.unresolved.push(message.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::config::{settings::apply_environment_overrides_with, Config};
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::{
        backup_path, diagnose_and_repair, diagnose_and_repair_with, persist_repair, repair_json,
        repair_toml, DoctorReport,
    };

    const SPARSE_LOCAL: &str = r#"# keep this comment
backend = "local"
future_setting = "preserve-me"

[local]
default_vault = "default"
"#;

    const REQUIRED_TOP_LEVEL: &[&str] = &[
        "debug",
        "subscription_id",
        "default_vault",
        "default_resource_group",
        "default_location",
        "tenant_id",
        "output_json",
        "no_color",
    ];

    fn repair_names(fields: &[&str]) -> Vec<String> {
        fields.iter().map(|field| (*field).to_string()).collect()
    }

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
    async fn sparse_toml_is_backed_up_and_atomically_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let original_bytes = SPARSE_LOCAL.as_bytes();
        std::fs::write(&path, original_bytes).unwrap();

        let report = diagnose_and_repair(&path).await.unwrap();

        let backup = report.backup_path.as_ref().unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), original_bytes);
        assert!(backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("xv.conf.backup-"));
        let repaired = std::fs::read_to_string(&path).unwrap();
        toml::from_str::<Config>(&repaired).unwrap();
        assert!(report.is_healthy());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn existing_fixed_timestamp_backup_is_not_overwritten_or_followed_by_config_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let original = b"original config bytes";
        let repaired = b"repaired config bytes";
        let now = Utc.with_ymd_and_hms(2026, 8, 7, 21, 15, 30).unwrap();
        let backup = backup_path(&path, now);
        assert_eq!(backup, dir.path().join("xv.conf.backup-20260807T211530Z"));
        std::fs::write(&path, original).unwrap();
        std::fs::write(&backup, b"existing backup").unwrap();

        let error = persist_repair(&path, original, repaired, now)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("backup"));
        assert_eq!(std::fs::read(&backup).unwrap(), b"existing backup");
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn semantic_azure_missing_ids_reports_manual_actions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let original = toml::to_string_pretty(&Config::default()).unwrap();
        std::fs::write(&path, &original).unwrap();

        let report = diagnose_and_repair_with(&path, |_| {}).await.unwrap();
        let unresolved = report.unresolved.join("\n");

        assert!(!report.is_healthy());
        assert!(unresolved.contains("subscription_id"));
        assert!(unresolved.contains("xv config set subscription_id <id>"));
        assert!(unresolved.contains("tenant_id"));
        assert!(unresolved.contains("xv config set tenant_id <id>"));
        assert!(report.backup_path.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn semantic_environment_ids_validate_without_being_persisted() {
        const SUBSCRIPTION: &str = "doctor-environment-subscription";
        const TENANT: &str = "doctor-environment-tenant";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let original = toml::to_string_pretty(&Config::default()).unwrap();
        std::fs::write(&path, &original).unwrap();
        let values = HashMap::from([
            (
                "AZURE_SUBSCRIPTION_ID".to_string(),
                SUBSCRIPTION.to_string(),
            ),
            ("AZURE_TENANT_ID".to_string(), TENANT.to_string()),
        ]);

        let report = diagnose_and_repair_with(&path, |effective| {
            apply_environment_overrides_with(effective, |name| values.get(name).cloned());
        })
        .await
        .unwrap();
        let persisted = std::fs::read_to_string(&path).unwrap();

        assert!(report.is_healthy());
        assert!(report.backup_path.is_none());
        assert_eq!(persisted, original);
        assert!(!persisted.contains(SUBSCRIPTION));
        assert!(!persisted.contains(TENANT));
    }

    #[tokio::test]
    async fn semantic_aws_without_block_is_unresolved_and_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let config = Config {
            backend: Some("aws".into()),
            ..Config::default()
        };
        let original = toml::to_string_pretty(&config).unwrap();
        std::fs::write(&path, &original).unwrap();

        let report = diagnose_and_repair_with(&path, |_| {}).await.unwrap();
        let persisted = std::fs::read_to_string(&path).unwrap();

        assert!(!report.is_healthy());
        assert!(report.unresolved.join("\n").contains("[aws]"));
        assert!(report.backup_path.is_none());
        assert_eq!(persisted, original);
        assert!(!persisted.contains("[aws]"));
    }

    #[tokio::test]
    async fn aws_remediation_names_top_level_doctor_command() {
        let dir = tempfile::tempdir().unwrap();
        let missing_block_path = dir.path().join("missing-block.conf");
        let missing_block = Config {
            backend: Some("aws".into()),
            ..Config::default()
        };
        std::fs::write(
            &missing_block_path,
            toml::to_string_pretty(&missing_block).unwrap(),
        )
        .unwrap();

        let missing_block_report = diagnose_and_repair_with(&missing_block_path, |_| {})
            .await
            .unwrap();

        let missing_region_path = dir.path().join("missing-region.conf");
        let missing_region = Config {
            backend: Some("aws".into()),
            aws: Some(crate::config::settings::AwsConfig::default()),
            ..Config::default()
        };
        std::fs::write(
            &missing_region_path,
            toml::to_string_pretty(&missing_region).unwrap(),
        )
        .unwrap();

        let missing_region_report = diagnose_and_repair_with(&missing_region_path, |_| {})
            .await
            .unwrap();

        for report in [missing_block_report, missing_region_report] {
            let guidance = report.unresolved.join("\n");
            assert!(guidance.contains("xv doctor"), "guidance: {guidance}");
            assert!(
                !guidance.contains("xv config doctor"),
                "guidance: {guidance}"
            );
        }
    }

    #[tokio::test]
    async fn semantic_deterministic_repairs_persist_despite_missing_azure_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let original = b"# sparse Azure config\n";
        std::fs::write(&path, original).unwrap();

        let report = diagnose_and_repair_with(&path, |_| {}).await.unwrap();
        let persisted = std::fs::read_to_string(&path).unwrap();

        assert!(!report.is_healthy());
        assert!(!report.repairs.is_empty());
        assert!(report.backup_path.is_some());
        assert_eq!(
            std::fs::read(report.backup_path.as_ref().unwrap()).unwrap(),
            original
        );
        toml::from_str::<Config>(&persisted).unwrap();
        assert!(report.unresolved.join("\n").contains("subscription_id"));
    }

    #[test]
    fn repairs_sparse_toml_without_replacing_unknown_or_local_values() {
        let mut document = SPARSE_LOCAL.parse().unwrap();

        let repairs = repair_toml(&mut document);
        let repaired = document.to_string();
        let config: Config = toml::from_str(&repaired).unwrap();

        assert_eq!(repairs, repair_names(REQUIRED_TOP_LEVEL));
        assert!(repaired.contains("# keep this comment"));
        assert!(repaired.contains("future_setting = \"preserve-me\""));
        assert!(repaired.contains("backend = \"local\""));
        assert!(repaired.contains("[local]"));
        assert!(repaired.contains("default_vault = \"default\""));
        assert!(!config.debug);
        assert!(config.subscription_id.is_empty());
        assert!(config.default_vault.is_empty());
        assert_eq!(config.default_resource_group, "Vaults");
        assert_eq!(config.default_location, "eastus");
        assert!(config.tenant_id.is_empty());
        assert!(!config.output_json);
        assert!(!config.no_color);
    }

    #[test]
    fn repairs_missing_blob_defaults_without_replacing_existing_values() {
        let mut document = r#"backend = "local"
debug = false
subscription_id = ""
default_vault = ""
default_resource_group = "Vaults"
default_location = "eastus"
tenant_id = ""
output_json = false
no_color = false

[blob_config]
endpoint = "https://existing.example"
progress_threshold_mb = 7
"#
        .parse()
        .unwrap();

        let repairs = repair_toml(&mut document);
        let repaired = document.to_string();
        let config: Config = toml::from_str(&repaired).unwrap();
        let blob = config.blob_config.unwrap();

        assert_eq!(
            repairs,
            repair_names(&[
                "blob_config.storage_account",
                "blob_config.container_name",
                "blob_config.enable_large_file_support",
                "blob_config.chunk_size_mb",
                "blob_config.max_concurrent_uploads",
            ])
        );
        assert_eq!(blob.storage_account, "");
        assert_eq!(blob.container_name, "crosstache-files");
        assert_eq!(blob.endpoint.as_deref(), Some("https://existing.example"));
        assert!(blob.enable_large_file_support);
        assert_eq!(blob.chunk_size_mb, 4);
        assert_eq!(blob.max_concurrent_uploads, 3);
        assert_eq!(blob.progress_threshold_mb, 7);
    }

    #[test]
    fn repairs_missing_inline_blob_defaults_without_replacing_existing_values() {
        let mut document = r#"backend = "local"
debug = false
subscription_id = ""
default_vault = ""
default_resource_group = "Vaults"
default_location = "eastus"
tenant_id = ""
output_json = false
no_color = false
blob_config = { endpoint = "https://existing.example", progress_threshold_mb = 7 }
"#
        .parse()
        .unwrap();

        let repairs = repair_toml(&mut document);
        let repaired = document.to_string();
        let config: Config = toml::from_str(&repaired).unwrap();
        let blob = config.blob_config.unwrap();

        assert_eq!(
            repairs,
            repair_names(&[
                "blob_config.storage_account",
                "blob_config.container_name",
                "blob_config.enable_large_file_support",
                "blob_config.chunk_size_mb",
                "blob_config.max_concurrent_uploads",
            ])
        );
        assert!(document["blob_config"].is_inline_table());
        assert_eq!(blob.endpoint.as_deref(), Some("https://existing.example"));
        assert_eq!(blob.progress_threshold_mb, 7);
        assert_eq!(blob.storage_account, "");
        assert_eq!(blob.container_name, "crosstache-files");
        assert!(blob.enable_large_file_support);
        assert_eq!(blob.chunk_size_mb, 4);
        assert_eq!(blob.max_concurrent_uploads, 3);
    }

    #[test]
    fn repairs_sparse_json_without_replacing_unknown_or_local_values() {
        let mut value = json!({
            "backend": "local",
            "future_setting": "preserve-me",
            "local": { "default_vault": "default" },
        });

        let repairs = repair_json(&mut value);
        let config: Config = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(repairs, repair_names(REQUIRED_TOP_LEVEL));
        assert_eq!(value["future_setting"], "preserve-me");
        assert_eq!(value["backend"], "local");
        assert_eq!(value["local"]["default_vault"], "default");
        assert!(!config.debug);
        assert!(config.subscription_id.is_empty());
        assert!(config.default_vault.is_empty());
        assert_eq!(config.default_resource_group, "Vaults");
        assert_eq!(config.default_location, "eastus");
        assert!(config.tenant_id.is_empty());
        assert!(!config.output_json);
        assert!(!config.no_color);
    }

    #[tokio::test]
    async fn invalid_occupied_values_remain_unresolved_without_disclosing_values() {
        const FUTURE_SENTINEL: &str = "doctor-private-future-setting";
        const CREDENTIAL_SENTINEL: &str = "doctor-private-subscription-id";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let original = format!(
            r#"backend = "local"
debug = "false"
subscription_id = "{CREDENTIAL_SENTINEL}"
default_vault = ""
default_resource_group = "Vaults"
default_location = "eastus"
tenant_id = "tenant-id"
output_json = false
no_color = false
future_setting = "{FUTURE_SENTINEL}"
"#
        );
        std::fs::write(&path, &original).unwrap();

        let report = diagnose_and_repair(&path).await.unwrap();

        assert!(!report.is_healthy());
        assert!(report.repairs.is_empty());
        assert!(report.backup_path.is_none());
        assert!(report.unresolved.join("\n").contains("debug"));
        assert!(report.unresolved.join("\n").contains("boolean"));
        assert_diagnostics_exclude(&report, FUTURE_SENTINEL);
        assert_diagnostics_exclude(&report, CREDENTIAL_SENTINEL);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn invalid_json_occupied_value_identifies_field_without_disclosing_values() {
        const DEBUG_SENTINEL: &str = "doctor-private-json-debug-value";
        const FUTURE_SENTINEL: &str = "doctor-private-json-future-setting";
        const CREDENTIAL_SENTINEL: &str = "doctor-private-json-subscription-id";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let original = format!(
            r#"{{
  "backend": "local",
  "debug": "{DEBUG_SENTINEL}",
  "subscription_id": "{CREDENTIAL_SENTINEL}",
  "default_vault": "",
  "default_resource_group": "Vaults",
  "default_location": "eastus",
  "tenant_id": "tenant-id",
  "output_json": false,
  "no_color": false,
  "future_setting": "{FUTURE_SENTINEL}"
}}"#
        );
        std::fs::write(&path, &original).unwrap();

        let report = diagnose_and_repair(&path).await.unwrap();
        let diagnostics = report.unresolved.join("\n");

        assert!(!report.is_healthy());
        assert!(report.repairs.is_empty());
        assert!(report.backup_path.is_none());
        assert!(diagnostics.contains("debug"));
        assert!(diagnostics.contains("boolean"));
        assert!(diagnostics.contains("line 3 column"));
        assert_diagnostics_exclude(&report, DEBUG_SENTINEL);
        assert_diagnostics_exclude(&report, FUTURE_SENTINEL);
        assert_diagnostics_exclude(&report, CREDENTIAL_SENTINEL);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
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
