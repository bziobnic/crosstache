use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::backend::{
    atomic_rename_available, conditional_record_conversion_available, Backend, BackendKind,
    BackendRegistry,
};
use crate::config::project::{
    find_project_config, resolve_effective_backend_config, resolve_env_with_source,
    BackendSelectionSource, EnvProfile, EnvironmentSelectionSource, ProjectConfig,
};
use crate::config::{Config, ContextManager};
use crate::error::{CrosstacheError, Result};
use crate::workspace::{Workspace, WorkspaceEntryConfig, WorkspaceSource};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ContextSource {
    Cli,
    Environment,
    ProjectEnvironment,
    Project,
    WorkspaceEntry,
    GlobalConfig,
    BuiltIn,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkspaceEntrySummary {
    pub(crate) alias: String,
    pub(crate) backend: String,
    pub(crate) vault: String,
    pub(crate) default: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkspaceSummary {
    pub(crate) alias: String,
    pub(crate) configured: bool,
    pub(crate) entries: Vec<WorkspaceEntrySummary>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectSummary {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EnvironmentSummary {
    pub(crate) name: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ContextSources {
    pub(crate) backend: ContextSource,
    pub(crate) vault: ContextSource,
    pub(crate) workspace: ContextSource,
    pub(crate) project: ContextSource,
    pub(crate) environment: ContextSource,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConnectionSummary {
    pub(crate) state: String,
    pub(crate) message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CapabilitySummary {
    pub(crate) secrets: bool,
    pub(crate) vaults: bool,
    pub(crate) files: bool,
    pub(crate) folders: bool,
    pub(crate) groups: bool,
    pub(crate) notes: bool,
    pub(crate) expiry: bool,
    pub(crate) soft_delete: bool,
    pub(crate) restore: bool,
    pub(crate) purge: bool,
    pub(crate) scheduled_purge: bool,
    pub(crate) trash: bool,
    pub(crate) versioning: bool,
    pub(crate) rbac: bool,
    pub(crate) audit: bool,
    pub(crate) rotation: bool,
    pub(crate) conversion: bool,
    pub(crate) conditional_conversion: bool,
    pub(crate) atomic_rename: bool,
    pub(crate) metadata: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SecuritySummary {
    pub(crate) clipboard_timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EffectiveUiContext {
    pub(crate) backend: String,
    pub(crate) backend_kind: BackendKind,
    pub(crate) vault: String,
    pub(crate) workspace: WorkspaceSummary,
    pub(crate) project: Option<ProjectSummary>,
    pub(crate) environment: Option<EnvironmentSummary>,
    pub(crate) sources: ContextSources,
    pub(crate) connection: ConnectionSummary,
    pub(crate) capabilities: CapabilitySummary,
    pub(crate) security: SecuritySummary,
    pub(crate) version: &'static str,
}

pub(crate) struct ResolvedUiContext {
    pub(crate) context: EffectiveUiContext,
    pub(crate) backend: Arc<dyn Backend>,
}

pub(crate) async fn get_context(
    State(state): State<Arc<super::WebState>>,
) -> Json<EffectiveUiContext> {
    Json(state.base_context())
}

#[derive(Debug, Deserialize)]
pub(crate) struct ActivateContextRequest {
    alias: String,
    backend: String,
    vault: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ActivateContextResponse {
    context: EffectiveUiContext,
}

#[derive(Debug, Serialize)]
pub(crate) struct ActivateWorkspaceResponse {
    context: EffectiveUiContext,
    secrets: Vec<crate::secret::manager::SecretSummary>,
}

async fn resolve_activation_candidate(
    state: &super::WebState,
    request: &ActivateContextRequest,
) -> std::result::Result<(Arc<dyn Backend>, EffectiveUiContext), super::api::ApiError> {
    let target = state.scoped_target(
        Some(&request.alias),
        Some(&request.backend),
        Some(&request.vault),
    )?;
    let backend = target.backend;
    let connection = match backend.health_check().await {
        Ok(()) => ConnectionSummary {
            state: "connected".into(),
            message: None,
        },
        Err(_) => ConnectionSummary {
            state: "unavailable".into(),
            message: Some("The selected backend is unavailable.".into()),
        },
    };
    let mut context = target.context;
    context.connection = connection;
    Ok((backend, context))
}

pub(crate) async fn activate_context(
    State(state): State<Arc<super::WebState>>,
    Json(request): Json<ActivateContextRequest>,
) -> std::result::Result<Json<ActivateContextResponse>, super::api::ApiError> {
    let (_, context) = resolve_activation_candidate(&state, &request).await?;
    Ok(Json(ActivateContextResponse { context }))
}

pub(crate) async fn activate_workspace(
    State(state): State<Arc<super::WebState>>,
    Json(request): Json<ActivateContextRequest>,
) -> std::result::Result<Json<ActivateWorkspaceResponse>, super::api::ApiError> {
    let (backend, context) = resolve_activation_candidate(&state, &request).await?;
    let secrets = backend.secrets().list_secrets(&context.vault, None).await?;
    Ok(Json(ActivateWorkspaceResponse { context, secrets }))
}

struct ProjectResolution {
    path: PathBuf,
    config: ProjectConfig,
}

impl ProjectResolution {
    fn profile(&self, config: &Config) -> Result<Option<ResolvedProfile<'_>>> {
        Ok(
            resolve_env_with_source(&self.config, config.env_flag.as_deref())?.map(|resolved| {
                ResolvedProfile {
                    name: resolved.name,
                    profile: resolved.profile,
                    source: match resolved.source {
                        EnvironmentSelectionSource::Environment => ContextSource::Environment,
                        EnvironmentSelectionSource::Cli => ContextSource::Cli,
                        EnvironmentSelectionSource::Project => ContextSource::Project,
                    },
                }
            }),
        )
    }
}

struct ResolvedProfile<'a> {
    name: &'a str,
    profile: &'a EnvProfile,
    source: ContextSource,
}

#[allow(dead_code)] // Public service seam consumed by workspace switching in the next UI slice.
pub(crate) async fn resolve_ui_context(
    config: &Config,
    registry: &BackendRegistry,
    cwd: &Path,
) -> Result<EffectiveUiContext> {
    Ok(resolve_ui_context_and_backend(config, registry, cwd)
        .await?
        .context)
}

pub(crate) async fn resolve_ui_context_and_backend(
    config: &Config,
    registry: &BackendRegistry,
    cwd: &Path,
) -> Result<ResolvedUiContext> {
    let effective = resolve_effective_backend_config(config, cwd).await?;
    resolve_ui_context_from_effective(&effective, registry, cwd).await
}

pub(crate) async fn resolve_ui_context_from_effective(
    effective: &crate::config::project::EffectiveBackendConfig,
    registry: &BackendRegistry,
    cwd: &Path,
) -> Result<ResolvedUiContext> {
    let context_manager = ContextManager::load_for_cwd(cwd).await?;
    let config = &effective.config;
    let project = find_project_config(cwd)
        .await?
        .map(|(path, config)| ProjectResolution { path, config });
    let profile = match project.as_ref() {
        Some(project) => project.profile(config)?,
        None => None,
    };
    let workspace = crate::workspace::resolve_workspace_from(config, Some(cwd), &context_manager)
        .await?
        .ok_or_else(|| {
            CrosstacheError::config(
                "internal error: effective workspace resolution returned no workspace",
            )
        })?;
    let target =
        crate::workspace::resolve::materialize_default_entry(config, &workspace, registry)?;
    let backend_source = backend_source(
        effective.source,
        profile.as_ref(),
        &workspace,
        &context_manager,
    );
    let vault_source = vault_source(config, profile.as_ref(), &workspace, &context_manager);
    let connection = match target.backend.health_check().await {
        Ok(()) => ConnectionSummary {
            state: "connected".into(),
            message: None,
        },
        Err(_) => ConnectionSummary {
            state: "unavailable".into(),
            message: Some("The selected backend is unavailable.".into()),
        },
    };
    let capabilities = CapabilitySummary::from_backend(target.backend.as_ref());
    let project_summary = project.as_ref().map(project_summary);
    let environment_summary = profile.as_ref().map(|profile| EnvironmentSummary {
        name: profile.name.to_string(),
    });
    let sources = ContextSources {
        backend: backend_source,
        vault: vault_source,
        workspace: match workspace.source {
            WorkspaceSource::ProjectToml => ContextSource::ProjectEnvironment,
            WorkspaceSource::Context => ContextSource::GlobalConfig,
            WorkspaceSource::Degenerate => ContextSource::BuiltIn,
        },
        project: if project.is_some() {
            ContextSource::Project
        } else {
            ContextSource::BuiltIn
        },
        environment: profile
            .as_ref()
            .map(|profile| profile.source)
            .unwrap_or(ContextSource::BuiltIn),
    };
    let workspace_summary = WorkspaceSummary {
        alias: target.entry.alias.clone(),
        configured: workspace.is_configured(),
        entries: workspace
            .entries
            .iter()
            .map(|entry| WorkspaceEntrySummary {
                alias: entry.alias.clone(),
                backend: entry.backend.clone(),
                vault: entry.vault.clone(),
                default: entry.default,
            })
            .collect(),
    };
    let context = EffectiveUiContext {
        backend: target.entry.backend.clone(),
        backend_kind: target.backend.kind(),
        vault: target.entry.vault.clone(),
        workspace: workspace_summary,
        project: project_summary,
        environment: environment_summary,
        sources,
        connection,
        capabilities,
        security: SecuritySummary {
            clipboard_timeout_seconds: config.clipboard_timeout,
        },
        version: env!("CARGO_PKG_VERSION"),
    };
    Ok(ResolvedUiContext {
        context,
        backend: target.backend,
    })
}

fn project_summary(project: &ProjectResolution) -> ProjectSummary {
    let directory = project.path.parent().unwrap_or(project.path.as_path());
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("project")
        .to_string();
    ProjectSummary {
        name,
        path: directory.to_path_buf(),
    }
}

fn backend_source(
    effective_source: BackendSelectionSource,
    profile: Option<&ResolvedProfile<'_>>,
    workspace: &Workspace,
    context: &ContextManager,
) -> ContextSource {
    if workspace.is_configured()
        && default_entry_config(workspace, profile, context)
            .and_then(|entry| entry.backend.as_ref())
            .is_some()
    {
        return ContextSource::WorkspaceEntry;
    }
    match effective_source {
        BackendSelectionSource::Cli => ContextSource::Cli,
        BackendSelectionSource::Environment => ContextSource::Environment,
        BackendSelectionSource::ProjectEnvironment => ContextSource::ProjectEnvironment,
        BackendSelectionSource::GlobalConfig => ContextSource::GlobalConfig,
        BackendSelectionSource::BuiltIn => ContextSource::BuiltIn,
    }
}

fn vault_source(
    config: &Config,
    profile: Option<&ResolvedProfile<'_>>,
    workspace: &Workspace,
    context: &ContextManager,
) -> ContextSource {
    if workspace.is_configured() {
        return ContextSource::WorkspaceEntry;
    }
    if profile.is_some_and(|profile| profile.profile.vault.is_some()) {
        ContextSource::ProjectEnvironment
    } else if context.current_vault().is_some()
        || !config.default_vault.is_empty()
        || config
            .local
            .as_ref()
            .and_then(|local| local.default_vault.as_ref())
            .is_some()
    {
        ContextSource::GlobalConfig
    } else {
        ContextSource::BuiltIn
    }
}

fn default_entry_config<'a>(
    workspace: &Workspace,
    profile: Option<&'a ResolvedProfile<'a>>,
    context: &'a ContextManager,
) -> Option<&'a WorkspaceEntryConfig> {
    let entries = match workspace.source {
        WorkspaceSource::ProjectToml => &profile?.profile.vaults,
        WorkspaceSource::Context => &context.workspace.as_ref()?.entries,
        WorkspaceSource::Degenerate => return None,
    };
    let default = workspace.default_entry().ok()?;
    entries
        .iter()
        .find(|entry| entry.resolved_alias() == default.alias)
}

impl CapabilitySummary {
    pub(crate) fn from_backend(backend: &dyn Backend) -> Self {
        let capabilities = backend.capabilities();
        let conditional_conversion = conditional_record_conversion_available(backend);
        Self {
            secrets: true,
            vaults: capabilities.has_vaults,
            files: cfg!(feature = "file-ops") && capabilities.has_file_storage,
            folders: capabilities.has_folders,
            groups: capabilities.has_groups,
            notes: capabilities.has_notes,
            expiry: capabilities.has_expiry,
            soft_delete: capabilities.has_soft_delete,
            restore: capabilities.has_restore,
            purge: capabilities.has_purge,
            scheduled_purge: capabilities.has_scheduled_purge,
            trash: capabilities.has_soft_delete,
            versioning: capabilities.has_versioning,
            rbac: capabilities.has_rbac,
            audit: capabilities.has_audit,
            rotation: capabilities.has_secret_rotation,
            conversion: conditional_conversion,
            conditional_conversion,
            atomic_rename: atomic_rename_available(backend),
            metadata: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    #[test]
    fn conditional_conversion_is_not_advertised_without_revision_validation() {
        let backend = crate::web::testutil::stub::StubBackend::new().without_revision_validation();
        let context = crate::web::testutil::test_context(&backend, "default", 30);

        assert!(!context.capabilities.conversion);
        assert!(!context.capabilities.conditional_conversion);
    }

    use crate::backend::BackendRegistry;
    use crate::config::settings::{Config, LocalConfig, NamedBackendEntry};

    use super::{resolve_ui_context, ContextSource};

    struct Fixture {
        root: tempfile::TempDir,
        config: Config,
        registry: BackendRegistry,
    }

    impl Fixture {
        async fn project_workspace() -> Self {
            let root = tempfile::tempdir().expect("temp root");
            let project = root.path().join("checkout");
            let cwd = project.join("services").join("api");
            tokio::fs::create_dir_all(cwd.join(".xv"))
                .await
                .expect("fixture directories");
            tokio::fs::write(
                project.join(".xv.toml"),
                r#"
default_env = "prod"

[env.prod]
backend = "local"
vault = "ignored-profile-vault"
vaults = [
  { vault = "payments", alias = "work", default = true },
  { vault = "sandbox", backend = "local-b", alias = "stage" },
]
"#,
            )
            .await
            .expect("project config");
            tokio::fs::write(
                cwd.join(".xv").join("context"),
                serde_json::to_vec(&json!({
                    "current": {
                        "vault_name": "ignored-context-vault",
                        "resource_group": "credential-marker",
                        "subscription_id": "token-marker",
                        "storage_container": null,
                        "last_used": "2026-07-22T00:00:00Z",
                        "usage_count": 1
                    },
                    "recent": [],
                    "workspace": {
                        "entries": [{
                            "vault": "ignored-context-workspace",
                            "backend": "local-b",
                            "alias": "personal",
                            "default": true
                        }]
                    }
                }))
                .expect("context json"),
            )
            .await
            .expect("local context");

            let mut named_backends = HashMap::new();
            let name = "local-b";
            named_backends.insert(
                name.to_string(),
                NamedBackendEntry::Local(LocalConfig {
                    store_path: Some(
                        root.path()
                            .join(format!("{name}-store"))
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    key_file: Some(
                        root.path()
                            .join(format!("{name}-key"))
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    default_vault: Some("config-local-vault".into()),
                    encrypt_metadata: None,
                    opaque_filenames: None,
                }),
            );
            let config = Config {
                backend: Some("local".into()),
                disk_backend: Some("local-b".into()),
                subscription_id: "credential-marker".into(),
                tenant_id: "token-marker".into(),
                template: Some("secret-value-marker".into()),
                default_vault: "config-global-vault".into(),
                local: Some(LocalConfig {
                    store_path: Some(
                        root.path()
                            .join("local-store")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    key_file: Some(root.path().join("local-key").to_string_lossy().into_owned()),
                    default_vault: Some("config-local-vault".into()),
                    encrypt_metadata: None,
                    opaque_filenames: None,
                }),
                named_backends,
                clipboard_timeout: 17,
                ..Default::default()
            };
            let registry =
                BackendRegistry::with_lazy(&config, &["local".to_string(), "local-b".to_string()])
                    .expect("lazy registry");

            Self {
                root,
                config,
                registry,
            }
        }

        fn cwd(&self) -> std::path::PathBuf {
            self.root
                .path()
                .join("checkout")
                .join("services")
                .join("api")
        }
    }

    #[tokio::test]
    async fn context_names_every_effective_source_without_secrets() {
        let fixture = Fixture::project_workspace().await;

        let context = resolve_ui_context(&fixture.config, &fixture.registry, &fixture.cwd())
            .await
            .expect("context");
        let json = serde_json::to_value(context).expect("serialize context");

        assert_eq!(json["backend"], "local");
        assert_eq!(json["backend_kind"], "local");
        assert_eq!(json["vault"], "payments");
        assert_eq!(json["workspace"]["alias"], "work");
        assert_eq!(json["workspace"]["entries"].as_array().unwrap().len(), 2);
        assert_eq!(json["project"]["name"], "checkout");
        assert_eq!(json["environment"]["name"], "prod");
        assert_eq!(json["sources"]["backend"], "project-environment");
        assert_eq!(json["sources"]["vault"], "workspace-entry");
        assert_eq!(json["sources"]["workspace"], "project-environment");
        assert_eq!(json["sources"]["project"], "project");
        assert_eq!(json["sources"]["environment"], "project");
        assert_eq!(json["security"]["clipboard_timeout_seconds"], 17);
        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));

        let text = json.to_string().to_ascii_lowercase();
        for forbidden in [
            "credential",
            "token",
            "secret-value-marker",
            "subscription",
            "tenant",
            "diagnostic",
        ] {
            assert!(
                !text.contains(forbidden),
                "serialized context leaked {forbidden}"
            );
        }
    }

    #[tokio::test]
    async fn explicit_workspace_backend_uses_workspace_source_and_resolved_capabilities() {
        let fixture = Fixture::project_workspace().await;
        let project = fixture.root.path().join("checkout");
        tokio::fs::write(
            project.join(".xv.toml"),
            r#"
default_env = "prod"

[env.prod]
backend = "local"
vaults = [
  { vault = "sandbox", backend = "local-b", alias = "stage", default = true },
]
"#,
        )
        .await
        .expect("replace project config");

        let context = resolve_ui_context(&fixture.config, &fixture.registry, &fixture.cwd())
            .await
            .expect("context");

        assert_eq!(context.backend, "local-b");
        assert_eq!(context.backend_kind.to_string(), "local");
        assert_eq!(context.sources.backend, ContextSource::WorkspaceEntry);
        assert!(context.capabilities.folders);
        assert!(context.capabilities.groups);
        assert!(context.capabilities.notes);
        assert!(context.capabilities.expiry);
    }

    #[tokio::test]
    async fn context_route_serializes_the_effective_model_with_existing_guards() {
        let app = crate::web::build_router(crate::web::testutil::test_state());
        let response = app
            .oneshot(
                Request::get("/api/context")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(json["backend"], "stub");
        assert_eq!(json["backend_kind"], "local");
        assert_eq!(json["vault"], "default");
        assert_eq!(json["workspace"]["alias"], "default");
        assert_eq!(json["sources"]["vault"], "built-in");
        assert_eq!(json["connection"]["state"], "connected");
        assert_eq!(json["capabilities"]["conditional_conversion"], true);
        assert_eq!(json["capabilities"]["atomic_rename"], true);
        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn candidate_context_is_display_safe_and_workspace_activation_is_atomic() {
        let app = crate::web::build_router(crate::web::testutil::test_state());
        let request = json!({
            "alias": "default",
            "backend": "stub",
            "vault": "default"
        });
        let (status, body) = crate::web::api::tests::get_json(
            app.clone(),
            "POST",
            "/api/context/activate",
            Some(request.clone()),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["context"]["backend"], "stub");
        assert_eq!(body["context"]["vault"], "default");
        assert!(body.get("secrets").is_none());
        assert!(!body.to_string().contains("\"name\":"));

        let (status, body) = crate::web::api::tests::get_json(
            app,
            "POST",
            "/api/workspaces/activate",
            Some(request),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["context"]["backend"], "stub");
        assert_eq!(body["context"]["vault"], "default");
        assert_eq!(body["secrets"], json!([]));
    }

    #[tokio::test]
    async fn scoped_destructive_write_uses_the_requested_attached_backend() {
        use std::sync::Arc;

        use crate::secret::manager::SecretRequest;
        use crate::web::testutil::stub::StubBackend;
        use zeroize::Zeroizing;

        fn request(name: &str) -> SecretRequest {
            SecretRequest {
                name: name.into(),
                value: Zeroizing::new("protected".into()),
                content_type: None,
                enabled: Some(true),
                expires_on: None,
                not_before: None,
                tags: None,
                groups: None,
                note: None,
                folder: None,
            }
        }

        let primary = Arc::new(StubBackend::with_capabilities(
            "primary",
            Default::default(),
        ));
        let stage = Arc::new(StubBackend::with_capabilities("stage", Default::default()));
        primary
            .secrets
            .lock()
            .unwrap()
            .insert("victim".into(), request("victim"));
        stage
            .secrets
            .lock()
            .unwrap()
            .insert("victim".into(), request("victim"));
        let primary_backend: Arc<dyn crate::backend::Backend> = primary.clone();
        let stage_backend: Arc<dyn crate::backend::Backend> = stage.clone();
        let registry = Arc::new(BackendRegistry::for_test(
            "primary",
            vec![
                ("primary", primary_backend.clone()),
                ("stage", stage_backend),
            ],
        ));
        let mut context =
            crate::web::testutil::test_context(primary_backend.as_ref(), "payments", 30);
        context.workspace.configured = true;
        context.workspace.alias = "work".into();
        context.workspace.entries = vec![
            super::WorkspaceEntrySummary {
                alias: "work".into(),
                backend: "primary".into(),
                vault: "payments".into(),
                default: true,
            },
            super::WorkspaceEntrySummary {
                alias: "stage".into(),
                backend: "stage".into(),
                vault: "sandbox".into(),
                default: false,
            },
        ];
        let root = tempfile::tempdir().expect("preferences");
        let state = Arc::new(crate::web::WebState::new(
            primary_backend,
            context,
            "test-token".into(),
            crate::records::builtin_types(),
            crate::web::preferences::PreferenceStore::new(root.path().join("ui.json"), 30),
            registry,
        ));
        let app = crate::web::build_router(state);

        let (status, _) = crate::web::api::tests::get_json(
            app,
            "DELETE",
            "/api/secrets/victim?alias=stage&backend=stage&vault=sandbox",
            None,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(primary.secrets.lock().unwrap().contains_key("victim"));
        assert!(!stage.secrets.lock().unwrap().contains_key("victim"));
    }

    #[tokio::test]
    async fn partial_or_unattached_request_scope_never_falls_back() {
        let app = crate::web::build_router(crate::web::testutil::test_state());

        let (partial_status, partial) = crate::web::api::tests::get_json(
            app.clone(),
            "GET",
            "/api/secrets?vault=default",
            None,
        )
        .await;
        assert_eq!(partial_status, StatusCode::BAD_REQUEST);
        assert_eq!(partial["error"]["field"], "workspace");

        let (wrong_status, wrong) = crate::web::api::tests::get_json(
            app,
            "GET",
            "/api/secrets?alias=default&backend=other&vault=default",
            None,
        )
        .await;
        assert_eq!(wrong_status, StatusCode::BAD_REQUEST);
        assert_eq!(wrong["error"]["field"], "workspace");
    }

    #[tokio::test]
    async fn delayed_out_of_order_workspace_responses_are_tab_safe_and_leave_no_server_state() {
        use std::sync::Arc;
        use std::time::Duration;

        use crate::web::testutil::stub::StubBackend;

        let primary: Arc<dyn crate::backend::Backend> = Arc::new(StubBackend::with_capabilities(
            "primary",
            Default::default(),
        ));
        let delayed: Arc<dyn crate::backend::Backend> = Arc::new(StubBackend::with_list_delay(
            "stage",
            Duration::from_millis(40),
        ));
        let registry = Arc::new(BackendRegistry::for_test(
            "primary",
            vec![("primary", primary.clone()), ("stage", delayed)],
        ));
        let mut context = crate::web::testutil::test_context(primary.as_ref(), "payments", 30);
        context.workspace.configured = true;
        context.workspace.alias = "work".into();
        context.workspace.entries = vec![
            super::WorkspaceEntrySummary {
                alias: "work".into(),
                backend: "primary".into(),
                vault: "payments".into(),
                default: true,
            },
            super::WorkspaceEntrySummary {
                alias: "stage".into(),
                backend: "stage".into(),
                vault: "sandbox".into(),
                default: false,
            },
        ];
        let root = tempfile::tempdir().expect("preferences");
        let state = Arc::new(crate::web::WebState::new(
            primary,
            context,
            "test-token".into(),
            crate::records::builtin_types(),
            crate::web::preferences::PreferenceStore::new(root.path().join("ui.json"), 30),
            registry,
        ));
        let app = crate::web::build_router(state);

        let delayed_tab = tokio::spawn(crate::web::api::tests::get_json(
            app.clone(),
            "POST",
            "/api/workspaces/activate",
            Some(json!({"alias":"stage","backend":"stage","vault":"sandbox"})),
        ));
        tokio::time::sleep(Duration::from_millis(5)).await;
        let (_, primary_tab) = crate::web::api::tests::get_json(
            app.clone(),
            "POST",
            "/api/workspaces/activate",
            Some(json!({"alias":"work","backend":"primary","vault":"payments"})),
        )
        .await;
        assert_eq!(primary_tab["context"]["backend"], "primary");
        assert_eq!(primary_tab["context"]["vault"], "payments");

        let (_, late_response) = delayed_tab.await.expect("delayed tab response");
        assert_eq!(late_response["context"]["backend"], "stage");
        assert_eq!(late_response["context"]["vault"], "sandbox");

        // Dropping or losing either activation response cannot mutate shared
        // process state: a new tab still starts from the resolved base context.
        let (_, fresh_tab) =
            crate::web::api::tests::get_json(app, "GET", "/api/context", None).await;
        assert_eq!(fresh_tab["backend"], "primary");
        assert_eq!(fresh_tab["vault"], "payments");
    }

    #[tokio::test]
    async fn activation_list_failure_preserves_the_previous_routed_target() {
        use std::sync::Arc;

        use crate::web::testutil::stub::StubBackend;

        let primary: Arc<dyn crate::backend::Backend> = Arc::new(StubBackend::with_capabilities(
            "primary",
            Default::default(),
        ));
        let failing: Arc<dyn crate::backend::Backend> = Arc::new(StubBackend::with_list_error(
            "stage",
            "safe fixture failure",
        ));
        let registry = Arc::new(BackendRegistry::for_test(
            "primary",
            vec![("primary", primary.clone()), ("stage", failing)],
        ));
        let mut context = crate::web::testutil::test_context(primary.as_ref(), "payments", 30);
        context.workspace.configured = true;
        context.workspace.alias = "work".into();
        context.workspace.entries = vec![
            super::WorkspaceEntrySummary {
                alias: "work".into(),
                backend: "primary".into(),
                vault: "payments".into(),
                default: true,
            },
            super::WorkspaceEntrySummary {
                alias: "stage".into(),
                backend: "stage".into(),
                vault: "sandbox".into(),
                default: false,
            },
        ];
        let root = tempfile::tempdir().expect("preferences");
        let state = Arc::new(crate::web::WebState::new(
            primary,
            context,
            "test-token".into(),
            crate::records::builtin_types(),
            crate::web::preferences::PreferenceStore::new(root.path().join("ui.json"), 30),
            registry,
        ));
        let app = crate::web::build_router(state);

        let (status, _) = crate::web::api::tests::get_json(
            app.clone(),
            "POST",
            "/api/workspaces/activate",
            Some(json!({"alias":"stage","backend":"stage","vault":"sandbox"})),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        let (_, context) =
            crate::web::api::tests::get_json(app.clone(), "GET", "/api/context", None).await;
        assert_eq!(context["backend"], "primary");
        assert_eq!(context["vault"], "payments");
        let (status, _) = crate::web::api::tests::get_json(
            app,
            "PUT",
            "/api/secrets/still-primary",
            Some(json!({"value":"not persisted in context"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn capabilities_come_from_workspace_backend_not_registry_default() {
        use std::sync::Arc;

        use crate::backend::BackendCapabilities;
        use crate::web::testutil::stub::StubBackend;

        let root = tempfile::tempdir().expect("temp root");
        let cwd = root.path().join("limited-project");
        tokio::fs::create_dir_all(cwd.join(".xv"))
            .await
            .expect("fixture directories");
        tokio::fs::write(
            cwd.join(".xv.toml"),
            r#"
default_env = "dev"

[env.dev]
vaults = [
  { vault = "limited-vault", backend = "limited", alias = "scope", default = true },
]
"#,
        )
        .await
        .expect("project config");
        tokio::fs::write(
            cwd.join(".xv").join("context"),
            br#"{"current":null,"recent":[],"workspace":null}"#,
        )
        .await
        .expect("isolated context");

        let full_caps = BackendCapabilities {
            has_folders: true,
            has_groups: true,
            ..Default::default()
        };
        let limited_caps = BackendCapabilities::default();
        let full: Arc<dyn crate::backend::Backend> =
            Arc::new(StubBackend::with_capabilities("full", full_caps));
        let limited: Arc<dyn crate::backend::Backend> =
            Arc::new(StubBackend::with_capabilities("limited", limited_caps));
        let registry =
            BackendRegistry::for_test("full", vec![("full", full.clone()), ("limited", limited)]);

        let named = NamedBackendEntry::Local(LocalConfig {
            store_path: None,
            key_file: None,
            default_vault: None,
            encrypt_metadata: None,
            opaque_filenames: None,
        });
        let config = Config {
            backend: Some("full".into()),
            disk_backend: Some("full".into()),
            named_backends: HashMap::from([
                ("full".into(), named.clone()),
                ("limited".into(), named),
            ]),
            ..Default::default()
        };

        let context = resolve_ui_context(&config, &registry, &cwd)
            .await
            .expect("context");

        assert!(registry.active().capabilities().has_folders);
        assert_eq!(context.backend, "limited");
        assert!(!context.capabilities.folders);
        assert!(!context.capabilities.groups);
        assert!(!context.capabilities.conditional_conversion);
        assert!(!context.capabilities.atomic_rename);
    }

    #[tokio::test]
    async fn folded_default_backend_is_still_named_as_built_in_source() {
        use std::sync::Arc;

        use crate::web::testutil::stub::StubBackend;

        let root = tempfile::tempdir().expect("temp root");
        let cwd = root.path().join("plain-directory");
        tokio::fs::create_dir_all(cwd.join(".xv"))
            .await
            .expect("fixture directories");
        tokio::fs::write(
            cwd.join(".xv").join("context"),
            br#"{"current":null,"recent":[],"workspace":null}"#,
        )
        .await
        .expect("isolated context");
        let backend: Arc<dyn crate::backend::Backend> =
            Arc::new(StubBackend::with_capabilities("azure", Default::default()));
        let registry = BackendRegistry::for_test("azure", vec![("azure", backend)]);
        let config = Config {
            // main.rs folds the built-in default into this field before the
            // web command starts; disk_backend retains the absent source.
            backend: Some("azure".into()),
            disk_backend: None,
            pre_flag_backend: Some("azure".into()),
            default_vault: "configured-vault".into(),
            ..Default::default()
        };

        let context = resolve_ui_context(&config, &registry, &cwd)
            .await
            .expect("context");

        assert!(context.project.is_none());
        assert!(context.environment.is_none());
        assert_eq!(context.sources.backend, ContextSource::BuiltIn);
        assert_eq!(context.sources.vault, ContextSource::GlobalConfig);
        assert_eq!(context.sources.workspace, ContextSource::BuiltIn);
    }

    #[tokio::test]
    async fn connection_summary_never_serializes_backend_diagnostics() {
        use std::sync::Arc;

        use crate::web::testutil::stub::StubBackend;

        let root = tempfile::tempdir().expect("temp root");
        let cwd = root.path().join("connection-project");
        tokio::fs::create_dir_all(cwd.join(".xv"))
            .await
            .expect("fixture directories");
        tokio::fs::write(
            cwd.join(".xv").join("context"),
            br#"{"current":null,"recent":[],"workspace":null}"#,
        )
        .await
        .expect("isolated context");
        let backend: Arc<dyn crate::backend::Backend> = Arc::new(StubBackend::with_health_error(
            "local",
            "diagnostic-token-marker",
        ));
        let registry = BackendRegistry::for_test("local", vec![("local", backend)]);
        let config = Config {
            backend: Some("local".into()),
            disk_backend: Some("local".into()),
            ..Default::default()
        };

        let context = resolve_ui_context(&config, &registry, &cwd)
            .await
            .expect("context still resolves");
        let json = serde_json::to_string(&context).expect("serialize");

        assert_eq!(context.connection.state, "unavailable");
        assert_eq!(
            context.connection.message.as_deref(),
            Some("The selected backend is unavailable.")
        );
        assert!(!json.contains("diagnostic-token-marker"));
    }

    #[tokio::test]
    async fn cli_backend_precedes_project_environment_for_implicit_workspace_backend() {
        let mut fixture = Fixture::project_workspace().await;
        fixture.config.backend = Some("local-b".into());
        fixture.config.cli_backend = Some("local-b".into());
        fixture.config.cli_backend_was_arg = true;

        let context = resolve_ui_context(&fixture.config, &fixture.registry, &fixture.cwd())
            .await
            .expect("context");

        assert_eq!(context.backend, "local-b");
        assert_eq!(context.sources.backend, ContextSource::Cli);
        assert_eq!(context.sources.vault, ContextSource::WorkspaceEntry);
        assert_eq!(context.environment.as_ref().unwrap().name, "prod");
    }

    #[tokio::test]
    async fn context_workspace_precedes_single_project_vault_but_project_workspace_precedes_context(
    ) {
        let fixture = Fixture::project_workspace().await;
        let project_file = fixture.root.path().join("checkout").join(".xv.toml");
        tokio::fs::write(
            &project_file,
            r#"
default_env = "prod"

[env.prod]
backend = "local"
vault = "project-vault"
"#,
        )
        .await
        .expect("single-vault project config");

        let from_context = resolve_ui_context(&fixture.config, &fixture.registry, &fixture.cwd())
            .await
            .expect("context workspace");
        assert_eq!(from_context.vault, "ignored-context-workspace");
        assert_eq!(from_context.workspace.alias, "personal");
        assert_eq!(from_context.sources.vault, ContextSource::WorkspaceEntry);
        assert_eq!(from_context.sources.workspace, ContextSource::GlobalConfig);

        tokio::fs::write(
            fixture.cwd().join(".xv").join("context"),
            serde_json::to_vec(&json!({
                "current": {
                    "vault_name": "context-vault",
                    "resource_group": null,
                    "subscription_id": null,
                    "storage_container": null,
                    "last_used": "2026-07-22T00:00:00Z",
                    "usage_count": 1
                },
                "recent": [],
                "workspace": null
            }))
            .expect("context json"),
        )
        .await
        .expect("context without workspace");

        let from_profile = resolve_ui_context(&fixture.config, &fixture.registry, &fixture.cwd())
            .await
            .expect("project profile");
        assert_eq!(from_profile.backend, "local");
        assert_eq!(from_profile.vault, "project-vault");
        assert!(!from_profile.workspace.configured);
        assert_eq!(
            from_profile.sources.backend,
            ContextSource::ProjectEnvironment
        );
        assert_eq!(
            from_profile.sources.vault,
            ContextSource::ProjectEnvironment
        );
    }

    #[tokio::test]
    async fn desktop_raw_config_folds_project_backend_before_workspace_and_route_state() {
        use std::sync::Arc;

        use crate::backend::{BackendCapabilities, BackendKind};
        use crate::web::testutil::stub::StubBackend;

        let root = tempfile::tempdir().expect("temp root");
        let cwd = root.path().join("desktop-project");
        tokio::fs::create_dir_all(cwd.join(".xv"))
            .await
            .expect("fixture directories");
        tokio::fs::write(
            cwd.join(".xv.toml"),
            r#"
default_env = "dev"

[env.dev]
backend = "local"
vault = "desktop-vault"
vaults = [
  { vault = "desktop-vault", alias = "work", default = true },
]
"#,
        )
        .await
        .expect("project config");
        tokio::fs::write(
            cwd.join(".xv").join("context"),
            br#"{"current":null,"recent":[],"workspace":null}"#,
        )
        .await
        .expect("isolated context");

        let global_backend = Arc::new(StubBackend::with_capabilities(
            "azure",
            BackendCapabilities {
                has_folders: false,
                ..Default::default()
            },
        ));
        let registry = BackendRegistry::for_test(
            "azure",
            vec![(
                "azure",
                global_backend.clone() as Arc<dyn crate::backend::Backend>,
            )],
        );
        let raw_config = Config {
            backend: Some("azure".into()),
            // Intentionally incomplete Azure configuration. Desktop must
            // select and validate the project-local backend before trying it.
            subscription_id: String::new(),
            tenant_id: String::new(),
            local: Some(LocalConfig {
                store_path: Some(root.path().join("store").to_string_lossy().into_owned()),
                key_file: Some(root.path().join("key").to_string_lossy().into_owned()),
                default_vault: Some("desktop-vault".into()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
            ..Default::default()
        };

        let startup = crate::config::project::resolve_effective_backend_config(&raw_config, &cwd)
            .await
            .expect("effective startup config");
        startup
            .config
            .validate()
            .expect("selected local config validates before incomplete Azure");
        let startup_registry = BackendRegistry::from_config(&startup.config)
            .expect("selected backend registry exists");
        assert_eq!(startup_registry.active().kind(), BackendKind::Local);

        let resolved = super::resolve_ui_context_and_backend(&raw_config, &registry, &cwd)
            .await
            .expect("project-local context");

        assert_eq!(resolved.context.backend, "local");
        assert_eq!(resolved.context.backend_kind, BackendKind::Local);
        assert_eq!(resolved.context.vault, "desktop-vault");
        assert_eq!(
            resolved.context.sources.backend,
            ContextSource::ProjectEnvironment
        );
        assert!(resolved.context.capabilities.folders);
        assert_eq!(resolved.backend.name(), "local");
        assert_eq!(global_backend.secrets.lock().unwrap().len(), 0);

        let routed_backend = resolved.backend;
        let routed_registry = Arc::new(BackendRegistry::new(routed_backend.clone()));
        let state = Arc::new(crate::web::WebState::new(
            routed_backend,
            resolved.context,
            "test-token".into(),
            crate::records::builtin_types(),
            crate::web::preferences::PreferenceStore::new(root.path().join("ui.json"), 30),
            routed_registry,
        ));
        let app = crate::web::build_router(state);
        let (status, _) = crate::web::api::tests::get_json(
            app.clone(),
            "PUT",
            "/api/secrets/desktop-proof",
            Some(json!({"value": "route-used-project-backend"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, secrets) = crate::web::api::tests::get_json(app, "GET", "/api/secrets", None).await;
        assert_eq!(secrets[0]["name"], "desktop-proof");
        assert_eq!(global_backend.secrets.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn desktop_raw_config_folds_project_backend_for_single_project_vault() {
        use std::sync::Arc;

        use crate::backend::BackendKind;
        use crate::web::testutil::stub::StubBackend;

        let root = tempfile::tempdir().expect("temp root");
        let cwd = root.path().join("single-vault-project");
        tokio::fs::create_dir_all(cwd.join(".xv"))
            .await
            .expect("fixture directories");
        tokio::fs::write(
            cwd.join(".xv.toml"),
            r#"
default_env = "dev"

[env.dev]
backend = "local"
vault = "single-vault"
"#,
        )
        .await
        .expect("project config");
        tokio::fs::write(
            cwd.join(".xv").join("context"),
            br#"{"current":null,"recent":[],"workspace":null}"#,
        )
        .await
        .expect("isolated context");
        let registry = BackendRegistry::for_test(
            "azure",
            vec![(
                "azure",
                Arc::new(StubBackend::new()) as Arc<dyn crate::backend::Backend>,
            )],
        );
        let raw_config = Config {
            backend: Some("azure".into()),
            local: Some(LocalConfig {
                store_path: Some(root.path().join("store").to_string_lossy().into_owned()),
                key_file: Some(root.path().join("key").to_string_lossy().into_owned()),
                default_vault: Some("single-vault".into()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
            ..Default::default()
        };

        let resolved = super::resolve_ui_context_and_backend(&raw_config, &registry, &cwd)
            .await
            .expect("single project vault");

        assert_eq!(resolved.context.backend, "local");
        assert_eq!(resolved.context.backend_kind, BackendKind::Local);
        assert_eq!(resolved.context.vault, "single-vault");
        assert!(!resolved.context.workspace.configured);
        assert_eq!(
            resolved.context.sources.backend,
            ContextSource::ProjectEnvironment
        );
        assert_eq!(
            resolved.context.sources.vault,
            ContextSource::ProjectEnvironment
        );
        assert_eq!(resolved.backend.name(), "local");
    }
}
