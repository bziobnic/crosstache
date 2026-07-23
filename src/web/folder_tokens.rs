use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use fs2::FileExt;
use hmac::{Hmac, Mac};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::{api::ApiError, WebState};
use crate::error::{CrosstacheError, Result};
use crate::utils::helpers::open_private_lock_file_no_follow;

const KEY_BYTES: usize = 32;
const TOKEN_DOMAIN: &[u8] = b"xv-folder-token-v1";

#[derive(Clone)]
pub(crate) struct FolderTokenService {
    key: [u8; KEY_BYTES],
}

impl FolderTokenService {
    pub(crate) fn from_key(key: [u8; KEY_BYTES]) -> Self {
        Self { key }
    }

    pub(crate) fn random() -> Self {
        let mut key = [0u8; KEY_BYTES];
        rand::rng().fill_bytes(&mut key);
        Self::from_key(key)
    }

    pub(crate) fn load_or_create(path: &Path) -> Result<Self> {
        let _lock = lock_key_path(path)?;
        let key = match open_key_file_no_follow(path) {
            Ok(file) => read_key_from_file(file, path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let service = Self::random();
                match create_key_file_exclusive(path, &service.key) {
                    Ok(()) => read_key_file_no_follow(path)?,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        read_key_file_no_follow(path)?
                    }
                    Err(error) => return Err(key_io_error("create", path, error)),
                }
            }
            Err(error) => return Err(key_io_error("open", path, error)),
        };
        Ok(Self::from_key(key))
    }

    fn token(&self, kind: &str, parts: &[&str]) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key)
            .expect("HMAC-SHA256 accepts 32-byte keys");
        mac.update(TOKEN_DOMAIN);
        mac.update(&(kind.len() as u64).to_be_bytes());
        mac.update(kind.as_bytes());
        for part in parts {
            mac.update(&(part.len() as u64).to_be_bytes());
            mac.update(part.as_bytes());
        }
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    pub(crate) fn scope_token(&self, backend: &str, vault: &str, surface: &str) -> String {
        self.token("scope", &[backend, vault, surface])
    }

    pub(crate) fn folder_token(&self, scope_token: &str, path: &str) -> String {
        self.token("folder", &[scope_token, path])
    }
}

fn key_io_error(action: &str, path: &Path, error: std::io::Error) -> CrosstacheError {
    CrosstacheError::config(format!(
        "Failed to {action} folder-token key '{}': {error}",
        path.display()
    ))
}

fn open_key_file_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}

fn read_key_from_file(mut file: File, path: &Path) -> Result<[u8; KEY_BYTES]> {
    let metadata = file
        .metadata()
        .map_err(|error| key_io_error("inspect", path, error))?;
    validate_private_regular_file(&metadata, path, "Folder-token key")?;
    if metadata.len() != KEY_BYTES as u64 {
        return Err(CrosstacheError::config(format!(
            "Folder-token key '{}' must contain exactly {KEY_BYTES} bytes, found {}",
            path.display(),
            metadata.len()
        )));
    }
    let mut key = [0u8; KEY_BYTES];
    file.read_exact(&mut key)
        .map_err(|error| key_io_error("read", path, error))?;
    let mut extra = [0u8; 1];
    if file
        .read(&mut extra)
        .map_err(|error| key_io_error("read", path, error))?
        != 0
    {
        return Err(CrosstacheError::config(format!(
            "Folder-token key '{}' changed while it was being read",
            path.display()
        )));
    }
    Ok(key)
}

fn validate_private_regular_file(
    metadata: &std::fs::Metadata,
    path: &Path,
    label: &str,
) -> Result<()> {
    if !metadata.file_type().is_file() {
        return Err(CrosstacheError::config(format!(
            "{label} '{}' must be a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(CrosstacheError::config(format!(
                "{label} '{}' must be owner-only (mode 600), found {:03o}",
                path.display(),
                mode & 0o777
            )));
        }
    }
    Ok(())
}

fn read_key_file_no_follow(path: &Path) -> Result<[u8; KEY_BYTES]> {
    let file = open_key_file_no_follow(path).map_err(|error| key_io_error("open", path, error))?;
    read_key_from_file(file, path)
}

fn create_key_file_exclusive(path: &Path, key: &[u8; KEY_BYTES]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path)?;
    file.write_all(key)?;
    file.sync_all()
}

fn lock_key_path(path: &Path) -> Result<File> {
    let lock_path = path.with_file_name(format!(
        ".{}.lock",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let lock = open_private_lock_file_no_follow(&lock_path).map_err(|error| {
        CrosstacheError::config(format!(
            "Failed to open lock for folder-token key '{}': {error}",
            path.display()
        ))
    })?;
    let metadata = lock
        .metadata()
        .map_err(|error| key_io_error("inspect lock for", path, error))?;
    validate_private_regular_file(&metadata, &lock_path, "Folder-token lock")?;
    lock.lock_exclusive()
        .map_err(|error| key_io_error("lock", path, error))?;
    Ok(lock)
}

pub(crate) fn folder_token_key_path_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name("ui-folder-token.key")
}

#[derive(Deserialize)]
pub(crate) struct FolderTokenQuery {
    alias: Option<String>,
    backend: Option<String>,
    vault: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct FolderTokenRequest {
    surface: String,
    folders: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct FolderTokenEntry {
    path: String,
    token: String,
}

#[derive(Serialize)]
pub(crate) struct FolderTokenResponse {
    version: u8,
    scope_token: String,
    folders: Vec<FolderTokenEntry>,
}

pub(crate) async fn issue_tokens(
    State(state): State<Arc<WebState>>,
    Query(query): Query<FolderTokenQuery>,
    Json(request): Json<FolderTokenRequest>,
) -> std::result::Result<Json<FolderTokenResponse>, ApiError> {
    if request.surface != "secrets" && request.surface != "files" {
        return Err(ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "Choose a supported folder surface.",
            field: Some("surface"),
        });
    }
    if request.folders.len() > 10_000 {
        return Err(ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "Too many folder paths were requested.",
            field: Some("folders"),
        });
    }
    let target = state.scoped_target(
        query.alias.as_deref(),
        query.backend.as_deref(),
        query.vault.as_deref(),
    )?;
    let scope_token = state.folder_tokens.scope_token(
        &target.context.backend,
        &target.context.vault,
        &request.surface,
    );
    let mut seen_paths = HashSet::new();
    let mut seen_tokens = HashSet::new();
    let mut folders = Vec::with_capacity(request.folders.len());
    for path in request.folders {
        if path.is_empty() || path.len() > 4096 {
            return Err(ApiError::Validation {
                status: StatusCode::BAD_REQUEST,
                message: "Folder paths must contain between 1 and 4096 bytes.",
                field: Some("folders"),
            });
        }
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        let token = state.folder_tokens.folder_token(&scope_token, &path);
        if !seen_tokens.insert(token.clone()) {
            return Err(ApiError::App(CrosstacheError::config(
                "Folder-token collision detected.",
            )));
        }
        folders.push(FolderTokenEntry { path, token });
    }
    Ok(Json(FolderTokenResponse {
        version: 1,
        scope_token,
        folders,
    }))
}

#[cfg(test)]
mod tests {
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use serde_json::json;
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use tower::ServiceExt;

    use sha2::{Digest, Sha256};

    use super::FolderTokenService;

    #[test]
    fn hmac_tokens_are_stable_keyed_and_not_dictionary_derivable() {
        let one = FolderTokenService::from_key([0x11; 32]);
        let same_installation = FolderTokenService::from_key([0x11; 32]);
        let other_installation = FolderTokenService::from_key([0x22; 32]);
        let scope = one.scope_token("local", "payments", "secrets");
        let token = one.folder_token(&scope, "apps/prod");

        assert_eq!(token, same_installation.folder_token(&scope, "apps/prod"));
        assert_ne!(
            token,
            other_installation.folder_token(
                &other_installation.scope_token("local", "payments", "secrets"),
                "apps/prod",
            ),
        );
        for candidate in ["apps/prod", "prod", "local/payments/apps/prod"] {
            assert_ne!(
                token,
                URL_SAFE_NO_PAD.encode(Sha256::digest(candidate.as_bytes()))
            );
        }
    }

    #[test]
    fn installation_key_is_stable_private_and_separates_installations() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_path = first.path().join("ui-folder-token.key");
        let second_path = second.path().join("ui-folder-token.key");

        let one = FolderTokenService::load_or_create(&first_path).unwrap();
        let reloaded = FolderTokenService::load_or_create(&first_path).unwrap();
        let other = FolderTokenService::load_or_create(&second_path).unwrap();
        let scope = one.scope_token("azure", "payments", "files");

        assert_eq!(
            one.folder_token(&scope, "reports/2026"),
            reloaded.folder_token(&scope, "reports/2026"),
        );
        assert_ne!(
            one.folder_token(&scope, "reports/2026"),
            other.folder_token(
                &other.scope_token("azure", "payments", "files"),
                "reports/2026",
            ),
        );
        assert_eq!(std::fs::read(&first_path).unwrap().len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&first_path).unwrap().permissions().mode() & 0o777,
                0o600,
            );
        }
        assert_eq!(
            super::folder_token_key_path_for(Path::new("/tmp/xv/xv.conf")),
            Path::new("/tmp/xv/ui-folder-token.key"),
        );
    }

    #[test]
    fn first_run_creates_missing_private_config_parent() {
        let root = tempfile::tempdir().unwrap();
        let config_path = root.path().join("fresh-xdg").join("xv").join("config.toml");
        let key_path = super::folder_token_key_path_for(&config_path);

        let service = FolderTokenService::load_or_create(&key_path).unwrap();
        let scope = service.scope_token("local", "default", "secrets");

        assert_eq!(service.folder_token(&scope, "apps").len(), 43);
        assert_eq!(std::fs::read(&key_path).unwrap().len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(key_path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let lock_path = key_path.with_file_name(".ui-folder-token.key.lock");
            assert_eq!(
                std::fs::metadata(lock_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn concurrent_first_creation_uses_exactly_one_stable_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = Arc::new(directory.path().join("ui-folder-token.key"));
        let barrier = Arc::new(Barrier::new(12));
        let workers: Vec<_> = (0..12)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let service = FolderTokenService::load_or_create(&path).unwrap();
                    let scope = service.scope_token("local", "payments", "secrets");
                    service.folder_token(&scope, "apps/prod")
                })
            })
            .collect();
        let tokens: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();

        assert!(tokens.iter().all(|token| token == &tokens[0]));
        assert_eq!(std::fs::read(path.as_ref()).unwrap().len(), 32);
    }

    #[test]
    fn partial_and_insecure_existing_key_files_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let partial = directory.path().join("partial.key");
        std::fs::write(&partial, [0x11; 7]).unwrap();
        assert!(FolderTokenService::load_or_create(&partial).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let insecure = directory.path().join("insecure.key");
            std::fs::write(&insecure, [0x22; 32]).unwrap();
            std::fs::set_permissions(&insecure, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(FolderTokenService::load_or_create(&insecure).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_reader_rejects_a_symlink_at_the_actual_open() {
        use std::os::unix::fs::symlink;
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.key");
        let link = directory.path().join("ui-folder-token.key");
        std::fs::write(&target, [0x33; 32]).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &link).unwrap();

        assert!(super::read_key_file_no_follow(&link).is_err());
        assert!(FolderTokenService::load_or_create(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn first_run_rejects_an_attacker_controlled_symlink_parent() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let attacker = root.path().join("attacker");
        std::fs::create_dir(&attacker).unwrap();
        std::fs::create_dir(attacker.join("xv")).unwrap();
        let linked_parent = root.path().join("fresh-xdg");
        symlink(&attacker, &linked_parent).unwrap();
        let key_path = linked_parent.join("xv").join("ui-folder-token.key");

        assert!(FolderTokenService::load_or_create(&key_path).is_err());
        assert!(!attacker.join("xv").join("ui-folder-token.key").exists());
        assert!(!attacker
            .join("xv")
            .join(".ui-folder-token.key.lock")
            .exists());
    }

    #[tokio::test]
    async fn authenticated_endpoint_returns_stable_opaque_tokens_without_caching() {
        let app =
            super::super::build_router(super::super::testutil::test_state_with_token("test-token"));
        let response = app
            .oneshot(
                Request::post("/api/folder-tokens")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "surface": "secrets",
                            "folders": [" apps/prod ", "__all__", "a/b", "a/b"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["version"], 1);
        assert_eq!(body["scope_token"].as_str().unwrap().len(), 43);
        assert_eq!(body["folders"].as_array().unwrap().len(), 3);
        assert_eq!(body["folders"][0]["path"], " apps/prod ");
        for folder in body["folders"].as_array().unwrap() {
            assert_eq!(folder["token"].as_str().unwrap().len(), 43);
            assert_ne!(folder["token"], folder["path"]);
        }
    }

    #[tokio::test]
    async fn endpoint_requires_auth_and_returns_structured_validation_errors() {
        let state = super::super::testutil::test_state_with_token("test-token");
        let app = super::super::build_router(state);
        let unauthorized = app
            .clone()
            .oneshot(
                Request::post("/api/folder-tokens")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"surface":"secrets","folders":["a"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(unauthorized.headers()[header::CACHE_CONTROL], "no-store");

        let invalid = app
            .oneshot(
                Request::post("/api/folder-tokens")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"surface":"history","folders":["a"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(invalid.headers()[header::CACHE_CONTROL], "no-store");
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(invalid.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"]["code"], "xv-invalid-argument");
        assert_eq!(body["error"]["field"], "surface");
    }
}
