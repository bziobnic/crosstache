// The command ABI intentionally returns the complete, display-safe recovery
// model so the frontend can render stable codes, hints, and redacted details.
#![allow(clippy::result_large_err)]

use crosstache::config::setup::{
    build_setup_config, setup_and_save, SetupOutcome, SetupPreview, SetupRequest,
};
use crosstache::config::{load_config_no_validation, Config};
use crosstache::error::{CrosstacheError, SafeSetupError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, State, WebviewWindow};

const STARTUP_STATE_EVENT: &str = "xv://startup-state";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StartupState {
    LoadingConfiguration,
    Connecting,
    SetupRequired,
    RecoverableFailure,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupEvent {
    ConfigMissing,
    ConfigLoaded,
    SetupVerified,
    Connected,
    StartupFailed,
    RetryRequested,
}

pub fn transition(state: StartupState, event: StartupEvent) -> StartupState {
    match (state, event) {
        (StartupState::LoadingConfiguration, StartupEvent::ConfigMissing) => {
            StartupState::SetupRequired
        }
        (StartupState::LoadingConfiguration, StartupEvent::ConfigLoaded)
        | (StartupState::SetupRequired, StartupEvent::SetupVerified)
        | (StartupState::RecoverableFailure, StartupEvent::SetupVerified) => {
            StartupState::Connecting
        }
        (StartupState::Connecting, StartupEvent::Connected) => StartupState::Ready,
        (
            StartupState::LoadingConfiguration
            | StartupState::Connecting
            | StartupState::SetupRequired
            | StartupState::RecoverableFailure
            | StartupState::Ready,
            StartupEvent::StartupFailed,
        ) => StartupState::RecoverableFailure,
        (StartupState::RecoverableFailure, StartupEvent::RetryRequested) => {
            StartupState::LoadingConfiguration
        }
        _ => state,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupSnapshot {
    #[serde(rename = "kind")]
    pub state: StartupState,
    pub config_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SafeSetupError>,
}

impl StartupSnapshot {
    pub fn loading(config_path: impl Into<String>) -> Self {
        Self {
            state: StartupState::LoadingConfiguration,
            config_path: config_path.into(),
            backend: None,
            vault: None,
            error: None,
        }
    }

    pub fn connecting(
        backend: impl Into<String>,
        vault: impl Into<String>,
        config_path: impl Into<String>,
    ) -> Self {
        Self {
            state: StartupState::Connecting,
            config_path: config_path.into(),
            backend: Some(backend.into()),
            vault: Some(vault.into()),
            error: None,
        }
    }

    pub fn setup_required(config_path: impl Into<String>) -> Self {
        Self {
            state: StartupState::SetupRequired,
            config_path: config_path.into(),
            backend: None,
            vault: None,
            error: None,
        }
    }

    pub fn recoverable(error: SafeSetupError, config_path: impl Into<String>) -> Self {
        Self {
            state: StartupState::RecoverableFailure,
            config_path: config_path.into(),
            backend: Some(error.backend.clone()),
            vault: Some(error.vault.clone()),
            error: Some(error),
        }
    }

    pub fn ready(
        backend: impl Into<String>,
        vault: impl Into<String>,
        config_path: impl Into<String>,
    ) -> Self {
        Self {
            state: StartupState::Ready,
            config_path: config_path.into(),
            backend: Some(backend.into()),
            vault: Some(vault.into()),
            error: None,
        }
    }
}

struct StartupData {
    generation: u64,
    snapshot: StartupSnapshot,
    active_operation: Option<StartupOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupOperation {
    Startup,
    Setup,
}

pub struct StartupStore(Mutex<StartupData>);

impl StartupStore {
    pub fn new(config_path: String) -> Self {
        Self(Mutex::new(StartupData {
            generation: 0,
            snapshot: StartupSnapshot::loading(config_path),
            active_operation: None,
        }))
    }

    pub fn from_environment() -> Self {
        let path = Config::get_config_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        Self::new(path)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StartupData> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn snapshot(&self) -> StartupSnapshot {
        self.lock().snapshot.clone()
    }

    pub fn begin_attempt(&self) -> u64 {
        let mut data = self.lock();
        data.generation = data.generation.wrapping_add(1);
        let config_path = data.snapshot.config_path.clone();
        data.snapshot = StartupSnapshot::loading(config_path);
        data.active_operation = Some(StartupOperation::Startup);
        data.generation
    }

    fn begin_retry_attempt(&self) -> Option<u64> {
        let mut data = self.lock();
        if data.active_operation.is_some()
            || data.snapshot.state != StartupState::RecoverableFailure
            || transition(data.snapshot.state, StartupEvent::RetryRequested)
                != StartupState::LoadingConfiguration
        {
            return None;
        }
        data.generation = data.generation.wrapping_add(1);
        let config_path = data.snapshot.config_path.clone();
        data.snapshot = StartupSnapshot::loading(config_path);
        data.active_operation = Some(StartupOperation::Startup);
        Some(data.generation)
    }

    fn begin_setup_attempt(&self) -> Option<u64> {
        let mut data = self.lock();
        if data.active_operation.is_some()
            || !matches!(
                data.snapshot.state,
                StartupState::SetupRequired | StartupState::RecoverableFailure
            )
        {
            return None;
        }
        data.generation = data.generation.wrapping_add(1);
        data.active_operation = Some(StartupOperation::Setup);
        Some(data.generation)
    }

    fn advance(&self, generation: u64, event: StartupEvent, snapshot: StartupSnapshot) -> bool {
        let mut data = self.lock();
        if data.generation != generation || transition(data.snapshot.state, event) != snapshot.state
        {
            return false;
        }
        data.snapshot = snapshot;
        if is_terminal(data.snapshot.state) {
            data.active_operation = None;
        }
        true
    }
}

fn is_terminal(state: StartupState) -> bool {
    matches!(
        state,
        StartupState::SetupRequired | StartupState::RecoverableFailure | StartupState::Ready
    )
}

pub fn request_scope(request: &SetupRequest) -> (&'static str, &str) {
    match request {
        SetupRequest::Local { vault, .. } => ("local", vault),
        SetupRequest::Azure { vault, .. } => ("azure", vault),
        SetupRequest::Aws { vault_prefix, .. } => ("aws", vault_prefix),
    }
}

pub fn diagnostics_for_copy(snapshot: &StartupSnapshot) -> Option<&str> {
    snapshot
        .error
        .as_ref()
        .map(|error| error.diagnostics.as_str())
}

pub fn can_navigate(state: StartupState) -> bool {
    state == StartupState::Ready
}

fn config_path() -> Result<PathBuf, SafeSetupError> {
    Config::get_config_path().map_err(|error| safe_error("load-config", "unknown", "", &error))
}

fn safe_error(
    operation: &str,
    backend: &str,
    vault: &str,
    error: &CrosstacheError,
) -> SafeSetupError {
    SafeSetupError::from_error(operation, backend, vault, error)
}

fn generic_safe_error(operation: &str, diagnostic: &str) -> SafeSetupError {
    let mut error = SafeSetupError::from_message(diagnostic);
    error.operation = operation.into();
    error
}

fn emit_snapshot(window: &WebviewWindow, snapshot: &StartupSnapshot) {
    let _ = window.emit(STARTUP_STATE_EVENT, snapshot);
}

fn advance_and_emit(
    window: &WebviewWindow,
    store: &StartupStore,
    generation: u64,
    event: StartupEvent,
    snapshot: StartupSnapshot,
) -> bool {
    if !store.advance(generation, event, snapshot.clone()) {
        return false;
    }
    emit_snapshot(window, &snapshot);
    true
}

fn publish_failure(
    window: &WebviewWindow,
    store: &StartupStore,
    generation: u64,
    config_path: &Path,
    error: SafeSetupError,
) {
    let snapshot = StartupSnapshot::recoverable(error, config_path.display().to_string());
    let _ = advance_and_emit(
        window,
        store,
        generation,
        StartupEvent::StartupFailed,
        snapshot,
    );
}

pub fn project_directory() -> Result<Option<PathBuf>, SafeSetupError> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--project" {
            return args.next().map(PathBuf::from).map(Some).ok_or_else(|| {
                generic_safe_error(
                    "load-project",
                    "The desktop --project option did not include a directory.",
                )
            });
        }
    }

    Ok(std::env::var_os("XV_DESKTOP_PROJECT").map(PathBuf::from))
}

async fn setup_base(path: &Path) -> Config {
    if path.exists() {
        load_config_no_validation().await.unwrap_or_default()
    } else {
        Config::default()
    }
}

#[tauri::command]
pub fn startup_status(state: State<'_, StartupStore>) -> StartupSnapshot {
    state.snapshot()
}

#[tauri::command]
pub async fn preview_setup(request: SetupRequest) -> Result<SetupPreview, SafeSetupError> {
    let (backend, vault) = request_scope(&request);
    let path = config_path()?;
    let candidate = build_setup_config(&request, setup_base(&path).await)
        .map_err(|error| safe_error("preview-setup", backend, vault, &error))?;

    Ok(SetupPreview {
        backend: candidate.effective_backend_name().into(),
        vault: candidate.default_vault,
    })
}

#[tauri::command]
pub async fn apply_setup(
    request: SetupRequest,
    window: WebviewWindow,
    state: State<'_, StartupStore>,
) -> Result<SetupOutcome, SafeSetupError> {
    let (backend, vault) = request_scope(&request);
    let backend = backend.to_string();
    let vault = vault.to_string();
    let path = config_path()?;
    let generation = state.begin_setup_attempt().ok_or_else(|| {
        generic_safe_error(
            "apply-setup",
            "Setup can only begin from setup or recovery, with no operation in progress.",
        )
    })?;
    let base = setup_base(&path).await;

    let outcome = match setup_and_save(request, base, &path).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let safe = safe_error("apply-setup", &backend, &vault, &error);
            publish_failure(&window, &state, generation, &path, safe.clone());
            return Err(safe);
        }
    };

    let connecting = StartupSnapshot::connecting(&backend, &vault, path.display().to_string());
    if !advance_and_emit(
        &window,
        &state,
        generation,
        StartupEvent::SetupVerified,
        connecting,
    ) {
        return Err(generic_safe_error(
            "apply-setup",
            "The setup attempt was superseded.",
        ));
    }

    connect_saved_configuration(&window, &state, generation, &path, &backend, &vault).await?;
    Ok(outcome)
}

#[tauri::command]
pub async fn retry_startup(
    window: WebviewWindow,
    state: State<'_, StartupStore>,
) -> Result<StartupSnapshot, SafeSetupError> {
    let generation = state.begin_retry_attempt().ok_or_else(|| {
        generic_safe_error(
            "retry-startup",
            "Retry can only begin from recovery, with no operation in progress.",
        )
    })?;
    let loading = state.snapshot();
    emit_snapshot(&window, &loading);
    run_startup_attempt(window, &state, generation).await
}

#[tauri::command]
pub fn open_config() -> Result<(), SafeSetupError> {
    let path = config_path()?;
    opener::open(path).map_err(|_| {
        generic_safe_error("open-config", "The configuration file could not be opened.")
    })
}

#[tauri::command]
pub fn copy_diagnostics(state: State<'_, StartupStore>) -> Result<(), SafeSetupError> {
    let diagnostics = diagnostics_for_copy(&state.snapshot())
        .map(str::to_owned)
        .ok_or_else(|| {
            generic_safe_error("copy-diagnostics", "No recovery diagnostics are available.")
        })?;
    let mut clipboard = arboard::Clipboard::new().map_err(|_| {
        generic_safe_error("copy-diagnostics", "The system clipboard is not available.")
    })?;
    clipboard.set_text(diagnostics).map_err(|_| {
        generic_safe_error(
            "copy-diagnostics",
            "The diagnostics could not be copied to the system clipboard.",
        )
    })
}

async fn connect_saved_configuration(
    window: &WebviewWindow,
    store: &StartupStore,
    generation: u64,
    path: &Path,
    backend: &str,
    vault: &str,
) -> Result<StartupSnapshot, SafeSetupError> {
    let config = match load_config_no_validation().await {
        Ok(config) => config,
        Err(error) => {
            let safe = safe_error("load-config", backend, vault, &error);
            publish_failure(window, store, generation, path, safe.clone());
            return Err(safe);
        }
    };
    connect_configuration(window, store, generation, path, config, backend, vault).await
}

async fn connect_configuration(
    window: &WebviewWindow,
    store: &StartupStore,
    generation: u64,
    path: &Path,
    config: Config,
    backend: &str,
    vault: &str,
) -> Result<StartupSnapshot, SafeSetupError> {
    let server = match crosstache::web::prepare_web(config, None, None).await {
        Ok(server) => server,
        Err(error) => {
            let safe = safe_error("connect", backend, vault, &error);
            publish_failure(window, store, generation, path, safe.clone());
            return Err(safe);
        }
    };
    let url = match server.url().parse() {
        Ok(url) => url,
        Err(_) => {
            let safe = generic_safe_error(
                "connect",
                "The embedded UI returned an invalid local address.",
            );
            publish_failure(window, store, generation, path, safe.clone());
            return Err(safe);
        }
    };
    let ready = StartupSnapshot::ready(backend, vault, path.display().to_string());
    if !advance_and_emit(
        window,
        store,
        generation,
        StartupEvent::Connected,
        ready.clone(),
    ) {
        return Err(generic_safe_error(
            "connect",
            "The startup attempt was superseded.",
        ));
    }

    debug_assert!(can_navigate(ready.state));
    if window.navigate(url).is_err() {
        let safe = generic_safe_error("connect", "The embedded UI could not be opened.");
        publish_failure(window, store, generation, path, safe.clone());
        return Err(safe);
    }

    tauri::async_runtime::spawn(async move {
        if server.serve().await.is_err() {
            eprintln!("xv desktop embedded server stopped unexpectedly");
        }
    });
    Ok(ready)
}

pub async fn run_startup(
    window: WebviewWindow,
    store: &StartupStore,
) -> Result<StartupSnapshot, SafeSetupError> {
    let generation = store.begin_attempt();
    let loading = store.snapshot();
    emit_snapshot(&window, &loading);
    run_startup_attempt(window, store, generation).await
}

async fn run_startup_attempt(
    window: WebviewWindow,
    store: &StartupStore,
    generation: u64,
) -> Result<StartupSnapshot, SafeSetupError> {
    let path = match config_path() {
        Ok(path) => path,
        Err(error) => {
            let fallback = PathBuf::new();
            publish_failure(&window, store, generation, &fallback, error.clone());
            return Err(error);
        }
    };

    let project = match project_directory() {
        Ok(project) => project,
        Err(error) => {
            publish_failure(&window, store, generation, &path, error.clone());
            return Err(error);
        }
    };
    if let Some(project) = project {
        if std::env::set_current_dir(&project).is_err() {
            let safe = generic_safe_error(
                "load-project",
                "The configured project directory could not be used.",
            );
            publish_failure(&window, store, generation, &path, safe.clone());
            return Err(safe);
        }
    }

    if !path.exists() {
        let required = StartupSnapshot::setup_required(path.display().to_string());
        if !advance_and_emit(
            &window,
            store,
            generation,
            StartupEvent::ConfigMissing,
            required.clone(),
        ) {
            return Err(generic_safe_error(
                "load-config",
                "The startup attempt was superseded.",
            ));
        }
        return Ok(required);
    }

    let config = match load_config_no_validation().await {
        Ok(config) => config,
        Err(error) => {
            let safe = safe_error("load-config", "unknown", "", &error);
            publish_failure(&window, store, generation, &path, safe.clone());
            return Err(safe);
        }
    };
    let backend = config.effective_backend_name().to_string();
    let vault = config.default_vault.clone();
    let connecting = StartupSnapshot::connecting(&backend, &vault, path.display().to_string());
    if !advance_and_emit(
        &window,
        store,
        generation,
        StartupEvent::ConfigLoaded,
        connecting,
    ) {
        return Err(generic_safe_error(
            "connect",
            "The startup attempt was superseded.",
        ));
    }

    connect_configuration(&window, store, generation, &path, config, &backend, &vault).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crosstache::config::setup::SetupRequest;
    use crosstache::error::SafeSetupError;
    use std::path::PathBuf;

    #[test]
    fn missing_config_routes_to_setup_required() {
        assert_eq!(
            transition(
                StartupState::LoadingConfiguration,
                StartupEvent::ConfigMissing
            ),
            StartupState::SetupRequired
        );
    }

    #[test]
    fn verified_setup_connects_before_ready() {
        assert_eq!(
            transition(StartupState::SetupRequired, StartupEvent::SetupVerified),
            StartupState::Connecting
        );
        assert_eq!(
            transition(StartupState::Connecting, StartupEvent::Connected),
            StartupState::Ready
        );
        assert_eq!(
            transition(
                StartupState::RecoverableFailure,
                StartupEvent::SetupVerified
            ),
            StartupState::Connecting
        );
    }

    #[test]
    fn startup_failures_are_recoverable_and_retry_reloads_configuration() {
        assert_eq!(
            transition(
                StartupState::LoadingConfiguration,
                StartupEvent::StartupFailed
            ),
            StartupState::RecoverableFailure
        );
        assert_eq!(
            transition(
                StartupState::RecoverableFailure,
                StartupEvent::RetryRequested
            ),
            StartupState::LoadingConfiguration
        );
    }

    #[test]
    fn loaded_configuration_connects_and_invalid_events_do_not_skip_phases() {
        assert_eq!(
            transition(
                StartupState::LoadingConfiguration,
                StartupEvent::ConfigLoaded
            ),
            StartupState::Connecting
        );
        assert_eq!(
            transition(StartupState::SetupRequired, StartupEvent::Connected),
            StartupState::SetupRequired
        );
        assert_eq!(
            transition(StartupState::LoadingConfiguration, StartupEvent::Connected),
            StartupState::LoadingConfiguration
        );
    }

    #[test]
    fn snapshots_never_serialize_loopback_urls_or_tokens() {
        let snapshot = StartupSnapshot::connecting("azure", "team-vault", "/safe/xv.conf");
        let serialized = serde_json::to_string(&snapshot).unwrap();

        assert!(!serialized.contains("127.0.0.1"));
        assert!(!serialized.contains("token="));
        assert!(serialized.contains(r#""kind":"connecting""#));
        assert_eq!(snapshot.state, StartupState::Connecting);
    }

    #[test]
    fn setup_request_scope_is_display_safe_and_provider_secret_free() {
        let cases = [
            (
                SetupRequest::Local {
                    store_path: PathBuf::from("/tmp/store"),
                    key_file: PathBuf::from("/tmp/key.txt"),
                    vault: "local-vault".into(),
                },
                ("local", "local-vault"),
            ),
            (
                SetupRequest::Azure {
                    subscription_id: "subscription".into(),
                    tenant_id: "tenant".into(),
                    vault: "azure-vault".into(),
                    resource_group: "group".into(),
                    location: "eastus".into(),
                },
                ("azure", "azure-vault"),
            ),
            (
                SetupRequest::Aws {
                    region: "us-east-1".into(),
                    profile: Some("work".into()),
                    vault_prefix: "aws-vault".into(),
                },
                ("aws", "aws-vault"),
            ),
        ];

        for (request, expected) in cases {
            assert_eq!(request_scope(&request), expected);
        }
    }

    #[test]
    fn copy_diagnostics_reads_only_the_stored_redacted_failure() {
        let error = SafeSetupError::from_message(
            "Authorization: Bearer raw-token; config=/private/xv.conf",
        );
        let snapshot = StartupSnapshot::recoverable(error, "/safe/xv.conf");

        let diagnostics = diagnostics_for_copy(&snapshot).unwrap();

        assert!(!diagnostics.contains("raw-token"));
        assert!(!diagnostics.contains("/private/xv.conf"));
        assert!(diagnostics_for_copy(&StartupSnapshot::loading("/safe/xv.conf")).is_none());
    }

    #[test]
    fn navigation_is_permitted_only_after_ready() {
        for state in [
            StartupState::LoadingConfiguration,
            StartupState::Connecting,
            StartupState::SetupRequired,
            StartupState::RecoverableFailure,
        ] {
            assert!(!can_navigate(state));
        }
        assert!(can_navigate(StartupState::Ready));
    }

    #[test]
    fn superseded_attempts_cannot_publish_ready_or_failure() {
        let store = StartupStore::new("/safe/xv.conf".into());
        let first = store.begin_attempt();
        assert!(store.advance(
            first,
            StartupEvent::ConfigLoaded,
            StartupSnapshot::connecting("local", "vault", "/safe/xv.conf")
        ));
        let second = store.begin_attempt();

        assert!(!store.advance(
            first,
            StartupEvent::Connected,
            StartupSnapshot::ready("local", "vault", "/safe/xv.conf")
        ));
        assert!(!store.advance(
            first,
            StartupEvent::StartupFailed,
            StartupSnapshot::recoverable(
                SafeSetupError::from_message("old failure"),
                "/safe/xv.conf"
            )
        ));
        assert!(store.advance(
            second,
            StartupEvent::ConfigLoaded,
            StartupSnapshot::connecting("local", "vault", "/safe/xv.conf")
        ));
    }

    #[test]
    fn commands_can_only_begin_attempts_from_their_owned_states() {
        let store = StartupStore::new("/safe/xv.conf".into());
        assert!(store.begin_retry_attempt().is_none());
        assert!(store.begin_setup_attempt().is_none());

        let loading = store.begin_attempt();
        assert!(store.advance(
            loading,
            StartupEvent::ConfigMissing,
            StartupSnapshot::setup_required("/safe/xv.conf")
        ));
        assert!(store.begin_setup_attempt().is_some());
        assert!(store.begin_setup_attempt().is_none());
        assert!(store.begin_retry_attempt().is_none());

        let generation = store.begin_attempt();
        assert!(store.advance(
            generation,
            StartupEvent::StartupFailed,
            StartupSnapshot::recoverable(SafeSetupError::from_message("redacted"), "/safe/xv.conf")
        ));
        assert!(store.begin_retry_attempt().is_some());
        assert!(store.begin_retry_attempt().is_none());
        assert!(store.begin_setup_attempt().is_none());
    }
}
