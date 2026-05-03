# Backend Pluggability — Phase 2 (v0.8 → v0.9) Design

**Date:** 2026-05-03
**Status:** Draft — pending review
**Owner:** Scott Zionic
**Inputs:** `docs/superpowers/specs/2026-04-29-strategic-improvements-phase-1-design.md` (Phase 1 spec); `docs/superpowers/specs/backend-trait-checklist.md` (read-surface audit); `dev/ROADMAP.md`; current code under `src/`.

---

## 1. Strategic context

Phase 1 shipped the "loved features" push (v0.6→v0.7): structured errors, env profiles, fuzzy search, leak scanner, and TUI. The strategic positioning doc identified two gaps — loved-features parity (now closed) and **backend pluggability** (the largest remaining strategic prize).

No general-purpose CLI secrets manager is truly backend-agnostic. `aws-vault` is AWS-only. HashiCorp Vault's CLI is server-only. `sops` encrypts files but doesn't manage secrets lifecycle. The cross-backend gap remains wide open.

Phase 2 closes it in two moves:
1. **Extract a backend trait boundary** — refactor the Azure implementation behind generic traits so the codebase is backend-agnostic.
2. **Ship a local age-encrypted file backend** — validates the trait design, enables offline use, and gives users a zero-infrastructure secrets manager.

### 1.1 Why local-age-file first

| Criterion | Local age file | AWS SM | HashiCorp Vault |
|-----------|---------------|--------|-----------------|
| Time to build | ~1 week | ~2 weeks | ~2 weeks |
| Validates trait minimality | ✅ maximally different from cloud API | ❌ similar shape to Azure | ❌ similar shape to Azure |
| User value | Offline-first, no cloud account needed | Broader market | Enterprise only |
| Dependency weight | `age` crate (~200KB) | AWS SDK (~5MB) | HTTP client only |
| Tests without credentials | ✅ fully hermetic | ❌ needs moto/mock | ❌ needs mock server |

The local backend is the strongest trait validator — if the trait works for both a cloud REST API and a local encrypted file, it will work for anything. Cloud backends follow as Phase 2b/2c.

---

## 2. Phase 2 scope

### 2.1 Deliverables (in sequence order)

1. **Backend trait extraction** — define `Backend`, `SecretBackend`, `VaultBackend`, `FileBackend` traits; move Azure behind them.
2. **Backend registry + config** — runtime backend selection via config/CLI flag; backend-specific config sections.
3. **Local age-encrypted file backend** — full secrets CRUD against a local directory of age-encrypted files.
4. **Backend capability negotiation** — graceful degradation when a backend doesn't support a feature (e.g., local backend has no RBAC).
5. **Cross-backend `xv migrate`** — copy secrets between backends (Azure→local, local→Azure).
6. **Hermetic tests for local backend** — full E2E test coverage with no external dependencies.

### 2.2 Deliberately deferred

- AWS Secrets Manager backend (Phase 2b).
- HashiCorp Vault backend (Phase 2c).
- TUI edit mode (v0.9 or later — orthogonal).
- Backend-specific TUI themes.
- Multi-backend composite views ("see all secrets across all backends").
- Plugin/extension system for third-party backends.
- Remote age backend (age-encrypted files on S3/GCS).

---

## 3. Architecture

### 3.1 Trait hierarchy

The trait design follows a capability-based approach. Not every backend supports every feature — the traits encode what's required vs. optional.

```rust
// src/backend/mod.rs

/// Core trait every backend must implement.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Human-readable name: "azure", "local", "aws", etc.
    fn name(&self) -> &'static str;

    /// What this backend can do.
    fn capabilities(&self) -> BackendCapabilities;

    /// Secret operations (required — every backend manages secrets).
    fn secrets(&self) -> &dyn SecretBackend;

    /// Vault/namespace operations (optional — not all backends have vaults).
    fn vaults(&self) -> Option<&dyn VaultBackend> { None }

    /// File/blob operations (optional — not all backends store files).
    fn files(&self) -> Option<&dyn FileBackend> { None }

    /// Validate config and connectivity. Called once at startup.
    async fn health_check(&self) -> Result<(), BackendError>;
}

/// Capabilities bitfield — drives graceful degradation in CLI/TUI.
#[derive(Debug, Clone)]
pub struct BackendCapabilities {
    pub has_vaults: bool,           // multi-vault namespace support
    pub has_file_storage: bool,     // blob/file operations
    pub has_rbac: bool,             // access control / sharing
    pub has_audit: bool,            // audit log / activity events
    pub has_versioning: bool,       // secret version history
    pub has_soft_delete: bool,      // recoverable deletion
    pub has_secret_rotation: bool,  // scheduled rotation
    pub has_groups: bool,           // secret grouping/tagging
    pub has_folders: bool,          // hierarchical organization
    pub has_notes: bool,            // secret annotations
    pub has_expiry: bool,           // expiration dates
    pub max_secret_size: Option<usize>,  // size limits (Azure: 25KB, local: no limit)
    pub max_name_length: Option<usize>,  // naming constraints
    pub name_charset: NameCharset,  // what characters are valid in secret names
}

pub enum NameCharset {
    AlphanumericHyphen,  // Azure Key Vault
    Unrestricted,        // Local backend (filesystem-safe after encoding)
    Custom(fn(&str) -> bool),
}
```

### 3.2 SecretBackend trait

This is the core trait — every backend must implement it. Derived from the backend-trait-checklist.

```rust
// src/backend/secret.rs

#[async_trait]
pub trait SecretBackend: Send + Sync {
    /// Create or update a secret. Returns the new version.
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError>;

    /// Get a secret by name, optionally including the value.
    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError>;

    /// Get a specific version of a secret.
    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError>;

    /// List all secrets in a vault, optionally filtered by group.
    async fn list_secrets(
        &self,
        vault: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>, BackendError>;

    /// Delete a secret (soft-delete if backend supports it).
    async fn delete_secret(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<(), BackendError>;

    /// Update secret metadata (tags, groups, enabled state, etc.).
    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError>;

    // --- Optional operations (default = Unsupported error) ---

    /// List all versions of a secret.
    async fn list_versions(
        &self,
        _vault: &str,
        _name: &str,
    ) -> Result<Vec<SecretProperties>, BackendError> {
        Err(BackendError::Unsupported("version history"))
    }

    /// Rollback to a previous version.
    async fn rollback(
        &self,
        _vault: &str,
        _name: &str,
        _version: &str,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("rollback"))
    }

    /// Restore a soft-deleted secret.
    async fn restore_secret(
        &self,
        _vault: &str,
        _name: &str,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("restore"))
    }

    /// Permanently purge a deleted secret.
    async fn purge_secret(
        &self,
        _vault: &str,
        _name: &str,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("purge"))
    }

    /// Check if a secret exists.
    async fn secret_exists(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<bool, BackendError> {
        match self.get_secret(vault, name, false).await {
            Ok(_) => Ok(true),
            Err(BackendError::NotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// List deleted secrets (only if soft-delete supported).
    async fn list_deleted_secrets(
        &self,
        _vault: &str,
    ) -> Result<Vec<SecretSummary>, BackendError> {
        Err(BackendError::Unsupported("list deleted secrets"))
    }

    /// Backup a secret to portable bytes.
    async fn backup_secret(
        &self,
        _vault: &str,
        _name: &str,
    ) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unsupported("backup"))
    }

    /// Restore a secret from backup bytes.
    async fn restore_from_backup(
        &self,
        _vault: &str,
        _backup: &[u8],
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("restore from backup"))
    }
}
```

### 3.3 VaultBackend trait

Optional — only backends with multi-vault/namespace support implement this.

```rust
// src/backend/vault.rs

#[async_trait]
pub trait VaultBackend: Send + Sync {
    async fn create_vault(&self, request: VaultCreateRequest) -> Result<VaultProperties, BackendError>;
    async fn get_vault(&self, name: &str) -> Result<VaultProperties, BackendError>;
    async fn list_vaults(&self) -> Result<Vec<VaultSummary>, BackendError>;
    async fn delete_vault(&self, name: &str) -> Result<(), BackendError>;

    // --- Optional ---
    async fn update_vault(
        &self,
        _name: &str,
        _request: VaultUpdateRequest,
    ) -> Result<VaultProperties, BackendError> {
        Err(BackendError::Unsupported("update vault"))
    }

    async fn restore_vault(&self, _name: &str) -> Result<VaultProperties, BackendError> {
        Err(BackendError::Unsupported("restore vault"))
    }

    async fn purge_vault(&self, _name: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("purge vault"))
    }

    // --- RBAC (optional, only if has_rbac) ---
    async fn grant_access(
        &self, _vault: &str, _principal: &str, _level: AccessLevel,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("RBAC"))
    }

    async fn revoke_access(
        &self, _vault: &str, _principal: &str,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("RBAC"))
    }

    async fn list_access(
        &self, _vault: &str,
    ) -> Result<Vec<VaultRole>, BackendError> {
        Err(BackendError::Unsupported("RBAC"))
    }
}
```

### 3.4 FileBackend trait

Optional — only backends with file/blob storage implement this.

```rust
// src/backend/file.rs

#[async_trait]
pub trait FileBackend: Send + Sync {
    async fn upload_file(&self, request: FileUploadRequest, reporter: Option<&dyn ProgressReporter>) -> Result<FileInfo, BackendError>;
    async fn download_file(&self, name: &str, reporter: Option<&dyn ProgressReporter>) -> Result<Vec<u8>, BackendError>;
    async fn list_files(&self, request: FileListRequest) -> Result<Vec<FileInfo>, BackendError>;
    async fn delete_file(&self, name: &str) -> Result<(), BackendError>;
    async fn get_file_info(&self, name: &str) -> Result<FileInfo, BackendError>;
}
```

### 3.5 BackendError

A backend-agnostic error type that maps to CrosstacheError at the boundary.

```rust
// src/backend/error.rs

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("secret not found: {name}")]
    NotFound { name: String, suggestion: Option<String> },

    #[error("vault not found: {name}")]
    VaultNotFound { name: String, suggestion: Option<String> },

    #[error("authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("operation not supported by {backend} backend: {feature}")]
    Unsupported { backend: &'static str, feature: &'static str },

    #[error("conflict: {0}")]
    Conflict(String),  // e.g., secret already exists when create-only

    #[error("rate limited — retry after {retry_after_secs:?}s")]
    RateLimited { retry_after_secs: Option<u64> },

    #[error("network error: {0}")]
    Network(String),

    #[error("backend error: {0}")]
    Internal(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// Blanket conversion to CrosstacheError
impl From<BackendError> for CrosstacheError { ... }
```

### 3.6 Backend-agnostic domain models

The existing domain models (`SecretProperties`, `SecretSummary`, `VaultProperties`, etc.) are already mostly backend-agnostic. A few Azure-specific fields need attention:

| Model | Azure-specific field | Resolution |
|-------|---------------------|------------|
| `SecretProperties` | `recovery_level` | Move to `backend_metadata: HashMap<String, String>` |
| `VaultProperties` | `resource_group`, `subscription_id`, `sku` | Move to `backend_metadata` |
| `VaultProperties` | `access_policies` | Keep on trait — RBAC is a named capability |
| `SecretInfo` | `vault_uri` | Rename to `location` — backend provides its own URI/path |

The `backend_metadata` bag preserves round-trip fidelity for backend-specific fields without polluting the generic model. The TUI and CLI can display metadata under a "Backend Details" section.

---

## 4. Module reorganization

### 4.1 New module layout

```
src/
├── backend/
│   ├── mod.rs           -- Backend trait, BackendCapabilities, BackendKind enum
│   ├── error.rs         -- BackendError enum
│   ├── secret.rs        -- SecretBackend trait
│   ├── vault.rs         -- VaultBackend trait
│   ├── file.rs          -- FileBackend trait (feature-gated: file-ops)
│   ├── registry.rs      -- BackendRegistry: name → Box<dyn Backend> dispatch
│   ├── azure/
│   │   ├── mod.rs       -- AzureBackend impl Backend
│   │   ├── auth.rs      -- ← moved from src/auth/provider.rs
│   │   ├── secrets.rs   -- ← extracted from src/secret/manager.rs (AzureSecretOperations)
│   │   ├── vaults.rs    -- ← extracted from src/vault/operations.rs (AzureVaultOperations)
│   │   ├── files.rs     -- ← extracted from src/blob/manager.rs (feature-gated)
│   │   ├── models.rs    -- Azure-specific response parsing, API types
│   │   └── config.rs    -- Azure-specific config (subscription, tenant, RG, etc.)
│   └── local/
│       ├── mod.rs       -- LocalBackend impl Backend
│       ├── secrets.rs   -- Age-encrypted file secret operations
│       ├── vaults.rs    -- Directory-based vault namespaces
│       ├── crypto.rs    -- Age encryption/decryption, key management
│       ├── index.rs     -- Metadata index (secret names, groups, tags — without decrypting values)
│       └── config.rs    -- Local backend config (store path, key file, etc.)
├── secret/
│   ├── mod.rs
│   ├── models.rs        -- SecretProperties, SecretSummary, SecretRequest (backend-agnostic)
│   └── manager.rs       -- SecretManager (calls dyn SecretBackend, no Azure deps)
├── vault/
│   ├── mod.rs
│   ├── models.rs        -- VaultProperties, VaultSummary, VaultRole (backend-agnostic)
│   └── manager.rs       -- VaultManager (calls dyn VaultBackend, no Azure deps)
├── blob/                -- renamed to file/ or kept, calls dyn FileBackend
│   ...
├── cli/                 -- unchanged structure, but uses BackendRegistry
├── tui/                 -- unchanged structure, but uses dyn Backend
├── config/
│   ├── settings.rs      -- Config with backend-agnostic core + backend_config: BackendConfig enum
│   ...
└── ...
```

### 4.2 Migration strategy

The reorganization happens in phases to keep the codebase compilable at every commit:

1. **Create `src/backend/` with trait definitions** — new files, no changes to existing code.
2. **Implement traits for Azure** — `AzureSecretBackend` wraps existing `AzureSecretOperations`, etc. Initially a thin adapter.
3. **Create `BackendRegistry`** — dispatches to Azure by default (backward compat).
4. **Migrate `SecretManager`/`VaultManager`** — change from `Arc<dyn SecretOperations>` to `Arc<dyn SecretBackend>`.
5. **Migrate CLI handlers** — change from direct `DefaultAzureCredentialProvider` construction to `BackendRegistry::resolve()`.
6. **Migrate TUI data layer** — same registry pattern.
7. **Move Azure code** — relocate `src/auth/`, `src/vault/operations.rs`, `src/secret/manager.rs` (ops impl) into `src/backend/azure/`.
8. **Implement local backend**.

---

## 5. Backend registry and configuration

### 5.1 BackendRegistry

```rust
// src/backend/registry.rs

pub struct BackendRegistry {
    backends: HashMap<&'static str, Arc<dyn Backend>>,
    default: &'static str,
}

impl BackendRegistry {
    /// Build from config. Instantiates only the configured backend.
    pub async fn from_config(config: &Config) -> Result<Self, CrosstacheError> {
        let backend: Arc<dyn Backend> = match &config.backend {
            BackendConfig::Azure(azure_config) => {
                Arc::new(AzureBackend::new(azure_config).await?)
            }
            BackendConfig::Local(local_config) => {
                Arc::new(LocalBackend::new(local_config)?)
            }
        };
        let name = backend.name();
        let mut backends = HashMap::new();
        backends.insert(name, backend);
        Ok(Self { backends, default: name })
    }

    /// Get the active backend.
    pub fn active(&self) -> &dyn Backend {
        self.backends[self.default].as_ref()
    }
}
```

### 5.2 Config changes

```toml
# ~/.config/xv/xv.conf (TOML format)

# Backend selection — "azure" (default for existing users) or "local"
backend = "azure"

# Azure-specific config (only loaded when backend = "azure")
[azure]
subscription_id = "..."
tenant_id = "..."
default_resource_group = "..."
default_vault = "myproject-kv"
credential_priority = "cli"  # cli | managed_identity | environment | default

[azure.blob]
storage_account = "..."
container = "xv-files"

# Local backend config (only loaded when backend = "local")
[local]
store_path = "~/.xv/store"         # where encrypted files live
key_file = "~/.xv/key.txt"        # age identity file
default_vault = "default"          # default namespace
```

The migration path for existing users:
- Existing config files have no `backend` key → defaults to `"azure"`.
- Existing Azure-specific fields (`subscription_id`, `tenant_id`, etc.) are read into `[azure]` section even if written at top level (backward compat).
- `xv init` wizard gains a backend selection step.

### 5.3 CLI flag

```bash
xv --backend local set myapp/db-password "hunter2"
xv --backend azure list
xv list  # uses config default
```

The `--backend` flag is added to the global `Cli` struct. When present, it overrides the config default for that invocation.

---

## 6. Local age-encrypted file backend

### 6.1 Storage layout

```
~/.xv/store/
├── key.txt                          # age identity (private key)
├── recipients.txt                   # age recipients (public keys — for sharing)
├── vaults/
│   ├── default/
│   │   ├── .vault.json              # vault metadata (name, created, tags)
│   │   ├── .index.json.age          # encrypted index (secret names, groups, tags, metadata)
│   │   ├── secrets/
│   │   │   ├── db-password.age      # encrypted secret value
│   │   │   ├── db-password.meta.json # unencrypted metadata (name, groups, created, enabled, expiry)
│   │   │   ├── api-key.age
│   │   │   ├── api-key.meta.json
│   │   │   └── .versions/
│   │   │       ├── db-password/
│   │   │       │   ├── v1.age       # previous version values
│   │   │       │   ├── v1.meta.json
│   │   │       │   ├── v2.age
│   │   │       │   └── v2.meta.json
│   │   │       └── api-key/
│   │   │           └── ...
│   │   └── files/                   # optional file storage (feature-gated)
│   │       ├── cert.pem.age
│   │       └── cert.pem.meta.json
│   └── staging/
│       ├── .vault.json
│       ├── .index.json.age
│       └── secrets/
│           └── ...
```

Design decisions:
- **Metadata is NOT encrypted** — allows `xv list`, `xv find`, group filtering without decrypting anything. Only values are encrypted.
- **One file per secret** — simple, git-friendly, avoids the "big encrypted blob" problem.
- **Version history** via `.versions/` subdirectory — same format as current secrets.
- **Vault = directory** — natural namespacing, easy to back up, sync, or git-track.
- **age encryption** — modern, audited, composable (supports multiple recipients for team sharing).

### 6.2 Key management

```bash
# First run — xv init for local backend
xv init --backend local

# Generates:
# 1. age identity (private key) → ~/.xv/key.txt (0600)
# 2. age recipient (public key) → ~/.xv/recipients.txt
# 3. Default vault directory → ~/.xv/store/vaults/default/
```

Key storage:
- Identity file at `~/.xv/key.txt`, permissions `0600`.
- The key file path is configurable in `[local]` config.
- Supports `AGE_KEY_FILE` env var override (for CI/CD).
- Supports `AGE_KEY` env var for inline key (for containers).
- Future: hardware key support via age plugins (yubikey, etc.).

### 6.3 Encryption flow

```
set_secret(vault, name, value):
  1. Encrypt value with age using recipients.txt → name.age
  2. Write metadata to name.meta.json (unencrypted)
  3. If secret exists, move current name.age to .versions/name/vN.age
  4. Update .index.json.age (re-encrypt index)
  5. Set file permissions to 0600

get_secret(vault, name, include_value=true):
  1. Read name.meta.json for metadata
  2. If include_value: decrypt name.age with key.txt → plaintext value
  3. Return SecretProperties with value in Zeroizing<String>

list_secrets(vault, group_filter):
  1. Read all *.meta.json files (no decryption needed)
  2. Filter by group if specified
  3. Return Vec<SecretSummary>
```

### 6.4 Capability mapping

| Capability | Local backend | Notes |
|-----------|---------------|-------|
| `has_vaults` | ✅ | Directories = vaults |
| `has_file_storage` | ✅ | Same directory, age-encrypted |
| `has_rbac` | ❌ | No identity provider — use filesystem perms |
| `has_audit` | ❌ | No audit log (could add git-log-based audit later) |
| `has_versioning` | ✅ | .versions/ directory |
| `has_soft_delete` | ✅ | .trash/ directory with TTL |
| `has_groups` | ✅ | Stored in metadata |
| `has_folders` | ✅ | Stored in metadata |
| `has_notes` | ✅ | Stored in metadata |
| `has_expiry` | ✅ | Stored in metadata, checked on read |
| `max_secret_size` | None (unlimited) | Limited only by filesystem |
| `max_name_length` | 255 | Filesystem limit |
| `name_charset` | Unrestricted | URL-encoded for filesystem safety |

### 6.5 Dependencies

```toml
[dependencies]
age = "0.10"               # age encryption library
secrecy = "0.8"            # Secret<String> wrapper (pairs with zeroize)
```

The `age` crate is well-maintained (Filippo Valsorda), audited, and already used in production by `sops`, `passage`, and others.

---

## 7. Capability negotiation and graceful degradation

When a command targets a feature the active backend doesn't support, xv should fail gracefully with a clear message — never panic or show a stack trace.

### 7.1 CLI behavior

```bash
$ xv --backend local share myapp/db-password user@example.com
Error [xv-unsupported]: The local backend does not support access sharing.
Hint: Use the azure backend for RBAC-based secret sharing.

$ xv --backend local audit myapp/db-password
Error [xv-unsupported]: The local backend does not support audit logs.

$ xv --backend local versions myapp/db-password
# Works fine — local backend supports versioning
```

### 7.2 TUI behavior

The TUI already has a `BackendCapabilities` struct that controls which panes/overlays are shown. Phase 2 connects this to real capability data:

- Audit overlay: hidden when `!capabilities.has_audit`
- RBAC pane: hidden when `!capabilities.has_rbac`
- Version history: shown only when `capabilities.has_versioning`
- Backend-specific metadata: rendered in a "Details" section using `backend_metadata`

### 7.3 Implementation pattern

```rust
// In CLI handlers (src/cli/secret_ops.rs)
fn handle_share(backend: &dyn Backend, args: ShareArgs) -> Result<()> {
    let caps = backend.capabilities();
    if !caps.has_rbac {
        return Err(CrosstacheError::Unsupported {
            backend: backend.name(),
            feature: "access sharing",
            hint: Some("Use the azure backend for RBAC-based secret sharing."),
        });
    }
    // ... proceed with RBAC operations
}
```

---

## 8. Cross-backend migration

### 8.1 `xv migrate` command

```bash
# Copy all secrets from Azure to local
xv migrate --from azure --to local

# Copy a specific vault
xv migrate --from azure --to local --vault myproject-kv

# Copy specific secrets
xv migrate --from azure --to local --vault myproject-kv --filter "db-*"

# Dry run
xv migrate --from azure --to local --dry-run
```

Implementation:
1. List secrets from source backend.
2. For each secret, `get_secret(include_value=true)` from source.
3. `set_secret()` on target backend, preserving metadata (groups, tags, notes).
4. Report: "Migrated 47 secrets (3 skipped — already exist)".
5. Values are held in `Zeroizing<String>` during transfer — never written to disk unencrypted.

### 8.2 Metadata mapping

Not all metadata transfers cleanly between backends. The `migrate` command handles this:

| Source field | Target: local | Target: azure |
|-------------|---------------|---------------|
| `groups` | ✅ preserved | ✅ preserved (tags) |
| `note` | ✅ preserved | ✅ preserved (tag) |
| `folder` | ✅ preserved | ✅ preserved (tag) |
| `expiry` | ✅ preserved | ✅ preserved (attribute) |
| `content_type` | ✅ preserved | ✅ preserved |
| `recovery_level` | ❌ ignored | ✅ Azure-specific |
| `resource_group` | ❌ ignored | ✅ Azure-specific |
| `version_history` | ❌ current only | ❌ current only |

---

## 9. Error model changes

### 9.1 New error variants

```rust
// Added to CrosstacheError enum in src/error.rs

/// Backend doesn't support this operation.
#[error("{backend} backend does not support {feature}")]
Unsupported {
    backend: String,
    feature: String,
    hint: Option<String>,
},
// Error code: xv-unsupported, exit code: 45

/// Backend configuration error.
#[error("backend config error: {0}")]
BackendConfigError(String),
// Error code: xv-backend-config, exit code: 3

/// Encryption/decryption error (local backend).
#[error("encryption error: {0}")]
EncryptionError(String),
// Error code: xv-encryption, exit code: 46

/// Key file not found or unreadable.
#[error("key file not found: {path}")]
KeyFileError { path: String },
// Error code: xv-key-file, exit code: 47
```

### 9.2 Exit code allocation

Existing families preserved. New allocations:
- `45` — unsupported operation
- `46` — encryption error
- `47` — key file error
- `48–49` — reserved for future backend errors

---

## 10. Implementation plan

### 10.1 Sequencing (estimated ~3 weeks)

**Week 1: Trait extraction**
- PR #1: Create `src/backend/` with trait definitions (Backend, SecretBackend, VaultBackend, FileBackend, BackendError). No existing code changes.
- PR #2: Implement `AzureSecretBackend` wrapping existing `AzureSecretOperations`. Both old and new paths compile.
- PR #3: Implement `AzureVaultBackend` wrapping existing `AzureVaultOperations`.
- PR #4: Implement `AzureFileBackend` wrapping existing `BlobManager`.
- PR #5: Create `AzureBackend` (top-level Backend impl) composing the three sub-backends.

**Week 2: Registry + migration**
- PR #6: Create `BackendRegistry`, add `backend` config key and `--backend` CLI flag. All existing paths go through registry (backward compat — Azure is default).
- PR #7: Migrate `SecretManager` from `Arc<dyn SecretOperations>` to `Arc<dyn SecretBackend>`.
- PR #8: Migrate `VaultManager` from `Arc<dyn VaultOperations>` to `Arc<dyn VaultBackend>`.
- PR #9: Migrate CLI handlers to use registry instead of direct Azure construction.
- PR #10: Migrate TUI data layer to use registry.
- PR #11: Move Azure source files into `src/backend/azure/`. Update all imports.

**Week 3: Local backend + polish**
- PR #12: Implement `LocalBackend` — secrets CRUD with age encryption.
- PR #13: Implement local vault operations (directory-based).
- PR #14: Implement local version history.
- PR #15: Implement `xv migrate` command.
- PR #16: Hermetic E2E tests for local backend.
- PR #17: Update README, docs, `xv init` wizard.
- PR #18: Capability negotiation in CLI + TUI.

### 10.2 Release plan

- **v0.8.0-rc.1** — trait extraction complete, Azure working through new traits (no user-visible changes).
- **v0.8.0-rc.2** — local backend functional, `xv migrate` working.
- **v0.8.0** — full release with both backends, docs, tests.

### 10.3 Testing strategy

1. **Unit tests** — each backend module tested in isolation. Local backend is fully hermetic (tempdir). Azure backend tests use mockall against the trait.
2. **Integration tests** — existing CLI integration tests run against both backends (parameterized).
3. **Migration tests** — round-trip: create secrets in one backend, migrate, verify in the other.
4. **TUI tests** — existing TUI view tests should work unchanged (they test rendering, not backend).

---

## 11. Risk assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Trait surface too narrow — Azure features don't fit | Low | High | Existing traits (SecretOperations, VaultOperations) already cover the surface. backend_metadata handles overflow. |
| Trait surface too wide — local backend can't implement required methods | Low | Medium | Default impls return Unsupported. Only 6 required methods on SecretBackend. |
| age crate API instability | Very low | Low | age 0.10 is stable, widely used. Pin version. |
| Performance regression from trait indirection | Very low | Low | Dynamic dispatch cost is negligible vs. network I/O (Azure) or disk I/O (local). |
| Config migration breaks existing users | Medium | High | Backward compat: missing `backend` key defaults to `"azure"`. Top-level Azure fields still read. |
| Secret name encoding for filesystem | Low | Medium | URL-encode names for filesystem. Decode on read. Round-trip tested. |

---

## 12. Open questions

1. **Should the local backend support file storage?** Current design says yes (age-encrypted files in a `files/` subdirectory). Could defer to reduce scope.

2. **Git integration for local backend?** The directory structure is git-friendly by design. Should `xv` auto-commit changes? Or leave that to the user? Recommendation: defer — let users `git init` their store if they want history.

3. **Should `xv scan` work against the local backend?** The scanner reads secret values to match against source code. With the local backend, all values are local anyway. Answer: yes, same flow — `get_secret(include_value=true)` through the trait.

4. **Multi-recipient encryption for team sharing?** The `recipients.txt` file supports multiple age public keys. Should `xv` manage recipients (add/remove team members)? Recommendation: basic support in v0.8 — `xv local add-recipient <public-key>`. Full team management deferred.

---

## 13. Success criteria

Phase 2 is complete when:

1. All existing `xv` commands work unchanged against the Azure backend through the new trait layer.
2. `xv --backend local` supports: set, get, list, delete, find, scan, tui, versions, rollback, inject, run.
3. `xv migrate --from azure --to local` and `--from local --to azure` both work.
4. Unsupported operations (share, audit) produce clear error messages with hints.
5. All tests pass. CI green. No regressions.
6. `xv init` guides new users through backend selection.
7. README documents both backends with examples.
