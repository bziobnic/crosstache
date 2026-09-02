//! Backend wrapper enforcing agent policy on secret operations.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;

use super::decision_log::DecisionLog;
use super::policy::{
    evaluate, evaluate_list_scope, AccessRequest, CompiledPolicy, Decision, DenyReason, Operation,
};
use super::{AgentIdentity, AuditContext, AUDIT_CONTEXT};
#[cfg(feature = "file-ops")]
use crate::backend::FileBackend;
use crate::backend::{
    AuditBackend, Backend, BackendCapabilities, BackendError, BackendKind, SecretBackend,
    VaultBackend,
};
use crate::secret::manager::{
    DeletedSecretSummary, SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
};

const DENIED_MESSAGE: &str = "agent policy denied this secret operation";
const MAX_WORKSPACE_BYTES: usize = 512;
const MAX_RESOURCE_BYTES: usize = 1024;
const LIST_SCOPE_RESOURCE: &str = "<list-scope>";
const MAX_LIST_ITEM_DECISIONS: usize = 256;

/// Enforces a compiled policy while preserving non-secret backend surfaces.
pub struct PolicyEnforcedBackend {
    inner: Arc<dyn Backend>,
    identity: AgentIdentity,
    policy: CompiledPolicy,
    decisions: DecisionLog,
}

impl PolicyEnforcedBackend {
    pub(crate) fn new(
        inner: Arc<dyn Backend>,
        identity: AgentIdentity,
        policy: CompiledPolicy,
        decisions: DecisionLog,
    ) -> Self {
        Self {
            inner,
            identity,
            policy,
            decisions,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        inner: Arc<dyn Backend>,
        identity: AgentIdentity,
        policy: CompiledPolicy,
        decision_path: std::path::PathBuf,
    ) -> Self {
        Self::new(
            inner,
            identity,
            policy,
            DecisionLog::for_test(decision_path),
        )
    }

    fn authorize(
        &self,
        workspace: &str,
        secret: &str,
        operation: Operation,
        raw_disclosure_requested: bool,
    ) -> Result<AuditContext, BackendError> {
        validate_audit_text("workspace", workspace, MAX_WORKSPACE_BYTES)?;
        validate_audit_text("secret resource", secret, MAX_RESOURCE_BYTES)?;
        let request = AccessRequest {
            policy: &self.policy,
            workspace,
            secret,
            operation,
            raw_disclosure_requested,
        };
        let decision = evaluate(&self.identity, &request);
        self.decisions.record(
            &self.identity,
            workspace,
            secret,
            operation,
            &decision,
            self.policy.version(),
        )?;
        match decision {
            Decision::Allow { policy_version, .. } => Ok(AuditContext {
                identity: self.identity.clone(),
                policy_version,
                decision: "allow".into(),
            }),
            Decision::Deny { .. } => Err(BackendError::PermissionDenied(DENIED_MESSAGE.into())),
        }
    }

    fn authorize_list_scope(&self, workspace: &str) -> Result<AuditContext, BackendError> {
        validate_audit_text("workspace", workspace, MAX_WORKSPACE_BYTES)?;
        let decision = evaluate_list_scope(&self.identity, &self.policy, workspace);
        self.decisions.record(
            &self.identity,
            workspace,
            LIST_SCOPE_RESOURCE,
            Operation::List,
            &decision,
            self.policy.version(),
        )?;
        match decision {
            Decision::Allow { policy_version, .. } => Ok(AuditContext {
                identity: self.identity.clone(),
                policy_version,
                decision: "allow".into(),
            }),
            Decision::Deny { .. } => Err(BackendError::PermissionDenied(DENIED_MESSAGE.into())),
        }
    }

    fn list_item_allowed(
        &self,
        workspace: &str,
        name: &str,
        index: usize,
    ) -> Result<bool, BackendError> {
        // A provider-returned name is untrusted durable-audit input. An
        // invalid one is neither logged nor exposed to the caller.
        if validate_audit_text("secret resource", name, MAX_RESOURCE_BYTES).is_err() {
            return Ok(false);
        }
        let decision = evaluate(
            &self.identity,
            &AccessRequest {
                policy: &self.policy,
                workspace,
                secret: name,
                operation: Operation::List,
                raw_disclosure_requested: false,
            },
        );
        // The scope record always proves evaluation occurred. Keep useful
        // item-level evidence without allowing a huge backend response to
        // grow the durable decision log without bound.
        if index < MAX_LIST_ITEM_DECISIONS {
            self.decisions.record(
                &self.identity,
                workspace,
                name,
                Operation::List,
                &decision,
                self.policy.version(),
            )?;
        }
        Ok(matches!(decision, Decision::Allow { .. }))
    }

    async fn checked<T, F>(
        &self,
        workspace: &str,
        secret: &str,
        operation: Operation,
        raw_disclosure_requested: bool,
        call: F,
    ) -> Result<T, BackendError>
    where
        F: Future<Output = Result<T, BackendError>>,
    {
        let context = self.authorize(workspace, secret, operation, raw_disclosure_requested)?;
        AUDIT_CONTEXT.scope(context, call).await
    }
}

#[async_trait]
impl Backend for PolicyEnforcedBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn kind(&self) -> BackendKind {
        self.inner.kind()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }

    fn secrets(&self) -> &dyn SecretBackend {
        self
    }

    fn vaults(&self) -> Option<&dyn VaultBackend> {
        self.inner.vaults()
    }

    fn audit(&self) -> Option<&dyn AuditBackend> {
        self.inner.audit()
    }

    #[cfg(feature = "file-ops")]
    fn files(&self) -> Option<&dyn FileBackend> {
        self.inner.files()
    }

    async fn health_check(&self) -> Result<(), BackendError> {
        self.inner.health_check().await
    }
}

#[async_trait]
impl SecretBackend for PolicyEnforcedBackend {
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        let name = request.name.clone();
        self.checked(
            vault,
            &name,
            Operation::Set,
            false,
            self.inner.secrets().set_secret(vault, request),
        )
        .await
    }

    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Get,
            include_value,
            self.inner.secrets().get_secret(vault, name, include_value),
        )
        .await
    }

    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Get,
            include_value,
            self.inner
                .secrets()
                .get_secret_version(vault, name, version, include_value),
        )
        .await
    }

    async fn list_secrets(
        &self,
        vault: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>, BackendError> {
        let scope_context = self.authorize_list_scope(vault)?;

        let summaries = AUDIT_CONTEXT
            .scope(
                scope_context,
                self.inner.secrets().list_secrets(vault, group_filter),
            )
            .await?;
        let mut visible = Vec::with_capacity(summaries.len());
        for (index, summary) in summaries.into_iter().enumerate() {
            if self.list_item_allowed(vault, &summary.name, index)? {
                visible.push(summary);
            }
        }
        Ok(visible)
    }

    async fn delete_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.checked(
            vault,
            name,
            Operation::Delete,
            false,
            self.inner.secrets().delete_secret(vault, name),
        )
        .await
    }

    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Update,
            false,
            self.inner.secrets().update_secret(vault, name, request),
        )
        .await
    }

    fn supports_conditional_update(&self) -> bool {
        self.inner.secrets().supports_conditional_update()
    }

    fn supports_revision_validation(&self) -> bool {
        self.inner.secrets().supports_revision_validation()
    }

    async fn get_secret_snapshot(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<crate::backend::secret::SecretSnapshot, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Get,
            include_value,
            self.inner
                .secrets()
                .get_secret_snapshot(vault, name, include_value),
        )
        .await
    }

    async fn update_secret_if_revision(
        &self,
        vault: &str,
        name: &str,
        expected_revision: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Update,
            false,
            self.inner
                .secrets()
                .update_secret_if_revision(vault, name, expected_revision, request),
        )
        .await
    }

    async fn validate_secret_revision(
        &self,
        vault: &str,
        name: &str,
        expected_revision: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Get,
            false,
            self.inner
                .secrets()
                .validate_secret_revision(vault, name, expected_revision),
        )
        .await
    }

    async fn create_secret_if_absent(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        let name = request.name.clone();
        self.checked(
            vault,
            &name,
            Operation::Set,
            false,
            self.inner.secrets().create_secret_if_absent(vault, request),
        )
        .await
    }

    fn supports_atomic_rename(&self) -> bool {
        self.inner.secrets().supports_atomic_rename()
    }

    async fn rename_secret_if_revision(
        &self,
        vault: &str,
        name: &str,
        new_name: &str,
        expected_revision: &str,
    ) -> Result<SecretProperties, BackendError> {
        let source_context = self.authorize(vault, name, Operation::Rename, false)?;
        self.authorize(vault, new_name, Operation::Rename, false)?;
        AUDIT_CONTEXT
            .scope(
                source_context,
                self.inner.secrets().rename_secret_if_revision(
                    vault,
                    name,
                    new_name,
                    expected_revision,
                ),
            )
            .await
    }

    async fn rename_secret(
        &self,
        vault: &str,
        name: &str,
        new_name: &str,
    ) -> Result<SecretProperties, BackendError> {
        let source_context = self.authorize(vault, name, Operation::Rename, false)?;
        self.authorize(vault, new_name, Operation::Rename, false)?;
        AUDIT_CONTEXT
            .scope(
                source_context,
                self.inner.secrets().rename_secret(vault, name, new_name),
            )
            .await
    }

    async fn list_versions(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<Vec<SecretProperties>, BackendError> {
        self.checked(
            vault,
            name,
            Operation::List,
            false,
            self.inner.secrets().list_versions(vault, name),
        )
        .await
    }

    async fn rollback(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Rollback,
            false,
            self.inner.secrets().rollback(vault, name, version),
        )
        .await
    }

    async fn restore_secret(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Restore,
            false,
            self.inner.secrets().restore_secret(vault, name),
        )
        .await
    }

    async fn purge_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.checked(
            vault,
            name,
            Operation::Purge,
            false,
            self.inner.secrets().purge_secret(vault, name),
        )
        .await
    }

    async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Get,
            false,
            self.inner.secrets().secret_exists(vault, name),
        )
        .await
    }

    async fn list_deleted_secrets(
        &self,
        vault: &str,
    ) -> Result<Vec<DeletedSecretSummary>, BackendError> {
        let scope_context = self.authorize_list_scope(vault)?;
        let summaries = AUDIT_CONTEXT
            .scope(
                scope_context,
                self.inner.secrets().list_deleted_secrets(vault),
            )
            .await?;
        let mut visible = Vec::with_capacity(summaries.len());
        for (index, summary) in summaries.into_iter().enumerate() {
            if self.list_item_allowed(vault, &summary.name, index)? {
                visible.push(summary);
            }
        }
        Ok(visible)
    }

    async fn backup_secret(&self, vault: &str, name: &str) -> Result<Vec<u8>, BackendError> {
        self.checked(
            vault,
            name,
            Operation::Get,
            true,
            self.inner.secrets().backup_secret(vault, name),
        )
        .await
    }

    async fn restore_from_backup(
        &self,
        vault: &str,
        backup: &[u8],
    ) -> Result<SecretProperties, BackendError> {
        let _ = backup;
        validate_audit_text("workspace", vault, MAX_WORKSPACE_BYTES)?;
        let decision = Decision::Deny {
            reason: DenyReason::DestinationBindingUnavailable,
        };
        self.decisions.record(
            &self.identity,
            vault,
            "<backup-destination-unavailable>",
            Operation::Restore,
            &decision,
            self.policy.version(),
        )?;
        Err(BackendError::PermissionDenied(DENIED_MESSAGE.into()))
    }

    async fn native_rotate(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.checked(
            vault,
            name,
            Operation::Rotate,
            false,
            self.inner.secrets().native_rotate(vault, name),
        )
        .await
    }
}

fn validate_audit_text(label: &str, value: &str, max_bytes: usize) -> Result<(), BackendError> {
    if value.len() > max_bytes {
        return Err(BackendError::InvalidArgument(format!(
            "{label} is too long for agent decision auditing (maximum {max_bytes} bytes)"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(BackendError::InvalidArgument(format!(
            "{label} contains a control character and cannot be written to the agent decision log"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::settings::AgentConfig;

    struct CountingBackend {
        calls: AtomicUsize,
        list_results: Vec<SecretSummary>,
        deleted_list_results: Vec<DeletedSecretSummary>,
    }

    #[async_trait]
    impl Backend for CountingBackend {
        fn name(&self) -> &'static str {
            "counting"
        }
        fn kind(&self) -> BackendKind {
            BackendKind::Local
        }
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }
        fn secrets(&self) -> &dyn SecretBackend {
            self
        }
        async fn health_check(&self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    #[async_trait]
    impl SecretBackend for CountingBackend {
        async fn set_secret(
            &self,
            _: &str,
            _: SecretRequest,
        ) -> Result<SecretProperties, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BackendError::Unsupported("test".into()))
        }
        async fn get_secret(
            &self,
            _: &str,
            name: &str,
            _: bool,
        ) -> Result<SecretProperties, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if name == "existing" || name.starts_with("allowed/") {
                Ok(test_properties(name))
            } else {
                Err(BackendError::NotFound {
                    name: name.into(),
                    suggestion: None,
                })
            }
        }
        async fn get_secret_version(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: bool,
        ) -> Result<SecretProperties, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BackendError::Unsupported("test".into()))
        }
        async fn list_secrets(
            &self,
            _: &str,
            _: Option<&str>,
        ) -> Result<Vec<SecretSummary>, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.list_results.clone())
        }
        async fn list_deleted_secrets(
            &self,
            _: &str,
        ) -> Result<Vec<DeletedSecretSummary>, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.deleted_list_results.clone())
        }
        async fn delete_secret(&self, _: &str, _: &str) -> Result<(), BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn update_secret(
            &self,
            _: &str,
            _: &str,
            _: SecretUpdateRequest,
        ) -> Result<SecretProperties, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BackendError::Unsupported("test".into()))
        }
        async fn rename_secret_if_revision(
            &self,
            _: &str,
            _: &str,
            new_name: &str,
            _: &str,
        ) -> Result<SecretProperties, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(test_properties(new_name))
        }
        async fn rename_secret(
            &self,
            _: &str,
            _: &str,
            new_name: &str,
        ) -> Result<SecretProperties, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(test_properties(new_name))
        }
        async fn backup_secret(&self, _: &str, _: &str) -> Result<Vec<u8>, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![1, 2, 3])
        }
        async fn restore_from_backup(
            &self,
            _: &str,
            _: &[u8],
        ) -> Result<SecretProperties, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(test_properties("restored"))
        }
    }

    fn test_properties(name: &str) -> SecretProperties {
        SecretProperties {
            name: name.into(),
            original_name: name.into(),
            value: None,
            version: "v1".into(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: Default::default(),
            content_type: String::new(),
            recovery_level: None,
        }
    }

    fn test_summary(name: &str) -> SecretSummary {
        SecretSummary {
            name: name.into(),
            original_name: name.into(),
            note: None,
            folder: None,
            groups: None,
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            content_type: String::new(),
            tags: Default::default(),
        }
    }

    fn test_deleted_summary(name: &str) -> DeletedSecretSummary {
        DeletedSecretSummary {
            name: name.into(),
            original_name: name.into(),
            deleted_on: None,
            scheduled_purge_on: None,
        }
    }

    fn denied_wrapper(path: &std::path::Path) -> (Arc<CountingBackend>, PolicyEnforcedBackend) {
        let inner = Arc::new(CountingBackend {
            calls: AtomicUsize::new(0),
            list_results: Vec::new(),
            deleted_list_results: Vec::new(),
        });
        let policy = CompiledPolicy::compile(&AgentConfig {
            enforce: true,
            ..Default::default()
        })
        .unwrap();
        let wrapped = PolicyEnforcedBackend::new(
            inner.clone(),
            AgentIdentity::new(super::super::IdentitySource::EnvAssertion, "agent"),
            policy,
            DecisionLog::for_test(path.to_path_buf()),
        );
        (inner, wrapped)
    }

    fn scoped_wrapper(
        path: &std::path::Path,
        raw_disclosure: bool,
    ) -> (Arc<CountingBackend>, PolicyEnforcedBackend) {
        let inner = Arc::new(CountingBackend {
            calls: AtomicUsize::new(0),
            list_results: Vec::new(),
            deleted_list_results: Vec::new(),
        });
        let policy = CompiledPolicy::compile(&AgentConfig {
            enforce: true,
            policy: vec![crate::config::settings::AgentPolicyRule {
                name: "allowed".into(),
                identity: "github:o/r:*".into(),
                identity_source: "github-oidc".into(),
                workspace: "prod".into(),
                secrets: vec!["allowed/*".into(), "existing".into()],
                operations: vec!["get".into(), "rename".into(), "restore".into()],
                raw_disclosure,
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let wrapped = PolicyEnforcedBackend::new(
            inner.clone(),
            AgentIdentity::new(
                super::super::IdentitySource::GithubOidc,
                "github:o/r:.github/workflows/ci.yml@refs/heads/main",
            ),
            policy,
            DecisionLog::for_test(path.to_path_buf()),
        );
        (inner, wrapped)
    }

    fn list_wrapper(
        path: &std::path::Path,
        list_results: Vec<SecretSummary>,
    ) -> (Arc<CountingBackend>, PolicyEnforcedBackend) {
        let inner = Arc::new(CountingBackend {
            calls: AtomicUsize::new(0),
            list_results,
            deleted_list_results: Vec::new(),
        });
        let policy = CompiledPolicy::compile(&AgentConfig {
            enforce: true,
            policy: vec![crate::config::settings::AgentPolicyRule {
                name: "deploy-list".into(),
                identity: "github:o/r:*".into(),
                identity_source: "github-oidc".into(),
                workspace: "prod".into(),
                secrets: vec!["deploy/*".into()],
                operations: vec!["list".into()],
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let wrapped = PolicyEnforcedBackend::new(
            inner.clone(),
            AgentIdentity::new(
                super::super::IdentitySource::GithubOidc,
                "github:o/r:.github/workflows/ci.yml@refs/heads/main",
            ),
            policy,
            DecisionLog::for_test(path.to_path_buf()),
        );
        (inner, wrapped)
    }

    fn deleted_list_wrapper(
        path: &std::path::Path,
        deleted_list_results: Vec<DeletedSecretSummary>,
    ) -> (Arc<CountingBackend>, PolicyEnforcedBackend) {
        let inner = Arc::new(CountingBackend {
            calls: AtomicUsize::new(0),
            list_results: Vec::new(),
            deleted_list_results,
        });
        let policy = CompiledPolicy::compile(&AgentConfig {
            enforce: true,
            policy: vec![crate::config::settings::AgentPolicyRule {
                name: "deploy-list".into(),
                identity: "github:o/r:*".into(),
                identity_source: "github-oidc".into(),
                workspace: "prod".into(),
                secrets: vec!["deploy/*".into()],
                operations: vec!["list".into()],
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let wrapped = PolicyEnforcedBackend::new(
            inner.clone(),
            AgentIdentity::new(
                super::super::IdentitySource::GithubOidc,
                "github:o/r:.github/workflows/ci.yml@refs/heads/main",
            ),
            policy,
            DecisionLog::for_test(path.to_path_buf()),
        );
        (inner, wrapped)
    }

    #[tokio::test]
    async fn denied_operation_writes_decision_and_never_calls_backend() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = denied_wrapper(&temp.path().join("decisions.jsonl"));
        let error = wrapped
            .secrets()
            .get_secret("prod", "existing", true)
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::PermissionDenied(_)));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        let records = wrapped.decisions.read_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].decision.as_deref(), Some("deny"));
        assert_eq!(records[0].operation, "get");
        assert_eq!(records[0].resource_name, "prod/existing");
    }

    #[tokio::test]
    async fn denied_existing_and_nonexistent_secrets_are_indistinguishable() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = denied_wrapper(&temp.path().join("decisions.jsonl"));
        let existing = wrapped
            .secrets()
            .get_secret("prod", "existing", true)
            .await
            .unwrap_err()
            .to_string();
        let missing = wrapped
            .secrets()
            .get_secret("prod", "does-not-exist", true)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(existing, missing);
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn list_scope_fetches_once_and_filters_every_returned_secret_name() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = list_wrapper(
            &temp.path().join("decisions.jsonl"),
            vec![
                test_summary("deploy/key"),
                test_summary("deploy/nested/admin"),
                test_summary("restricted/admin"),
            ],
        );
        let visible = wrapped.secrets().list_secrets("prod", None).await.unwrap();
        assert_eq!(
            visible
                .iter()
                .map(|summary| summary.name.as_str())
                .collect::<Vec<_>>(),
            vec!["deploy/key"]
        );
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        let records = wrapped.decisions.read_all().unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].resource_name, "prod/<list-scope>");
        assert_eq!(records[0].decision.as_deref(), Some("allow"));
        assert_eq!(records[1].decision.as_deref(), Some("allow"));
        assert_eq!(records[2].decision.as_deref(), Some("deny"));
        assert_eq!(records[3].decision.as_deref(), Some("deny"));
    }

    #[tokio::test]
    async fn list_without_an_applicable_scope_rule_never_calls_backend() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = denied_wrapper(&temp.path().join("decisions.jsonl"));
        let error = wrapped
            .secrets()
            .list_secrets("prod", None)
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::PermissionDenied(_)));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        let records = wrapped.decisions.read_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].resource_name, "prod/<list-scope>");
        assert_eq!(records[0].decision.as_deref(), Some("deny"));
    }

    #[tokio::test]
    async fn empty_list_still_records_an_auditable_scope_decision() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = list_wrapper(&temp.path().join("decisions.jsonl"), Vec::new());
        assert!(wrapped
            .secrets()
            .list_secrets("prod", None)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        let records = wrapped.decisions.read_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].resource_name, "prod/<list-scope>");
        assert_eq!(records[0].decision.as_deref(), Some("allow"));
    }

    #[tokio::test]
    async fn deleted_list_returns_only_the_policy_scoped_subset() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = deleted_list_wrapper(
            &temp.path().join("decisions.jsonl"),
            vec![
                test_deleted_summary("deploy/key"),
                test_deleted_summary("deploy/nested/admin"),
                test_deleted_summary("restricted/admin"),
            ],
        );

        let visible = wrapped
            .secrets()
            .list_deleted_secrets("prod")
            .await
            .unwrap();

        assert_eq!(
            visible
                .iter()
                .map(|summary| summary.name.as_str())
                .collect::<Vec<_>>(),
            vec!["deploy/key"]
        );
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        let records = wrapped.decisions.read_all().unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].resource_name, "prod/<list-scope>");
        assert_eq!(records[0].decision.as_deref(), Some("allow"));
        assert_eq!(records[1].decision.as_deref(), Some("allow"));
        assert_eq!(records[2].decision.as_deref(), Some("deny"));
        assert_eq!(records[3].decision.as_deref(), Some("deny"));
    }

    #[tokio::test]
    async fn denied_deleted_list_never_calls_the_backend() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = denied_wrapper(&temp.path().join("decisions.jsonl"));

        let error = wrapped
            .secrets()
            .list_deleted_secrets("prod")
            .await
            .unwrap_err();

        assert!(matches!(error, BackendError::PermissionDenied(_)));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        let records = wrapped.decisions.read_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].resource_name, "prod/<list-scope>");
        assert_eq!(records[0].decision.as_deref(), Some("deny"));
    }

    #[tokio::test]
    async fn deleted_list_decision_audit_is_complete_at_scope_and_bounded_per_item() {
        let temp = tempfile::tempdir().unwrap();
        let results = (0..(MAX_LIST_ITEM_DECISIONS + 20))
            .map(|index| test_deleted_summary(&format!("deploy/key-{index}")))
            .collect();
        let (inner, wrapped) = deleted_list_wrapper(&temp.path().join("decisions.jsonl"), results);

        let visible = wrapped
            .secrets()
            .list_deleted_secrets("prod")
            .await
            .unwrap();

        assert_eq!(visible.len(), MAX_LIST_ITEM_DECISIONS + 20);
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        let records = wrapped.decisions.read_all().unwrap();
        assert_eq!(records.len(), 1 + MAX_LIST_ITEM_DECISIONS);
        assert_eq!(records[0].resource_name, "prod/<list-scope>");
        assert_eq!(records[0].decision.as_deref(), Some("allow"));
        assert_eq!(records[1].resource_name, "prod/deploy/key-0");
        assert_eq!(
            records.last().unwrap().resource_name,
            format!("prod/deploy/key-{}", MAX_LIST_ITEM_DECISIONS - 1)
        );
    }

    #[tokio::test]
    async fn verified_raw_get_requires_raw_rule_but_metadata_get_is_allowed() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = scoped_wrapper(&temp.path().join("decisions.jsonl"), false);
        wrapped
            .secrets()
            .get_secret("prod", "existing", false)
            .await
            .unwrap();
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        let error = wrapped
            .secrets()
            .get_secret("prod", "existing", true)
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::PermissionDenied(_)));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn backup_is_value_bearing_and_requires_raw_rule_permission() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = scoped_wrapper(&temp.path().join("decisions.jsonl"), false);
        let error = wrapped
            .secrets()
            .backup_secret("prod", "existing")
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::PermissionDenied(_)));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rename_authorizes_source_and_destination_before_either_backend_method() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = scoped_wrapper(&temp.path().join("decisions.jsonl"), false);
        for conditional in [false, true] {
            let error = if conditional {
                wrapped
                    .secrets()
                    .rename_secret_if_revision("prod", "allowed/source", "restricted/admin", "rev")
                    .await
                    .unwrap_err()
            } else {
                wrapped
                    .secrets()
                    .rename_secret("prod", "allowed/source", "restricted/admin")
                    .await
                    .unwrap_err()
            };
            assert!(matches!(error, BackendError::PermissionDenied(_)));
        }
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        let records = wrapped.decisions.read_all().unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].resource_name, "prod/allowed/source");
        assert_eq!(records[0].decision.as_deref(), Some("allow"));
        assert_eq!(records[1].resource_name, "prod/restricted/admin");
        assert_eq!(records[1].decision.as_deref(), Some("deny"));
    }

    #[tokio::test]
    async fn restore_from_backup_fails_closed_without_a_destination_binding() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = scoped_wrapper(&temp.path().join("decisions.jsonl"), true);
        let error = wrapped
            .secrets()
            .restore_from_backup("prod", &[1, 2, 3])
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::PermissionDenied(_)));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        let records = wrapped.decisions.read_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].decision.as_deref(), Some("deny"));
        assert!(records[0]
            .deny_reason
            .as_deref()
            .unwrap()
            .contains("destination"));
        assert_ne!(records[0].resource_name, "prod/*");
    }

    #[tokio::test]
    async fn oversized_or_control_character_resources_never_reach_backend_or_log() {
        let temp = tempfile::tempdir().unwrap();
        let (inner, wrapped) = scoped_wrapper(&temp.path().join("decisions.jsonl"), true);
        for name in [
            "x".repeat(MAX_RESOURCE_BYTES + 1),
            "bad\u{1b}name".to_string(),
        ] {
            let error = wrapped
                .secrets()
                .get_secret("prod", &name, true)
                .await
                .unwrap_err();
            assert!(matches!(error, BackendError::InvalidArgument(_)));
        }
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        assert!(wrapped.decisions.read_all().unwrap().is_empty());
    }

    #[test]
    fn non_secret_surfaces_delegate_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let (_inner, wrapped) = denied_wrapper(&temp.path().join("decisions.jsonl"));
        assert!(wrapped.vaults().is_none());
        assert!(wrapped.audit().is_none());
    }
}
