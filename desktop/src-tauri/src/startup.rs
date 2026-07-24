// The command ABI intentionally returns the complete, display-safe recovery
// model so the frontend can render stable codes, hints, and redacted details.
#![allow(clippy::result_large_err)]

use crosstache::config::setup::{
    build_setup_config, setup_and_save, SetupOutcome, SetupPreview, SetupRequest,
};
use crosstache::config::{load_config_no_validation, Config};
use crosstache::error::{CrosstacheError, SafeSetupError};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State, WebviewWindow};

const STARTUP_STATE_EVENT: &str = "xv://startup-state";
const PACKAGE_SMOKE_ROOT: &str = "XV_DESKTOP_PACKAGE_SMOKE_ROOT";
const PACKAGE_SMOKE_MARKER: &str = "XV_PACKAGE_SMOKE_STATE=";

#[derive(Debug, Clone)]
struct PackageSmokeEnvironment {
    root: PathBuf,
    temp_parent: PathBuf,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    no_parent_config: Option<String>,
    project: Option<OsString>,
    backend: Option<String>,
    age_key: Option<OsString>,
    age_key_file: Option<OsString>,
    argument_count: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct PackageSmokeSetup {
    request: SetupRequest,
    config_path: PathBuf,
}

fn canonical_directory(path: &Path) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    canonical.is_dir().then_some(canonical)
}

fn is_package_smoke_root_name(name: &str) -> bool {
    name.strip_prefix("xv-package-smoke.")
        .is_some_and(|suffix| {
            suffix.len() == 6 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn package_smoke_setup(environment: PackageSmokeEnvironment) -> Option<PackageSmokeSetup> {
    let root = canonical_directory(&environment.root)?;
    let temp_parent = canonical_directory(&environment.temp_parent)?;
    let root_name = root.file_name()?.to_str()?;
    if root.parent()? != temp_parent
        || !is_package_smoke_root_name(root_name)
        || environment.no_parent_config.as_deref() != Some("1")
        || environment.project.is_some()
        || environment.backend.is_some()
        || environment.age_key.is_some()
        || environment.age_key_file.is_some()
        || environment.argument_count != 1
    {
        return None;
    }

    let home = canonical_directory(&environment.home)?;
    let config = canonical_directory(&environment.config)?;
    let data = canonical_directory(&environment.data)?;
    if home != root.join("home") || config != root.join("config") || data != root.join("data") {
        return None;
    }

    let config_path = config.join("xv/xv.conf");
    if config_path.exists() {
        return None;
    }

    Some(PackageSmokeSetup {
        request: SetupRequest::Local {
            store_path: root.join("store"),
            key_file: root.join("local-key.txt"),
            vault: "package-smoke".into(),
        },
        config_path,
    })
}

fn package_smoke_setup_from_environment() -> Option<PackageSmokeSetup> {
    let root = std::env::var_os(PACKAGE_SMOKE_ROOT).map(PathBuf::from)?;
    let environment = PackageSmokeEnvironment {
        root,
        temp_parent: std::env::temp_dir(),
        home: PathBuf::from(std::env::var_os("HOME")?),
        config: PathBuf::from(std::env::var_os("XDG_CONFIG_HOME")?),
        data: PathBuf::from(std::env::var_os("XDG_DATA_HOME")?),
        no_parent_config: std::env::var("XV_NO_PARENT_CONFIG").ok(),
        project: std::env::var_os("XV_DESKTOP_PROJECT"),
        backend: std::env::var("XV_BACKEND").ok(),
        age_key: std::env::var_os("AGE_KEY"),
        age_key_file: std::env::var_os("AGE_KEY_FILE"),
        argument_count: std::env::args_os().count(),
    };
    let setup = package_smoke_setup(environment)?;
    (Config::get_config_path().ok()? == setup.config_path).then_some(setup)
}

fn package_smoke_marker_value(state: StartupState) -> Option<&'static str> {
    match state {
        StartupState::SetupRequired => Some("setup-required"),
        StartupState::Ready => Some("ready"),
        _ => None,
    }
}

fn package_smoke_marker(state: StartupState) {
    if let Some(marker) = package_smoke_marker_value(state) {
        eprintln!("{PACKAGE_SMOKE_MARKER}{marker}");
    }
}

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

pub fn transition(state: StartupState, event: StartupEvent) -> Option<StartupState> {
    match (state, event) {
        (StartupState::LoadingConfiguration, StartupEvent::ConfigMissing) => {
            Some(StartupState::SetupRequired)
        }
        (StartupState::LoadingConfiguration, StartupEvent::ConfigLoaded)
        | (StartupState::SetupRequired, StartupEvent::SetupVerified)
        | (StartupState::RecoverableFailure, StartupEvent::SetupVerified) => {
            Some(StartupState::Connecting)
        }
        (StartupState::Connecting, StartupEvent::Connected) => Some(StartupState::Ready),
        (
            StartupState::LoadingConfiguration
            | StartupState::Connecting
            | StartupState::SetupRequired
            | StartupState::RecoverableFailure
            | StartupState::Ready,
            StartupEvent::StartupFailed,
        ) => Some(StartupState::RecoverableFailure),
        (StartupState::RecoverableFailure, StartupEvent::RetryRequested) => {
            Some(StartupState::LoadingConfiguration)
        }
        _ => None,
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

pub struct StartupStore {
    data: Mutex<StartupData>,
    launch_project: Result<Option<PathBuf>, SafeSetupError>,
}

impl StartupStore {
    #[cfg(test)]
    fn new(config_path: String) -> Self {
        Self::with_launch_project(config_path, Ok(None))
    }

    fn with_launch_project(
        config_path: String,
        launch_project: Result<Option<PathBuf>, SafeSetupError>,
    ) -> Self {
        Self {
            data: Mutex::new(StartupData {
                generation: 0,
                snapshot: StartupSnapshot::loading(config_path),
                active_operation: None,
            }),
            launch_project,
        }
    }

    pub fn from_environment() -> Self {
        let path = Config::get_config_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let launch_project = std::env::current_dir()
            .map_err(|_| {
                generic_safe_error(
                    "load-project",
                    "The desktop launch directory could not be resolved.",
                )
            })
            .and_then(|base| {
                resolve_launch_project(
                    std::env::args_os().skip(1),
                    std::env::var_os("XV_DESKTOP_PROJECT"),
                    &base,
                )
            });
        Self::with_launch_project(path, launch_project)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StartupData> {
        self.data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn launch_project(&self) -> Result<Option<PathBuf>, SafeSetupError> {
        self.launch_project.clone()
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
                != Some(StartupState::LoadingConfiguration)
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
        if data.generation != generation
            || transition(data.snapshot.state, event) != Some(snapshot.state)
        {
            return false;
        }
        data.snapshot = snapshot;
        if is_terminal(data.snapshot.state) {
            data.active_operation = None;
        }
        true
    }

    fn advance_from(
        &self,
        generation: u64,
        expected: StartupState,
        event: StartupEvent,
        snapshot: StartupSnapshot,
    ) -> bool {
        let mut data = self.lock();
        if data.generation != generation
            || data.snapshot.state != expected
            || transition(data.snapshot.state, event) != Some(snapshot.state)
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

async fn apply_setup_commit_contract<Commit, Connection>(
    backend: &str,
    vault: &str,
    commit: Commit,
    connection: Connection,
) -> Result<SetupOutcome, SafeSetupError>
where
    Commit: Future<Output = Result<SetupOutcome, CrosstacheError>>,
    Connection: Future<Output = Result<(), SafeSetupError>>,
{
    let outcome = commit
        .await
        .map_err(|error| safe_error("apply-setup", backend, vault, &error))?;
    let _ = connection.await;
    Ok(outcome)
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

fn bundled_recovery_url() -> tauri::Url {
    tauri::Url::parse("tauri://localhost").expect("bundled recovery URL is a valid constant")
}

fn handle_server_result<T, E>(
    store: &StartupStore,
    generation: u64,
    config_path: &str,
    backend: &str,
    vault: &str,
    result: Result<T, E>,
) -> Option<StartupSnapshot> {
    if result.is_ok() {
        return None;
    }
    let error = safe_error(
        "serve",
        backend,
        vault,
        &CrosstacheError::unknown("The embedded server stopped unexpectedly."),
    );
    let snapshot = StartupSnapshot::recoverable(error, config_path);
    store
        .advance_from(
            generation,
            StartupState::Ready,
            StartupEvent::StartupFailed,
            snapshot.clone(),
        )
        .then_some(snapshot)
}

fn resolve_launch_project<I>(
    args: I,
    environment_project: Option<OsString>,
    launch_directory: &Path,
) -> Result<Option<PathBuf>, SafeSetupError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut requested = None;
    while let Some(arg) = args.next() {
        if arg == "--project" {
            requested = Some(args.next().map(PathBuf::from).ok_or_else(|| {
                generic_safe_error(
                    "load-project",
                    "The desktop --project option did not include a directory.",
                )
            })?);
            break;
        }
    }
    let Some(requested) = requested.or_else(|| environment_project.map(PathBuf::from)) else {
        return Ok(None);
    };
    let absolute = if requested.is_absolute() {
        requested
    } else {
        launch_directory.join(requested)
    };
    let canonical = absolute.canonicalize().map_err(|_| {
        generic_safe_error(
            "load-project",
            "The configured project directory could not be resolved.",
        )
    })?;
    if !canonical.is_dir() {
        return Err(generic_safe_error(
            "load-project",
            "The configured project scope is not a directory.",
        ));
    }

    Ok(Some(canonical))
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
    apply_setup_request(request, &window, &state).await
}

async fn apply_setup_request(
    request: SetupRequest,
    window: &WebviewWindow,
    state: &StartupStore,
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

    let connection = async {
        let connecting = StartupSnapshot::connecting(&backend, &vault, path.display().to_string());
        if !advance_and_emit(
            window,
            state,
            generation,
            StartupEvent::SetupVerified,
            connecting,
        ) {
            return Err(generic_safe_error(
                "apply-setup",
                "The setup attempt was superseded.",
            ));
        }

        connect_saved_configuration(window, state, generation, &path, &backend, &vault)
            .await
            .map(|_| ())
    };
    let outcome = match apply_setup_commit_contract(
        &backend,
        &vault,
        setup_and_save(request, base, &path),
        connection,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(safe) => {
            publish_failure(window, state, generation, &path, safe.clone());
            return Err(safe);
        }
    };

    // Persistence is the setup commit point. Any later connection failure has
    // already published recovery (unless superseded) and must not be reported
    // as a failed setup, because the verified configuration is committed.
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

    let server_window = window.clone();
    let server_handle = window.app_handle().clone();
    let server_config_path = path.display().to_string();
    let server_backend = backend.to_string();
    let server_vault = vault.to_string();
    tauri::async_runtime::spawn(async move {
        let result = server.serve().await;
        let store = server_handle.state::<StartupStore>();
        if let Some(recovery) = handle_server_result(
            &store,
            generation,
            &server_config_path,
            &server_backend,
            &server_vault,
            result,
        ) {
            // The stored snapshot is authoritative for the newly loaded
            // recovery page (`startup_status`). A navigation error is ignored
            // here so it cannot replace the original safe serve failure with
            // a raw Tauri diagnostic.
            let _ = server_window.navigate(bundled_recovery_url());
            emit_snapshot(&server_window, &recovery);
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

    let project = match store.launch_project() {
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
        if let Some(smoke) =
            package_smoke_setup_from_environment().filter(|smoke| smoke.config_path == path)
        {
            package_smoke_marker(StartupState::SetupRequired);
            apply_setup_request(smoke.request, &window, store).await?;
            let snapshot = store.snapshot();
            if snapshot.state == StartupState::Ready {
                package_smoke_marker(StartupState::Ready);
            }
            return Ok(snapshot);
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
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn ready_store() -> (StartupStore, u64) {
        let store = StartupStore::new("/safe/xv.conf".into());
        let generation = store.begin_attempt();
        assert!(store.advance(
            generation,
            StartupEvent::ConfigLoaded,
            StartupSnapshot::connecting("local", "vault", "/safe/xv.conf")
        ));
        assert!(store.advance(
            generation,
            StartupEvent::Connected,
            StartupSnapshot::ready("local", "vault", "/safe/xv.conf")
        ));
        (store, generation)
    }

    fn package_smoke_environment(root: &Path, temp_parent: &Path) -> PackageSmokeEnvironment {
        let home = root.join("home");
        let config = root.join("config");
        let data = root.join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        PackageSmokeEnvironment {
            root: root.to_path_buf(),
            temp_parent: temp_parent.to_path_buf(),
            home,
            config,
            data,
            no_parent_config: Some("1".into()),
            project: None,
            backend: None,
            age_key: None,
            age_key_file: None,
            argument_count: 1,
        }
    }

    #[test]
    fn package_smoke_accepts_only_the_fixed_isolated_local_setup() {
        let parent = tempfile::tempdir().unwrap();
        let temp_parent = parent.path().canonicalize().unwrap();
        let root = temp_parent.join("xv-package-smoke.ABC123");
        let environment = package_smoke_environment(&root, &temp_parent);
        let setup = package_smoke_setup(environment.clone()).unwrap();

        assert_eq!(
            setup.request,
            SetupRequest::Local {
                store_path: root.join("store"),
                key_file: root.join("local-key.txt"),
                vault: "package-smoke".into(),
            }
        );
        assert_eq!(setup.config_path, root.join("config/xv/xv.conf"));
    }

    #[test]
    fn package_smoke_rejects_every_environment_or_argument_escape_without_writing() {
        let parent = tempfile::tempdir().unwrap();
        let temp_parent = parent.path().canonicalize().unwrap();
        let root = temp_parent.join("xv-package-smoke.DEF456");
        let environment = package_smoke_environment(&root, &temp_parent);
        let wrong_home = root.join("wrong-home");
        let wrong_config = root.join("wrong-config");
        let wrong_data = root.join("wrong-data");
        for path in [&wrong_home, &wrong_config, &wrong_data] {
            std::fs::create_dir_all(path).unwrap();
        }

        let wrong_name = temp_parent.join("not-a-package-smoke-root");
        let wrong_name_environment = package_smoke_environment(&wrong_name, &temp_parent);
        let wrong_length = temp_parent.join("xv-package-smoke.short");
        let wrong_length_environment = package_smoke_environment(&wrong_length, &temp_parent);
        let wrong_characters = temp_parent.join("xv-package-smoke.AB!123");
        let wrong_characters_environment =
            package_smoke_environment(&wrong_characters, &temp_parent);

        let rejected = vec![
            PackageSmokeEnvironment {
                temp_parent: root.clone(),
                ..environment.clone()
            },
            wrong_name_environment,
            wrong_length_environment,
            wrong_characters_environment,
            PackageSmokeEnvironment {
                home: wrong_home,
                ..environment.clone()
            },
            PackageSmokeEnvironment {
                config: wrong_config,
                ..environment.clone()
            },
            PackageSmokeEnvironment {
                data: wrong_data,
                ..environment.clone()
            },
            PackageSmokeEnvironment {
                no_parent_config: Some("0".into()),
                ..environment.clone()
            },
            PackageSmokeEnvironment {
                no_parent_config: None,
                ..environment.clone()
            },
            PackageSmokeEnvironment {
                project: Some("outside-project".into()),
                ..environment.clone()
            },
            PackageSmokeEnvironment {
                backend: Some("local".into()),
                ..environment.clone()
            },
            PackageSmokeEnvironment {
                age_key: Some("AGE-SECRET-KEY-1outside".into()),
                ..environment.clone()
            },
            PackageSmokeEnvironment {
                age_key_file: Some(root.join("outside-key").into_os_string()),
                ..environment.clone()
            },
            PackageSmokeEnvironment {
                argument_count: 2,
                ..environment.clone()
            },
        ];

        for invalid in rejected {
            assert!(package_smoke_setup(invalid).is_none());
            assert!(!root.join("store").exists());
            assert!(!root.join("local-key.txt").exists());
        }
        let existing_config = root.join("config/xv/xv.conf");
        std::fs::create_dir_all(existing_config.parent().unwrap()).unwrap();
        std::fs::write(&existing_config, "prior configuration").unwrap();
        assert!(package_smoke_setup(environment).is_none());
        assert!(!root.join("store").exists());
        assert!(!root.join("local-key.txt").exists());
        assert_eq!(
            std::fs::read_to_string(existing_config).unwrap(),
            "prior configuration"
        );
    }

    #[test]
    fn package_smoke_markers_are_limited_to_stable_state_names() {
        assert_eq!(
            package_smoke_marker_value(StartupState::SetupRequired),
            Some("setup-required")
        );
        assert_eq!(
            package_smoke_marker_value(StartupState::Ready),
            Some("ready")
        );
        for state in [
            StartupState::LoadingConfiguration,
            StartupState::Connecting,
            StartupState::RecoverableFailure,
        ] {
            assert_eq!(package_smoke_marker_value(state), None);
        }
    }

    #[test]
    fn missing_config_routes_to_setup_required() {
        assert_eq!(
            transition(
                StartupState::LoadingConfiguration,
                StartupEvent::ConfigMissing
            ),
            Some(StartupState::SetupRequired)
        );
    }

    #[test]
    fn verified_setup_connects_before_ready() {
        assert_eq!(
            transition(StartupState::SetupRequired, StartupEvent::SetupVerified),
            Some(StartupState::Connecting)
        );
        assert_eq!(
            transition(StartupState::Connecting, StartupEvent::Connected),
            Some(StartupState::Ready)
        );
        assert_eq!(
            transition(
                StartupState::RecoverableFailure,
                StartupEvent::SetupVerified
            ),
            Some(StartupState::Connecting)
        );
    }

    #[test]
    fn startup_failures_are_recoverable_and_retry_reloads_configuration() {
        assert_eq!(
            transition(
                StartupState::LoadingConfiguration,
                StartupEvent::StartupFailed
            ),
            Some(StartupState::RecoverableFailure)
        );
        assert_eq!(
            transition(
                StartupState::RecoverableFailure,
                StartupEvent::RetryRequested
            ),
            Some(StartupState::LoadingConfiguration)
        );
    }

    #[test]
    fn loaded_configuration_connects_and_invalid_events_do_not_skip_phases() {
        assert_eq!(
            transition(
                StartupState::LoadingConfiguration,
                StartupEvent::ConfigLoaded
            ),
            Some(StartupState::Connecting)
        );
        assert_eq!(
            transition(StartupState::SetupRequired, StartupEvent::Connected),
            None
        );
        assert_eq!(
            transition(StartupState::LoadingConfiguration, StartupEvent::Connected),
            None
        );
    }

    #[test]
    fn invalid_same_state_event_is_rejected_without_mutation() {
        let store = StartupStore::new("/safe/xv.conf".into());
        let generation = store.begin_attempt();
        let before = store.snapshot();

        assert!(!store.advance(
            generation,
            StartupEvent::Connected,
            StartupSnapshot::loading("/different/path")
        ));
        assert_eq!(store.snapshot(), before);
        assert!(store.advance(
            generation,
            StartupEvent::ConfigMissing,
            StartupSnapshot::setup_required("/safe/xv.conf")
        ));
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

    #[test]
    fn post_commit_connection_failure_returns_success_with_committed_bytes() {
        tauri::async_runtime::block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let config_path = directory.path().join("xv.conf");
            let prior = b"previous configuration bytes";
            std::fs::write(&config_path, prior).unwrap();
            let request = SetupRequest::Local {
                store_path: directory.path().join("store"),
                key_file: directory.path().join("identity.txt"),
                vault: "committed-vault".into(),
            };
            let commit = setup_and_save(request, Config::default(), &config_path);
            let connection_failure =
                generic_safe_error("connect", "Injected post-commit connection failure.");
            let store = StartupStore::new(config_path.display().to_string());
            let loading = store.begin_attempt();
            assert!(store.advance(
                loading,
                StartupEvent::ConfigMissing,
                StartupSnapshot::setup_required(config_path.display().to_string())
            ));
            let setup = store.begin_setup_attempt().unwrap();
            let connection = async {
                assert!(store.advance(
                    setup,
                    StartupEvent::SetupVerified,
                    StartupSnapshot::connecting(
                        "local",
                        "committed-vault",
                        config_path.display().to_string(),
                    )
                ));
                assert!(store.advance(
                    setup,
                    StartupEvent::StartupFailed,
                    StartupSnapshot::recoverable(
                        connection_failure.clone(),
                        config_path.display().to_string(),
                    )
                ));
                Err(connection_failure.clone())
            };

            let result =
                apply_setup_commit_contract("local", "committed-vault", commit, connection)
                    .await
                    .unwrap();

            assert_eq!(result.preview.backend, "local");
            assert_eq!(result.preview.vault, "committed-vault");
            assert_eq!(store.snapshot().state, StartupState::RecoverableFailure);
            assert!(!serde_json::to_string(&result)
                .unwrap()
                .contains("connection_error"));
            assert_ne!(std::fs::read(&config_path).unwrap(), prior);
        });
    }

    #[test]
    fn active_server_failure_enters_recovery_and_enables_retry() {
        let (store, generation) = ready_store();

        let recovery = handle_server_result(
            &store,
            generation,
            "/safe/xv.conf",
            "local",
            "vault",
            Err::<(), _>("injected serve failure"),
        )
        .unwrap();

        assert_eq!(recovery.state, StartupState::RecoverableFailure);
        assert_eq!(store.snapshot(), recovery);
        assert!(store.begin_retry_attempt().is_some());
        let serialized = serde_json::to_string(&recovery).unwrap();
        assert!(!serialized.contains("injected serve failure"));
        assert!(!serialized.contains("token="));
        assert_eq!(bundled_recovery_url().as_str(), "tauri://localhost");
    }

    #[test]
    fn stale_server_failure_cannot_replace_the_current_generation() {
        let (store, stale_generation) = ready_store();
        let current_generation = store.begin_attempt();
        assert!(store.advance(
            current_generation,
            StartupEvent::ConfigLoaded,
            StartupSnapshot::connecting("local", "new-vault", "/safe/xv.conf")
        ));
        assert!(store.advance(
            current_generation,
            StartupEvent::Connected,
            StartupSnapshot::ready("local", "new-vault", "/safe/xv.conf")
        ));
        let current = store.snapshot();

        assert!(handle_server_result(
            &store,
            stale_generation,
            "/safe/xv.conf",
            "local",
            "vault",
            Err::<(), _>("stale injected serve failure"),
        )
        .is_none());
        assert_eq!(store.snapshot(), current);
    }

    #[test]
    fn server_failure_is_inert_until_its_generation_is_ready() {
        let store = StartupStore::new("/safe/xv.conf".into());
        let generation = store.begin_attempt();
        assert!(store.advance(
            generation,
            StartupEvent::ConfigLoaded,
            StartupSnapshot::connecting("local", "vault", "/safe/xv.conf")
        ));
        let connecting = store.snapshot();

        assert!(handle_server_result(
            &store,
            generation,
            "/safe/xv.conf",
            "local",
            "vault",
            Err::<(), _>("premature injected serve failure"),
        )
        .is_none());
        assert_eq!(store.snapshot(), connecting);
    }

    #[test]
    fn relative_launch_project_is_resolved_once_before_cwd_changes() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir_all(project.join("project")).unwrap();
        let args = vec![OsString::from("--project"), OsString::from("project")];
        let resolved = resolve_launch_project(args.clone(), None, directory.path()).unwrap();
        let store = StartupStore::with_launch_project("/safe/xv.conf".into(), Ok(resolved.clone()));

        assert_eq!(
            store.launch_project().unwrap(),
            Some(project.canonicalize().unwrap())
        );
        assert_eq!(store.launch_project().unwrap(), resolved);
        assert_ne!(
            resolve_launch_project(args, None, &project).unwrap(),
            resolved
        );
    }
}
