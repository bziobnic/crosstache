use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::{DocumentMut, Table};

use crate::{
    config::{BlobConfig, Config},
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
    table: &mut Table,
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
        .and_then(toml_edit::Item::as_table_mut)
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

fn json_schema_diagnostic(error: &serde_json::Error) -> String {
    format!(
        "JSON schema error: expected {}.",
        expected_type(&error.to_string())
    )
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

    let (repairs, schema_error) = match parsed {
        ParsedConfig::Toml(mut document) => {
            let repairs = repair_toml(&mut document);
            let candidate = document.to_string();
            let error = toml::from_str::<Config>(&candidate)
                .err()
                .map(|error| toml_schema_diagnostic(&candidate, &error));
            (repairs, error)
        }
        ParsedConfig::Json(mut value) => {
            let repairs = repair_json(&mut value);
            let error = serde_json::from_value::<Config>(value)
                .err()
                .map(|error| json_schema_diagnostic(&error));
            (repairs, error)
        }
    };
    let mut report = report;
    report.repairs = repairs;

    match schema_error {
        None => Ok(report.with_check(
            DoctorCheckStatus::Ok,
            format!("Configuration file '{}' is valid.", path.display()),
        )),
        Some(error) => Ok(report.with_unresolved(error)),
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use serde_json::json;

    use super::{diagnose_and_repair, repair_json, repair_toml, DoctorReport};

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
