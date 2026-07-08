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

    // SecretBackend impl arrives in Task 3; a panicking placeholder keeps
    // Task 2 compiling since auth tests never call it.
    #[async_trait]
    impl SecretBackend for StubBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            _request: SecretRequest,
        ) -> Result<SecretProperties, BackendError> {
            unimplemented!()
        }
        async fn get_secret(
            &self,
            _vault: &str,
            _name: &str,
            _include_value: bool,
        ) -> Result<SecretProperties, BackendError> {
            unimplemented!()
        }
        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> Result<SecretProperties, BackendError> {
            unimplemented!()
        }
        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> Result<Vec<SecretSummary>, BackendError> {
            unimplemented!()
        }
        async fn delete_secret(&self, _vault: &str, _name: &str) -> Result<(), BackendError> {
            unimplemented!()
        }
        async fn update_secret(
            &self,
            _vault: &str,
            _name: &str,
            _request: SecretUpdateRequest,
        ) -> Result<SecretProperties, BackendError> {
            unimplemented!()
        }
    }
}
