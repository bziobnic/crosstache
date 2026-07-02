# Rename Fix Implementation Plan (`xv update --rename`, issue #295)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the spec (`docs/superpowers/specs/2026-07-02-rename-fix-design.md`): make `xv update --rename` actually move a secret on all three backends via a new provided `SecretBackend::rename_secret` trait method (read value+metadata → create under new name → delete old), restore the `RenameIncomplete` error + exit code 43, delete the ignorable `SecretUpdateRequest.new_name` field, and wire the CLI so other update flags apply first, then the rename.

**Architecture:** One default trait method in `src/backend/secret.rs` composed from the trait's own primitives (`get_secret`/`set_secret`/`delete_secret`/`secret_exists`) covers Azure, local, and AWS with zero backend-specific rename code — all three backends expose groups/note/folder under canonical tag keys in `SecretProperties.tags`. Partial failure (new created, old delete failed) surfaces `BackendError::RenameIncomplete`, mapped to a restored `CrosstacheError::RenameIncomplete` (verbatim v0.16.0 shape, exit 43, `xv-rename-incomplete`). The CLI splits `--rename` out of `SecretUpdateRequest` entirely: update-in-place first (when other flags are present), then rename.

**Tech Stack:** Rust, `async-trait`, `thiserror`, existing backend trait stack, `tests/e2e_local_backend.rs` `TestEnv` harness, LocalStack (gated), live Azure e2e (ignored).

## Global Constraints

- Branch: `fix/rename-295` (current; commit the spec and this plan on it before executing tasks).
- **NEVER run `xv init` and NEVER write to `~/.config/xv/`** (a prior agent clobbered the user's real config). All local-backend CLI testing goes through `tests/e2e_local_backend.rs`'s `TestEnv` (isolated temp `XDG_CONFIG_HOME` + `XV_BACKEND=local`). Never invoke bare `xv`/`cargo run` without an isolating `XDG_CONFIG_HOME` unless the command is provably read-only. (The live Azure e2e *reads* the real config — that is fine; it must never write it.)
- **Any live Azure fixture must be cleaned up** — delete every created secret even on failure (use uniquely timestamped names + the harness `cleanup` helper). The `heythere` test vault has purge protection, so purging is blocked by vault policy — soft-delete-only cleanup is acceptable and documented in the harness header.
- **No machine-shape changes to shipped commands.** The only user-visible changes: rename works, the restored `RenameIncomplete` error + exit code 43 + `xv-rename-incomplete` code, the `Successfully renamed secret 'a' to 'b'` success line, and docs. `xv update NAME --note x` (no rename) output stays byte-identical.
- **Commit style `feat:`/`fix:`/`docs:`** and every commit message ends with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- **`cargo fmt` before every commit.**
- **Locate anchors by symbol, not line number** — line numbers cited below are current as of writing and may drift.
- The new secret is deliberately **never rolled back** on partial failure (no secret material may be lost), and the old name is never purged.
- Placeholders (`TODO`, stub tests, `unimplemented!`) are plan failures — every step below carries its full code.

---

### Task 1: Error foundation — `RenameIncomplete` + exit code 43 + docs row

**Files:**
- Modify: `src/error.rs` (variant, `code()`, `exit_code()`, tests)
- Modify: `src/backend/error.rs` (variant, `From` mapping, test)
- Modify: `docs/exit-codes.md` (restore the 43 row)

**Interfaces:**
- Consumes: nothing.
- Produces (Tasks 2 and 4 rely on these exact shapes):
  - `CrosstacheError::RenameIncomplete { source: String, destination: String, vault: String, cause: Box<CrosstacheError> }` — code `xv-rename-incomplete`, exit code 43.
  - `BackendError::RenameIncomplete { source: String, destination: String, vault: String, cause: Box<BackendError> }` — converts via `From<BackendError> for CrosstacheError` field-for-field.

- [ ] **Step 1: Write the failing tests**

In `src/error.rs`'s test module:

Add to the `cases` vec inside `test_code_for_every_variant` (anchor: the `(CrosstacheError::unknown("x"), "xv-unknown")` entry — insert just before it):

```rust
            (
                CrosstacheError::RenameIncomplete {
                    source: "old".into(),
                    destination: "new".into(),
                    vault: "v".into(),
                    cause: Box::new(CrosstacheError::unknown("x")),
                },
                "xv-rename-incomplete",
            ),
```

In `test_exit_code_families` (anchor: the `rate_limited` assertion in the `// 40–49` block), add after it:

```rust
        assert_eq!(
            CrosstacheError::RenameIncomplete {
                source: "old".into(),
                destination: "new".into(),
                vault: "v".into(),
                cause: Box::new(CrosstacheError::network("x")),
            }
            .exit_code(),
            43
        );
```

Add a new test next to `test_exit_code_families`:

```rust
    #[test]
    fn rename_incomplete_names_both_copies_and_the_recovery_steps() {
        let err = CrosstacheError::RenameIncomplete {
            source: "old-name".into(),
            destination: "new-name".into(),
            vault: "my-vault".into(),
            cause: Box::new(CrosstacheError::network("dial tcp: timeout")),
        };
        let msg = err.to_string();
        assert!(msg.contains("'old-name'") && msg.contains("'new-name'"), "{msg}");
        assert!(msg.contains("vault 'my-vault'"), "{msg}");
        assert!(msg.contains("Both secrets still exist"), "{msg}");
        assert!(
            msg.contains("`xv get new-name`") && msg.contains("`xv delete old-name`"),
            "recovery steps missing: {msg}"
        );
        assert!(msg.contains("dial tcp: timeout"), "cause not surfaced: {msg}");
    }
```

In the `serialized_security_surfaces_have_no_value_like_fields` fixture (anchor: the `SecuritySurface` entry with `name: "ScanLeakDetected"`), add immediately after it:

```rust
            SecuritySurface {
                category: "error variant",
                name: "RenameIncomplete",
                fields: &["source", "destination", "vault", "cause"],
                allowed_value_like_fields: &[],
            },
```

In `src/backend/error.rs`'s test module (anchor: `not_found_converts_to_secret_not_found`), add:

```rust
    #[test]
    fn rename_incomplete_maps_preserving_names_and_cause() {
        let be = BackendError::RenameIncomplete {
            source: "old".into(),
            destination: "new".into(),
            vault: "v".into(),
            cause: Box::new(BackendError::Network("dial timeout".into())),
        };
        let ce: CrosstacheError = be.into();
        match ce {
            CrosstacheError::RenameIncomplete {
                source,
                destination,
                vault,
                cause,
            } => {
                assert_eq!(source, "old");
                assert_eq!(destination, "new");
                assert_eq!(vault, "v");
                assert!(
                    matches!(*cause, CrosstacheError::NetworkError(_)),
                    "cause must convert recursively: {cause:?}"
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib error`
Expected: compile error — `RenameIncomplete` is not a variant of either enum.

- [ ] **Step 3: Add the variants and mapping**

`src/error.rs`, in the `CrosstacheError` enum, immediately after the `ScanLeakDetected` variant (anchor: `ScanLeakDetected { count: usize },`) — this is the exact v0.16.0 shape and display text:

```rust
    #[error("Rename of secret '{source}' to '{destination}' in vault '{vault}' is incomplete: the new secret was created, but deleting the original failed: {cause}. Both secrets still exist and no secret material was lost. Next steps: with vault '{vault}' active, verify the new secret (`xv get {destination}`), then delete the original (`xv delete {source}`) or retry the deletion later.")]
    RenameIncomplete {
        source: String,
        destination: String,
        vault: String,
        #[source]
        cause: Box<CrosstacheError>,
    },
```

In `code()` (anchor: `Self::ScanLeakDetected { .. } => "xv-scan-leak-detected",`), add after it:

```rust
            Self::RenameIncomplete { .. } => "xv-rename-incomplete",
```

In `exit_code()` (anchor: `Self::RateLimited(_) => 42,`), add after it:

```rust
            Self::RenameIncomplete { .. } => 43,
```

`src/backend/error.rs`, in the `BackendError` enum, immediately before the `Internal` variant (anchor: `/// An internal error inside the backend implementation.`):

```rust
    /// A rename created the new secret but failed to delete the original.
    /// Both copies exist; `cause` is the delete failure.
    #[error("rename of '{source}' to '{destination}' in vault '{vault}' is incomplete: deleting the original failed: {cause}")]
    RenameIncomplete {
        source: String,
        destination: String,
        vault: String,
        cause: Box<BackendError>,
    },
```

In `impl From<BackendError> for CrosstacheError` (anchor: `BackendError::Network(msg) => CrosstacheError::NetworkError(msg),`), add before the `Internal` arm:

```rust
            BackendError::RenameIncomplete {
                source,
                destination,
                vault,
                cause,
            } => CrosstacheError::RenameIncomplete {
                source,
                destination,
                vault,
                cause: Box::new((*cause).into()),
            },
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib error && cargo clippy --all-targets`
Expected: PASS, 0 warnings.

- [ ] **Step 5: Restore the exit-codes doc row**

In `docs/exit-codes.md`, in the table, insert between the `40` row and the `50` row (this is the v0.16.0 row verbatim):

```markdown
| `43`  | Rename incomplete     | rename created the new secret but failed to delete the original; both copies still exist (`xv-rename-incomplete`) |
```

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/error.rs src/backend/error.rs docs/exit-codes.md
git commit -m "feat: restore RenameIncomplete error with exit code 43 across the backend boundary

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Provided trait method `SecretBackend::rename_secret`

**Files:**
- Modify: `src/backend/secret.rs` (default method, `rename_request_from_properties` helper, new test module with a stub backend)

**Interfaces:**
- Consumes: `BackendError::RenameIncomplete` (Task 1).
- Produces (Tasks 3-6 rely on these exact signatures):
  - `async fn rename_secret(&self, vault: &str, name: &str, new_name: &str) -> Result<SecretProperties, BackendError>` — provided method on `SecretBackend`, returns the properties of the newly created secret.
  - `pub(crate) fn rename_request_from_properties(new_name: &str, current: &SecretProperties) -> Result<SecretRequest, BackendError>` in `src/backend/secret.rs`.

- [ ] **Step 1: Write the failing tests**

Append to `src/backend/secret.rs` (the file currently has no test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::manager::SecretRequest;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use zeroize::Zeroizing;

    /// In-memory SecretBackend: enough behavior to exercise the provided
    /// `rename_secret` (set/get/delete/exists); everything else Unsupported.
    struct StubBackend {
        secrets: Mutex<HashMap<String, SecretRequest>>,
        fail_delete: bool,
    }

    impl StubBackend {
        fn new(fail_delete: bool) -> Self {
            Self {
                secrets: Mutex::new(HashMap::new()),
                fail_delete,
            }
        }
    }

    /// Mirror how real backends surface metadata: groups/note/folder appear
    /// under canonical tag keys in `SecretProperties.tags`.
    fn props_from_request(req: &SecretRequest, include_value: bool) -> SecretProperties {
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
            _group_filter: Option<&str>,
        ) -> Result<Vec<SecretSummary>, BackendError> {
            Ok(vec![])
        }

        async fn delete_secret(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
            if self.fail_delete {
                return Err(BackendError::Network("simulated outage".into()));
            }
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
            _name: &str,
            _request: SecretUpdateRequest,
        ) -> Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("update".into()))
        }
    }

    fn seeded_request(name: &str) -> SecretRequest {
        let mut tags = HashMap::new();
        tags.insert("custom".to_string(), "kept".to_string());
        SecretRequest {
            name: name.to_string(),
            value: Zeroizing::new("the-value".to_string()),
            content_type: Some("text/plain".to_string()),
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: Some(tags),
            groups: Some(vec!["team-a".to_string(), "team-b".to_string()]),
            note: Some("ride along".to_string()),
            folder: Some("proj/db".to_string()),
        }
    }

    #[tokio::test]
    async fn rename_moves_value_and_metadata() {
        let backend = StubBackend::new(false);
        backend
            .set_secret("v", seeded_request("old-name"))
            .await
            .unwrap();

        let created = backend.rename_secret("v", "old-name", "new-name").await.unwrap();
        assert_eq!(created.name, "new-name");

        let got = backend.get_secret("v", "new-name", true).await.unwrap();
        assert_eq!(got.value.as_ref().map(|v| v.as_str()), Some("the-value"));
        assert_eq!(got.tags.get("groups").map(String::as_str), Some("team-a,team-b"));
        assert_eq!(got.tags.get("note").map(String::as_str), Some("ride along"));
        assert_eq!(got.tags.get("folder").map(String::as_str), Some("proj/db"));
        assert_eq!(got.tags.get("custom").map(String::as_str), Some("kept"));
        // original_name is regenerated for the new name, not copied.
        assert_eq!(got.tags.get("original_name").map(String::as_str), Some("new-name"));
        assert_eq!(got.content_type, "text/plain");

        assert!(matches!(
            backend.get_secret("v", "old-name", false).await,
            Err(BackendError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn rename_to_existing_name_is_a_conflict_and_mutates_nothing() {
        let backend = StubBackend::new(false);
        backend.set_secret("v", seeded_request("a")).await.unwrap();
        backend.set_secret("v", seeded_request("b")).await.unwrap();

        let err = backend.rename_secret("v", "a", "b").await.unwrap_err();
        assert!(matches!(err, BackendError::Conflict(_)), "{err:?}");
        // Both still present and untouched.
        assert!(backend.get_secret("v", "a", true).await.is_ok());
        assert!(backend.get_secret("v", "b", true).await.is_ok());
    }

    #[tokio::test]
    async fn rename_to_same_name_is_invalid_argument() {
        let backend = StubBackend::new(false);
        backend.set_secret("v", seeded_request("a")).await.unwrap();
        let err = backend.rename_secret("v", "a", "a").await.unwrap_err();
        assert!(matches!(err, BackendError::InvalidArgument(_)), "{err:?}");
    }

    #[tokio::test]
    async fn rename_of_missing_secret_is_not_found() {
        let backend = StubBackend::new(false);
        let err = backend.rename_secret("v", "ghost", "new").await.unwrap_err();
        assert!(matches!(err, BackendError::NotFound { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn rename_partial_failure_reports_rename_incomplete_with_both_copies() {
        let backend = StubBackend::new(true); // delete always fails
        backend.set_secret("v", seeded_request("old-name")).await.unwrap();

        let err = backend
            .rename_secret("v", "old-name", "new-name")
            .await
            .unwrap_err();
        match err {
            BackendError::RenameIncomplete {
                source,
                destination,
                vault,
                cause,
            } => {
                assert_eq!(source, "old-name");
                assert_eq!(destination, "new-name");
                assert_eq!(vault, "v");
                assert!(matches!(*cause, BackendError::Network(_)), "{cause:?}");
            }
            other => panic!("wrong error: {other:?}"),
        }
        // Both copies survive — the new secret is never rolled back.
        assert!(backend.get_secret("v", "old-name", true).await.is_ok());
        assert!(backend.get_secret("v", "new-name", true).await.is_ok());
    }

    #[test]
    fn rename_request_rebuilds_first_class_fields_from_tags() {
        let mut tags = HashMap::new();
        tags.insert("groups".to_string(), "a, b".to_string());
        tags.insert("note".to_string(), "n".to_string());
        tags.insert("folder".to_string(), "f/g".to_string());
        tags.insert("original_name".to_string(), "old".to_string());
        tags.insert("created_by".to_string(), "crosstache".to_string());
        tags.insert("custom".to_string(), "kept".to_string());
        let props = SecretProperties {
            name: "old".to_string(),
            original_name: "old".to_string(),
            value: Some(Zeroizing::new("v".to_string())),
            version: "v3".to_string(),
            version_number: Some(3),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: false,
            expires_on: None,
            not_before: None,
            tags,
            content_type: "text/plain".to_string(),
            recovery_level: None,
        };

        let req = rename_request_from_properties("new", &props).unwrap();
        assert_eq!(req.name, "new");
        assert_eq!(req.value.as_str(), "v");
        assert_eq!(req.groups, Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(req.note.as_deref(), Some("n"));
        assert_eq!(req.folder.as_deref(), Some("f/g"));
        assert_eq!(req.content_type.as_deref(), Some("text/plain"));
        assert_eq!(req.enabled, Some(false));
        let t = req.tags.expect("user tags kept");
        assert_eq!(t.get("custom").map(String::as_str), Some("kept"));
        assert!(!t.contains_key("original_name") && !t.contains_key("created_by"));
        assert!(!t.contains_key("groups") && !t.contains_key("note") && !t.contains_key("folder"));
    }

    #[test]
    fn rename_request_aborts_without_a_value() {
        let props = SecretProperties {
            name: "old".to_string(),
            original_name: "old".to_string(),
            value: None,
            version: "v1".to_string(),
            version_number: None,
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::new(),
            content_type: String::new(),
            recovery_level: None,
        };
        let err = rename_request_from_properties("new", &props).unwrap_err();
        assert!(matches!(err, BackendError::Internal(_)), "{err:?}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backend::secret`
Expected: compile error — `rename_secret` and `rename_request_from_properties` not found.

- [ ] **Step 3: Implement the provided method and helper**

In `src/backend/secret.rs`, inside `trait SecretBackend`, immediately after the required `update_secret` method (before the `// Optional operations` banner), add:

```rust
    /// Rename a secret: read value + metadata, create it under `new_name`
    /// (tags, groups, note, folder, content type, expiry ride along), then
    /// delete the old name via the backend's normal delete (soft delete /
    /// recovery window / trash). Version history does not carry over.
    ///
    /// If the new secret is created but deleting the original fails, this
    /// returns [`BackendError::RenameIncomplete`] and deliberately does NOT
    /// roll back the new secret — no secret material may be lost.
    async fn rename_secret(
        &self,
        vault: &str,
        name: &str,
        new_name: &str,
    ) -> Result<SecretProperties, BackendError> {
        if new_name == name {
            return Err(BackendError::InvalidArgument(format!(
                "secret is already named '{name}'"
            )));
        }
        if self.secret_exists(vault, new_name).await? {
            return Err(BackendError::Conflict(format!(
                "secret '{new_name}' already exists in vault '{vault}' — delete it first or pick another name"
            )));
        }

        let current = self.get_secret(vault, name, true).await?;
        let request = rename_request_from_properties(new_name, &current)?;
        let created = self.set_secret(vault, request).await?;

        if let Err(cause) = self.delete_secret(vault, name).await {
            return Err(BackendError::RenameIncomplete {
                source: name.to_string(),
                destination: new_name.to_string(),
                vault: vault.to_string(),
                cause: Box::new(cause),
            });
        }
        Ok(created)
    }
```

Below the trait (module level), add the helper and extend the manager import at the top of the file to include `SecretRequest` (it is already imported — the current `use crate::secret::manager::{...}` line lists it; verify and leave as-is):

```rust
/// Build the create-under-the-new-name request for a rename from the source
/// secret's properties. Groups/note/folder live under canonical tag keys in
/// `SecretProperties.tags` on every backend; lift them into the first-class
/// `SecretRequest` fields so each backend re-encodes them natively, and strip
/// the bookkeeping tags (`original_name`, `created_by`) that `set_secret`
/// regenerates for the new name.
pub(crate) fn rename_request_from_properties(
    new_name: &str,
    current: &SecretProperties,
) -> Result<SecretRequest, BackendError> {
    let value = current.value.clone().ok_or_else(|| {
        BackendError::Internal(format!(
            "backend returned no value for '{}'; rename aborted before creating anything",
            current.name
        ))
    })?;

    let mut tags = current.tags.clone();
    let groups = tags
        .remove("groups")
        .map(|g| {
            g.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|g| !g.is_empty());
    let note = tags.remove("note");
    let folder = tags.remove("folder");
    tags.remove("original_name");
    tags.remove("created_by");

    Ok(SecretRequest {
        name: new_name.to_string(),
        value,
        content_type: (!current.content_type.is_empty()).then(|| current.content_type.clone()),
        enabled: Some(current.enabled),
        expires_on: current.expires_on,
        not_before: current.not_before,
        tags: if tags.is_empty() { None } else { Some(tags) },
        groups,
        note,
        folder,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib backend::secret && cargo clippy --all-targets`
Expected: all 7 new tests PASS, 0 warnings.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/backend/secret.rs
git commit -m "feat: provided SecretBackend::rename_secret (create-new, delete-old, RenameIncomplete on partial failure)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Local backend proof — plaintext AND opaque stores

**Files:**
- Modify: `src/backend/local/secrets.rs` (test module only — the default method needs no local-specific code; these tests prove it against real files, trash, versions, and the opaque index)

**Interfaces:**
- Consumes: `rename_secret` (Task 2); existing test helpers `test_backend()`, `test_backend_opaque()`, `make_request(name, value)` in the local test module.
- Produces: nothing new (tests only).

- [ ] **Step 1: Write the failing tests**

In `src/backend/local/secrets.rs`'s `#[cfg(test)] mod tests` (anchor: near the existing `delete_secret` test), add:

```rust
    /// Shared rename assertions, run against both store layouts.
    async fn assert_rename_roundtrip(backend: &LocalSecretBackend) {
        let mut req = make_request("old-name", "v1");
        req.groups = Some(vec!["team".to_string()]);
        req.note = Some("keep".to_string());
        req.folder = Some("proj".to_string());
        backend.set_secret("default", req).await.unwrap();

        let created = backend
            .rename_secret("default", "old-name", "new-name")
            .await
            .unwrap();
        assert_eq!(created.name, "new-name");

        let got = backend.get_secret("default", "new-name", true).await.unwrap();
        assert_eq!(got.value.as_ref().map(|v| v.as_str()), Some("v1"));
        assert_eq!(got.tags.get("groups").map(String::as_str), Some("team"));
        assert_eq!(got.tags.get("note").map(String::as_str), Some("keep"));
        assert_eq!(got.tags.get("folder").map(String::as_str), Some("proj"));
        assert_eq!(got.original_name, "new-name");

        // Old name is out of the active set and waiting in trash.
        assert!(matches!(
            backend.get_secret("default", "old-name", false).await,
            Err(BackendError::NotFound { .. })
        ));
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert!(
            deleted.iter().any(|d| d.name == "old-name"),
            "old name must land in trash: {deleted:?}"
        );

        // Version history does not carry over — the new name starts fresh.
        let versions = backend.list_versions("default", "new-name").await.unwrap();
        assert_eq!(versions.len(), 1, "{versions:?}");
    }

    #[tokio::test]
    async fn rename_moves_secret_in_plaintext_store() {
        let (backend, _tmp) = test_backend();
        assert_rename_roundtrip(&backend).await;
    }

    #[tokio::test]
    async fn rename_moves_secret_in_opaque_store_without_leaking_names() {
        let (backend, tmp) = test_backend_opaque();
        assert_rename_roundtrip(&backend).await;

        // Opaque property holds after a rename: no on-disk filename under the
        // secrets dir contains either the old or the new secret name.
        let sdir = tmp.path().join("vaults").join("default").join("secrets");
        for entry in fs::read_dir(&sdir).unwrap().flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            assert!(
                !fname.contains("new-name") && !fname.contains("old-name"),
                "leaky stem after rename: {fname}"
            );
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail... or pass**

Run: `cargo test --lib backend::local::secrets::tests::rename_moves`
Expected: **PASS on the first run** — that is the point of this task: the Task 2 default method must already work over the local primitives in both layouts. If either test FAILS, the default method or a local primitive has a real bug: debug it (do not weaken the test). Likely suspects: `resolve_active_stem` for the new name, or trash collision handling in `delete_secret`.

- [ ] **Step 3: Verify the wider local suite still passes**

Run: `cargo test --lib backend::local && cargo clippy --all-targets`
Expected: PASS, 0 warnings.

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add src/backend/local/secrets.rs
git commit -m "test: prove trait-level rename against the local backend in plaintext and opaque stores

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: CLI wiring + delete `SecretUpdateRequest.new_name` + Azure cleanup

**Files:**
- Modify: `src/secret/manager.rs` (delete the `new_name` field, ~line 134)
- Modify: `src/cli/secret_ops.rs` (`execute_secret_update_direct`, ~lines 1531-1557)
- Modify: `src/backend/azure/secrets.rs` (`update_secret` ~lines 214-224 and 332; test fixture ~line 446)
- Modify: `src/backend/local/secrets.rs` (test fixtures ~lines 2518, 2554, 2585)
- Modify: `tests/e2e_azure_backend.rs` (~line 212), `tests/aws_backend_tests.rs` (~line 481), `tests/e2e_aws_backend.rs` (~line 306)
- Test: `tests/e2e_local_backend.rs`

**Interfaces:**
- Consumes: `rename_secret` (Task 2); `RenameIncomplete` conversion (Task 1).
- Produces: `SecretUpdateRequest` WITHOUT `new_name` (all later code must not reference it); CLI semantics: in-place update first (when other flags present), then rename; success line `Successfully renamed secret '<old>' to '<new>'`.

- [ ] **Step 1: Write the failing e2e tests**

In `tests/e2e_local_backend.rs`, add:

```rust
#[test]
fn update_rename_moves_secret_and_metadata() {
    let env = TestEnv::new();
    env.set_secret_with_args(
        "old-name",
        "rename-me",
        &["--note", "keep this note", "--group", "team-a"],
    );

    // NOTE: output::success prints to STDERR (src/utils/output.rs), so run
    // the raw command to assert the rename success line.
    let output = env
        .xv()
        .args(["update", "old-name", "--rename", "new-name"])
        .output()
        .expect("run xv update --rename");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "update --rename failed:\n{stderr}");
    assert!(
        stderr.contains("renamed") && stderr.contains("new-name"),
        "expected a rename success line on stderr:\n{stderr}"
    );

    // Value moved.
    assert_eq!(env.get_raw("new-name"), "rename-me");

    // Metadata rode along, and the old name is out of the listing.
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(
        json.contains("new-name") && json.contains("keep this note") && json.contains("team-a"),
        "metadata missing after rename:\n{json}"
    );
    assert!(!json.contains("old-name"), "old name still listed:\n{json}");

    // The old name no longer resolves.
    let (_, stderr) = env.xv_fail(&["get", "old-name"]);
    assert!(stderr.to_lowercase().contains("not found"), "{stderr}");
}

#[test]
fn update_rename_applies_other_flags_first() {
    let env = TestEnv::new();
    env.set_secret_with_args("combo", "v1", &["--note", "old note"]);

    // Success lines go to stderr; the behavioral assertions below are the
    // real check, so xv_ok (which only asserts exit status) is enough here.
    env.xv_ok(&["update", "combo", "--note", "new note", "--rename", "combo-renamed"]);

    assert_eq!(env.get_raw("combo-renamed"), "v1");
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("combo-renamed") && json.contains("new note"), "{json}");
    assert!(!json.contains("old note"), "stale note survived the update:\n{json}");
}

#[test]
fn update_rename_refuses_to_overwrite_an_existing_secret() {
    let env = TestEnv::new();
    env.set_secret("keep-me", "original");
    env.set_secret("mover", "moving");

    let (_, stderr) = env.xv_fail(&["update", "mover", "--rename", "keep-me"]);
    assert!(stderr.contains("already exists"), "{stderr}");

    // Nothing was clobbered or deleted.
    assert_eq!(env.get_raw("keep-me"), "original");
    assert_eq!(env.get_raw("mover"), "moving");
}

#[test]
fn update_rename_to_the_same_name_is_an_error() {
    let env = TestEnv::new();
    env.set_secret("same", "v");
    let (_, stderr) = env.xv_fail(&["update", "same", "--rename", "same"]);
    assert!(stderr.contains("already named"), "{stderr}");
}

#[test]
fn update_rename_of_a_missing_secret_fails() {
    let env = TestEnv::new();
    let (_, stderr) = env.xv_fail(&["update", "ghost", "--rename", "anything"]);
    assert!(stderr.to_lowercase().contains("not found"), "{stderr}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test e2e_local_backend update_rename`
Expected: FAIL — `update_rename_moves_secret_and_metadata` finds no rename (old name still listed, `get new-name` fails), the conflict/same-name tests succeed when they should fail, etc. (Today rename is a silent no-op on local.)

- [ ] **Step 3: Delete the field and rewire the CLI**

`src/secret/manager.rs`, in `struct SecretUpdateRequest` (anchor: `pub struct SecretUpdateRequest`), delete the line:

```rust
    pub new_name: Option<String>, // For renaming
```

`src/cli/secret_ops.rs`, in `execute_secret_update_direct` (anchor: `let request = crate::secret::manager::SecretUpdateRequest {`), replace the block from that `let request = ...` through the `return Ok(());` of the trait path (currently the request build, the `update_secret` call, the success print, and the cache invalidation) with:

```rust
        let renaming = rename.is_some();
        let has_other_updates = resolved_value.is_some()
            || merged_tags.is_some()
            || merged_groups.is_some()
            || enabled.is_some()
            || !expires_update.is_unchanged()
            || !not_before_update.is_unchanged()
            || !note_update.is_unchanged()
            || !folder_update.is_unchanged();

        // Apply in-place updates first, under the old name; then rename.
        // `--rename` alone skips the no-op update round-trip, and a bare
        // `xv update NAME` keeps its historical all-unchanged update call.
        if has_other_updates || !renaming {
            let request = crate::secret::manager::SecretUpdateRequest {
                name: name.to_string(),
                value: resolved_value,
                content_type: None,
                enabled,
                expires_on: expires_update,
                not_before: not_before_update,
                tags: merged_tags,
                groups: merged_groups,
                note: note_update,
                folder: folder_update,
                replace_tags,
                replace_groups,
            };
            let props = reg
                .active()
                .secrets()
                .update_secret(&vault_name, name, request)
                .await?;
            output::success(&format!(
                "Successfully updated secret '{}'",
                props.original_name
            ));
        }

        if let Some(ref new_name) = rename {
            let props = reg
                .active()
                .secrets()
                .rename_secret(&vault_name, name, new_name)
                .await?;
            output::success(&format!(
                "Successfully renamed secret '{name}' to '{}'",
                props.original_name
            ));
        }

        // Invalidate the secrets list cache for metadata, value, rename, or enablement changes.
        invalidate_trait_secret_cache(&config, &vault_name);
        return Ok(());
```

- [ ] **Step 4: Fix the compile fallout (the compiler is the checklist)**

Run `cargo check --all-targets 2>&1 | grep -A2 new_name` and fix every site — the full expected list:

- `src/backend/azure/secrets.rs`, `update_secret`:
  - `let attributes_only = request.value.is_none() && request.new_name.is_none();` → `let attributes_only = request.value.is_none();`
  - In the comment above it, change "Attributes/tags-only updates (no value change, no rename) go" → "Attributes/tags-only updates (no value change) go".
  - `name: request.new_name.unwrap_or_else(|| request.name.clone()),` → `name: request.name.clone(),`
- `src/backend/azure/secrets.rs` test fixture `base_request` (anchor: `fn base_request(name: &str) -> SecretUpdateRequest`): delete the `new_name: None,` line.
- `src/backend/local/secrets.rs`: delete the three `new_name: None,` lines in the `update_secret_metadata`, `update_secret_with_new_value_creates_version` (×2 constructors) tests.
- `tests/e2e_azure_backend.rs` (~line 212), `tests/aws_backend_tests.rs` (~line 481), `tests/e2e_aws_backend.rs` (~line 306): delete each `new_name: None,` line.

Then verify no reference survives: `rg -n "new_name" src/secret/manager.rs src/backend/ src/cli/secret_ops.rs` — the only remaining `new_name` hits in the repo are the `rename_secret` trait method parameters (Task 2) and the unrelated `xv copy`/`xv move`/file-ops target-name parameters.

- [ ] **Step 5: Run tests to verify they pass**

Run:
```bash
cargo test --test e2e_local_backend update_rename
cargo test --lib
cargo clippy --all-targets && cargo check --features aws
```
Expected: all PASS, 0 warnings. Also re-run the shipped-shape guard: `cargo test --test e2e_local_backend` in full — no update-related test may regress (bare `xv update NAME --note x` output is unchanged).

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/secret/manager.rs src/cli/secret_ops.rs src/backend/azure/secrets.rs src/backend/local/secrets.rs tests/e2e_azure_backend.rs tests/aws_backend_tests.rs tests/e2e_aws_backend.rs tests/e2e_local_backend.rs
git commit -m "fix: route xv update --rename through rename_secret; drop the ignorable new_name field (#295)

Other update flags apply first (in place, under the old name), then the
rename runs as create-new/delete-old. Deleting SecretUpdateRequest.new_name
makes a silently-ignored rename impossible by construction.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: AWS coverage — LocalStack rename roundtrip

**Files:**
- Modify: `tests/aws_localstack_tests.rs`

**Interfaces:**
- Consumes: `rename_secret` (Task 2). AWS needs no production code: its `update_secret` never read `new_name`, and the default method composes its existing `get_secret`/`set_secret`/`delete_secret` (scheduled deletion, 30-day window — the same call `xv delete` uses).
- Produces: nothing new (tests only).

- [ ] **Step 1: Add the gated test**

In `tests/aws_localstack_tests.rs` (anchor: after `localstack_set_get_round_trip`):

```rust
#[tokio::test]
async fn localstack_rename_round_trip() {
    if skip_unless_enabled() {
        return;
    }
    let backend = build_backend().await;
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    let request = SecretRequest {
        name: "rename-src".into(),
        value: Zeroizing::new("rename-value".into()),
        groups: Some(vec!["team".into()]),
        note: Some("ride along".into()),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        folder: None,
    };
    backend.secrets().set_secret(&vault, request).await.unwrap();

    let created = backend
        .secrets()
        .rename_secret(&vault, "rename-src", "rename-dst")
        .await
        .unwrap();
    assert_eq!(created.name, "rename-dst");

    let got = backend
        .secrets()
        .get_secret(&vault, "rename-dst", true)
        .await
        .unwrap();
    assert_eq!(
        got.value.as_ref().map(|v| v.as_str().to_string()),
        Some("rename-value".to_string())
    );
    // props_from_describe re-exposes xv: tags under the canonical keys.
    assert_eq!(got.tags.get("groups").map(String::as_str), Some("team"));
    assert_eq!(got.tags.get("note").map(String::as_str), Some("ride along"));

    // The old name is scheduled for deletion (30-day recovery window — the
    // same delete `xv delete` performs), so it drops out of ListSecrets.
    // NOTE: don't assert via secret_exists — DescribeSecret still returns
    // scheduled-deletion entries, which is what makes the rename-back-within-
    // the-window case a Conflict by design.
    let listed = backend.secrets().list_secrets(&vault, None).await.unwrap();
    assert!(
        !listed.iter().any(|s| s.name == "rename-src"),
        "old name still listed: {listed:?}"
    );
    assert!(listed.iter().any(|s| s.name == "rename-dst"));

    // Cleanup: force-purge both names so reruns never hit the recovery window.
    let _ = backend.secrets().purge_secret(&vault, "rename-dst").await;
    let _ = backend.secrets().purge_secret(&vault, "rename-src").await;
}
```

- [ ] **Step 2: Compile-verify (LocalStack optional)**

Run: `cargo test --features aws --test aws_localstack_tests -- --nocapture`
Expected: compiles; the test PASSES against a running LocalStack (`AWS_INTEGRATION_TESTS=1`, `AWS_ENDPOINT_URL=http://localhost:4566`, `AWS_ACCESS_KEY_ID=test`, `AWS_SECRET_ACCESS_KEY=test`, `AWS_REGION=us-east-1`) or **skips silently** without one — either outcome is green. Also run `cargo clippy --all-targets --features aws` — 0 warnings.

If LocalStack is available and the `groups`/`note` assertions fail because LocalStack's DescribeSecret tag emulation differs from real AWS, keep the value/listing assertions and relax only the tag assertions to `got.tags.get("groups").is_some()` — note it in the commit message. Do not touch production code for a LocalStack emulation gap.

- [ ] **Step 3: Commit**

```bash
cargo fmt
git add tests/aws_localstack_tests.rs
git commit -m "test: LocalStack rename roundtrip for the AWS backend

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Docs, live Azure roundtrip, and final gates

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `tests/e2e_azure_backend.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: the release-notes record; live-Azure proof that the trait rename really soft-deletes the old name.

- [ ] **Step 1: Add the live Azure e2e (ignored, self-cleaning)**

In `tests/e2e_azure_backend.rs` (anchor: after `e2e_azure_secret_full_lifecycle`):

```rust
#[tokio::test]
#[ignore]
async fn e2e_azure_rename_roundtrip() {
    let backend = azure_backend().await;
    let vault = test_vault();
    let source = unique_name("rn-src");
    let dest = unique_name("rn-dst");

    let mut req = make_request(&source, "rename-me");
    req.groups = Some(vec!["e2e".to_string()]);
    req.note = Some("rename e2e".to_string());
    backend
        .secrets()
        .set_secret(&vault, req)
        .await
        .expect("create rename source");

    let created = backend
        .secrets()
        .rename_secret(&vault, &source, &dest)
        .await
        .expect("rename_secret should succeed");
    assert_eq!(created.name, dest);

    let got = backend
        .secrets()
        .get_secret(&vault, &dest, true)
        .await
        .expect("get renamed secret");
    assert_eq!(got.value.as_ref().map(|v| v.as_str()), Some("rename-me"));
    assert_eq!(got.tags.get("note").map(String::as_str), Some("rename e2e"));
    assert_eq!(got.tags.get("groups").map(String::as_str), Some("e2e"));

    // The old name must be soft-deleted (GET returns 404 once the delete
    // lands; Key Vault applies it promptly after DELETE returns).
    let exists = backend
        .secrets()
        .secret_exists(&vault, &source)
        .await
        .expect("exists check on the old name");
    assert!(!exists, "source '{source}' should be soft-deleted after rename");

    // Cleanup: soft-delete the destination. The vault has purge protection,
    // so purging is blocked by policy — soft delete + unique names is the
    // documented harness contract. `source` is already soft-deleted.
    cleanup(&backend, &vault, &[&dest]).await;
}
```

- [ ] **Step 2: Run the live test**

Run: `cargo test --test e2e_azure_backend e2e_azure_rename_roundtrip -- --ignored --nocapture --test-threads=1`
Expected: PASS (requires `az` login and reachability of the `heythere` vault; override with `XV_E2E_AZURE_VAULT`). If the environment has no Azure credentials, record that the live check was skipped and why — do not fake it.

- [ ] **Step 3: CHANGELOG entry**

At the top of `CHANGELOG.md`, above the `## v0.17.0` heading, add:

```markdown
## Unreleased

### Fixed

- **`xv update --rename` works again on every backend (#295).** Rename is now a real trait-level operation (`SecretBackend::rename_secret`): read value + metadata, create under the new name (user tags, groups, note, folder, content type, and expiry ride along), then delete the old name with the backend's normal delete. Previously Azure created the duplicate and never deleted the original, while local and AWS silently ignored the flag; the `SecretUpdateRequest.new_name` field is gone so a backend can never ignore a rename again. Combined with other update flags, the in-place updates apply first, then the rename. Renaming onto an existing name is refused (`xv-conflict`); version history does not carry over. On Azure the old name is left soft-deleted (visible in `xv ls --deleted`; renaming back within the retention window conflicts); on AWS it sits in the standard 30-day recovery window; on local it lands in trash.
- **`RenameIncomplete` is restored** (removed in the v0.17.0 legacy cleanup while unreachable): if the new secret is created but deleting the original fails, `xv update --rename` exits 43 with code `xv-rename-incomplete`, names both copies and the vault, and prints the recovery steps (`xv get <new>`, then `xv delete <old>` or retry). The new secret is deliberately never rolled back. The 43 row is back in `docs/exit-codes.md`.
```

- [ ] **Step 4: Full gates**

```bash
cargo fmt
cargo clippy --all-targets
cargo clippy --all-targets --features aws
cargo test --lib
cargo test --test e2e_local_backend
cargo test
```
Expected: all green, 0 clippy warnings. (`cargo test` runs the non-ignored integration suites; the Azure live test stays ignored.)

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md tests/e2e_azure_backend.rs
git commit -m "docs: changelog + live Azure rename roundtrip e2e (closes #295)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Spec Coverage Map

| Spec section / requirement | Task |
|---|---|
| Decision 1 — trait-level rename on all three backends, metadata preserved, no version carry-over | 2 (method + helper), 3 (local proof), 5 (AWS), 6 (Azure live) |
| Decision 2 — dedicated error variant + exit 43 restored to docs/exit-codes.md, text names both copies + recovery | 1 (variants, code, exit, doc row), 2 (partial-failure path + test) |
| Decision 3 — Azure old name left soft-deleted, no purge, retention conflict documented | 2 (delete via `delete_secret`), 6 (live assertion + harness cleanup), 1+6 (docs) |
| Decision 4 — provided trait method, no per-backend overrides | 2 |
| Decision 5 — `new_name` field deleted, compiler-enforced | 4 |
| Decision 6 — combination semantics: update-in-place first, then rename | 4 (CLI wiring + `update_rename_applies_other_flags_first`) |
| Decision 7 — destination-exists Conflict + same-name InvalidArgument guards | 2 (unit), 4 (e2e) |
| Decision 8 — AWS scheduled deletion (30-day window) as the delete step, rename-back conflict documented | 5 (test + note), 6 (CHANGELOG) |
| Decision 9 — tags stripped/regenerated (`original_name`, `created_by`) | 2 (`rename_request_rebuilds_first_class_fields_from_tags`) |
| Decision 10 — disabled-secret limitation (Azure 403 before mutation) | inherent to Design (get-with-value first); recorded in spec + CHANGELOG wording, no code needed |
| Design: error plumbing mirrors v0.16.0 (display text, security surface, mapping) | 1 |
| Design: CLI success line + unchanged bare-update output + cache invalidation | 4 |
| Design: local opaque store handled with zero rename-specific code | 3 |
| Testing: stub-backend unit, local both stores, hermetic e2e, LocalStack, live Azure, gates | 2, 3, 4, 5, 6 |
| Out of scope: `xv copy`/`xv move` untouched, no rollback/purge, no capability flag | all tasks (nothing touches those paths) |

## Self-Review Notes

- **Spec coverage:** every Decision (1-10) and Design bullet maps to a task above; verified no gap.
- **Placeholders:** none — every code step carries complete code; the only conditional instruction (LocalStack tag-emulation relaxation in Task 5) specifies the exact fallback assertion.
- **Type consistency:** `rename_secret(&self, vault: &str, name: &str, new_name: &str) -> Result<SecretProperties, BackendError>` is used identically in Tasks 2, 3, 4, 5, 6; `RenameIncomplete` field names (`source`, `destination`, `vault`, `cause`) match across `src/error.rs`, `src/backend/error.rs`, and all tests; `rename_request_from_properties(new_name: &str, current: &SecretProperties)` matches its Task 2 definition everywhere it is referenced.
