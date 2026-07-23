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
    let registry = Arc::new(crate::backend::BackendRegistry::new(backend.clone()));
    Arc::new(WebState::new(
        backend,
        context,
        token.to_string(),
        crate::records::builtin_types(),
        super::preferences::PreferenceStore::new(path, clipboard_timeout),
        registry,
    ))
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
        capabilities: CapabilitySummary::from_backend(backend),
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
    use std::time::Duration;

    use async_trait::async_trait;

    use crate::backend::error::BackendError;
    use crate::backend::secret::SecretSnapshot;
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
        list_error: Option<&'static str>,
        list_delay: Option<Duration>,
        delete_error: Option<&'static str>,
        update_error: Option<&'static str>,
        conversion_cas_race_value: Option<&'static str>,
        revision_validation_supported: bool,
        atomic_rename_supported: bool,
        rename_source_race: bool,
        pub secrets: Mutex<HashMap<String, SecretRequest>>,
        revisions: Mutex<HashMap<String, String>>,
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
                    has_atomic_record_conversion: true,
                    has_conditional_record_conversion: true,
                    has_atomic_rename: true,
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
            let revision_validation_supported = capabilities.has_conditional_record_conversion;
            let atomic_rename_supported = capabilities.has_atomic_rename;
            Self {
                name,
                capabilities,
                health_error: None,
                list_error: None,
                list_delay: None,
                delete_error: None,
                update_error: None,
                conversion_cas_race_value: None,
                revision_validation_supported,
                atomic_rename_supported,
                rename_source_race: false,
                secrets: Mutex::new(HashMap::new()),
                revisions: Mutex::new(HashMap::new()),
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

        pub(crate) fn with_list_error(name: &'static str, list_error: &'static str) -> Self {
            let mut backend = Self::with_capabilities(name, BackendCapabilities::default());
            backend.list_error = Some(list_error);
            backend
        }

        pub(crate) fn with_list_delay(name: &'static str, list_delay: Duration) -> Self {
            let mut backend = Self::with_capabilities(name, BackendCapabilities::default());
            backend.list_delay = Some(list_delay);
            backend
        }

        pub(crate) fn with_delete_error(name: &'static str, delete_error: &'static str) -> Self {
            let mut backend = Self::with_capabilities(
                name,
                BackendCapabilities {
                    has_atomic_rename: true,
                    ..BackendCapabilities::default()
                },
            );
            backend.delete_error = Some(delete_error);
            backend
        }

        pub(crate) fn with_update_error(
            name: &'static str,
            capabilities: BackendCapabilities,
            update_error: &'static str,
        ) -> Self {
            let mut backend = Self::with_capabilities(name, capabilities);
            backend.update_error = Some(update_error);
            backend
        }

        pub(crate) fn with_conversion_cas_race(name: &'static str, value: &'static str) -> Self {
            let mut backend = Self::new();
            backend.name = name;
            backend.conversion_cas_race_value = Some(value);
            backend
        }

        pub(crate) fn with_rename_source_race(name: &'static str) -> Self {
            let mut backend = Self::new();
            backend.name = name;
            backend.rename_source_race = true;
            backend
        }

        pub(crate) fn without_revision_validation(mut self) -> Self {
            self.revision_validation_supported = false;
            self
        }

        pub(crate) fn without_atomic_rename_support(mut self) -> Self {
            self.atomic_rename_supported = false;
            self
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
        fn supports_conditional_update(&self) -> bool {
            self.capabilities.has_conditional_record_conversion
        }

        fn supports_revision_validation(&self) -> bool {
            self.revision_validation_supported
        }

        fn supports_atomic_rename(&self) -> bool {
            self.atomic_rename_supported
        }

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
            self.revisions
                .lock()
                .unwrap()
                .insert(props.name.clone(), uuid::Uuid::new_v4().to_string());
            Ok(props)
        }

        async fn create_secret_if_absent(
            &self,
            _vault: &str,
            request: SecretRequest,
        ) -> Result<SecretProperties, BackendError> {
            let mut secrets = self.secrets.lock().unwrap();
            if secrets.contains_key(&request.name) {
                return Err(BackendError::Conflict(format!(
                    "secret '{}' already exists",
                    request.name
                )));
            }
            let props = props_from_request(&request, false);
            secrets.insert(request.name.clone(), request);
            self.revisions
                .lock()
                .unwrap()
                .insert(props.name.clone(), uuid::Uuid::new_v4().to_string());
            Ok(props)
        }

        async fn get_secret_snapshot(
            &self,
            vault: &str,
            name: &str,
            include_value: bool,
        ) -> Result<SecretSnapshot, BackendError> {
            let properties = self.get_secret(vault, name, include_value).await?;
            let revision = self
                .revisions
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| BackendError::Internal("missing test revision".into()))?;
            Ok(SecretSnapshot {
                properties,
                revision,
            })
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
            if let Some(delay) = self.list_delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(message) = self.list_error {
                return Err(BackendError::Internal(message.into()));
            }
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
                    enabled: req.enabled.unwrap_or(true),
                    expires_on: req.expires_on,
                    content_type: req.content_type.clone().unwrap_or_default(),
                    tags: req.tags.clone().unwrap_or_default(),
                })
                .collect();
            Ok(summaries)
        }

        async fn delete_secret(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
            if let Some(message) = self.delete_error {
                return Err(BackendError::Internal(message.into()));
            }
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
            self.revisions.lock().unwrap().remove(name);
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
            self.revisions
                .lock()
                .unwrap()
                .insert(name.to_string(), uuid::Uuid::new_v4().to_string());
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
            if let Some(message) = self.update_error {
                return Err(BackendError::Internal(message.into()));
            }
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
            self.revisions
                .lock()
                .unwrap()
                .insert(name.to_string(), uuid::Uuid::new_v4().to_string());
            Ok(props)
        }

        async fn update_secret_if_revision(
            &self,
            vault: &str,
            name: &str,
            expected_revision: &str,
            request: SecretUpdateRequest,
        ) -> Result<SecretProperties, BackendError> {
            if let Some(value) = self.conversion_cas_race_value {
                let mut secrets = self.secrets.lock().unwrap();
                let current = secrets
                    .get_mut(name)
                    .ok_or_else(|| BackendError::NotFound {
                        name: name.to_string(),
                        suggestion: None,
                    })?;
                current.value = zeroize::Zeroizing::new(value.to_string());
                self.revisions
                    .lock()
                    .unwrap()
                    .insert(name.to_string(), uuid::Uuid::new_v4().to_string());
            }
            if self
                .revisions
                .lock()
                .unwrap()
                .get(name)
                .is_none_or(|current| current != expected_revision)
            {
                return Err(BackendError::SourceRevisionConflict {
                    name: name.to_string(),
                });
            }
            self.update_secret(vault, name, request).await
        }

        async fn validate_secret_revision(
            &self,
            vault: &str,
            name: &str,
            expected_revision: &str,
        ) -> Result<SecretProperties, BackendError> {
            if let Some(value) = self.conversion_cas_race_value {
                let mut secrets = self.secrets.lock().unwrap();
                let current = secrets
                    .get_mut(name)
                    .ok_or_else(|| BackendError::NotFound {
                        name: name.to_string(),
                        suggestion: None,
                    })?;
                current.value = zeroize::Zeroizing::new(value.to_string());
                self.revisions
                    .lock()
                    .unwrap()
                    .insert(name.to_string(), uuid::Uuid::new_v4().to_string());
            }
            if self
                .revisions
                .lock()
                .unwrap()
                .get(name)
                .is_none_or(|current| current != expected_revision)
            {
                return Err(BackendError::SourceRevisionConflict {
                    name: name.to_string(),
                });
            }
            self.get_secret(vault, name, false).await
        }

        async fn rename_secret_if_revision(
            &self,
            _vault: &str,
            name: &str,
            new_name: &str,
            expected_revision: &str,
        ) -> Result<SecretProperties, BackendError> {
            if self.rename_source_race {
                self.revisions
                    .lock()
                    .unwrap()
                    .insert(name.to_string(), uuid::Uuid::new_v4().to_string());
            }
            if self
                .revisions
                .lock()
                .unwrap()
                .get(name)
                .is_none_or(|current| current != expected_revision)
            {
                return Err(BackendError::SourceRevisionConflict {
                    name: name.to_string(),
                });
            }
            let mut secrets = self.secrets.lock().unwrap();
            if secrets.contains_key(new_name) {
                return Err(BackendError::DestinationExists {
                    name: new_name.to_string(),
                });
            }
            if let Some(message) = self.delete_error {
                return Err(BackendError::Internal(message.into()));
            }
            let mut request = secrets.remove(name).ok_or_else(|| BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            })?;
            request.name = new_name.to_string();
            let props = props_from_request(&request, false);
            secrets.insert(new_name.to_string(), request);
            let mut revisions = self.revisions.lock().unwrap();
            revisions.remove(name);
            revisions.insert(new_name.to_string(), uuid::Uuid::new_v4().to_string());
            Ok(props)
        }

        async fn rename_secret(
            &self,
            vault: &str,
            name: &str,
            new_name: &str,
        ) -> Result<SecretProperties, BackendError> {
            let revision = self
                .revisions
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })?;
            self.rename_secret_if_revision(vault, name, new_name, &revision)
                .await
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
