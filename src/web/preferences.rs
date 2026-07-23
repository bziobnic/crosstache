use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CrosstacheError, Result};

use super::api::ApiError;
use super::WebState;

const UI_PREFERENCES_VERSION: u64 = 1;

fn current_version() -> u64 {
    UI_PREFERENCES_VERSION
}

fn default_theme() -> String {
    "system".to_string()
}

fn default_exposure_timeout_seconds() -> u64 {
    30
}

fn default_density() -> String {
    "comfortable".to_string()
}

fn default_folder_expansion() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ColumnWidthsV1 {
    pub(crate) secrets: Vec<f64>,
    pub(crate) files: Vec<f64>,
}

impl Default for ColumnWidthsV1 {
    fn default() -> Self {
        Self {
            secrets: vec![28.0, 15.0, 14.0, 25.0, 18.0],
            files: vec![42.0, 12.0, 24.0, 22.0],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UiPreferencesV1 {
    #[serde(default = "current_version")]
    pub(crate) version: u64,
    #[serde(default = "default_theme")]
    pub(crate) theme: String,
    #[serde(default = "default_exposure_timeout_seconds")]
    pub(crate) exposure_timeout_seconds: u64,
    #[serde(default = "default_density")]
    pub(crate) density: String,
    #[serde(default = "default_folder_expansion")]
    pub(crate) folder_expansion: bool,
    pub(crate) column_widths: ColumnWidthsV1,
}

impl Default for UiPreferencesV1 {
    fn default() -> Self {
        Self {
            version: UI_PREFERENCES_VERSION,
            theme: default_theme(),
            exposure_timeout_seconds: default_exposure_timeout_seconds(),
            density: default_density(),
            folder_expansion: default_folder_expansion(),
            column_widths: ColumnWidthsV1::default(),
        }
    }
}

impl UiPreferencesV1 {
    pub(crate) fn from_json(value: Value) -> Result<Self> {
        reject_vault_data_keys(&value)?;

        let object = value.as_object().ok_or_else(|| {
            CrosstacheError::invalid_argument("UI preferences must be a JSON object")
        })?;
        let version = object.get("version").and_then(Value::as_u64).unwrap_or(0);
        if version > UI_PREFERENCES_VERSION {
            return Err(CrosstacheError::invalid_argument(format!(
                "Unsupported UI preference version {version}"
            )));
        }

        let mut preferences: Self = serde_json::from_value(value)?;
        preferences.version = UI_PREFERENCES_VERSION;
        preferences.validate()?;
        Ok(preferences)
    }

    fn validate(&self) -> Result<()> {
        if !matches!(self.theme.as_str(), "system" | "light" | "dark") {
            return Err(CrosstacheError::invalid_argument(
                "UI preference 'theme' must be system, light, or dark",
            ));
        }
        if !matches!(self.density.as_str(), "comfortable" | "compact") {
            return Err(CrosstacheError::invalid_argument(
                "UI preference 'density' must be comfortable or compact",
            ));
        }
        validate_widths("secrets", &self.column_widths.secrets, 5)?;
        validate_widths("files", &self.column_widths.files, 4)?;
        Ok(())
    }
}

fn validate_widths(name: &str, widths: &[f64], expected: usize) -> Result<()> {
    if widths.len() != expected
        || widths
            .iter()
            .any(|width| !width.is_finite() || *width <= 0.0)
    {
        return Err(CrosstacheError::invalid_argument(format!(
            "UI preference column widths for {name} are invalid"
        )));
    }
    Ok(())
}

fn reject_vault_data_keys(value: &Value) -> Result<()> {
    reject_vault_data_keys_in_context(value, false, 0)
}

fn reject_vault_data_keys_in_context(
    value: &Value,
    sensitive_context: bool,
    depth: usize,
) -> Result<()> {
    const EXPLICIT_FORBIDDEN: &[&str] = &[
        "secret",
        "secretname",
        "secretnames",
        "secretnote",
        "secretnotes",
        "secretvalue",
        "secretvalues",
        "searchquery",
        "searchhistory",
        "clipboard",
        "clipboardcontent",
        "clipboardcontents",
        "credential",
        "credentials",
        "credentialtoken",
        "credentialstoken",
        "password",
        "accesstoken",
        "authtoken",
        "apikey",
        "vaultdata",
    ];
    const AMBIGUOUS_LEAVES: &[&str] = &["value", "note", "notes", "query", "token", "tokens"];
    const SENSITIVE_CONTEXTS: &[&str] = &[
        "auth",
        "authentication",
        "clipboard",
        "credential",
        "credentials",
        "record",
        "search",
        "secret",
        "secrets",
        "session",
        "vault",
    ];

    match value {
        Value::Object(object) => {
            for (key, nested) in object {
                let normalized = canonical_key(key);
                let ambiguous_sensitive = AMBIGUOUS_LEAVES.contains(&normalized.as_str())
                    && (depth == 0 || sensitive_context);
                if EXPLICIT_FORBIDDEN.contains(&normalized.as_str()) || ambiguous_sensitive {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "UI preferences cannot contain vault data key '{key}'"
                    )));
                }
                let nested_sensitive =
                    sensitive_context || SENSITIVE_CONTEXTS.contains(&normalized.as_str());
                reject_vault_data_keys_in_context(nested, nested_sensitive, depth + 1)?;
            }
        }
        Value::Array(values) => {
            for nested in values {
                reject_vault_data_keys_in_context(nested, sensitive_context, depth)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn canonical_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn preference_path_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name("ui.json")
}

#[derive(Debug, Clone)]
pub(crate) struct PreferenceStore {
    path: PathBuf,
    clipboard_timeout: u64,
}

impl PreferenceStore {
    pub(crate) fn new(path: PathBuf, clipboard_timeout: u64) -> Self {
        Self {
            path,
            clipboard_timeout,
        }
    }

    pub(crate) async fn load(&self) -> Result<UiPreferencesV1> {
        let mut preferences = match tokio::fs::read(&self.path).await {
            Ok(bytes) => self.parse_json(serde_json::from_slice(&bytes)?)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.defaults(),
            Err(error) => return Err(error.into()),
        };
        self.clamp_exposure_timeout(&mut preferences);
        Ok(preferences)
    }

    pub(crate) async fn save_json(&self, value: Value) -> Result<UiPreferencesV1> {
        let mut preferences = self.parse_json(value)?;
        self.clamp_exposure_timeout(&mut preferences);
        let mut bytes = serde_json::to_vec_pretty(&preferences)?;
        bytes.push(b'\n');
        crate::utils::helpers::atomic_write_file_no_follow_async(&self.path, &bytes, true).await?;
        Ok(preferences)
    }

    fn clamp_exposure_timeout(&self, preferences: &mut UiPreferencesV1) {
        if self.clipboard_timeout != 0 {
            preferences.exposure_timeout_seconds = preferences
                .exposure_timeout_seconds
                .min(self.clipboard_timeout);
        }
    }

    fn default_exposure_timeout(&self) -> u64 {
        if self.clipboard_timeout == 0 {
            default_exposure_timeout_seconds()
        } else {
            self.clipboard_timeout
        }
    }

    fn defaults(&self) -> UiPreferencesV1 {
        UiPreferencesV1 {
            exposure_timeout_seconds: self.default_exposure_timeout(),
            ..UiPreferencesV1::default()
        }
    }

    fn parse_json(&self, value: Value) -> Result<UiPreferencesV1> {
        let has_exposure_timeout = value
            .as_object()
            .is_some_and(|object| object.contains_key("exposure_timeout_seconds"));
        let mut preferences = UiPreferencesV1::from_json(value)?;
        if !has_exposure_timeout {
            preferences.exposure_timeout_seconds = self.default_exposure_timeout();
        }
        Ok(preferences)
    }
}

pub(crate) async fn get_preferences(
    State(state): State<Arc<WebState>>,
) -> std::result::Result<Json<UiPreferencesV1>, ApiError> {
    Ok(Json(state.preferences.load().await?))
}

pub(crate) async fn put_preferences(
    State(state): State<Arc<WebState>>,
    Json(value): Json<Value>,
) -> std::result::Result<Json<UiPreferencesV1>, ApiError> {
    Ok(Json(state.preferences.save_json(value).await?))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use axum::http::StatusCode;
    use serde_json::json;

    use crate::web::api::tests::get_json;
    use crate::web::testutil;

    use super::{preference_path_for, PreferenceStore, UiPreferencesV1};

    #[test]
    fn preferences_reject_vault_data_keys() {
        let json = json!({"version": 1, "theme": "dark", "secret_name": "DB_URL"});
        assert!(UiPreferencesV1::from_json(json).is_err());

        let nested = json!({
            "version": 1,
            "theme": "dark",
            "future_presentation": {"secret_value": "do-not-store"}
        });
        assert!(UiPreferencesV1::from_json(nested).is_err());
    }

    #[test]
    fn preferences_reject_known_vault_data_key_aliases() {
        for key in [
            "secret.name",
            "secretValue",
            "secret-note",
            "search.query",
            "search_history",
            "clipboard.contents",
            "clipboard_content",
            "credentials.token",
            "api_key",
        ] {
            assert!(
                UiPreferencesV1::from_json(json!({"version": 1, (key): "sensitive"})).is_err(),
                "accepted forbidden preference key {key}"
            );
        }
    }

    #[test]
    fn ambiguous_future_presentation_leaf_keys_are_ignored_outside_sensitive_contexts() {
        let preferences = UiPreferencesV1::from_json(json!({
            "version": 1,
            "design": {
                "value": "compact",
                "note": "future design metadata",
                "token": "spacing-unit",
                "tokens": {"surface": "forest"}
            }
        }))
        .unwrap();

        let canonical = serde_json::to_value(preferences).unwrap();
        assert_eq!(canonical.as_object().unwrap().len(), 6);
        assert!(canonical.get("design").is_none());
    }

    #[test]
    fn ambiguous_leaf_keys_are_rejected_in_sensitive_contexts() {
        for value in [
            json!({"version": 1, "auth": {"token": "bearer"}}),
            json!({"version": 1, "search": {"query": "DB_URL"}}),
            json!({"version": 1, "record": {"note": "secret note"}}),
        ] {
            assert!(UiPreferencesV1::from_json(value).is_err());
        }
    }

    #[test]
    fn preference_path_is_ui_json_next_to_config() {
        assert_eq!(
            preference_path_for(Path::new("/tmp/xv/xv.conf")),
            PathBuf::from("/tmp/xv/ui.json")
        );
    }

    #[test]
    fn defaults_are_safe_and_versioned() {
        let preferences = UiPreferencesV1::default();
        assert_eq!(preferences.version, 1);
        assert_eq!(preferences.theme, "system");
        assert_eq!(preferences.exposure_timeout_seconds, 30);
        assert_eq!(preferences.density, "comfortable");
        assert!(preferences.folder_expansion);
        assert_eq!(
            preferences.column_widths.secrets,
            vec![28.0, 15.0, 14.0, 25.0, 18.0]
        );
        assert_eq!(
            preferences.column_widths.files,
            vec![42.0, 12.0, 24.0, 22.0]
        );
    }

    #[test]
    fn unversioned_preferences_migrate_and_unknown_presentation_fields_are_ignored() {
        let preferences = UiPreferencesV1::from_json(json!({
            "theme": "dark",
            "density": "compact",
            "future_presentation": {"accent": "green"}
        }))
        .unwrap();

        assert_eq!(preferences.version, 1);
        assert_eq!(preferences.theme, "dark");
        assert_eq!(preferences.density, "compact");
        assert_eq!(preferences.exposure_timeout_seconds, 30);
    }

    #[test]
    fn unsupported_versions_are_rejected_without_guessing_at_migration() {
        assert!(UiPreferencesV1::from_json(json!({"version": 2})).is_err());
    }

    #[tokio::test]
    async fn missing_file_uses_defaults_clamped_to_nonzero_security_policy() {
        let temp = tempfile::tempdir().unwrap();
        let store = PreferenceStore::new(temp.path().join("ui.json"), 12);

        let preferences = store.load().await.unwrap();

        assert_eq!(
            preferences,
            UiPreferencesV1 {
                exposure_timeout_seconds: 12,
                ..UiPreferencesV1::default()
            }
        );
        assert!(!temp.path().join("ui.json").exists());
    }

    #[tokio::test]
    async fn missing_timeout_uses_nonzero_policy_or_30_second_fallback() {
        for (policy, expected) in [(12, 12), (30, 30), (60, 60), (0, 30)] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("ui.json");
            let store = PreferenceStore::new(path.clone(), policy);
            assert_eq!(
                store.load().await.unwrap().exposure_timeout_seconds,
                expected
            );

            std::fs::write(&path, br#"{"version":1,"theme":"dark"}"#).unwrap();
            assert_eq!(
                store.load().await.unwrap().exposure_timeout_seconds,
                expected
            );
        }
    }

    #[tokio::test]
    async fn zero_security_policy_does_not_clamp_user_exposure_choice() {
        let temp = tempfile::tempdir().unwrap();
        let store = PreferenceStore::new(temp.path().join("ui.json"), 0);

        let saved = store
            .save_json(json!({
                "version": 1,
                "exposure_timeout_seconds": 75
            }))
            .await
            .unwrap();

        assert_eq!(saved.exposure_timeout_seconds, 75);
        assert_eq!(store.load().await.unwrap().exposure_timeout_seconds, 75);
    }

    #[tokio::test]
    async fn storage_migrates_legacy_input_and_persists_only_whitelisted_fields() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ui.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "theme": "dark",
                "future_presentation": {"accent": "green"}
            }))
            .unwrap(),
        )
        .unwrap();
        let store = PreferenceStore::new(path.clone(), 20);
        let migrated = store.load().await.unwrap();
        assert_eq!(migrated.version, 1);
        assert_eq!(migrated.theme, "dark");
        assert_eq!(migrated.exposure_timeout_seconds, 20);

        store
            .save_json(json!({
                "version": 1,
                "theme": "light",
                "exposure_timeout_seconds": 25,
                "future_presentation": {"accent": "green"}
            }))
            .await
            .unwrap();

        let disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(disk["theme"], "light");
        assert_eq!(disk["exposure_timeout_seconds"], 20);
        assert!(disk.get("future_presentation").is_none());
        assert_eq!(disk.as_object().unwrap().len(), 6);
        assert!(std::fs::read_dir(temp.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preference_file_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested").join("ui.json");
        let store = PreferenceStore::new(path.clone(), 30);
        store.save_json(json!({"version": 1})).await.unwrap();

        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn preference_routes_load_defaults_and_persist_clamped_updates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ui.json");
        let state = testutil::test_state_with_preferences(path.clone(), 10);
        let app = crate::web::build_router(state);

        let (status, defaults) = get_json(app.clone(), "GET", "/api/preferences", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(defaults["version"], 1);
        assert_eq!(defaults["theme"], "system");
        assert_eq!(defaults["exposure_timeout_seconds"], 10);

        let (status, saved) = get_json(
            app.clone(),
            "PUT",
            "/api/preferences",
            Some(json!({
                "version": 1,
                "theme": "dark",
                "exposure_timeout_seconds": 60,
                "density": "compact",
                "folder_expansion": false,
                "column_widths": {
                    "secrets": [30, 15, 14, 23, 18],
                    "files": [40, 14, 24, 22]
                },
                "future_presentation": {"accent": "green"}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved["theme"], "dark");
        assert_eq!(saved["exposure_timeout_seconds"], 10);
        assert!(saved.get("future_presentation").is_none());

        let disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(disk, saved);
        let (status, loaded) = get_json(app, "GET", "/api/preferences", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(loaded, saved);
    }

    #[tokio::test]
    async fn preference_route_rejects_vault_data_with_structured_error_and_no_write() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ui.json");
        let app = crate::web::build_router(testutil::test_state_with_preferences(path.clone(), 30));

        let (status, error) = get_json(
            app,
            "PUT",
            "/api/preferences",
            Some(json!({
                "version": 1,
                "theme": "dark",
                "secret_name": "DB_URL"
            })),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "xv-invalid-argument");
        assert!(error["error"]["message"].is_string());
        assert!(error["error"]["hint"].is_string());
        assert!(!path.exists());
    }
}
