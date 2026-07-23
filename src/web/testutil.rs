//! Test-only helpers for the web module.

use std::path::PathBuf;
use std::sync::Arc;

use super::WebState;

pub(crate) fn test_state_with_token(token: &str) -> Arc<WebState> {
    let path = std::env::temp_dir()
        .join(format!("xv-web-test-{}", uuid::Uuid::new_v4()))
        .join("ui.json");
    test_state_with_token_and_preferences(token, path, 30)
}

fn test_state_with_token_and_preferences(
    token: &str,
    path: PathBuf,
    clipboard_timeout: u64,
) -> Arc<WebState> {
    let backend: Arc<dyn crate::backend::Backend> = Arc::new(stub::StubBackend::new());
    let context = test_context(backend.as_ref(), "default", clipboard_timeout);
    Arc::new(WebState {
        backend,
        context,
        token: token.to_string(),
        vault: "default".to_string(),
        types: crate::records::builtin_types(),
        preferences: super::preferences::PreferenceStore::new(path, clipboard_timeout),
    })
}

pub(crate) fn test_context(
    backend: &dyn crate::backend::Backend,
    vault: &str,
    clipboard_timeout: u64,
) -> super::context::EffectiveUiContext {
    use super::context::{
        CapabilitySummary, ConnectionSummary, ContextSource, ContextSources, EffectiveUiContext,
        SecuritySummary, WorkspaceEntrySummary, WorkspaceSummary,
    };

    EffectiveUiContext {
        backend: backend.name().to_string(),
        backend_kind: backend.kind(),
        vault: vault.to_string(),
        workspace: WorkspaceSummary {
            alias: vault.to_string(),
            configured: false,
            entries: vec![WorkspaceEntrySummary {
                alias: vault.to_string(),
                backend: backend.name().to_string(),
                vault: vault.to_string(),
                default: true,
            }],
        },
        project: None,
        environment: None,
        sources: ContextSources {
            backend: ContextSource::BuiltIn,
            vault: ContextSource::BuiltIn,
            workspace: ContextSource::BuiltIn,
            project: ContextSource::BuiltIn,
            environment: ContextSource::BuiltIn,
        },
        connection: ConnectionSummary {
            state: "connected".into(),
            message: None,
        },
        capabilities: CapabilitySummary::from(backend.capabilities()),
        security: SecuritySummary {
            clipboard_timeout_seconds: clipboard_timeout,
        },
        version: env!("CARGO_PKG_VERSION"),
    }
}

pub(crate) fn test_state_with_preferences(path: PathBuf, clipboard_timeout: u64) -> Arc<WebState> {
    test_state_with_token_and_preferences("test-token", path, clipboard_timeout)
}

pub(crate) fn test_state() -> Arc<WebState> {
    test_state_with_token("test-token")
}

pub(crate) mod stub {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use crate::backend::error::BackendError;
    use crate::backend::{Backend, BackendCapabilities, BackendKind, SecretBackend};
    use crate::secret::manager::{
        DeletedSecretSummary, SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
    };

    /// Stored file entry: (content, content_type, metadata).
    #[cfg(feature = "file-ops")]
    type StoredFile = (Vec<u8>, String, HashMap<String, String>);

    pub(crate) struct StubBackend {
        name: &'static str,
        capabilities: BackendCapabilities,
        health_error: Option<&'static str>,
        pub secrets: Mutex<HashMap<String, SecretRequest>>,
        pub deleted: Mutex<HashMap<String, SecretRequest>>,
        #[cfg(feature = "file-ops")]
        pub files: Mutex<HashMap<String, StoredFile>>,
    }

    impl StubBackend {
        pub fn new() -> Self {
            Self::with_capabilities(
                "stub",
                BackendCapabilities {
                    has_folders: true,
                    has_groups: true,
                    has_notes: true,
                    has_expiry: true,
                    has_soft_delete: true,
                    has_restore: true,
                    has_purge: true,
                    has_scheduled_purge: false,
                    #[cfg(feature = "file-ops")]
                    has_file_storage: true,
                    ..Default::default()
                },
            )
        }

        pub(crate) fn with_capabilities(
            name: &'static str,
            capabilities: BackendCapabilities,
        ) -> Self {
            Self {
                name,
                capabilities,
                health_error: None,
                secrets: Mutex::new(HashMap::new()),
                deleted: Mutex::new(HashMap::new()),
                #[cfg(feature = "file-ops")]
                files: Mutex::new(HashMap::new()),
            }
        }

        pub(crate) fn with_health_error(name: &'static str, health_error: &'static str) -> Self {
            let mut backend = Self::with_capabilities(name, BackendCapabilities::default());
            backend.health_error = Some(health_error);
            backend
        }
    }

    #[async_trait]
    impl Backend for StubBackend {
        fn name(&self) -> &'static str {
            self.name
        }
        fn kind(&self) -> BackendKind {
            BackendKind::Local
        }
        fn capabilities(&self) -> BackendCapabilities {
            self.capabilities.clone()
        }
        fn secrets(&self) -> &dyn SecretBackend {
            self
        }
        #[cfg(feature = "file-ops")]
        fn files(&self) -> Option<&dyn crate::backend::FileBackend> {
            Some(self)
        }
        async fn health_check(&self) -> Result<(), BackendError> {
            match self.health_error {
                Some(message) => Err(BackendError::Internal(message.into())),
                None => Ok(()),
            }
        }
    }

    /// Mirror how real backends surface metadata: groups/note/folder appear
    /// under canonical tag keys in `SecretProperties.tags`.
    /// (Duplicated from `src/backend/secret.rs`'s test module — that copy is
    /// test-only code private to a different module.)
    pub(crate) fn props_from_request(req: &SecretRequest, include_value: bool) -> SecretProperties {
        let mut tags = req.tags.clone().unwrap_or_default();
        if let Some(groups) = req.groups.as_ref().filter(|g| !g.is_empty()) {
            tags.insert("groups".to_string(), groups.join(","));
        }
        if let Some(note) = req.note.as_ref() {
            tags.insert("note".to_string(), note.clone());
        }
        if let Some(folder) = req.folder.as_ref() {
            tags.insert("folder".to_string(), folder.clone());
        }
        tags.insert("original_name".to_string(), req.name.clone());
        tags.insert("created_by".to_string(), "crosstache".to_string());
        SecretProperties {
            name: req.name.clone(),
            original_name: req.name.clone(),
            value: include_value.then(|| req.value.clone()),
            version: "v1".to_string(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: req.enabled.unwrap_or(true),
            expires_on: req.expires_on,
            not_before: req.not_before,
            tags,
            content_type: req.content_type.clone().unwrap_or_default(),
            recovery_level: None,
        }
    }

    #[async_trait]
    impl SecretBackend for StubBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            request: SecretRequest,
        ) -> Result<SecretProperties, BackendError> {
            let props = props_from_request(&request, false);
            self.secrets
                .lock()
                .unwrap()
                .insert(request.name.clone(), request);
            Ok(props)
        }

        async fn get_secret(
            &self,
            _vault: &str,
            name: &str,
            include_value: bool,
        ) -> Result<SecretProperties, BackendError> {
            self.secrets
                .lock()
                .unwrap()
                .get(name)
                .map(|r| props_from_request(r, include_value))
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }

        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("versions".into()))
        }

        async fn list_secrets(
            &self,
            _vault: &str,
            group_filter: Option<&str>,
        ) -> Result<Vec<SecretSummary>, BackendError> {
            let secrets = self.secrets.lock().unwrap();
            let summaries = secrets
                .values()
                .map(|req| {
                    let groups = req.groups.as_ref().map(|g| g.join(","));
                    (req, groups)
                })
                .filter(|(_, groups)| match (group_filter, groups) {
                    (Some(f), Some(g)) => g.contains(f),
                    (Some(_), None) => false,
                    (None, _) => true,
                })
                .map(|(req, groups)| SecretSummary {
                    name: req.name.clone(),
                    original_name: req.name.clone(),
                    note: req.note.clone(),
                    folder: req.folder.clone(),
                    groups,
                    updated_on: String::new(),
                    enabled: true,
                    content_type: req.content_type.clone().unwrap_or_default(),
                    tags: req.tags.clone().unwrap_or_default(),
                })
                .collect();
            Ok(summaries)
        }

        async fn delete_secret(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
            let request = self.secrets.lock().unwrap().remove(name).ok_or_else(|| {
                BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                }
            })?;
            self.deleted
                .lock()
                .unwrap()
                .insert(name.to_string(), request);
            Ok(())
        }

        async fn restore_secret(
            &self,
            _vault: &str,
            name: &str,
        ) -> Result<SecretProperties, BackendError> {
            let mut secrets = self.secrets.lock().unwrap();
            if secrets.contains_key(name) {
                return Err(BackendError::Conflict(format!(
                    "secret '{name}' already exists"
                )));
            }
            let request = self.deleted.lock().unwrap().remove(name).ok_or_else(|| {
                BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                }
            })?;
            let props = props_from_request(&request, false);
            secrets.insert(name.to_string(), request);
            Ok(props)
        }

        async fn purge_secret(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
            self.deleted
                .lock()
                .unwrap()
                .remove(name)
                .map(|_| ())
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }

        async fn list_deleted_secrets(
            &self,
            _vault: &str,
        ) -> Result<Vec<DeletedSecretSummary>, BackendError> {
            Ok(self
                .deleted
                .lock()
                .unwrap()
                .values()
                .map(|request| DeletedSecretSummary {
                    name: request.name.clone(),
                    original_name: request.name.clone(),
                    deleted_on: Some("2026-07-22T00:00:00Z".to_string()),
                    scheduled_purge_on: None,
                })
                .collect())
        }

        async fn update_secret(
            &self,
            _vault: &str,
            name: &str,
            request: SecretUpdateRequest,
        ) -> Result<SecretProperties, BackendError> {
            let mut secrets = self.secrets.lock().unwrap();
            let mut current = secrets
                .get(name)
                .cloned()
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })?;

            if let Some(value) = request.value {
                current.value = value;
            }
            if request.content_type.is_some() {
                current.content_type = request.content_type;
            }
            if request.enabled.is_some() {
                current.enabled = request.enabled;
            }
            current.expires_on = request.expires_on.apply(current.expires_on);
            current.not_before = request.not_before.apply(current.not_before);
            current.note = request.note.apply(current.note);
            current.folder = request.folder.apply(current.folder);
            if request.groups.is_some() {
                current.groups = request.groups;
            }
            if request.tags.is_some() {
                current.tags = request.tags;
            }

            let props = props_from_request(&current, false);
            secrets.insert(name.to_string(), current);
            Ok(props)
        }

        async fn secret_exists(&self, _vault: &str, name: &str) -> Result<bool, BackendError> {
            Ok(self.secrets.lock().unwrap().contains_key(name))
        }
    }

    #[cfg(feature = "file-ops")]
    #[async_trait]
    impl crate::backend::FileBackend for StubBackend {
        async fn upload_file(
            &self,
            _vault: &str,
            request: crate::blob::models::FileUploadRequest,
            _reporter: Option<&dyn crate::utils::progress::ProgressReporter>,
        ) -> Result<crate::blob::models::FileInfo, BackendError> {
            let info = file_info(
                &request.name,
                request.content.len() as u64,
                request
                    .content_type
                    .as_deref()
                    .unwrap_or("application/octet-stream"),
                request.metadata.clone(),
            );
            self.files.lock().unwrap().insert(
                request.name.clone(),
                (request.content, info.content_type.clone(), request.metadata),
            );
            Ok(info)
        }

        async fn download_file(
            &self,
            _vault: &str,
            name: &str,
            _reporter: Option<&dyn crate::utils::progress::ProgressReporter>,
        ) -> Result<Vec<u8>, BackendError> {
            self.files
                .lock()
                .unwrap()
                .get(name)
                .map(|(b, _, _)| b.clone())
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }

        async fn list_files(
            &self,
            _vault: &str,
            request: crate::blob::models::FileListRequest,
        ) -> Result<Vec<crate::blob::models::FileInfo>, BackendError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| request.prefix.as_deref().is_none_or(|p| n.starts_with(p)))
                .map(|(n, (b, ct, m))| file_info(n, b.len() as u64, ct, m.clone()))
                .collect())
        }

        async fn delete_file(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
            self.files
                .lock()
                .unwrap()
                .remove(name)
                .map(|_| ())
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }

        async fn get_file_info(
            &self,
            _vault: &str,
            name: &str,
        ) -> Result<crate::blob::models::FileInfo, BackendError> {
            self.files
                .lock()
                .unwrap()
                .get(name)
                .map(|(b, ct, m)| file_info(name, b.len() as u64, ct, m.clone()))
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }
    }

    #[cfg(feature = "file-ops")]
    fn file_info(
        name: &str,
        size: u64,
        content_type: &str,
        metadata: HashMap<String, String>,
    ) -> crate::blob::models::FileInfo {
        crate::blob::models::FileInfo {
            name: name.to_string(),
            size,
            content_type: content_type.to_string(),
            last_modified: chrono::Utc::now(),
            etag: String::new(),
            groups: Vec::new(),
            metadata,
            tags: std::collections::HashMap::new(),
        }
    }
}
