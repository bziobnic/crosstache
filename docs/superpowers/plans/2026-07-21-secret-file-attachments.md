# Secret File Attachments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Confidential (age-encrypted) file attachments associated with secrets, plus standalone encrypted file uploads, uniform across Azure/AWS/local backends.

**Architecture:** A per-vault age x25519 identity stored as a reserved secret (`xv-attachment-key`) in the vault's own secret store. A new `src/secret/attachments.rs` module encrypts before `FileBackend::upload_file` and decrypts after `download_file` — no trait or backend-impl changes. Attachments live at `attachments/<secret-name>/<filename>` in existing file storage; association is the naming convention.

**Tech Stack:** Rust, existing `age` 0.10 crate, existing `FileBackend`/`SecretBackend` traits, `clap` derive CLI.

**Spec:** `docs/superpowers/specs/2026-07-21-secret-file-attachments-design.md`

## Global Constraints

- Reserved secret name is exactly `xv-attachment-key`; encrypted-file metadata flag is exactly `xv-encrypted: age`.
- Attachment blob path is exactly `attachments/<secret-name>/<filename>`.
- No changes to the `FileBackend` or `SecretBackend` traits or any backend implementation.
- Errors: missing key on decrypt → message contains `attachment key not found in vault '<v>'`; decrypt failure → message contains `wrong or rotated attachment key`.
- Run `cargo fmt` and `cargo clippy --all-targets` before each commit; both must be clean.
- All tests: `cargo test --lib` must pass at the end of every task.

---

### Task 1: Attachments module — constants and key management

**Files:**
- Create: `src/secret/attachments.rs`
- Modify: `src/secret/mod.rs` (add `pub mod attachments;`)

**Interfaces:**
- Consumes: `crate::backend::secret::SecretBackend` (`get_secret`, `set_secret`), `crate::secret::manager::SecretRequest`, `crate::backend::error::BackendError`, `crate::error::{CrosstacheError, Result}`.
- Produces (later tasks rely on these exact items):
  - `pub const ATTACHMENT_KEY_SECRET: &str = "xv-attachment-key";`
  - `pub const ENC_METADATA_KEY: &str = "xv-encrypted";`
  - `pub const ENC_METADATA_VALUE: &str = "age";`
  - `pub fn attachment_prefix(secret_name: &str) -> String` — returns `attachments/<secret_name>/`
  - `pub fn attachment_blob_name(secret_name: &str, attachment: &str) -> String`
  - `pub async fn get_or_create_identity(secrets: &dyn SecretBackend, vault: &str) -> Result<age::x25519::Identity>`
  - `pub async fn get_identity(secrets: &dyn SecretBackend, vault: &str) -> Result<age::x25519::Identity>`

- [ ] **Step 1: Register the module and write failing tests**

In `src/secret/mod.rs` add:

```rust
pub mod attachments;
```

Create `src/secret/attachments.rs` with the module doc, constants, path helpers, and a test module containing an in-memory `SecretBackend` stub (modeled on the `StubBackend` in `src/backend/secret.rs` tests, but tracking `set_count`) plus the key-management tests:

```rust
//! Secret file attachments — client-side age encryption over `FileBackend`.
//!
//! Attachments are age-encrypted with a per-vault x25519 identity stored as
//! the reserved secret [`ATTACHMENT_KEY_SECRET`] in the vault's own secret
//! store, so access to attachment plaintext is gated by vault (secret-store)
//! permissions, not storage-layer permissions. Ciphertext lives in ordinary
//! file storage under `attachments/<secret-name>/<filename>`; the association
//! is the naming convention. See
//! `docs/superpowers/specs/2026-07-21-secret-file-attachments-design.md`.

use age::secrecy::ExposeSecret;
use zeroize::Zeroizing;

use crate::backend::error::BackendError;
use crate::backend::secret::SecretBackend;
use crate::error::{CrosstacheError, Result};
use crate::secret::manager::SecretRequest;

/// Reserved per-vault secret holding the age identity for attachments.
pub const ATTACHMENT_KEY_SECRET: &str = "xv-attachment-key";
/// File-metadata key marking client-side-encrypted content.
pub const ENC_METADATA_KEY: &str = "xv-encrypted";
/// File-metadata value for age encryption.
pub const ENC_METADATA_VALUE: &str = "age";

/// Blob-name prefix for a secret's attachments.
pub fn attachment_prefix(secret_name: &str) -> String {
    format!("attachments/{secret_name}/")
}

/// Full blob name for one attachment of a secret.
pub fn attachment_blob_name(secret_name: &str, attachment: &str) -> String {
    format!("{}{attachment}", attachment_prefix(secret_name))
}
```

Test module (same file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::secret::manager::{
        SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
    };

    /// In-memory SecretBackend: get/set only, everything else Unsupported.
    /// `set_count` asserts key reuse (no regeneration on second call).
    pub(super) struct StubSecrets {
        pub secrets: Mutex<HashMap<String, String>>,
        pub set_count: Mutex<usize>,
    }

    impl StubSecrets {
        pub fn new() -> Self {
            Self {
                secrets: Mutex::new(HashMap::new()),
                set_count: Mutex::new(0),
            }
        }
    }

    fn props(name: &str, value: Option<&str>) -> SecretProperties {
        SecretProperties {
            name: name.to_string(),
            original_name: name.to_string(),
            value: value.map(|v| Zeroizing::new(v.to_string())),
            version: "v1".to_string(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::new(),
            content_type: String::new(),
            recovery_level: None,
        }
    }

    #[async_trait]
    impl SecretBackend for StubSecrets {
        async fn set_secret(
            &self,
            _vault: &str,
            request: SecretRequest,
        ) -> Result<SecretProperties, BackendError> {
            *self.set_count.lock().unwrap() += 1;
            self.secrets
                .lock()
                .unwrap()
                .insert(request.name.clone(), request.value.to_string());
            Ok(props(&request.name, None))
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
                .map(|v| props(name, include_value.then_some(v.as_str())))
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
            _group_filter: Option<&str>,
        ) -> Result<Vec<SecretSummary>, BackendError> {
            Ok(vec![])
        }

        async fn delete_secret(&self, _vault: &str, _name: &str) -> Result<(), BackendError> {
            Err(BackendError::Unsupported("delete".into()))
        }

        async fn update_secret(
            &self,
            _vault: &str,
            _name: &str,
            _request: SecretUpdateRequest,
        ) -> Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("update".into()))
        }
    }

    #[test]
    fn attachment_paths() {
        assert_eq!(attachment_prefix("db-cert"), "attachments/db-cert/");
        assert_eq!(
            attachment_blob_name("db-cert", "cert.pem"),
            "attachments/db-cert/cert.pem"
        );
    }

    #[tokio::test]
    async fn get_or_create_generates_once_and_reuses() {
        let stub = StubSecrets::new();
        let id1 = get_or_create_identity(&stub, "v").await.unwrap();
        let id2 = get_or_create_identity(&stub, "v").await.unwrap();
        assert_eq!(*stub.set_count.lock().unwrap(), 1, "second call must reuse");
        assert_eq!(id1.to_public().to_string(), id2.to_public().to_string());
        // Stored value is a valid age identity string.
        let stored = stub
            .secrets
            .lock()
            .unwrap()
            .get(ATTACHMENT_KEY_SECRET)
            .unwrap()
            .clone();
        assert!(stored.starts_with("AGE-SECRET-KEY-1"), "{stored}");
    }

    #[tokio::test]
    async fn get_identity_missing_key_is_actionable() {
        let stub = StubSecrets::new();
        let err = get_identity(&stub, "prod").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("attachment key not found in vault 'prod'"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn get_identity_garbage_value_is_an_error() {
        let stub = StubSecrets::new();
        stub.secrets
            .lock()
            .unwrap()
            .insert(ATTACHMENT_KEY_SECRET.to_string(), "not-a-key".to_string());
        assert!(get_identity(&stub, "v").await.is_err());
    }
}
```

Note: the stub's `SecretRequest.value` is `Zeroizing<String>`; `request.value.to_string()` clones the inner string — fine for a test stub.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib attachments`
Expected: compile error — `get_or_create_identity` / `get_identity` not found.

- [ ] **Step 3: Implement key management**

Add to `src/secret/attachments.rs` (below the path helpers):

```rust
/// Parse an age identity out of a stored secret value.
fn parse_identity(value: &str, vault: &str) -> Result<age::x25519::Identity> {
    value.trim().parse::<age::x25519::Identity>().map_err(|e| {
        CrosstacheError::invalid_argument(format!(
            "secret '{ATTACHMENT_KEY_SECRET}' in vault '{vault}' does not hold a valid age identity: {e}"
        ))
    })
}

/// Fetch the vault's attachment identity. Errors (actionably) if absent.
pub async fn get_identity(
    secrets: &dyn SecretBackend,
    vault: &str,
) -> Result<age::x25519::Identity> {
    match secrets.get_secret(vault, ATTACHMENT_KEY_SECRET, true).await {
        Ok(props) => {
            let value = props.value.ok_or_else(|| {
                CrosstacheError::invalid_argument(format!(
                    "secret '{ATTACHMENT_KEY_SECRET}' in vault '{vault}' has no value"
                ))
            })?;
            parse_identity(&value, vault)
        }
        Err(BackendError::NotFound { .. }) => Err(CrosstacheError::invalid_argument(format!(
            "attachment key not found in vault '{vault}' — no attachments have been created here, or the '{ATTACHMENT_KEY_SECRET}' secret was deleted"
        ))),
        Err(e) => Err(e.into()),
    }
}

/// Fetch the vault's attachment identity, generating and storing it on first
/// use. After a create, the stored value is re-read and used, so a concurrent
/// create race converges on a single key.
pub async fn get_or_create_identity(
    secrets: &dyn SecretBackend,
    vault: &str,
) -> Result<age::x25519::Identity> {
    match secrets.get_secret(vault, ATTACHMENT_KEY_SECRET, true).await {
        Ok(props) => {
            let value = props.value.ok_or_else(|| {
                CrosstacheError::invalid_argument(format!(
                    "secret '{ATTACHMENT_KEY_SECRET}' in vault '{vault}' has no value"
                ))
            })?;
            parse_identity(&value, vault)
        }
        Err(BackendError::NotFound { .. }) => {
            let identity = age::x25519::Identity::generate();
            let request = SecretRequest {
                name: ATTACHMENT_KEY_SECRET.to_string(),
                value: Zeroizing::new(identity.to_string().expose_secret().to_string()),
                content_type: Some("application/x-age-identity".to_string()),
                enabled: Some(true),
                expires_on: None,
                not_before: None,
                tags: None,
                groups: None,
                note: Some(
                    "crosstache attachment encryption key — deleting this makes all \
                     attachments in this vault unreadable"
                        .to_string(),
                ),
                folder: None,
            };
            secrets.set_secret(vault, request).await?;
            // Re-read: under a concurrent first-create, whichever write landed
            // last is authoritative; using the stored value converges all
            // clients on one key.
            let props = secrets.get_secret(vault, ATTACHMENT_KEY_SECRET, true).await?;
            let value = props.value.ok_or_else(|| {
                CrosstacheError::invalid_argument(format!(
                    "secret '{ATTACHMENT_KEY_SECRET}' in vault '{vault}' has no value"
                ))
            })?;
            parse_identity(&value, vault)
        }
        Err(e) => Err(e.into()),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib attachments`
Expected: 4 tests PASS.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets
git add src/secret/mod.rs src/secret/attachments.rs
git commit -m "feat(attachments): per-vault age key management in vault secret store"
```

---

### Task 2: Encrypted upload/download + list/delete helpers

**Files:**
- Modify: `src/secret/attachments.rs`

**Interfaces:**
- Consumes: Task 1 items; `crate::backend::file::FileBackend`; `crate::backend::local::crypto::{encrypt_bytes, decrypt_bytes, is_age_encrypted}` (already `pub`, local module is not feature-gated); `crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest}`; `crate::utils::progress::ProgressReporter`.
- Produces (later tasks rely on these exact signatures):
  - `pub async fn upload_encrypted(secrets: &dyn SecretBackend, files: &dyn FileBackend, vault: &str, request: FileUploadRequest, reporter: Option<&dyn ProgressReporter>) -> Result<FileInfo>`
  - `pub async fn download_decrypted(secrets: &dyn SecretBackend, files: &dyn FileBackend, vault: &str, name: &str, reporter: Option<&dyn ProgressReporter>) -> Result<Vec<u8>>` — transparently decrypts flagged files, passes unflagged files through unchanged
  - `pub async fn list_attachments(files: &dyn FileBackend, vault: &str, secret_name: &str) -> Result<Vec<FileInfo>>`
  - `pub async fn delete_attachments(files: &dyn FileBackend, vault: &str, secret_name: &str) -> Result<usize>` — returns number deleted

- [ ] **Step 1: Write failing tests**

Add to the existing `tests` module an in-memory `FileBackend` stub and the round-trip tests:

```rust
    use crate::backend::file::FileBackend;
    use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
    use crate::utils::progress::ProgressReporter;

    /// In-memory FileBackend storing (content, metadata) per name.
    pub(super) struct StubFiles {
        pub files: Mutex<HashMap<String, (Vec<u8>, HashMap<String, String>)>>,
    }

    impl StubFiles {
        pub fn new() -> Self {
            Self {
                files: Mutex::new(HashMap::new()),
            }
        }
    }

    fn file_info(name: &str, size: u64, metadata: HashMap<String, String>) -> FileInfo {
        FileInfo {
            name: name.to_string(),
            size,
            content_type: "application/octet-stream".to_string(),
            last_modified: chrono::Utc::now(),
            etag: String::new(),
            groups: Vec::new(),
            metadata,
            tags: HashMap::new(),
        }
    }

    #[async_trait]
    impl FileBackend for StubFiles {
        async fn upload_file(
            &self,
            _vault: &str,
            request: FileUploadRequest,
            _reporter: Option<&dyn ProgressReporter>,
        ) -> Result<FileInfo, BackendError> {
            let info = file_info(
                &request.name,
                request.content.len() as u64,
                request.metadata.clone(),
            );
            self.files
                .lock()
                .unwrap()
                .insert(request.name, (request.content, request.metadata));
            Ok(info)
        }

        async fn download_file(
            &self,
            _vault: &str,
            name: &str,
            _reporter: Option<&dyn ProgressReporter>,
        ) -> Result<Vec<u8>, BackendError> {
            self.files
                .lock()
                .unwrap()
                .get(name)
                .map(|(c, _)| c.clone())
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }

        async fn list_files(
            &self,
            _vault: &str,
            request: FileListRequest,
        ) -> Result<Vec<FileInfo>, BackendError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| {
                    request
                        .prefix
                        .as_ref()
                        .is_none_or(|p| name.starts_with(p.as_str()))
                })
                .map(|(name, (c, m))| file_info(name, c.len() as u64, m.clone()))
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

        async fn get_file_info(&self, _vault: &str, name: &str) -> Result<FileInfo, BackendError> {
            self.files
                .lock()
                .unwrap()
                .get(name)
                .map(|(c, m)| file_info(name, c.len() as u64, m.clone()))
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }
    }

    fn upload_req(name: &str, content: &[u8]) -> FileUploadRequest {
        FileUploadRequest {
            name: name.to_string(),
            content: content.to_vec(),
            content_type: None,
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn encrypted_round_trip() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        let plaintext = b"-----BEGIN CERT-----\x00\xffbinary ok";

        upload_encrypted(&secrets, &files, "v", upload_req("attachments/db/cert.pem", plaintext), None)
            .await
            .unwrap();

        // Stored blob is ciphertext, flagged, and not the plaintext.
        {
            let store = files.files.lock().unwrap();
            let (stored, meta) = store.get("attachments/db/cert.pem").unwrap();
            assert!(crate::backend::local::crypto::is_age_encrypted(stored));
            assert_ne!(stored.as_slice(), plaintext);
            assert_eq!(meta.get(ENC_METADATA_KEY).map(String::as_str), Some(ENC_METADATA_VALUE));
        }

        let roundtrip = download_decrypted(&secrets, &files, "v", "attachments/db/cert.pem", None)
            .await
            .unwrap();
        assert_eq!(roundtrip.as_slice(), plaintext);
    }

    #[tokio::test]
    async fn download_passes_through_unencrypted_files() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        files
            .upload_file("v", upload_req("plain.txt", b"hello"), None)
            .await
            .unwrap();
        let content = download_decrypted(&secrets, &files, "v", "plain.txt", None)
            .await
            .unwrap();
        assert_eq!(content, b"hello");
    }

    #[tokio::test]
    async fn download_flagged_file_without_key_names_the_problem() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        upload_encrypted(&secrets, &files, "v", upload_req("attachments/s/f", b"x"), None)
            .await
            .unwrap();
        // Simulate key deletion.
        secrets.secrets.lock().unwrap().remove(ATTACHMENT_KEY_SECRET);
        let err = download_decrypted(&secrets, &files, "v", "attachments/s/f", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("attachment key not found in vault 'v'"), "{err}");
    }

    #[tokio::test]
    async fn download_with_wrong_key_is_actionable() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        upload_encrypted(&secrets, &files, "v", upload_req("attachments/s/f", b"x"), None)
            .await
            .unwrap();
        // Replace the key with a different (valid) identity.
        let other = age::x25519::Identity::generate();
        secrets.secrets.lock().unwrap().insert(
            ATTACHMENT_KEY_SECRET.to_string(),
            other.to_string().expose_secret().to_string(),
        );
        let err = download_decrypted(&secrets, &files, "v", "attachments/s/f", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("wrong or rotated attachment key"), "{err}");
    }

    #[tokio::test]
    async fn list_and_delete_scope_to_one_secrets_prefix() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        for name in [
            "attachments/db/cert.pem",
            "attachments/db/key.pem",
            "attachments/other/f.txt",
        ] {
            upload_encrypted(&secrets, &files, "v", upload_req(name, b"x"), None)
                .await
                .unwrap();
        }
        files.upload_file("v", upload_req("normal.txt", b"y"), None).await.unwrap();

        let listed = list_attachments(&files, "v", "db").await.unwrap();
        let mut names: Vec<_> = listed.iter().map(|f| f.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["attachments/db/cert.pem", "attachments/db/key.pem"]);

        let deleted = delete_attachments(&files, "v", "db").await.unwrap();
        assert_eq!(deleted, 2);
        assert!(list_attachments(&files, "v", "db").await.unwrap().is_empty());
        // Other secret's attachment and normal files untouched.
        assert!(files.files.lock().unwrap().contains_key("attachments/other/f.txt"));
        assert!(files.files.lock().unwrap().contains_key("normal.txt"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib attachments`
Expected: compile error — `upload_encrypted` etc. not found.

- [ ] **Step 3: Implement**

Add to `src/secret/attachments.rs`:

```rust
use crate::backend::file::FileBackend;
use crate::backend::local::crypto;
use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
use crate::utils::progress::ProgressReporter;

/// Age-encrypt `request.content` with the vault's attachment key (created on
/// first use) and upload the ciphertext, flagged `xv-encrypted: age`.
pub async fn upload_encrypted(
    secrets: &dyn SecretBackend,
    files: &dyn FileBackend,
    vault: &str,
    mut request: FileUploadRequest,
    reporter: Option<&dyn ProgressReporter>,
) -> Result<FileInfo> {
    let identity = get_or_create_identity(secrets, vault).await?;
    let recipient = identity.to_public();
    request.content = crypto::encrypt_bytes(&request.content, &[recipient])?;
    request
        .metadata
        .insert(ENC_METADATA_KEY.to_string(), ENC_METADATA_VALUE.to_string());
    files
        .upload_file(vault, request, reporter)
        .await
        .map_err(CrosstacheError::from)
}

/// Download a file, transparently decrypting it when it carries the
/// `xv-encrypted: age` metadata flag. Unflagged files (including user-supplied
/// `.age` files encrypted with foreign keys) pass through untouched.
pub async fn download_decrypted(
    secrets: &dyn SecretBackend,
    files: &dyn FileBackend,
    vault: &str,
    name: &str,
    reporter: Option<&dyn ProgressReporter>,
) -> Result<Vec<u8>> {
    let data = files
        .download_file(vault, name, reporter)
        .await
        .map_err(CrosstacheError::from)?;
    // Cheap local sniff first: only consult metadata for age-shaped content.
    if !crypto::is_age_encrypted(&data) {
        return Ok(data);
    }
    let info = files
        .get_file_info(vault, name)
        .await
        .map_err(CrosstacheError::from)?;
    if info.metadata.get(ENC_METADATA_KEY).map(String::as_str) != Some(ENC_METADATA_VALUE) {
        return Ok(data); // foreign age file — not ours to decrypt
    }
    let identity = get_identity(secrets, vault).await?;
    let plaintext = crypto::decrypt_bytes(&data, &identity).map_err(|e| {
        CrosstacheError::invalid_argument(format!(
            "failed to decrypt '{name}' in vault '{vault}': wrong or rotated attachment key ({e})"
        ))
    })?;
    Ok(plaintext.to_vec())
}

/// List all attachments of `secret_name` (full blob names).
pub async fn list_attachments(
    files: &dyn FileBackend,
    vault: &str,
    secret_name: &str,
) -> Result<Vec<FileInfo>> {
    files
        .list_files(
            vault,
            FileListRequest {
                prefix: Some(attachment_prefix(secret_name)),
                groups: None,
                limit: None,
                delimiter: None,
            },
        )
        .await
        .map_err(CrosstacheError::from)
}

/// Delete every attachment of `secret_name`. Returns the number deleted.
pub async fn delete_attachments(
    files: &dyn FileBackend,
    vault: &str,
    secret_name: &str,
) -> Result<usize> {
    let attachments = list_attachments(files, vault, secret_name).await?;
    for a in &attachments {
        files
            .delete_file(vault, &a.name)
            .await
            .map_err(CrosstacheError::from)?;
    }
    Ok(attachments.len())
}
```

Note: `crypto::decrypt_bytes` returns `Zeroizing<Vec<u8>>`; `.to_vec()` copies out — matches the existing plaintext-`Vec<u8>` download contract.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib attachments`
Expected: all 9 attachments tests PASS.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets
git add src/secret/attachments.rs
git commit -m "feat(attachments): age encrypt/decrypt over FileBackend with list/delete helpers"
```

---

### Task 3: CLI — `xv attach`, `xv attachments`, `xv detach`

**Files:**
- Create: `src/cli/attach_ops.rs`
- Modify: `src/cli/mod.rs` (add `pub mod attach_ops;` alongside the other ops modules)
- Modify: `src/cli/commands.rs` (three new `Commands` variants + three dispatch arms)
- Modify: `src/cli/file_ops.rs` (make `file_storage_unsupported_error` `pub(crate)`)

**Interfaces:**
- Consumes: `crate::secret::attachments::{upload_encrypted, download_decrypted, list_attachments, attachment_blob_name, attachment_prefix}` (Task 1/2 signatures); `crate::cli::helpers::{resolve_workspace_or_default, confirm_destructive}`; `crate::cli::file_ops::file_storage_unsupported_error`; `crate::workspace::TargetMode`.
- Produces:
  - `pub(crate) async fn execute_attach(secret: String, file: String, name: Option<String>, config: Config) -> Result<()>`
  - `pub(crate) async fn execute_attachments(secret: String, get: Option<String>, output: Option<String>, config: Config) -> Result<()>`
  - `pub(crate) async fn execute_detach(secret: String, name: String, force: bool, config: Config) -> Result<()>`

This is CLI plumbing over the tested Task 2 helpers, driven by end-to-end verification rather than unit tests (the resolution path needs a live backend; the local backend makes that cheap).

- [ ] **Step 1: Add the `Commands` variants**

In `src/cli/commands.rs`, inside `pub enum Commands` (place after the `Delete` variant):

```rust
    /// Attach an encrypted file to a secret (stored age-encrypted; readable
    /// only with vault access)
    Attach {
        /// Secret name
        secret: String,
        /// Local file path
        file: String,
        /// Attachment name (defaults to the file's basename)
        #[arg(long)]
        name: Option<String>,
    },
    /// List a secret's attachments, or download one with --get
    Attachments {
        /// Secret name
        secret: String,
        /// Download this attachment (decrypted)
        #[arg(long)]
        get: Option<String>,
        /// Output path for --get (defaults to the attachment name in the
        /// current directory)
        #[arg(short, long, requires = "get")]
        output: Option<String>,
    },
    /// Remove an attachment from a secret
    Detach {
        /// Secret name
        secret: String,
        /// Attachment name
        name: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },
```

In the dispatch `match` (near the `Commands::Delete { .. }` arm):

```rust
            Commands::Attach { secret, file, name } => {
                crate::cli::attach_ops::execute_attach(secret, file, name, config).await
            }
            Commands::Attachments {
                secret,
                get,
                output,
            } => {
                crate::cli::attach_ops::execute_attachments(secret, get, output, config).await
            }
            Commands::Detach {
                secret,
                name,
                force,
            } => crate::cli::attach_ops::execute_detach(secret, name, force, config).await,
```

- [ ] **Step 2: Implement `src/cli/attach_ops.rs`**

In `src/cli/file_ops.rs`, change `fn file_storage_unsupported_error` to `pub(crate) fn file_storage_unsupported_error`. In `src/cli/mod.rs`, add `pub mod attach_ops;`.

Create `src/cli/attach_ops.rs`:

```rust
//! Attachment subcommand execution (`xv attach`, `xv attachments`, `xv detach`).
//!
//! Thin CLI plumbing over [`crate::secret::attachments`]: resolve the target
//! secret's backend + vault, gate on file-storage capability, delegate.

use std::path::Path;
use std::sync::Arc;

use crate::backend::Backend;
use crate::cli::file_ops::file_storage_unsupported_error;
use crate::cli::helpers::{confirm_destructive, resolve_workspace_or_default};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::attachments;
use crate::utils::format::format_size;
use crate::utils::output;
use crate::workspace::TargetMode;

/// Reject attachment names that would escape the `attachments/<secret>/`
/// prefix or produce surprising nested paths.
fn validate_attachment_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
    {
        return Err(CrosstacheError::invalid_argument(format!(
            "invalid attachment name '{name}': must be a plain file name (no path separators)"
        )));
    }
    Ok(())
}

/// Resolve `(backend, vault, resolved_secret_name)` for an attachment verb and
/// gate on file-storage capability.
async fn resolve(
    secret: &str,
    config: &Config,
    mode: TargetMode,
) -> Result<(Arc<dyn Backend>, String, String)> {
    let (backend, _backend_name, vault, resolved_name) =
        resolve_workspace_or_default(secret, config, mode).await?;
    if backend.files().is_none() {
        return Err(file_storage_unsupported_error(backend.as_ref()));
    }
    Ok((backend, vault, resolved_name))
}

pub(crate) async fn execute_attach(
    secret: String,
    file: String,
    name: Option<String>,
    config: Config,
) -> Result<()> {
    let (backend, vault, secret_name) = resolve(&secret, &config, TargetMode::Write).await?;

    // Attaching to a missing secret is almost always a typo — fail early.
    if !backend.secrets().secret_exists(&vault, &secret_name).await? {
        return Err(CrosstacheError::invalid_argument(format!(
            "secret '{secret_name}' not found in vault '{vault}' — create it first with 'xv set'"
        )));
    }

    let path = Path::new(&file);
    if !path.exists() {
        return Err(CrosstacheError::config(format!("File not found: {file}")));
    }
    let attachment_name = match name {
        Some(n) => n,
        None => path
            .file_name()
            .ok_or_else(|| {
                CrosstacheError::invalid_argument(format!("cannot derive a file name from '{file}'"))
            })?
            .to_string_lossy()
            .to_string(),
    };
    validate_attachment_name(&attachment_name)?;

    let content = std::fs::read(path)
        .map_err(|e| CrosstacheError::config(format!("Failed to read file {file}: {e}")))?;
    let size = content.len() as u64;

    let request = crate::blob::models::FileUploadRequest {
        name: attachments::attachment_blob_name(&secret_name, &attachment_name),
        content,
        content_type: None,
        groups: Vec::new(),
        metadata: std::collections::HashMap::new(),
        tags: std::collections::HashMap::new(),
    };
    let files = backend.files().expect("resolve gated on files()");
    attachments::upload_encrypted(backend.secrets(), files, &vault, request, None).await?;
    output::success(&format!(
        "Attached '{attachment_name}' ({}) to secret '{secret_name}' (encrypted)",
        format_size(size)
    ));
    Ok(())
}

pub(crate) async fn execute_attachments(
    secret: String,
    get: Option<String>,
    output_path: Option<String>,
    config: Config,
) -> Result<()> {
    let (backend, vault, secret_name) = resolve(&secret, &config, TargetMode::Read).await?;
    let files = backend.files().expect("resolve gated on files()");

    if let Some(attachment_name) = get {
        validate_attachment_name(&attachment_name)?;
        let blob_name = attachments::attachment_blob_name(&secret_name, &attachment_name);
        let content =
            attachments::download_decrypted(backend.secrets(), files, &vault, &blob_name, None)
                .await?;
        let out = output_path.unwrap_or_else(|| attachment_name.clone());
        if Path::new(&out).exists() {
            return Err(CrosstacheError::config(format!(
                "File '{out}' already exists — pass --output to choose another path"
            )));
        }
        std::fs::write(&out, &content)
            .map_err(|e| CrosstacheError::config(format!("Failed to write '{out}': {e}")))?;
        output::success(&format!(
            "Downloaded attachment '{attachment_name}' to '{out}' ({})",
            format_size(content.len() as u64)
        ));
        return Ok(());
    }

    let listed = attachments::list_attachments(files, &vault, &secret_name).await?;
    if listed.is_empty() {
        output::info(&format!("No attachments on secret '{secret_name}'"));
        return Ok(());
    }
    let prefix = attachments::attachment_prefix(&secret_name);
    for f in &listed {
        let short = f.name.strip_prefix(&prefix).unwrap_or(&f.name);
        println!("{short}\t{}\t{}", format_size(f.size), f.last_modified.format("%Y-%m-%d %H:%M"));
    }
    println!("{} attachment(s) on '{secret_name}'", listed.len());
    Ok(())
}

pub(crate) async fn execute_detach(
    secret: String,
    name: String,
    force: bool,
    config: Config,
) -> Result<()> {
    let (backend, vault, secret_name) = resolve(&secret, &config, TargetMode::Write).await?;
    validate_attachment_name(&name)?;
    if !confirm_destructive(
        force,
        &format!("Remove attachment '{name}' from secret '{secret_name}'?"),
    )? {
        output::info("Aborted; attachment not removed.");
        return Ok(());
    }
    let files = backend.files().expect("resolve gated on files()");
    files
        .delete_file(&vault, &attachments::attachment_blob_name(&secret_name, &name))
        .await
        .map_err(CrosstacheError::from)?;
    output::success(&format!("Detached '{name}' from '{secret_name}'"));
    Ok(())
}
```

Note: displayed size is the ciphertext size for listings (that's what storage reports); attach/download report the plaintext size they handled. If `output::info`/`output::success`/`output::warn` signatures differ from these calls, match the usage in `src/cli/file_ops.rs`.

- [ ] **Step 3: Build and fix compile errors**

Run: `cargo build`
Expected: clean build. Common issues: `Backend::files()` returns `Option<&dyn FileBackend>` borrowed from the `Arc` — bind `let files = backend.files()...` after the last use of `backend.secrets()` if the borrow checker complains (both are `&` borrows, so simultaneous use is fine).

- [ ] **Step 4: End-to-end verification against the local backend**

Using a throwaway local-backend config (see `src/backend/local/` — a config with a `[local]` store path; if a dev config already exists, use it):

```bash
export XDG_CONFIG_HOME=$(mktemp -d)
cargo run -- config init --backend local 2>/dev/null || true  # or the repo's documented local setup
cargo run -- set demo-secret --value hunter2
echo "cert-bytes" > /tmp/cert.pem
cargo run -- attach demo-secret /tmp/cert.pem
cargo run -- attachments demo-secret
cargo run -- attachments demo-secret --get cert.pem --output /tmp/cert-out.pem
diff /tmp/cert.pem /tmp/cert-out.pem && echo ROUNDTRIP-OK
cargo run -- detach demo-secret cert.pem --force
cargo run -- attachments demo-secret   # expect "No attachments"
```

Expected: `ROUNDTRIP-OK`, and the store's `files/` directory contains ciphertext (not `cert-bytes`). Adjust the init incantation to the repo's actual local-backend setup command if it differs — the assertion that matters is the round trip.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets
git add src/cli/attach_ops.rs src/cli/mod.rs src/cli/commands.rs src/cli/file_ops.rs
git commit -m "feat(cli): xv attach / attachments / detach commands"
```

---

### Task 4: `xv file upload --encrypt` and auto-decrypting `xv file download`

**Files:**
- Modify: `src/cli/file.rs` (add `--encrypt` to `FileCommands::Upload`)
- Modify: `src/cli/file_ops.rs` (`FileOps` gains `secrets`; download auto-decrypts; upload takes `encrypt`)

**Interfaces:**
- Consumes: `attachments::{upload_encrypted, download_decrypted}` (Task 2 signatures).
- Produces: `FileOps` struct gains field `secrets: &'a dyn crate::backend::secret::SecretBackend` and its `new` gains the matching parameter — all 4 construction sites (`src/cli/file_ops.rs:151`, `:410`, `:2421`, `:2463`) pass `backend.secrets()`.

- [ ] **Step 1: Add the flag**

In `src/cli/file.rs`, `FileCommands::Upload`, add after `continue_on_error`:

```rust
        /// Encrypt the file client-side with the vault's attachment key
        /// before upload (readable only with vault access). Single-file
        /// uploads only.
        #[arg(long)]
        encrypt: bool,
```

- [ ] **Step 2: Thread `secrets` through `FileOps`**

In `src/cli/file_ops.rs`:

```rust
pub(crate) struct FileOps<'a> {
    files: &'a dyn FileBackend,
    secrets: &'a dyn crate::backend::secret::SecretBackend,
    vault: &'a str,
    backend_name: &'a str,
    kind: BackendKind,
}
```

Update `FileOps::new` to take `secrets: &'a dyn crate::backend::secret::SecretBackend` as its second parameter, and update all 4 call sites to `FileOps::new(files, backend.secrets(), &vault, &backend_name, backend.kind())`.

Replace the body of `FileOps::download_file` so every download path auto-decrypts flagged files:

```rust
    async fn download_file(
        &self,
        request: FileDownloadRequest,
        reporter: &dyn ProgressReporter,
    ) -> Result<Vec<u8>> {
        crate::secret::attachments::download_decrypted(
            self.secrets,
            self.files,
            self.vault,
            &request.name,
            Some(reporter),
        )
        .await
    }
```

Add an encrypted-upload method next to `upload_file`:

```rust
    async fn upload_file_encrypted(
        &self,
        request: FileUploadRequest,
        reporter: &dyn ProgressReporter,
    ) -> Result<FileInfo> {
        crate::secret::attachments::upload_encrypted(
            self.secrets,
            self.files,
            self.vault,
            request,
            Some(reporter),
        )
        .await
    }
```

- [ ] **Step 3: Wire `--encrypt` into the Upload arm**

In `execute_file_command`'s `FileCommands::Upload` arm, add `encrypt` to the destructuring. Before the `if recursive` branch add:

```rust
            // ponytail: --encrypt is single-file only; extend to multi/recursive when needed
            if encrypt && (recursive || files.len() > 1) {
                return Err(CrosstacheError::invalid_argument(
                    "--encrypt currently supports single-file uploads only",
                ));
            }
```

Add an `encrypt: bool` parameter to `execute_file_upload` (after `content_type`), pass `encrypt` from the Upload arm and `false` from any other caller (grep `execute_file_upload(` — `execute_file_upload_quick` routes through its own request construction; only change callers that call this exact function). Inside `execute_file_upload`, replace the upload call:

```rust
    let file_info = if encrypt {
        blob_manager
            .upload_file_encrypted(upload_request, reporter.as_ref())
            .await?
    } else {
        blob_manager
            .upload_file(upload_request, reporter.as_ref())
            .await?
    };
```

- [ ] **Step 4: Verify**

Run: `cargo build && cargo test --lib`
Expected: clean build, all tests pass (attachments round-trip tests already cover the encrypt/decrypt seam; `download_passes_through_unencrypted_files` covers normal downloads being unaffected).

End-to-end (same local store as Task 3):

```bash
echo "top secret" > /tmp/conf.txt
cargo run -- file upload /tmp/conf.txt --encrypt
cargo run -- file download conf.txt --output /tmp/conf-out.txt
diff /tmp/conf.txt /tmp/conf-out.txt && echo ENCRYPT-ROUNDTRIP-OK
```

Expected: `ENCRYPT-ROUNDTRIP-OK`.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets
git add src/cli/file.rs src/cli/file_ops.rs
git commit -m "feat(file): --encrypt upload flag and transparent decrypting download"
```

---

### Task 5: Hide the reserved key from `xv list`; guard its deletion

**Files:**
- Modify: `src/cli/secret_ops.rs` (`filter_secret_summaries_for_display` at ~line 1045; delete confirm in `execute_secret_delete_direct` at ~line 2685)

**Interfaces:**
- Consumes: `crate::secret::attachments::ATTACHMENT_KEY_SECRET`.
- Produces: nothing new — behavior changes only.

- [ ] **Step 1: Write the failing filter test**

`filter_secret_summaries_for_display` is the shared choke point for every list display path (6 call sites). Add to the test module in `src/cli/secret_ops.rs` (create `#[cfg(test)] mod tests` at the bottom if none exists; if one exists, append):

```rust
    #[test]
    fn reserved_attachment_key_is_hidden_from_listings() {
        fn summary(name: &str) -> crate::secret::manager::SecretSummary {
            crate::secret::manager::SecretSummary {
                name: name.to_string(),
                original_name: name.to_string(),
                note: None,
                folder: None,
                groups: None,
                updated_on: String::new(),
                enabled: true,
                content_type: String::new(),
                tags: std::collections::HashMap::new(),
            }
        }
        let secrets = vec![
            summary("normal"),
            summary(crate::secret::attachments::ATTACHMENT_KEY_SECRET),
        ];
        let out = filter_secret_summaries_for_display(secrets, None, true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "normal");
    }
```

(These are all of `SecretSummary`'s fields — `src/secret/manager.rs:163`; it does not derive `Default`.)

Run: `cargo test --lib reserved_attachment_key`
Expected: FAIL — 2 items returned.

- [ ] **Step 2: Implement the filter**

In `filter_secret_summaries_for_display` (src/cli/secret_ops.rs:1045), first line of the body:

```rust
    // The attachment key is infrastructure, not a user secret.
    secrets.retain(|s| s.name != crate::secret::attachments::ATTACHMENT_KEY_SECRET);
```

Run: `cargo test --lib reserved_attachment_key`
Expected: PASS.

- [ ] **Step 3: Deletion guard**

In `execute_secret_delete_direct`, single-name branch (src/cli/secret_ops.rs:~2685), replace the confirm call:

```rust
            let prompt = if resolved_name == crate::secret::attachments::ATTACHMENT_KEY_SECRET {
                format!(
                    "'{resolved_name}' is the attachment encryption key for vault '{vault_name}'. \
                     Deleting it makes ALL attachments in this vault permanently unreadable. Delete anyway?"
                )
            } else {
                format!("Delete secret '{resolved_name}'?")
            };
            if !confirm_destructive(force, &prompt)? {
```

- [ ] **Step 4: Verify and commit**

```bash
cargo test --lib && cargo fmt && cargo clippy --all-targets
git add src/cli/secret_ops.rs
git commit -m "feat(attachments): hide reserved key from listings, warn on its deletion"
```

---

### Task 6: Delete cascade + docs

**Files:**
- Modify: `src/cli/secret_ops.rs` (`execute_secret_delete_direct`, both the single-name and group branches)
- Modify: `CHANGELOG.md` (Unreleased/next-version entry)
- Modify: `CLAUDE.md` (Current Implementation Status section)

**Interfaces:**
- Consumes: `attachments::{list_attachments, delete_attachments}` (Task 2), `backend.files()`.
- Produces: nothing new — behavior + docs.

- [ ] **Step 1: Implement the cascade (single-name branch)**

In `execute_secret_delete_direct`'s single-name branch, after resolution and before the confirm (building on the Task 5 prompt):

```rust
            // Attachments cascade: count first so the confirmation is honest.
            let attachment_count = match backend.files() {
                Some(files) => {
                    crate::secret::attachments::list_attachments(files, &vault_name, &resolved_name)
                        .await
                        .map(|a| a.len())
                        .unwrap_or(0) // listing failure must not block deletion
                }
                None => 0,
            };
            let prompt = if resolved_name == crate::secret::attachments::ATTACHMENT_KEY_SECRET {
                format!(
                    "'{resolved_name}' is the attachment encryption key for vault '{vault_name}'. \
                     Deleting it makes ALL attachments in this vault permanently unreadable. Delete anyway?"
                )
            } else if attachment_count > 0 {
                format!(
                    "Delete secret '{resolved_name}' and its {attachment_count} attachment(s)?"
                )
            } else {
                format!("Delete secret '{resolved_name}'?")
            };
```

After the successful `delete_secret` call (before the cache invalidation):

```rust
            if attachment_count > 0 {
                if let Some(files) = backend.files() {
                    let n = crate::secret::attachments::delete_attachments(
                        files,
                        &vault_name,
                        &resolved_name,
                    )
                    .await?;
                    output::info(&format!("Deleted {n} attachment(s)"));
                }
            }
```

In the group-delete branch's per-secret loop, after each `delete_secret` succeeds:

```rust
                if let Some(files) = backend.files() {
                    let n = crate::secret::attachments::delete_attachments(
                        files, &vault_name, &s.name,
                    )
                    .await
                    .unwrap_or(0); // best-effort in bulk path
                    if n > 0 {
                        output::info(&format!("Deleted {n} attachment(s) of '{}'", s.name));
                    }
                }
```

- [ ] **Step 2: Manual verification (local backend)**

```bash
cargo run -- set doomed --value x
echo hi > /tmp/a.txt
cargo run -- attach doomed /tmp/a.txt
cargo run -- delete doomed   # prompt must mention "1 attachment(s)"
cargo run -- attachments doomed 2>&1 | head -1  # expect no attachments / not found
```

- [ ] **Step 3: Docs**

`CHANGELOG.md` — add under the unreleased/next-version heading (match the file's existing entry style):

```markdown
- **Secret File Attachments**: `xv attach <secret> <file>`, `xv attachments <secret> [--get <name>]`, `xv detach <secret> <name>` — files age-encrypted client-side with a per-vault key stored as the reserved `xv-attachment-key` secret, so attachment access is gated by vault permissions on every backend (Azure/AWS/local). Also `xv file upload --encrypt` for standalone confidential files; `xv file download` decrypts transparently. `xv delete` cascades a secret's attachments.
```

`CLAUDE.md` — in "Current Implementation Status", add one line following the existing bullet style:

```markdown
- **Secret File Attachments**: `xv attach`/`xv attachments`/`xv detach` plus `xv file upload --encrypt` — client-side age encryption with per-vault key custody in the vault's secret store (`xv-attachment-key`); see `docs/superpowers/specs/2026-07-21-secret-file-attachments-design.md`.
```

- [ ] **Step 4: Full verification and commit**

```bash
cargo test --lib && cargo test && cargo fmt && cargo clippy --all-targets
git add src/cli/secret_ops.rs CHANGELOG.md CLAUDE.md
git commit -m "feat(attachments): cascade attachment deletion with xv delete; docs"
```

(`cargo test` without `--lib` runs integration tests that may require Azure credentials; if they fail for credential reasons only, note it and proceed — the gate is `cargo test --lib`.)

---

## Self-Review Notes

- Spec §1 (key mgmt) → Task 1; §2 (encryption layer) → Task 2 + Task 4; §3 (association + cascade) → Task 2 helpers + Task 6; §4 (CLI) → Task 3 + Task 4; §5 (errors) → Tasks 1–2 tests assert the exact messages; §6 (testing) → Tasks 1, 2, 5.
- Deliberate simplifications (all with retrofit paths): buffered (not streaming) encryption; `--encrypt` single-file only; no per-file keys or rotation command; attachment listing is plain tab-separated output, not `TableFormatter`.
- Type consistency verified: `SecretSummary` fields in Task 5's test match `src/secret/manager.rs:163`; all `attachments::*` signatures used in Tasks 3–6 match Task 1/2 definitions.
