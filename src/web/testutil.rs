//! Test-only helpers for the web module.
#![cfg(test)]

use std::sync::Arc;

use super::WebState;

pub(crate) fn test_state_with_token(token: &str) -> Arc<WebState> {
    Arc::new(WebState {
        backend: Arc::new(stub::StubBackend::new()),
        token: token.to_string(),
        vault: "default".to_string(),
    })
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
        SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
    };

    pub(crate) struct StubBackend {
        pub secrets: Mutex<HashMap<String, SecretRequest>>,
    }

    impl StubBackend {
        pub fn new() -> Self {
            Self {
                secrets: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl Backend for StubBackend {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn kind(&self) -> BackendKind {
            BackendKind::Local
        }
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                has_folders: true,
                has_groups: true,
                has_notes: true,
                has_expiry: true,
                ..Default::default()
            }
        }
        fn secrets(&self) -> &dyn SecretBackend {
            self
        }
        async fn health_check(&self) -> Result<(), BackendError> {
            Ok(())
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
                })
                .collect();
            Ok(summaries)
        }

        async fn delete_secret(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
            self.secrets
                .lock()
                .unwrap()
                .remove(name)
                .map(|_| ())
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
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
    }
}
