# AWS Secrets Manager Backend Implementation Plan

> **Status:** ✅ Implemented in **v0.10.0-rc.1** (2026-05-13).
> Retained as design history.
> Roadmap & open work tracked in `ROADMAP.md` at the repo root.
> Implementation history lives in `CHANGELOG.md`. This file is retained as design context — do not edit to reflect current behavior; open a new spec instead.


> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `xv --backend aws` — a third backend implementation backed by AWS Secrets Manager, paired with hardened cross-cloud `xv migrate` as the v0.10 marquee feature. Behind a `aws` Cargo feature flag (default off). Prefix-based virtual vaults via `<vault>/.xv-vault` marker secrets; one region per backend instance with `[named_backends.*]` support for multi-region; RBAC, audit, S3, and native rotation deferred (graceful `Unsupported` errors).

**Architecture:** `AwsBackend` lives at `src/backend/aws/`, mirroring the existing `src/backend/azure/` layout. It composes `AwsSecretBackend` and `AwsVaultBackend` behind the existing `Backend` / `SecretBackend` / `VaultBackend` traits. Built on `aws-sdk-secretsmanager` + `aws-config`. Hermetic mock-based tests via `aws-smithy-mocks-experimental`; LocalStack-gated integration tests for full E2E. Multi-region support is achieved via a new `Config.named_backends: HashMap<String, NamedBackendEntry>` map; existing single-instance configs continue to work unchanged.

**Tech Stack:** Rust 2021. New deps: `aws-sdk-secretsmanager = "1"` (and `aws-config = "1"`) under feature `aws`; `aws-smithy-mocks-experimental` under `[dev-dependencies]`. Existing: `tokio`, `async-trait`, `serde`, `thiserror`, `zeroize`.

**Reference spec:** `docs/superpowers/specs/2026-05-09-aws-backend-phase-3-design.md`. Out of scope: AWS Parameter Store backend, S3 file storage, IAM-based `xv share`, CloudTrail-based `xv audit`, Lambda-based rotation, full version-history transfer in `xv migrate`, code-review remediation pass.

---

## File Structure

**Created:**

| Path | Responsibility |
|------|----------------|
| `src/backend/aws/mod.rs` | Module index. `AwsBackend` struct + `Backend` trait impl (name, kind, capabilities, secrets/vaults accessors, health_check). |
| `src/backend/aws/auth.rs` | AWS SDK client builder. Loads credentials via `aws-config` default chain; honors profile/region overrides. |
| `src/backend/aws/config.rs` | `AwsConfig` struct. Holds region, profile, optional `endpoint_url`, default_vault. |
| `src/backend/aws/secrets.rs` | `AwsSecretBackend` impl `SecretBackend`. Owns the `SecretsManagerClient` and the vault prefix; one method per trait method. |
| `src/backend/aws/vaults.rs` | `AwsVaultBackend` impl `VaultBackend`. Marker-secret-based vault create/list/delete. |
| `src/backend/aws/encoding.rs` | Name encoding/decoding. Marker secret name reservation (`.xv-vault`). Reserved-namespace check. |
| `src/backend/aws/metadata.rs` | Tag ↔ `SecretProperties` round-trip. Constants for tag keys (`xv:original_name`, `xv:groups`, etc.). |
| `src/backend/aws/errors.rs` | AWS SDK error → `BackendError` mapping. One `From` impl per SDK operation error type, plus a generic fallback. |
| `src/backend/aws/models.rs` | Internal AWS-specific helper types (e.g., `AwsTag` newtype if useful). May stay empty initially. |
| `tests/aws_backend_tests.rs` | Hermetic unit-style tests using `aws-smithy-mocks-experimental` to stub Secrets Manager API responses. |
| `tests/aws_localstack_tests.rs` | LocalStack-gated integration tests (skip silently when LocalStack unavailable). |
| `tests/migration_round_trip_tests.rs` | Cross-backend migration round-trip tests (Azure↔AWS, AWS↔Local) — gated where they need cloud credentials. |
| `tests/terraform/aws/main.tf` | Live AWS test infrastructure (test region, IAM role, cleanup). |
| `docs/migration.md` | User-facing cross-cloud migration guide. |

**Modified:**

| Path | Change |
|------|--------|
| `Cargo.toml` | Add `aws-sdk-secretsmanager`, `aws-config` as **optional** deps; add `[features] aws = ["dep:aws-sdk-secretsmanager", "dep:aws-config"]`; add `aws-smithy-mocks-experimental` to `[dev-dependencies]`. Bump version to `0.10.0-rc.1` (Task 38). |
| `src/backend/mod.rs` | Add `BackendKind::Aws` variant + parser aliases (`aws`, `secretsmanager`); add `NameCharset::AwsRelaxed` variant; gate `pub mod aws;` on `feature = "aws"`. |
| `src/backend/registry.rs` | Add `BackendKind::Aws` arm in `from_config`; add `Config.named_backends` resolution path. |
| `src/config/settings.rs` | Add `AwsConfig` struct, `Config.aws: Option<AwsConfig>` field, `NamedBackendEntry` struct, `Config.named_backends: HashMap<String, NamedBackendEntry>` field. |
| `src/cli/commands.rs` | Replace `--overwrite` on `Commands::Migrate` with `--on-conflict`; add `--aws-profile` and `--region` global flags on `Cli`. |
| `src/cli/migrate_ops.rs` | Extend `create_backend` with AWS arm; add pre-flight diff/summary; add idempotency tags; add bounded concurrency. |
| `src/cli/config_ops.rs` | Surface `Config.aws` and `Config.named_backends` in `xv config show` / `xv config set`. |
| `src/config/init.rs` | Add AWS branch to `run_interactive_setup`. |
| `README.md` | Add "AWS backend" subsection + cross-cloud migration callout. |
| `docs/FEATURES.md` | Update migration entry. |

> **Why `aws-sdk-secretsmanager` is feature-gated:** The AWS SDK ships ~1.5 MB of compiled code per service. Default-off keeps `cargo build` lean for users who don't need AWS. Distribution-channel binaries (homebrew/scoop/deb/rpm) ship with `--features aws`.

---

## Phase 1: Foundation (Cargo + types + module skeleton)

## Task 1: Add `aws` Cargo feature + AWS SDK dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add optional deps and feature**

In `Cargo.toml` under `[dependencies]`, add (after the existing Azure block):

```toml
# AWS SDK - feature-gated under `aws`
aws-sdk-secretsmanager = { version = "1", optional = true }
aws-config = { version = "1", optional = true, features = ["behavior-version-latest"] }
```

Under `[dev-dependencies]`, add:

```toml
aws-smithy-mocks-experimental = "0.2"
```

In the existing `[features]` block, replace:

```toml
[features]
default = ["file-ops"]
file-ops = []
tui = []
```

with:

```toml
[features]
default = ["file-ops"]
file-ops = []
tui = []
aws = ["dep:aws-sdk-secretsmanager", "dep:aws-config"]
```

- [ ] **Step 2: Verify default build still compiles**

Run: `cargo build`
Expected: clean compile, no AWS-related symbols pulled in.

- [ ] **Step 3: Verify `aws` feature compiles**

Run: `cargo build --features aws`
Expected: clean compile; `Cargo.lock` gains AWS SDK crates.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat(aws): add aws-sdk-secretsmanager dep + aws feature flag"
```

---

## Task 2: Add `BackendKind::Aws` variant

**Files:**
- Modify: `src/backend/mod.rs:45-73`
- Test: `src/backend/mod.rs` (existing tests module)

- [ ] **Step 1: Write failing test**

Append to `src/backend/mod.rs` test module (find with `grep -n "mod tests" src/backend/mod.rs`):

```rust
#[test]
fn backend_kind_parses_aws() {
    use std::str::FromStr;
    assert_eq!(BackendKind::from_str("aws").unwrap(), BackendKind::Aws);
    assert_eq!(BackendKind::from_str("AWS").unwrap(), BackendKind::Aws);
    assert_eq!(BackendKind::from_str("secretsmanager").unwrap(), BackendKind::Aws);
}

#[test]
fn backend_kind_aws_displays_as_aws() {
    assert_eq!(format!("{}", BackendKind::Aws), "aws");
}
```

- [ ] **Step 2: Verify test fails**

Run: `cargo test --lib backend::tests::backend_kind_parses_aws`
Expected: fails to compile — `BackendKind::Aws` does not exist.

- [ ] **Step 3: Add the variant + parser**

In `src/backend/mod.rs` modify the `BackendKind` enum, its `Display` impl, and its `FromStr` impl:

```rust
pub enum BackendKind {
    Azure,
    Local,
    Aws,
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Azure => write!(f, "azure"),
            Self::Local => write!(f, "local"),
            Self::Aws => write!(f, "aws"),
        }
    }
}

impl std::str::FromStr for BackendKind {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "azure" | "az" | "keyvault" => Ok(Self::Azure),
            "local" | "file" | "age" => Ok(Self::Local),
            "aws" | "secretsmanager" | "asm" => Ok(Self::Aws),
            _ => Err(format!(
                "unknown backend kind: {s}. Valid options: azure, local, aws"
            )),
        }
    }
}
```

- [ ] **Step 4: Verify tests pass**

Run: `cargo test --lib backend::tests::backend_kind_parses_aws backend::tests::backend_kind_aws_displays_as_aws`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/mod.rs
git commit -m "feat(aws): add BackendKind::Aws variant"
```

---

## Task 3: Add `NameCharset::AwsRelaxed` variant

**Files:**
- Modify: `src/backend/mod.rs` (NameCharset enum)

- [ ] **Step 1: Add the variant**

Locate `pub enum NameCharset` in `src/backend/mod.rs`. Replace with:

```rust
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum NameCharset {
    /// Only `[a-zA-Z0-9-]` — Azure Key Vault's constraint.
    AlphanumericHyphen,
    /// Any printable character (the backend encodes as needed).
    Unrestricted,
    /// AWS Secrets Manager: `[a-zA-Z0-9/_+=.@-]`.
    AwsRelaxed,
    /// Custom validation function.
    Custom(fn(&str) -> bool),
}

impl NameCharset {
    /// Returns true if `name` is valid under this charset.
    pub fn is_valid(&self, name: &str) -> bool {
        match self {
            Self::AlphanumericHyphen => name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            Self::Unrestricted => true,
            Self::AwsRelaxed => name.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '/' | '_' | '+' | '=' | '.' | '@' | '-')
            }),
            Self::Custom(f) => f(name),
        }
    }
}
```

- [ ] **Step 2: Add tests**

Append to the existing `mod tests` block in `src/backend/mod.rs`:

```rust
#[test]
fn aws_relaxed_charset_accepts_aws_chars() {
    let cs = NameCharset::AwsRelaxed;
    assert!(cs.is_valid("myproj/db-password"));
    assert!(cs.is_valid("api_key+v2"));
    assert!(cs.is_valid("alice@example.com"));
    assert!(cs.is_valid("v1.2.3"));
}

#[test]
fn aws_relaxed_charset_rejects_invalid_chars() {
    let cs = NameCharset::AwsRelaxed;
    assert!(!cs.is_valid("has space"));
    assert!(!cs.is_valid("has*star"));
    assert!(!cs.is_valid("has(paren)"));
}
```

- [ ] **Step 3: Verify**

Run: `cargo test --lib backend::tests::aws_relaxed_charset`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/backend/mod.rs
git commit -m "feat(aws): add NameCharset::AwsRelaxed + is_valid helper"
```

---

## Task 4: Create `src/backend/aws/` module skeleton

**Files:**
- Create: `src/backend/aws/mod.rs`
- Create: `src/backend/aws/auth.rs`
- Create: `src/backend/aws/config.rs`
- Create: `src/backend/aws/encoding.rs`
- Create: `src/backend/aws/errors.rs`
- Create: `src/backend/aws/metadata.rs`
- Create: `src/backend/aws/models.rs`
- Create: `src/backend/aws/secrets.rs`
- Create: `src/backend/aws/vaults.rs`
- Modify: `src/backend/mod.rs` (gate `pub mod aws`)

- [ ] **Step 1: Create `src/backend/aws/mod.rs`**

```rust
//! AWS Secrets Manager backend.
//!
//! Phase 3 / v0.10. See `docs/superpowers/specs/2026-05-09-aws-backend-phase-3-design.md`.

pub mod auth;
pub mod config;
pub mod encoding;
pub mod errors;
pub mod metadata;
pub mod models;
pub mod secrets;
pub mod vaults;

// AwsBackend struct fleshed out in Task 12.
```

- [ ] **Step 2: Create stubs for each submodule**

`src/backend/aws/auth.rs`:
```rust
//! AWS SDK client builder. Loads credentials via aws-config default chain.
```

`src/backend/aws/config.rs`:
```rust
//! `AwsConfig` struct and resolution helpers. See Task 5.
```

`src/backend/aws/encoding.rs`:
```rust
//! Name encoding/decoding for AWS Secrets Manager. See Task 10.
```

`src/backend/aws/errors.rs`:
```rust
//! AWS SDK error -> BackendError mapping. See Task 9.
```

`src/backend/aws/metadata.rs`:
```rust
//! Tag <-> SecretProperties round-trip. See Task 11.
```

`src/backend/aws/models.rs`:
```rust
//! Internal AWS-specific types.
```

`src/backend/aws/secrets.rs`:
```rust
//! `AwsSecretBackend` impl `SecretBackend`. See Tasks 14-28.
```

`src/backend/aws/vaults.rs`:
```rust
//! `AwsVaultBackend` impl `VaultBackend`. See Tasks 29-33.
```

- [ ] **Step 3: Wire the module behind the feature flag**

In `src/backend/mod.rs`, find the existing `pub mod azure;` and `pub mod local;` declarations. After them, add:

```rust
#[cfg(feature = "aws")]
pub mod aws;
```

- [ ] **Step 4: Verify both build modes still compile**

Run: `cargo build` (default features — no AWS)
Expected: clean.

Run: `cargo build --features aws`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/backend/aws/ src/backend/mod.rs
git commit -m "feat(aws): scaffold src/backend/aws/ module"
```

---

## Task 5: Add `AwsConfig` struct to settings.rs

**Files:**
- Modify: `src/config/settings.rs`

- [ ] **Step 1: Add the struct**

Locate `pub struct LocalConfig` in `src/config/settings.rs:21`. After the `LocalConfig` block (after the closing `}` near line 35), add:

```rust
/// Configuration for the AWS Secrets Manager backend.
///
/// Lives under `[aws]` in `xv.conf`. Only relevant when `backend = "aws"`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AwsConfig {
    /// AWS region, e.g. `us-east-1`. Falls through to `AWS_REGION` env var.
    #[serde(default)]
    pub region: Option<String>,

    /// AWS profile name. Falls through to `AWS_PROFILE`. Defaults to "default".
    #[serde(default)]
    pub profile: Option<String>,

    /// Optional endpoint URL override. Used for LocalStack and other AWS-compatible APIs.
    #[serde(default)]
    pub endpoint_url: Option<String>,

    /// Default vault name (= prefix) used when no `--vault` / context is set.
    #[serde(default)]
    pub default_vault: Option<String>,
}
```

- [ ] **Step 2: Add `Config.aws` field**

Find `pub local: Option<LocalConfig>` in `src/config/settings.rs:158`. Below it (before `clipboard_timeout`), add:

```rust
    /// Configuration for the AWS Secrets Manager backend.
    /// Only relevant when `backend = "aws"`.
    #[tabled(skip)]
    #[serde(default)]
    pub aws: Option<AwsConfig>,
```

- [ ] **Step 3: Update `Default for Config`**

Find `impl Default for Config` (around line 189). Add `aws: None,` to the struct literal in `default()`. The full updated `default()` should include both `local: None,` and `aws: None,`.

- [ ] **Step 4: Update validate() to skip Azure checks when backend is aws**

Find `pub fn validate(&self)` (around line 221). Replace the body's branch:

```rust
pub fn validate(&self) -> Result<()> {
    let backend = self.effective_backend_name();
    if backend == "azure" {
        if self.subscription_id.is_empty() {
            return Err(CrosstacheError::config("Subscription ID is required"));
        }
        if self.tenant_id.is_empty() {
            return Err(CrosstacheError::config("Tenant ID is required"));
        }
    }
    if backend == "aws" {
        let aws = self.aws.as_ref().ok_or_else(|| {
            CrosstacheError::config("[aws] config block is required when backend = \"aws\"")
        })?;
        if aws.region.is_none()
            && std::env::var("AWS_REGION").is_err()
            && std::env::var("AWS_DEFAULT_REGION").is_err()
        {
            return Err(CrosstacheError::config(
                "AWS region required: set [aws].region in config or AWS_REGION env var",
            ));
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Add unit test**

In `src/config/settings.rs` `mod tests` (find with `grep -n "mod tests" src/config/settings.rs`), add:

```rust
#[test]
fn validate_requires_aws_block_when_backend_is_aws() {
    let cfg = Config {
        backend: Some("aws".into()),
        aws: None,
        ..Default::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.to_string().contains("aws"), "got: {err}");
}

#[test]
fn validate_passes_when_aws_block_present_with_region() {
    let cfg = Config {
        backend: Some("aws".into()),
        aws: Some(AwsConfig {
            region: Some("us-east-1".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(cfg.validate().is_ok());
}
```

- [ ] **Step 6: Verify**

Run: `cargo test --lib config::settings::tests::validate_requires_aws_block_when_backend_is_aws config::settings::tests::validate_passes_when_aws_block_present_with_region`
Expected: PASS.

Run: `cargo build` (and `cargo build --features aws`).
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add src/config/settings.rs
git commit -m "feat(aws): add AwsConfig struct and Config.aws field"
```

---

## Task 6: Add `Config.named_backends` map for multi-region support

**Files:**
- Modify: `src/config/settings.rs`

- [ ] **Step 1: Add `NamedBackendEntry` enum**

After the `AwsConfig` struct from Task 5, add:

```rust
/// A named backend entry in `Config.named_backends`. Each entry is a
/// fully-self-contained backend configuration tagged with its type.
///
/// Used for multi-region AWS, multi-tenant Azure, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NamedBackendEntry {
    Aws(AwsConfig),
    Local(LocalConfig),
    // Azure intentionally omitted from this enum for now; existing
    // top-level Azure fields handle the single-instance case.
}
```

- [ ] **Step 2: Add `Config.named_backends` field**

Find the struct `Config` (around line 108). After `pub aws: Option<AwsConfig>` (added in Task 5), add:

```rust
    /// Named backend instances for multi-region / multi-tenant use.
    /// Active backend selected via `Config.backend` matching a key here.
    #[tabled(skip)]
    #[serde(default)]
    pub named_backends: std::collections::HashMap<String, NamedBackendEntry>,
```

- [ ] **Step 3: Update `Default for Config`**

In `default()`, add `named_backends: std::collections::HashMap::new(),`.

- [ ] **Step 4: Add unit test**

In `src/config/settings.rs` `mod tests`:

```rust
#[test]
fn named_backends_deserializes_aws_entry() {
    let toml_str = r#"
backend = "aws-east"

[named_backends.aws-east]
type = "aws"
region = "us-east-1"
profile = "prod"
default_vault = "myproj-kv"
"#;
    let cfg: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.backend.as_deref(), Some("aws-east"));
    let entry = cfg.named_backends.get("aws-east").unwrap();
    match entry {
        NamedBackendEntry::Aws(aws) => {
            assert_eq!(aws.region.as_deref(), Some("us-east-1"));
            assert_eq!(aws.profile.as_deref(), Some("prod"));
        }
        _ => panic!("expected Aws variant"),
    }
}
```

- [ ] **Step 5: Verify**

Run: `cargo test --lib config::settings::tests::named_backends_deserializes_aws_entry`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/config/settings.rs
git commit -m "feat(aws): add Config.named_backends map for multi-region"
```

---

## Phase 2: AWS backend skeleton (auth, errors, encoding, metadata, Backend trait)

## Task 7: Implement `auth.rs` — AWS SDK config builder

**Files:**
- Modify: `src/backend/aws/auth.rs`

- [ ] **Step 1: Replace stub with real impl**

```rust
//! AWS SDK config builder. Loads credentials via the aws-config default chain.
//!
//! No xv-specific credential priority abstraction (unlike Azure's
//! `--credential-priority`). AWS users have strong opinions about credential
//! resolution; the SDK chain is industry standard and we don't try to model it.

use crate::backend::aws::config::AwsConfig;
use crate::backend::error::BackendError;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;

/// Build a `SecretsManagerClient` from the resolved `AwsConfig` plus
/// per-invocation overrides (region, profile from CLI flags or env vars).
pub async fn build_client(
    aws_cfg: &AwsConfig,
    region_override: Option<String>,
    profile_override: Option<String>,
) -> Result<SecretsManagerClient, BackendError> {
    let region = region_override
        .or_else(|| aws_cfg.region.clone())
        .or_else(|| std::env::var("AWS_REGION").ok())
        .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
        .ok_or_else(|| {
            BackendError::AuthenticationFailed(
                "AWS region not set: provide [aws].region, AWS_REGION, or --region".into(),
            )
        })?;

    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(region));

    let profile = profile_override.or_else(|| aws_cfg.profile.clone());
    if let Some(ref p) = profile {
        loader = loader.profile_name(p);
    }

    if let Some(ref endpoint) = aws_cfg.endpoint_url {
        if !endpoint.is_empty() {
            loader = loader.endpoint_url(endpoint);
        }
    }

    let sdk_config = loader.load().await;
    Ok(SecretsManagerClient::new(&sdk_config))
}
```

- [ ] **Step 2: Verify build**

Run: `cargo build --features aws`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/backend/aws/auth.rs
git commit -m "feat(aws): add SDK client builder honoring config + overrides"
```

---

## Task 8: Implement `errors.rs` — AWS SDK error → BackendError mapping

**Files:**
- Modify: `src/backend/aws/errors.rs`

- [ ] **Step 1: Write the conversion**

```rust
//! AWS SDK error -> BackendError mapping.
//!
//! The AWS SDK exposes per-operation error enums (e.g. `GetSecretValueError`).
//! We provide free functions for each operation we use, so call sites stay
//! readable and the mapping is exhaustive over the variants we observe.

use crate::backend::error::BackendError;
use aws_sdk_secretsmanager::operation::create_secret::CreateSecretError;
use aws_sdk_secretsmanager::operation::delete_secret::DeleteSecretError;
use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretError;
use aws_sdk_secretsmanager::operation::get_secret_value::GetSecretValueError;
use aws_sdk_secretsmanager::operation::list_secret_version_ids::ListSecretVersionIdsError;
use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsError;
use aws_sdk_secretsmanager::operation::put_secret_value::PutSecretValueError;
use aws_sdk_secretsmanager::operation::restore_secret::RestoreSecretError;
use aws_sdk_secretsmanager::operation::tag_resource::TagResourceError;
use aws_sdk_secretsmanager::operation::untag_resource::UntagResourceError;
use aws_sdk_secretsmanager::operation::update_secret::UpdateSecretError;
use aws_sdk_secretsmanager::operation::update_secret_version_stage::UpdateSecretVersionStageError;
use aws_smithy_runtime_api::client::result::SdkError;

fn generic<E: std::fmt::Display>(op: &str, e: E) -> BackendError {
    BackendError::Internal(format!("aws {op}: {e}"))
}

fn handle_sdk<R, E: std::fmt::Display>(op: &str, e: SdkError<E, R>) -> BackendError {
    match e {
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) => {
            BackendError::Network(format!("aws {op}: {e}"))
        }
        SdkError::ServiceError(svc) => {
            // Caller will inspect svc.err() before calling this; this is a fallback.
            BackendError::Internal(format!("aws {op}: {}", svc.err()))
        }
        other => generic(op, other),
    }
}

pub fn from_create(e: SdkError<CreateSecretError>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        match svc.err() {
            CreateSecretError::ResourceExistsException(inner) => {
                return BackendError::Conflict(inner.to_string())
            }
            CreateSecretError::InvalidRequestException(inner) => {
                return BackendError::InvalidArgument(inner.to_string())
            }
            CreateSecretError::InvalidParameterException(inner) => {
                return BackendError::InvalidArgument(inner.to_string())
            }
            CreateSecretError::LimitExceededException(inner) => {
                return BackendError::RateLimited {
                    retry_after_secs: None,
                }
                .with_context(format!("limit: {inner}"))
            }
            _ => {}
        }
    }
    handle_sdk("CreateSecret", e)
}

pub fn from_get_value(name: &str, e: SdkError<GetSecretValueError>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        match svc.err() {
            GetSecretValueError::ResourceNotFoundException(_) => {
                return BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                }
            }
            GetSecretValueError::DecryptionFailure(inner) => {
                return BackendError::Internal(format!("decryption failed: {inner}"))
            }
            GetSecretValueError::InvalidRequestException(inner) => {
                return BackendError::InvalidArgument(inner.to_string())
            }
            _ => {}
        }
    }
    handle_sdk("GetSecretValue", e)
}

pub fn from_describe(name: &str, e: SdkError<DescribeSecretError>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let DescribeSecretError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("DescribeSecret", e)
}

pub fn from_list(e: SdkError<ListSecretsError>) -> BackendError {
    handle_sdk("ListSecrets", e)
}

pub fn from_delete(name: &str, e: SdkError<DeleteSecretError>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let DeleteSecretError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("DeleteSecret", e)
}

pub fn from_put_value(name: &str, e: SdkError<PutSecretValueError>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let PutSecretValueError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("PutSecretValue", e)
}

pub fn from_update(name: &str, e: SdkError<UpdateSecretError>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let UpdateSecretError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("UpdateSecret", e)
}

pub fn from_restore(name: &str, e: SdkError<RestoreSecretError>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let RestoreSecretError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("RestoreSecret", e)
}

pub fn from_list_versions(name: &str, e: SdkError<ListSecretVersionIdsError>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let ListSecretVersionIdsError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("ListSecretVersionIds", e)
}

pub fn from_update_stage(name: &str, e: SdkError<UpdateSecretVersionStageError>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let UpdateSecretVersionStageError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("UpdateSecretVersionStage", e)
}

pub fn from_tag(e: SdkError<TagResourceError>) -> BackendError {
    handle_sdk("TagResource", e)
}

pub fn from_untag(e: SdkError<UntagResourceError>) -> BackendError {
    handle_sdk("UntagResource", e)
}

trait ErrCtx {
    fn with_context(self, ctx: String) -> BackendError;
}

impl ErrCtx for BackendError {
    fn with_context(self, _ctx: String) -> BackendError {
        // RateLimited has no context field today; placeholder for future enrichment.
        self
    }
}
```

- [ ] **Step 2: Verify build**

Run: `cargo build --features aws`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/backend/aws/errors.rs
git commit -m "feat(aws): add SDK error -> BackendError mapping per operation"
```

---

## Task 9: Implement `encoding.rs` — name encoding, marker reservation

**Files:**
- Modify: `src/backend/aws/encoding.rs`

- [ ] **Step 1: Write tests first**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_name_joins_prefix() {
        assert_eq!(aws_name("myproj-kv", "db-password"), "myproj-kv/db-password");
    }

    #[test]
    fn strip_prefix_extracts_secret_name() {
        assert_eq!(strip_prefix("myproj-kv", "myproj-kv/db-password"), Some("db-password".to_string()));
        assert_eq!(strip_prefix("myproj-kv", "other-vault/db-password"), None);
    }

    #[test]
    fn marker_name_constant() {
        assert_eq!(marker_name("myproj-kv"), "myproj-kv/.xv-vault");
    }

    #[test]
    fn is_marker_detects_marker() {
        assert!(is_marker("myproj-kv/.xv-vault"));
        assert!(!is_marker("myproj-kv/db-password"));
    }

    #[test]
    fn validate_secret_name_rejects_reserved() {
        assert!(matches!(validate_secret_name(".xv-vault"), Err(_)));
        assert!(matches!(validate_secret_name(".xv-anything"), Err(_)));
    }

    #[test]
    fn validate_secret_name_rejects_empty() {
        assert!(matches!(validate_secret_name(""), Err(_)));
    }

    #[test]
    fn validate_secret_name_accepts_normal_names() {
        assert!(validate_secret_name("db-password").is_ok());
        assert!(validate_secret_name("api/v1/key").is_ok());
        assert!(validate_secret_name("v1.2.3-rc.1").is_ok());
    }
}
```

- [ ] **Step 2: Implement**

Replace the stub at the top of `src/backend/aws/encoding.rs`:

```rust
//! Name encoding/decoding for AWS Secrets Manager.
//!
//! AWS allows `[a-zA-Z0-9/_+=.@-]` and 512-char names. Our prefix-based
//! virtual vault scheme produces names like `myproj-kv/db-password`.
//!
//! The reserved namespace `.xv-*` is used for vault markers and other
//! xv-internal bookkeeping. User-supplied names starting with `.xv-` are
//! rejected at the `set_secret` boundary.

use crate::backend::error::BackendError;

/// The AWS Secrets Manager name length limit.
pub const MAX_NAME_LEN: usize = 512;

/// The marker filename inside each vault prefix.
const MARKER_BASENAME: &str = ".xv-vault";

/// Returns the full AWS name for a secret in a given vault.
pub fn aws_name(vault: &str, secret_name: &str) -> String {
    format!("{vault}/{secret_name}")
}

/// Strips the vault prefix from an AWS name, returning the inner secret name.
/// Returns None if the name doesn't belong to the given vault.
pub fn strip_prefix(vault: &str, full_name: &str) -> Option<String> {
    let prefix = format!("{vault}/");
    full_name.strip_prefix(&prefix).map(|s| s.to_string())
}

/// Returns the AWS name of the vault marker secret.
pub fn marker_name(vault: &str) -> String {
    format!("{vault}/{MARKER_BASENAME}")
}

/// Returns true if `full_name` is a vault marker secret name.
pub fn is_marker(full_name: &str) -> bool {
    full_name.ends_with(&format!("/{MARKER_BASENAME}"))
        || full_name == MARKER_BASENAME
}

/// Validate a user-facing secret name. Rejects empty names and names
/// starting with `.xv-` (reserved namespace).
pub fn validate_secret_name(name: &str) -> Result<(), BackendError> {
    if name.is_empty() {
        return Err(BackendError::InvalidArgument(
            "secret name cannot be empty".into(),
        ));
    }
    if name.starts_with(".xv-") {
        return Err(BackendError::InvalidArgument(format!(
            "secret name '{name}' is in the reserved '.xv-*' namespace"
        )));
    }
    Ok(())
}
```

- [ ] **Step 3: Verify**

Run: `cargo test --features aws --lib backend::aws::encoding::tests`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/backend/aws/encoding.rs
git commit -m "feat(aws): add name encoding + marker reservation + reserved-name check"
```

---

## Task 10: Implement `metadata.rs` — tag ↔ SecretProperties round-trip

**Files:**
- Modify: `src/backend/aws/metadata.rs`

- [ ] **Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn round_trip_full_metadata() {
        let mut user_tags = HashMap::new();
        user_tags.insert("team".to_string(), "platform".to_string());

        let props = TestProps {
            original_name: "db-password".into(),
            groups: vec!["backend".into(), "prod".into()],
            folder: Some("app/database".into()),
            created_by: Some("alice@example.com".into()),
            content_type: Some("text/plain".into()),
            note: Some("primary database admin password".into()),
            expires_at: Some("2027-01-01T00:00:00Z".into()),
            user_tags: user_tags.clone(),
        };

        let aws_tags = encode_tags(&props);
        let decoded = decode_tags(&aws_tags);

        assert_eq!(decoded.original_name, "db-password");
        assert_eq!(decoded.groups, vec!["backend", "prod"]);
        assert_eq!(decoded.folder.as_deref(), Some("app/database"));
        assert_eq!(decoded.user_tags.get("team").unwrap(), "platform");
    }

    #[test]
    fn empty_metadata_round_trips_to_empty_tags() {
        let props = TestProps::empty("name1");
        let aws_tags = encode_tags(&props);
        // Only original_name and one xv: tag entry expected
        assert!(aws_tags.iter().any(|(k, _)| k == "xv:original_name"));
    }
}
```

- [ ] **Step 2: Implement**

```rust
//! Tag <-> SecretProperties round-trip for AWS backend.
//!
//! AWS Secrets Manager allows up to 50 tags per secret (vs Azure's 15);
//! comfortable budget. Reserved keys live under the `xv:` prefix.

use std::collections::HashMap;

pub const TAG_ORIGINAL_NAME: &str = "xv:original_name";
pub const TAG_GROUPS: &str = "xv:groups";
pub const TAG_FOLDER: &str = "xv:folder";
pub const TAG_CREATED_BY: &str = "xv:created_by";
pub const TAG_CONTENT_TYPE: &str = "xv:content_type";
pub const TAG_EXPIRES_AT: &str = "xv:expires_at";
pub const TAG_TYPE: &str = "xv:type";
pub const TAG_VALUE_VAULT_MARKER: &str = "vault-marker";
pub const TAG_MIGRATED_FROM: &str = "xv:migrated_from";
pub const TAG_MIGRATED_AT: &str = "xv:migrated_at";

/// Subset of `SecretProperties` fields we actually round-trip.
/// Real `SecretProperties` from `crate::secret::models` is bigger; we map
/// to/from it at the call site.
#[derive(Debug, Default, Clone)]
pub struct TestProps {
    pub original_name: String,
    pub groups: Vec<String>,
    pub folder: Option<String>,
    pub created_by: Option<String>,
    pub content_type: Option<String>,
    pub note: Option<String>,            // -> AWS Description, not a tag
    pub expires_at: Option<String>,
    pub user_tags: HashMap<String, String>,
}

impl TestProps {
    pub fn empty(name: &str) -> Self {
        Self {
            original_name: name.into(),
            ..Default::default()
        }
    }
}

/// Encode metadata into AWS-tag-shaped `(key, value)` pairs.
/// Note: `note` is intentionally NOT encoded — it lives in AWS Description.
pub fn encode_tags(p: &TestProps) -> Vec<(String, String)> {
    let mut tags: Vec<(String, String)> = Vec::new();
    tags.push((TAG_ORIGINAL_NAME.into(), p.original_name.clone()));
    if !p.groups.is_empty() {
        tags.push((TAG_GROUPS.into(), p.groups.join(",")));
    }
    if let Some(ref f) = p.folder {
        tags.push((TAG_FOLDER.into(), f.clone()));
    }
    if let Some(ref c) = p.created_by {
        tags.push((TAG_CREATED_BY.into(), c.clone()));
    }
    if let Some(ref ct) = p.content_type {
        tags.push((TAG_CONTENT_TYPE.into(), ct.clone()));
    }
    if let Some(ref e) = p.expires_at {
        tags.push((TAG_EXPIRES_AT.into(), e.clone()));
    }
    for (k, v) in &p.user_tags {
        if !k.starts_with("xv:") {
            tags.push((k.clone(), v.clone()));
        }
    }
    tags
}

/// Decode AWS tags back into the metadata struct.
pub fn decode_tags(tags: &[(String, String)]) -> TestProps {
    let mut p = TestProps::default();
    let mut user_tags = HashMap::new();
    for (k, v) in tags {
        match k.as_str() {
            TAG_ORIGINAL_NAME => p.original_name = v.clone(),
            TAG_GROUPS => {
                p.groups = v
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            }
            TAG_FOLDER => p.folder = Some(v.clone()),
            TAG_CREATED_BY => p.created_by = Some(v.clone()),
            TAG_CONTENT_TYPE => p.content_type = Some(v.clone()),
            TAG_EXPIRES_AT => p.expires_at = Some(v.clone()),
            _ if !k.starts_with("xv:") => {
                user_tags.insert(k.clone(), v.clone());
            }
            _ => {} // unknown xv: tag — ignored on decode
        }
    }
    p.user_tags = user_tags;
    p
}

/// True if this tag is a vault marker tag (`xv:type=vault-marker`).
pub fn is_vault_marker_tag(key: &str, value: &str) -> bool {
    key == TAG_TYPE && value == TAG_VALUE_VAULT_MARKER
}
```

> **Note:** `TestProps` is a simplification used for the round-trip test pattern. In subsequent tasks we'll convert directly between AWS tags and the existing `crate::secret::models::SecretProperties` (which has additional fields like `enabled`, `version_id`, etc.). The conversion patterns established here are reused.

- [ ] **Step 3: Verify**

Run: `cargo test --features aws --lib backend::aws::metadata::tests`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/backend/aws/metadata.rs
git commit -m "feat(aws): add tag <-> metadata round-trip"
```

---

## Task 11: Implement `AwsBackend` struct + `Backend` trait

**Files:**
- Modify: `src/backend/aws/mod.rs`

- [ ] **Step 1: Replace the stub with the impl**

```rust
//! AWS Secrets Manager backend.

pub mod auth;
pub mod config;
pub mod encoding;
pub mod errors;
pub mod metadata;
pub mod models;
pub mod secrets;
pub mod vaults;

use std::sync::Arc;

use crate::backend::{
    Backend, BackendCapabilities, BackendKind, NameCharset, SecretBackend, VaultBackend,
};
use crate::backend::error::BackendError;
use crate::config::settings::AwsConfig;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;

pub struct AwsBackend {
    secrets_impl: Arc<secrets::AwsSecretBackend>,
    vaults_impl: Arc<vaults::AwsVaultBackend>,
}

impl AwsBackend {
    /// Build a backend from config + per-invocation overrides.
    /// Async because `aws-config::load()` is async.
    pub async fn new(
        aws_cfg: &AwsConfig,
        region_override: Option<String>,
        profile_override: Option<String>,
    ) -> Result<Self, BackendError> {
        let client: SecretsManagerClient =
            auth::build_client(aws_cfg, region_override, profile_override).await?;
        let client = Arc::new(client);
        Ok(Self {
            secrets_impl: Arc::new(secrets::AwsSecretBackend::new(client.clone())),
            vaults_impl: Arc::new(vaults::AwsVaultBackend::new(client)),
        })
    }
}

#[async_trait::async_trait]
impl Backend for AwsBackend {
    fn name(&self) -> &'static str {
        "aws"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Aws
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            has_vaults: true,
            has_file_storage: false,
            has_rbac: false,
            has_audit: false,
            has_versioning: true,
            has_soft_delete: true,
            has_secret_rotation: false,
            has_groups: true,
            has_folders: true,
            has_notes: true,
            has_expiry: true,
            max_secret_size: Some(65_536),
            max_name_length: Some(encoding::MAX_NAME_LEN),
            name_charset: NameCharset::AwsRelaxed,
        }
    }

    fn secrets(&self) -> &dyn SecretBackend {
        self.secrets_impl.as_ref()
    }

    fn vaults(&self) -> Option<&dyn VaultBackend> {
        Some(self.vaults_impl.as_ref())
    }

    async fn health_check(&self) -> Result<(), BackendError> {
        // Calling list_secrets with limit=1 verifies credentials + connectivity
        // without requiring any specific resource to exist.
        self.secrets_impl.health_check().await
    }
}
```

- [ ] **Step 2: Add stubs for `AwsSecretBackend` and `AwsVaultBackend`**

In `src/backend/aws/secrets.rs`, replace the stub with:

```rust
//! `AwsSecretBackend` impl `SecretBackend`.

use crate::backend::SecretBackend;
use crate::backend::error::BackendError;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use std::sync::Arc;

pub struct AwsSecretBackend {
    client: Arc<SecretsManagerClient>,
}

impl AwsSecretBackend {
    pub fn new(client: Arc<SecretsManagerClient>) -> Self {
        Self { client }
    }

    /// Lightweight health check: list secrets with limit=1.
    pub async fn health_check(&self) -> Result<(), BackendError> {
        self.client
            .list_secrets()
            .max_results(1)
            .send()
            .await
            .map_err(super::errors::from_list)?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl SecretBackend for AwsSecretBackend {
    // All trait methods filled in across Tasks 13-22. For now, leave them as
    // returning Unsupported so the trait-impl scaffold compiles.
}
```

In `src/backend/aws/vaults.rs`:

```rust
//! `AwsVaultBackend` impl `VaultBackend`.

use crate::backend::VaultBackend;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use std::sync::Arc;

pub struct AwsVaultBackend {
    client: Arc<SecretsManagerClient>,
}

impl AwsVaultBackend {
    pub fn new(client: Arc<SecretsManagerClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl VaultBackend for AwsVaultBackend {
    // Methods filled in across Tasks 24-28.
}
```

> **Note:** The empty trait-impl bodies will cause compile errors if the trait has required methods. Check `src/backend/secret.rs` and `src/backend/vault.rs` (or wherever the traits are defined) — for any required method not yet implemented here, add a stub returning `Err(BackendError::Unsupported("not yet implemented".into()))` to keep the compile clean. Subsequent tasks replace each stub with the real impl.

- [ ] **Step 3: Identify required trait methods**

Run: `grep -n "async fn" src/backend/secret.rs src/backend/vault.rs`

For each method without a default impl, add a stub in `secrets.rs` / `vaults.rs`:

```rust
async fn METHOD_NAME(&self, /* args */) -> Result<RETURN_TYPE, BackendError> {
    Err(BackendError::Unsupported("aws backend: not yet implemented".into()))
}
```

- [ ] **Step 4: Verify build**

Run: `cargo build --features aws`
Expected: clean compile.

- [ ] **Step 5: Commit**

```bash
git add src/backend/aws/
git commit -m "feat(aws): scaffold AwsBackend with capabilities + Backend trait impl"
```

---

## Task 12: Wire `BackendKind::Aws` into `BackendRegistry::from_config`

**Files:**
- Modify: `src/backend/registry.rs:48-67`

- [ ] **Step 1: Add the AWS arm**

Locate `pub fn from_config` in `src/backend/registry.rs`. Replace the `match kind` block:

```rust
pub fn from_config(config: &Config) -> Result<Self, BackendError> {
    let backend_name = config.effective_backend_name();

    // Resolve named-backend entry first if applicable
    if let Some(entry) = config.named_backends.get(backend_name) {
        return Self::from_named_entry(backend_name, entry);
    }

    let kind: BackendKind = backend_name
        .parse()
        .map_err(|e: String| BackendError::Internal(e))?;

    match kind {
        BackendKind::Azure => {
            let auth_provider = Self::create_azure_auth_provider(config)?;
            let backend = super::azure::AzureBackend::new(config, auth_provider.clone())?;
            let mut registry = Self::new(Arc::new(backend));
            registry.azure_auth = Some(auth_provider);
            Ok(registry)
        }
        BackendKind::Local => {
            let backend = super::local::LocalBackend::new(config.local.as_ref())?;
            Ok(Self::new(Arc::new(backend)))
        }
        #[cfg(feature = "aws")]
        BackendKind::Aws => {
            let aws_cfg = config.aws.as_ref().ok_or_else(|| {
                BackendError::Internal(
                    "[aws] config block missing — set backend = \"aws\" with [aws] block".into(),
                )
            })?;
            // We need an async runtime. The registry is sync; use a runtime handle.
            let backend = tokio::runtime::Handle::current()
                .block_on(super::aws::AwsBackend::new(aws_cfg, None, None))?;
            Ok(Self::new(Arc::new(backend)))
        }
        #[cfg(not(feature = "aws"))]
        BackendKind::Aws => Err(BackendError::Internal(
            "AWS backend not compiled in: rebuild with --features aws".into(),
        )),
    }
}

fn from_named_entry(
    name: &str,
    entry: &crate::config::settings::NamedBackendEntry,
) -> Result<Self, BackendError> {
    use crate::config::settings::NamedBackendEntry as NBE;
    match entry {
        #[cfg(feature = "aws")]
        NBE::Aws(aws_cfg) => {
            let backend = tokio::runtime::Handle::current()
                .block_on(super::aws::AwsBackend::new(aws_cfg, None, None))?;
            Ok(Self::new(Arc::new(backend)))
        }
        #[cfg(not(feature = "aws"))]
        NBE::Aws(_) => Err(BackendError::Internal(format!(
            "named backend '{name}' is aws but binary built without --features aws"
        ))),
        NBE::Local(local_cfg) => {
            let backend = super::local::LocalBackend::new(Some(local_cfg))?;
            Ok(Self::new(Arc::new(backend)))
        }
    }
}
```

> **Note on `block_on`:** This will only work if `from_config` is called from within an async context (which `xv` does — `tokio::main` wraps everything). If called from a sync entrypoint, this panics. The existing test `from_config_local_creates_backend` is sync — review and adjust. If broken, gate the AWS test with `#[tokio::test]`.

- [ ] **Step 2: Add a smoke test (gated)**

In `src/backend/registry.rs` `mod tests`, add:

```rust
#[cfg(feature = "aws")]
#[tokio::test]
async fn from_config_aws_requires_aws_block() {
    let config = Config {
        backend: Some("aws".to_string()),
        aws: None,
        ..Default::default()
    };
    let result = BackendRegistry::from_config(&config);
    assert!(result.is_err());
    let err_str = result.unwrap_err().to_string();
    assert!(err_str.contains("[aws]") || err_str.contains("aws"), "got: {err_str}");
}
```

- [ ] **Step 3: Verify**

Run: `cargo build --features aws`
Expected: clean.

Run: `cargo test --features aws --lib backend::registry::tests::from_config_aws_requires_aws_block`
Expected: PASS.

Run: `cargo test` (default features)
Expected: PASS — no regressions in existing Azure/Local paths.

- [ ] **Step 4: Commit**

```bash
git add src/backend/registry.rs
git commit -m "feat(aws): wire BackendKind::Aws + named-backend resolution into registry"
```

---

## Phase 3: SecretBackend trait — core CRUD

> **Test pattern note for Phase 3+:** Each Task in this phase pairs unit-style tests using `aws-smithy-mocks-experimental` (Task 13 sets up the harness) with hermetic mocked responses. After Task 13, subsequent tasks reuse the same harness module by importing from `tests/aws_backend_tests.rs`. LocalStack-backed integration tests are gated with `AWS_INTEGRATION_TESTS=1` and added in Task 31.

## Task 13: Set up the mock test harness

**Files:**
- Create: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Create the harness with a smoke test**

```rust
//! Hermetic AWS backend tests using aws-smithy-mocks-experimental.
//!
//! Each test builds a mock SecretsManager client by stubbing per-operation
//! responses, then exercises the `AwsSecretBackend` / `AwsVaultBackend`
//! against it. No AWS credentials, no network — fully deterministic.

#![cfg(feature = "aws")]

use aws_sdk_secretsmanager::Client;
use aws_smithy_mocks_experimental::{mock, mock_client, RuleMode};
use crosstache::backend::aws::{secrets::AwsSecretBackend, vaults::AwsVaultBackend, AwsBackend};
use std::sync::Arc;

/// Build a mock client from a list of operation rules.
pub fn mock_client_from_rules(rules: Vec<aws_smithy_mocks_experimental::Rule>) -> Client {
    mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &rules)
}

/// Build an `AwsSecretBackend` directly around a mock client.
pub fn aws_secret_backend(client: Client) -> AwsSecretBackend {
    AwsSecretBackend::new(Arc::new(client))
}

/// Build an `AwsVaultBackend` directly around a mock client.
pub fn aws_vault_backend(client: Client) -> AwsVaultBackend {
    AwsVaultBackend::new(Arc::new(client))
}

#[tokio::test]
async fn smoke_health_check_with_empty_list() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;

    let rule = mock!(Client::list_secrets)
        .then_output(|| ListSecretsOutput::builder().build());

    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);
    backend.health_check().await.expect("health check should pass");
}
```

> **Note:** `crosstache` must be exposed as a library for `crosstache::backend::aws::...` to resolve. If `src/lib.rs` does not exist, create it as a thin re-export of `src/main.rs`'s public modules (or add `lib.rs` to `Cargo.toml`'s `[lib]`). Most existing tests (`tests/local_backend_integration.rs` etc.) already use this pattern; mirror what they do.

- [ ] **Step 2: Verify**

Run: `cargo test --features aws --test aws_backend_tests smoke_health_check_with_empty_list`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/aws_backend_tests.rs
git commit -m "test(aws): add mock harness + smoke health check"
```

---

## Task 14: Implement `set_secret` (create path) + tests

**Files:**
- Modify: `src/backend/aws/secrets.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Inspect the SecretBackend trait method signatures**

Run: `grep -n "fn set_secret\|fn get_secret\|fn list_secrets\|SecretRequest\|SecretProperties" src/backend/secret.rs src/secret/models.rs | head -40`

This is the canonical source for argument/return types. Take note of `SecretRequest`, `SecretProperties`, `SecretSummary` shapes.

- [ ] **Step 2: Implement `set_secret`'s create branch**

In `src/backend/aws/secrets.rs`, replace the `set_secret` stub with:

```rust
async fn set_secret(
    &self,
    vault: &str,
    request: SecretRequest,
) -> Result<SecretProperties, BackendError> {
    use crate::backend::aws::encoding::{aws_name, validate_secret_name};
    use crate::backend::aws::metadata::{
        TAG_CONTENT_TYPE, TAG_CREATED_BY, TAG_EXPIRES_AT, TAG_FOLDER, TAG_GROUPS,
        TAG_ORIGINAL_NAME,
    };
    use aws_sdk_secretsmanager::types::Tag;

    validate_secret_name(&request.name)?;
    let aws_full_name = aws_name(vault, &request.name);

    // Build tag list
    let mut tags: Vec<Tag> = Vec::new();
    tags.push(
        Tag::builder()
            .key(TAG_ORIGINAL_NAME)
            .value(&request.name)
            .build(),
    );
    if !request.groups.is_empty() {
        tags.push(
            Tag::builder()
                .key(TAG_GROUPS)
                .value(request.groups.join(","))
                .build(),
        );
    }
    if let Some(ref f) = request.folder {
        tags.push(Tag::builder().key(TAG_FOLDER).value(f).build());
    }
    if let Some(ref c) = request.created_by {
        tags.push(Tag::builder().key(TAG_CREATED_BY).value(c).build());
    }
    if let Some(ref ct) = request.content_type {
        tags.push(Tag::builder().key(TAG_CONTENT_TYPE).value(ct).build());
    }
    if let Some(ref e) = request.expires_on {
        tags.push(
            Tag::builder()
                .key(TAG_EXPIRES_AT)
                .value(e.to_rfc3339())
                .build(),
        );
    }
    for (k, v) in &request.tags {
        if !k.starts_with("xv:") {
            tags.push(Tag::builder().key(k).value(v).build());
        }
    }

    // Try create first; fall through to update if AWS reports "already exists"
    let create_result = self
        .client
        .create_secret()
        .name(&aws_full_name)
        .secret_string(request.value.expose_secret().to_string())
        .description(request.note.clone().unwrap_or_default())
        .set_tags(if tags.is_empty() { None } else { Some(tags.clone()) })
        .send()
        .await;

    let version_id = match create_result {
        Ok(out) => out.version_id().unwrap_or("").to_string(),
        Err(e) => match super::errors::from_create(e) {
            BackendError::Conflict(_) => {
                // Already exists -> update instead. Implemented in Task 19.
                return self.update_existing_secret(vault, &request, &aws_full_name).await;
            }
            other => return Err(other),
        },
    };

    Ok(SecretProperties {
        name: request.name.clone(),
        value: None,
        version: version_id,
        ..Default::default()
    })
}
```

> The exact `SecretProperties` field set differs from the snippet above — adjust based on what `grep` revealed in Step 1. The pattern is: copy `name` and `version`, set `value: None`, defaults for the rest. The full re-fetch via `get_secret` after create is intentional in some backends; we choose to skip the round-trip here (the caller can re-fetch if needed).

- [ ] **Step 3: Add a stub for `update_existing_secret`**

Below the `set_secret` impl, add:

```rust
async fn update_existing_secret(
    &self,
    _vault: &str,
    _request: &SecretRequest,
    _aws_full_name: &str,
) -> Result<SecretProperties, BackendError> {
    // Implemented in Task 19.
    Err(BackendError::Unsupported(
        "aws update path: not yet implemented".into(),
    ))
}
```

- [ ] **Step 4: Add the test**

In `tests/aws_backend_tests.rs`:

```rust
#[tokio::test]
async fn set_secret_create_writes_to_aws() {
    use aws_sdk_secretsmanager::operation::create_secret::CreateSecretOutput;
    use crosstache::secret::manager::SecretRequest;
    use crosstache::backend::SecretBackend;
    use zeroize::Zeroizing;

    let rule = mock!(Client::create_secret)
        .match_requests(|req| req.name() == Some("myproj-kv/db-password"))
        .then_output(|| {
            CreateSecretOutput::builder()
                .name("myproj-kv/db-password")
                .arn("arn:aws:secretsmanager:us-east-1:123:secret:myproj-kv/db-password-abc")
                .version_id("v1")
                .build()
        });

    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);

    let request = SecretRequest {
        name: "db-password".into(),
        value: Zeroizing::new("hunter2".into()),
        groups: vec!["backend".into()],
        ..Default::default()
    };

    let result = backend.set_secret("myproj-kv", request).await
        .expect("set_secret should succeed");
    assert_eq!(result.name, "db-password");
    assert_eq!(result.version, "v1");
}
```

- [ ] **Step 5: Verify**

Run: `cargo test --features aws --test aws_backend_tests set_secret_create_writes_to_aws`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/backend/aws/secrets.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): set_secret create path + mock test"
```

---

## Task 15: Implement `get_secret` (no value) + tests

**Files:**
- Modify: `src/backend/aws/secrets.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement**

Replace the `get_secret` stub:

```rust
async fn get_secret(
    &self,
    vault: &str,
    name: &str,
    include_value: bool,
) -> Result<SecretProperties, BackendError> {
    use crate::backend::aws::encoding::aws_name;
    let aws_full_name = aws_name(vault, name);

    if !include_value {
        let describe = self
            .client
            .describe_secret()
            .secret_id(&aws_full_name)
            .send()
            .await
            .map_err(|e| super::errors::from_describe(name, e))?;

        return Ok(self.props_from_describe(&describe, name));
    }

    // include_value: parallelize describe + get_secret_value
    let describe_fut = self
        .client
        .describe_secret()
        .secret_id(&aws_full_name)
        .send();
    let value_fut = self
        .client
        .get_secret_value()
        .secret_id(&aws_full_name)
        .send();

    let (describe, value) = tokio::join!(describe_fut, value_fut);
    let describe = describe.map_err(|e| super::errors::from_describe(name, e))?;
    let value = value.map_err(|e| super::errors::from_get_value(name, e))?;

    let mut props = self.props_from_describe(&describe, name);
    props.value = value
        .secret_string()
        .map(|s| Zeroizing::new(s.to_string()));
    Ok(props)
}
```

- [ ] **Step 2: Add `props_from_describe` helper**

```rust
fn props_from_describe(
    &self,
    describe: &aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput,
    fallback_name: &str,
) -> SecretProperties {
    use crate::backend::aws::metadata::*;

    let mut props = SecretProperties {
        name: fallback_name.to_string(),
        value: None,
        ..Default::default()
    };

    if let Some(tags) = describe.tags() {
        for tag in tags {
            let k = tag.key().unwrap_or("");
            let v = tag.value().unwrap_or("");
            match k {
                TAG_ORIGINAL_NAME => props.name = v.to_string(),
                TAG_GROUPS => {
                    props.groups = v
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect()
                }
                TAG_FOLDER => props.folder = Some(v.to_string()),
                TAG_CREATED_BY => props.created_by = Some(v.to_string()),
                TAG_CONTENT_TYPE => props.content_type = Some(v.to_string()),
                TAG_EXPIRES_AT => {
                    props.expires_on = chrono::DateTime::parse_from_rfc3339(v)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc));
                }
                _ if !k.starts_with("xv:") => {
                    props.tags.insert(k.to_string(), v.to_string());
                }
                _ => {}
            }
        }
    }
    if let Some(desc) = describe.description() {
        if !desc.is_empty() {
            props.note = Some(desc.to_string());
        }
    }
    props
}
```

> Adjust field names against the actual `SecretProperties` definition (Task 14 Step 1 told you the shape).

- [ ] **Step 3: Add tests**

```rust
#[tokio::test]
async fn get_secret_no_value_returns_metadata_only() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::types::Tag;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::describe_secret)
        .then_output(|| {
            DescribeSecretOutput::builder()
                .name("myproj-kv/db-password")
                .arn("arn:aws:secretsmanager:us-east-1:123:secret:myproj-kv/db-password-abc")
                .description("primary db admin password")
                .tags(Tag::builder().key("xv:original_name").value("db-password").build())
                .tags(Tag::builder().key("xv:groups").value("backend,prod").build())
                .build()
        });

    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);

    let props = backend.get_secret("myproj-kv", "db-password", false).await.unwrap();
    assert_eq!(props.name, "db-password");
    assert_eq!(props.note.as_deref(), Some("primary db admin password"));
    assert_eq!(props.groups, vec!["backend", "prod"]);
    assert!(props.value.is_none());
}

#[tokio::test]
async fn get_secret_with_value_includes_value() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::get_secret_value::GetSecretValueOutput;
    use crosstache::backend::SecretBackend;

    let describe = mock!(Client::describe_secret).then_output(|| {
        DescribeSecretOutput::builder()
            .name("myproj-kv/db-password")
            .build()
    });
    let value = mock!(Client::get_secret_value).then_output(|| {
        GetSecretValueOutput::builder()
            .name("myproj-kv/db-password")
            .secret_string("hunter2")
            .version_id("v1")
            .build()
    });

    let client = mock_client_from_rules(vec![describe, value]);
    let backend = aws_secret_backend(client);

    let props = backend.get_secret("myproj-kv", "db-password", true).await.unwrap();
    assert!(props.value.is_some());
    assert_eq!(props.value.as_ref().map(|v| v.as_str()), Some("hunter2"));
}

#[tokio::test]
async fn get_secret_not_found_maps_to_backend_not_found() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretError;
    use aws_sdk_secretsmanager::types::error::ResourceNotFoundException;
    use crosstache::backend::SecretBackend;
    use crosstache::backend::error::BackendError;

    let rule = mock!(Client::describe_secret).then_error(|| {
        DescribeSecretError::ResourceNotFoundException(
            ResourceNotFoundException::builder()
                .message("Secrets Manager can't find the specified secret.")
                .build()
        )
    });

    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);

    let err = backend.get_secret("myproj-kv", "missing", false).await.unwrap_err();
    assert!(matches!(err, BackendError::NotFound { .. }), "got: {err:?}");
}
```

- [ ] **Step 4: Verify**

Run: `cargo test --features aws --test aws_backend_tests get_secret_`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/aws/secrets.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): get_secret with/without value + not-found mapping"
```

---

## Task 16: Implement `list_secrets` (paginated, prefix filter, marker exclusion) + tests

**Files:**
- Modify: `src/backend/aws/secrets.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement**

```rust
async fn list_secrets(
    &self,
    vault: &str,
    group_filter: Option<&str>,
) -> Result<Vec<SecretSummary>, BackendError> {
    use crate::backend::aws::encoding::{is_marker, strip_prefix};
    use crate::backend::aws::metadata::TAG_GROUPS;
    use aws_sdk_secretsmanager::types::{Filter, FilterNameStringType};

    let prefix = format!("{vault}/");
    let mut next_token: Option<String> = None;
    let mut summaries: Vec<SecretSummary> = Vec::new();

    loop {
        let mut req = self
            .client
            .list_secrets()
            .max_results(100)
            .filters(
                Filter::builder()
                    .key(FilterNameStringType::Name)
                    .values(prefix.clone())
                    .build(),
            );
        if let Some(t) = &next_token {
            req = req.next_token(t.clone());
        }

        let out = req.send().await.map_err(super::errors::from_list)?;

        for entry in out.secret_list.unwrap_or_default() {
            let aws_full_name = entry.name().unwrap_or("");
            // Defensive: AWS prefix filter is broad-match; verify exact prefix
            let secret_name = match strip_prefix(vault, aws_full_name) {
                Some(n) => n,
                None => continue,
            };
            // Skip vault marker
            if is_marker(aws_full_name) {
                continue;
            }
            // Group filter (client-side from tags)
            if let Some(group_want) = group_filter {
                let groups: Vec<String> = entry
                    .tags()
                    .unwrap_or_default()
                    .iter()
                    .find(|t| t.key() == Some(TAG_GROUPS))
                    .and_then(|t| t.value())
                    .map(|v| v.split(',').map(|s| s.to_string()).collect())
                    .unwrap_or_default();
                if !groups.iter().any(|g| g == group_want) {
                    continue;
                }
            }

            summaries.push(SecretSummary {
                name: secret_name,
                ..Default::default()
            });
        }

        next_token = out.next_token().map(|s| s.to_string());
        if next_token.is_none() {
            break;
        }
    }

    Ok(summaries)
}
```

> Field set on `SecretSummary` may differ; adjust against the actual definition.

- [ ] **Step 2: Add tests**

```rust
#[tokio::test]
async fn list_secrets_paginates_and_filters_marker() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::SecretBackend;

    let page1 = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("myproj-kv/.xv-vault").build())
            .secret_list(SecretListEntry::builder().name("myproj-kv/db-password").build())
            .next_token("tok1")
            .build()
    });
    let page2 = mock!(Client::list_secrets)
        .match_requests(|req| req.next_token() == Some("tok1"))
        .then_output(|| {
            ListSecretsOutput::builder()
                .secret_list(SecretListEntry::builder().name("myproj-kv/api-key").build())
                .build()
        });

    let client = mock_client_from_rules(vec![page1, page2]);
    let backend = aws_secret_backend(client);

    let secrets = backend.list_secrets("myproj-kv", None).await.unwrap();
    let names: Vec<String> = secrets.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"db-password".to_string()));
    assert!(names.contains(&"api-key".to_string()));
    assert!(!names.contains(&".xv-vault".to_string()), "marker should be excluded");
}
```

- [ ] **Step 3: Verify**

Run: `cargo test --features aws --test aws_backend_tests list_secrets_paginates_and_filters_marker`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/backend/aws/secrets.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): list_secrets pagination + prefix filter + marker exclusion"
```

---

## Task 17: Implement `delete_secret` (soft-delete) + tests

**Files:**
- Modify: `src/backend/aws/secrets.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement**

```rust
async fn delete_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
    use crate::backend::aws::encoding::aws_name;
    let aws_full_name = aws_name(vault, name);
    self.client
        .delete_secret()
        .secret_id(&aws_full_name)
        .recovery_window_in_days(30)
        .send()
        .await
        .map_err(|e| super::errors::from_delete(name, e))?;
    Ok(())
}
```

- [ ] **Step 2: Implement `purge_secret`**

```rust
async fn purge_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
    use crate::backend::aws::encoding::aws_name;
    let aws_full_name = aws_name(vault, name);
    self.client
        .delete_secret()
        .secret_id(&aws_full_name)
        .force_delete_without_recovery(true)
        .send()
        .await
        .map_err(|e| super::errors::from_delete(name, e))?;
    Ok(())
}
```

- [ ] **Step 3: Add tests**

```rust
#[tokio::test]
async fn delete_secret_uses_recovery_window() {
    use aws_sdk_secretsmanager::operation::delete_secret::DeleteSecretOutput;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::delete_secret)
        .match_requests(|req| {
            req.secret_id() == Some("myproj-kv/db-password")
                && req.recovery_window_in_days() == Some(30)
                && req.force_delete_without_recovery() != Some(true)
        })
        .then_output(|| DeleteSecretOutput::builder().build());

    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);
    backend.delete_secret("myproj-kv", "db-password").await.unwrap();
}

#[tokio::test]
async fn purge_secret_forces_immediate_delete() {
    use aws_sdk_secretsmanager::operation::delete_secret::DeleteSecretOutput;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::delete_secret)
        .match_requests(|req| req.force_delete_without_recovery() == Some(true))
        .then_output(|| DeleteSecretOutput::builder().build());

    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);
    backend.purge_secret("myproj-kv", "db-password").await.unwrap();
}
```

- [ ] **Step 4: Verify**

Run: `cargo test --features aws --test aws_backend_tests delete_secret_uses_recovery_window purge_secret_forces_immediate_delete`
Expected: 2 PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/aws/secrets.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): delete_secret with 30-day recovery, purge with force flag"
```

---

## Task 18: Implement `secret_exists` + tests

**Files:**
- Modify: `src/backend/aws/secrets.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement**

The default implementation in the trait calls `get_secret` and translates `NotFound` to `Ok(false)`. Override it on AWS to use `DescribeSecret` directly (cheaper):

```rust
async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
    use crate::backend::aws::encoding::aws_name;
    let aws_full_name = aws_name(vault, name);
    match self
        .client
        .describe_secret()
        .secret_id(&aws_full_name)
        .send()
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => match super::errors::from_describe(name, e) {
            BackendError::NotFound { .. } => Ok(false),
            other => Err(other),
        },
    }
}
```

- [ ] **Step 2: Add tests**

```rust
#[tokio::test]
async fn secret_exists_true_when_describe_succeeds() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::describe_secret)
        .then_output(|| DescribeSecretOutput::builder().name("myproj-kv/db").build());
    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);

    assert!(backend.secret_exists("myproj-kv", "db").await.unwrap());
}

#[tokio::test]
async fn secret_exists_false_on_not_found() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretError;
    use aws_sdk_secretsmanager::types::error::ResourceNotFoundException;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::describe_secret).then_error(|| {
        DescribeSecretError::ResourceNotFoundException(
            ResourceNotFoundException::builder().message("not found").build(),
        )
    });
    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);

    assert!(!backend.secret_exists("myproj-kv", "missing").await.unwrap());
}
```

- [ ] **Step 3: Verify**

Run: `cargo test --features aws --test aws_backend_tests secret_exists`
Expected: 2 PASS.

- [ ] **Step 4: Commit**

```bash
git add src/backend/aws/secrets.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): secret_exists override using DescribeSecret"
```

---

## Phase 4: SecretBackend trait — update, versions, recovery

## Task 19: Implement `set_secret` (update path) + `update_secret` (metadata) + tests

**Files:**
- Modify: `src/backend/aws/secrets.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement `update_existing_secret`**

Replace the stub created in Task 14:

```rust
async fn update_existing_secret(
    &self,
    vault: &str,
    request: &SecretRequest,
    aws_full_name: &str,
) -> Result<SecretProperties, BackendError> {
    use crate::backend::aws::metadata::*;
    use aws_sdk_secretsmanager::types::Tag;

    // Step 1: Put the new value as a new version
    let put_out = self
        .client
        .put_secret_value()
        .secret_id(aws_full_name)
        .secret_string(request.value.expose_secret().to_string())
        .send()
        .await
        .map_err(|e| super::errors::from_put_value(&request.name, e))?;

    // Step 2: Update description if provided
    if let Some(ref note) = request.note {
        self.client
            .update_secret()
            .secret_id(aws_full_name)
            .description(note)
            .send()
            .await
            .map_err(|e| super::errors::from_update(&request.name, e))?;
    }

    // Step 3: Update tags (replace strategy: untag everything, then re-tag)
    let describe = self
        .client
        .describe_secret()
        .secret_id(aws_full_name)
        .send()
        .await
        .map_err(|e| super::errors::from_describe(&request.name, e))?;

    let existing_keys: Vec<String> = describe
        .tags()
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t.key().map(|k| k.to_string()))
        .collect();
    if !existing_keys.is_empty() {
        self.client
            .untag_resource()
            .secret_id(aws_full_name)
            .set_tag_keys(Some(existing_keys))
            .send()
            .await
            .map_err(super::errors::from_untag)?;
    }

    let mut new_tags: Vec<Tag> = Vec::new();
    new_tags.push(
        Tag::builder()
            .key(TAG_ORIGINAL_NAME)
            .value(&request.name)
            .build(),
    );
    if !request.groups.is_empty() {
        new_tags.push(
            Tag::builder()
                .key(TAG_GROUPS)
                .value(request.groups.join(","))
                .build(),
        );
    }
    if let Some(ref f) = request.folder {
        new_tags.push(Tag::builder().key(TAG_FOLDER).value(f).build());
    }
    if let Some(ref c) = request.created_by {
        new_tags.push(Tag::builder().key(TAG_CREATED_BY).value(c).build());
    }
    if let Some(ref ct) = request.content_type {
        new_tags.push(Tag::builder().key(TAG_CONTENT_TYPE).value(ct).build());
    }
    if let Some(ref e) = request.expires_on {
        new_tags.push(
            Tag::builder()
                .key(TAG_EXPIRES_AT)
                .value(e.to_rfc3339())
                .build(),
        );
    }
    for (k, v) in &request.tags {
        if !k.starts_with("xv:") {
            new_tags.push(Tag::builder().key(k).value(v).build());
        }
    }
    self.client
        .tag_resource()
        .secret_id(aws_full_name)
        .set_tags(Some(new_tags))
        .send()
        .await
        .map_err(super::errors::from_tag)?;

    Ok(SecretProperties {
        name: request.name.clone(),
        value: None,
        version: put_out.version_id().unwrap_or("").to_string(),
        ..Default::default()
    })
}
```

- [ ] **Step 2: Implement `update_secret` (metadata-only)**

```rust
async fn update_secret(
    &self,
    vault: &str,
    name: &str,
    request: SecretUpdateRequest,
) -> Result<SecretProperties, BackendError> {
    use crate::backend::aws::encoding::aws_name;
    use crate::backend::aws::metadata::*;
    use aws_sdk_secretsmanager::types::Tag;

    let aws_full_name = aws_name(vault, name);

    // Description (note)
    if let Some(ref new_note) = request.note {
        self.client
            .update_secret()
            .secret_id(&aws_full_name)
            .description(new_note)
            .send()
            .await
            .map_err(|e| super::errors::from_update(name, e))?;
    }

    // Build only the deltas for tags
    let mut tags_to_set: Vec<Tag> = Vec::new();
    let mut keys_to_remove: Vec<String> = Vec::new();

    if let Some(ref groups) = request.groups {
        if groups.is_empty() {
            keys_to_remove.push(TAG_GROUPS.into());
        } else {
            tags_to_set.push(Tag::builder().key(TAG_GROUPS).value(groups.join(",")).build());
        }
    }
    if let Some(ref f) = request.folder {
        if f.is_empty() {
            keys_to_remove.push(TAG_FOLDER.into());
        } else {
            tags_to_set.push(Tag::builder().key(TAG_FOLDER).value(f).build());
        }
    }
    // Generic user tags: caller signals "remove" via Some("") or absent.
    for (k, v) in &request.tags {
        if v.is_empty() {
            keys_to_remove.push(k.clone());
        } else if !k.starts_with("xv:") {
            tags_to_set.push(Tag::builder().key(k).value(v).build());
        }
    }

    if !keys_to_remove.is_empty() {
        self.client
            .untag_resource()
            .secret_id(&aws_full_name)
            .set_tag_keys(Some(keys_to_remove))
            .send()
            .await
            .map_err(super::errors::from_untag)?;
    }
    if !tags_to_set.is_empty() {
        self.client
            .tag_resource()
            .secret_id(&aws_full_name)
            .set_tags(Some(tags_to_set))
            .send()
            .await
            .map_err(super::errors::from_tag)?;
    }

    self.get_secret(vault, name, false).await
}
```

> Adjust `SecretUpdateRequest` field names against the actual definition (find with `grep -n "SecretUpdateRequest" src/secret/`).

- [ ] **Step 3: Add a test**

```rust
#[tokio::test]
async fn set_secret_update_path_when_already_exists() {
    use aws_sdk_secretsmanager::operation::create_secret::CreateSecretError;
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::put_secret_value::PutSecretValueOutput;
    use aws_sdk_secretsmanager::operation::tag_resource::TagResourceOutput;
    use aws_sdk_secretsmanager::operation::update_secret::UpdateSecretOutput;
    use aws_sdk_secretsmanager::operation::untag_resource::UntagResourceOutput;
    use aws_sdk_secretsmanager::types::error::ResourceExistsException;
    use crosstache::secret::manager::SecretRequest;
    use crosstache::backend::SecretBackend;
    use zeroize::Zeroizing;

    let create_err = mock!(Client::create_secret).then_error(|| {
        CreateSecretError::ResourceExistsException(
            ResourceExistsException::builder().message("already exists").build(),
        )
    });
    let put_value = mock!(Client::put_secret_value)
        .then_output(|| PutSecretValueOutput::builder().version_id("v2").build());
    let update_secret =
        mock!(Client::update_secret).then_output(|| UpdateSecretOutput::builder().build());
    let describe = mock!(Client::describe_secret)
        .then_output(|| DescribeSecretOutput::builder().build());
    let untag = mock!(Client::untag_resource).then_output(|| UntagResourceOutput::builder().build());
    let tag = mock!(Client::tag_resource).then_output(|| TagResourceOutput::builder().build());

    let client = mock_client_from_rules(vec![create_err, put_value, update_secret, describe, untag, tag]);
    let backend = aws_secret_backend(client);

    let request = SecretRequest {
        name: "db-password".into(),
        value: Zeroizing::new("hunter3".into()),
        note: Some("rotated".into()),
        ..Default::default()
    };

    let result = backend.set_secret("myproj-kv", request).await.unwrap();
    assert_eq!(result.version, "v2");
}
```

- [ ] **Step 4: Verify**

Run: `cargo test --features aws --test aws_backend_tests set_secret_update_path_when_already_exists`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/aws/secrets.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): set_secret update path + update_secret (metadata only)"
```

---

## Task 20: Implement `list_versions` and `get_secret_version` + tests

**Files:**
- Modify: `src/backend/aws/secrets.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement `list_versions`**

```rust
async fn list_versions(
    &self,
    vault: &str,
    name: &str,
) -> Result<Vec<SecretProperties>, BackendError> {
    use crate::backend::aws::encoding::aws_name;
    let aws_full_name = aws_name(vault, name);
    let out = self
        .client
        .list_secret_version_ids()
        .secret_id(&aws_full_name)
        .include_deprecated(true)
        .send()
        .await
        .map_err(|e| super::errors::from_list_versions(name, e))?;

    let mut versions: Vec<SecretProperties> = Vec::new();
    for v in out.versions().unwrap_or_default() {
        let mut props = SecretProperties {
            name: name.to_string(),
            value: None,
            version: v.version_id().unwrap_or("").to_string(),
            ..Default::default()
        };
        if let Some(stages) = v.version_stages() {
            // Track AWSCURRENT / AWSPREVIOUS for caller display
            props.tags.insert(
                "aws:stages".into(),
                stages.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(","),
            );
        }
        if let Some(created) = v.created_date() {
            props.created_on = chrono::DateTime::from_timestamp(created.secs(), created.subsec_nanos())
                .map(|dt| dt.with_timezone(&chrono::Utc));
        }
        versions.push(props);
    }
    Ok(versions)
}
```

- [ ] **Step 2: Implement `get_secret_version`**

```rust
async fn get_secret_version(
    &self,
    vault: &str,
    name: &str,
    version: &str,
    include_value: bool,
) -> Result<SecretProperties, BackendError> {
    use crate::backend::aws::encoding::aws_name;
    let aws_full_name = aws_name(vault, name);

    if !include_value {
        // Versions don't carry tags directly; describe the secret head
        // and return version-specific fields from list_versions.
        let mut versions = self.list_versions(vault, name).await?;
        return versions
            .drain(..)
            .find(|p| p.version == version)
            .ok_or_else(|| BackendError::NotFound {
                name: format!("{name} (version {version})"),
                suggestion: None,
            });
    }

    let out = self
        .client
        .get_secret_value()
        .secret_id(&aws_full_name)
        .version_id(version)
        .send()
        .await
        .map_err(|e| super::errors::from_get_value(name, e))?;

    Ok(SecretProperties {
        name: name.to_string(),
        value: out.secret_string().map(|s| Zeroizing::new(s.to_string())),
        version: out.version_id().unwrap_or(version).to_string(),
        ..Default::default()
    })
}
```

- [ ] **Step 3: Add tests**

```rust
#[tokio::test]
async fn list_versions_returns_history() {
    use aws_sdk_secretsmanager::operation::list_secret_version_ids::ListSecretVersionIdsOutput;
    use aws_sdk_secretsmanager::types::SecretVersionsListEntry;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::list_secret_version_ids).then_output(|| {
        ListSecretVersionIdsOutput::builder()
            .versions(SecretVersionsListEntry::builder().version_id("v1").build())
            .versions(SecretVersionsListEntry::builder().version_id("v2").build())
            .build()
    });
    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);

    let versions = backend.list_versions("myproj-kv", "db-password").await.unwrap();
    let ids: Vec<String> = versions.iter().map(|v| v.version.clone()).collect();
    assert!(ids.contains(&"v1".to_string()));
    assert!(ids.contains(&"v2".to_string()));
}
```

- [ ] **Step 4: Verify**

Run: `cargo test --features aws --test aws_backend_tests list_versions_returns_history`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/aws/secrets.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): list_versions + get_secret_version with explicit version"
```

---

## Task 21: Implement `rollback` (UpdateSecretVersionStage) + tests

**Files:**
- Modify: `src/backend/aws/secrets.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement**

```rust
async fn rollback(
    &self,
    vault: &str,
    name: &str,
    version: &str,
) -> Result<SecretProperties, BackendError> {
    use crate::backend::aws::encoding::aws_name;
    let aws_full_name = aws_name(vault, name);

    // Find the version currently labeled AWSCURRENT
    let listed = self
        .client
        .list_secret_version_ids()
        .secret_id(&aws_full_name)
        .include_deprecated(true)
        .send()
        .await
        .map_err(|e| super::errors::from_list_versions(name, e))?;

    let current_version = listed
        .versions()
        .unwrap_or_default()
        .iter()
        .find(|v| {
            v.version_stages()
                .unwrap_or_default()
                .iter()
                .any(|s| s.as_str() == "AWSCURRENT")
        })
        .and_then(|v| v.version_id())
        .map(|s| s.to_string());

    // Move AWSCURRENT to target version
    self.client
        .update_secret_version_stage()
        .secret_id(&aws_full_name)
        .version_stage("AWSCURRENT")
        .move_to_version_id(version)
        .set_remove_from_version_id(current_version)
        .send()
        .await
        .map_err(|e| super::errors::from_update_stage(name, e))?;

    Ok(SecretProperties {
        name: name.to_string(),
        value: None,
        version: version.to_string(),
        ..Default::default()
    })
}
```

- [ ] **Step 2: Add a test**

```rust
#[tokio::test]
async fn rollback_moves_awscurrent_to_target_version() {
    use aws_sdk_secretsmanager::operation::list_secret_version_ids::ListSecretVersionIdsOutput;
    use aws_sdk_secretsmanager::operation::update_secret_version_stage::UpdateSecretVersionStageOutput;
    use aws_sdk_secretsmanager::types::SecretVersionsListEntry;
    use crosstache::backend::SecretBackend;

    let list = mock!(Client::list_secret_version_ids).then_output(|| {
        ListSecretVersionIdsOutput::builder()
            .versions(
                SecretVersionsListEntry::builder()
                    .version_id("v3")
                    .version_stages("AWSCURRENT".to_string())
                    .build(),
            )
            .versions(SecretVersionsListEntry::builder().version_id("v2").build())
            .build()
    });
    let update_stage = mock!(Client::update_secret_version_stage)
        .match_requests(|req| {
            req.move_to_version_id() == Some("v2")
                && req.remove_from_version_id() == Some("v3")
                && req.version_stage() == Some("AWSCURRENT")
        })
        .then_output(|| UpdateSecretVersionStageOutput::builder().build());

    let client = mock_client_from_rules(vec![list, update_stage]);
    let backend = aws_secret_backend(client);

    backend.rollback("myproj-kv", "db-password", "v2").await.unwrap();
}
```

- [ ] **Step 3: Verify**

Run: `cargo test --features aws --test aws_backend_tests rollback_moves_awscurrent_to_target_version`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/backend/aws/secrets.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): rollback via UpdateSecretVersionStage"
```

---

## Task 22: Implement `restore_secret` and `list_deleted_secrets` + tests

**Files:**
- Modify: `src/backend/aws/secrets.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement `restore_secret`**

```rust
async fn restore_secret(
    &self,
    vault: &str,
    name: &str,
) -> Result<SecretProperties, BackendError> {
    use crate::backend::aws::encoding::aws_name;
    let aws_full_name = aws_name(vault, name);
    self.client
        .restore_secret()
        .secret_id(&aws_full_name)
        .send()
        .await
        .map_err(|e| super::errors::from_restore(name, e))?;
    Ok(SecretProperties {
        name: name.to_string(),
        ..Default::default()
    })
}
```

- [ ] **Step 2: Implement `list_deleted_secrets`**

```rust
async fn list_deleted_secrets(
    &self,
    vault: &str,
) -> Result<Vec<SecretSummary>, BackendError> {
    use crate::backend::aws::encoding::{is_marker, strip_prefix};
    use aws_sdk_secretsmanager::types::{Filter, FilterNameStringType};

    let prefix = format!("{vault}/");
    let out = self
        .client
        .list_secrets()
        .max_results(100)
        .include_planned_deletion(true)
        .filters(
            Filter::builder()
                .key(FilterNameStringType::Name)
                .values(prefix.clone())
                .build(),
        )
        .send()
        .await
        .map_err(super::errors::from_list)?;

    let mut summaries: Vec<SecretSummary> = Vec::new();
    for entry in out.secret_list.unwrap_or_default() {
        let aws_full_name = entry.name().unwrap_or("");
        if entry.deleted_date().is_none() {
            continue;
        }
        let secret_name = match strip_prefix(vault, aws_full_name) {
            Some(n) => n,
            None => continue,
        };
        if is_marker(aws_full_name) {
            continue;
        }
        summaries.push(SecretSummary {
            name: secret_name,
            ..Default::default()
        });
    }
    Ok(summaries)
}
```

- [ ] **Step 3: Add tests**

```rust
#[tokio::test]
async fn restore_secret_calls_aws_restore() {
    use aws_sdk_secretsmanager::operation::restore_secret::RestoreSecretOutput;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::restore_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/db-password"))
        .then_output(|| RestoreSecretOutput::builder().build());

    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);

    let result = backend.restore_secret("myproj-kv", "db-password").await.unwrap();
    assert_eq!(result.name, "db-password");
}

#[tokio::test]
async fn list_deleted_secrets_filters_to_deleted_only() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::primitives::DateTime;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("myproj-kv/active").build())
            .secret_list(
                SecretListEntry::builder()
                    .name("myproj-kv/deleted-one")
                    .deleted_date(DateTime::from_secs(1_700_000_000))
                    .build(),
            )
            .build()
    });
    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_secret_backend(client);

    let deleted = backend.list_deleted_secrets("myproj-kv").await.unwrap();
    let names: Vec<String> = deleted.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, vec!["deleted-one".to_string()]);
}
```

- [ ] **Step 4: Verify**

Run: `cargo test --features aws --test aws_backend_tests restore_secret_calls_aws_restore list_deleted_secrets_filters_to_deleted_only`
Expected: 2 PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backend/aws/secrets.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): restore_secret + list_deleted_secrets"
```

---

## Phase 5: VaultBackend trait — marker-secret-based vaults

## Task 23: Implement `create_vault` (marker secret) + tests

**Files:**
- Modify: `src/backend/aws/vaults.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement**

Replace the `create_vault` stub:

```rust
async fn create_vault(
    &self,
    request: VaultCreateRequest,
) -> Result<VaultProperties, BackendError> {
    use crate::backend::aws::encoding::marker_name;
    use crate::backend::aws::metadata::{TAG_TYPE, TAG_VALUE_VAULT_MARKER};
    use aws_sdk_secretsmanager::types::Tag;
    use chrono::Utc;

    let marker = marker_name(&request.name);
    let mut tags: Vec<Tag> = Vec::new();
    tags.push(
        Tag::builder()
            .key(TAG_TYPE)
            .value(TAG_VALUE_VAULT_MARKER)
            .build(),
    );
    tags.push(
        Tag::builder()
            .key("xv:vault_name")
            .value(&request.name)
            .build(),
    );
    tags.push(
        Tag::builder()
            .key("xv:created_at")
            .value(Utc::now().to_rfc3339())
            .build(),
    );
    if let Some(ref user_tags) = request.tags {
        for (k, v) in user_tags {
            if !k.starts_with("xv:") {
                tags.push(Tag::builder().key(k).value(v).build());
            }
        }
    }

    self.client
        .create_secret()
        .name(&marker)
        .secret_string("{}")
        .description(format!("xv vault marker for '{}'", request.name))
        .set_tags(Some(tags))
        .send()
        .await
        .map_err(super::errors::from_create)?;

    Ok(VaultProperties {
        name: request.name,
        ..Default::default()
    })
}
```

> Adjust `VaultProperties` field set against the canonical definition.

- [ ] **Step 2: Add a test**

```rust
#[tokio::test]
async fn create_vault_writes_marker_secret() {
    use aws_sdk_secretsmanager::operation::create_secret::CreateSecretOutput;
    use crosstache::backend::VaultBackend;
    use crosstache::vault::models::VaultCreateRequest;

    let rule = mock!(Client::create_secret)
        .match_requests(|req| req.name() == Some("myproj-kv/.xv-vault"))
        .then_output(|| {
            CreateSecretOutput::builder()
                .name("myproj-kv/.xv-vault")
                .build()
        });

    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_vault_backend(client);
    let request = VaultCreateRequest {
        name: "myproj-kv".into(),
        ..Default::default()
    };

    let result = backend.create_vault(request).await.unwrap();
    assert_eq!(result.name, "myproj-kv");
}
```

- [ ] **Step 3: Verify and commit**

```
cargo test --features aws --test aws_backend_tests create_vault_writes_marker_secret
```

```bash
git add src/backend/aws/vaults.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): create_vault writes marker secret with xv:type tag"
```

---

## Task 24: Implement `get_vault` and `list_vaults` + tests

**Files:**
- Modify: `src/backend/aws/vaults.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement `get_vault`**

```rust
async fn get_vault(&self, name: &str) -> Result<VaultProperties, BackendError> {
    use crate::backend::aws::encoding::marker_name;
    let marker = marker_name(name);
    let describe = self
        .client
        .describe_secret()
        .secret_id(&marker)
        .send()
        .await
        .map_err(|e| match super::errors::from_describe(name, e) {
            BackendError::NotFound { .. } => BackendError::VaultNotFound {
                name: name.to_string(),
                suggestion: None,
            },
            other => other,
        })?;

    let mut props = VaultProperties {
        name: name.to_string(),
        ..Default::default()
    };
    if let Some(desc) = describe.description() {
        if !desc.is_empty() {
            props.tags.insert("description".into(), desc.to_string());
        }
    }
    if let Some(tags) = describe.tags() {
        for t in tags {
            let k = t.key().unwrap_or("");
            let v = t.value().unwrap_or("");
            if !k.starts_with("xv:") {
                props.tags.insert(k.to_string(), v.to_string());
            }
        }
    }
    Ok(props)
}
```

- [ ] **Step 2: Implement `list_vaults`**

```rust
async fn list_vaults(&self) -> Result<Vec<VaultSummary>, BackendError> {
    use crate::backend::aws::encoding::strip_prefix;
    use crate::backend::aws::metadata::{TAG_TYPE, TAG_VALUE_VAULT_MARKER};
    use aws_sdk_secretsmanager::types::{Filter, FilterNameStringType};

    let mut next_token: Option<String> = None;
    let mut summaries: Vec<VaultSummary> = Vec::new();

    loop {
        let mut req = self
            .client
            .list_secrets()
            .max_results(100)
            .filters(
                Filter::builder()
                    .key(FilterNameStringType::TagKey)
                    .values(TAG_TYPE.to_string())
                    .build(),
            )
            .filters(
                Filter::builder()
                    .key(FilterNameStringType::TagValue)
                    .values(TAG_VALUE_VAULT_MARKER.to_string())
                    .build(),
            );
        if let Some(t) = &next_token {
            req = req.next_token(t.clone());
        }

        let out = req.send().await.map_err(super::errors::from_list)?;
        for entry in out.secret_list.unwrap_or_default() {
            let aws_full_name = entry.name().unwrap_or("");
            // Marker name pattern: <vault>/.xv-vault — vault is everything before /.xv-vault
            if let Some(idx) = aws_full_name.rfind("/.xv-vault") {
                let vault = &aws_full_name[..idx];
                summaries.push(VaultSummary {
                    name: vault.to_string(),
                    ..Default::default()
                });
            }
        }
        next_token = out.next_token().map(|s| s.to_string());
        if next_token.is_none() {
            break;
        }
    }
    Ok(summaries)
}
```

- [ ] **Step 3: Add tests**

```rust
#[tokio::test]
async fn get_vault_returns_vault_not_found_when_marker_missing() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretError;
    use aws_sdk_secretsmanager::types::error::ResourceNotFoundException;
    use crosstache::backend::VaultBackend;
    use crosstache::backend::error::BackendError;

    let rule = mock!(Client::describe_secret).then_error(|| {
        DescribeSecretError::ResourceNotFoundException(
            ResourceNotFoundException::builder().message("not found").build(),
        )
    });
    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_vault_backend(client);

    let err = backend.get_vault("missing-vault").await.unwrap_err();
    assert!(matches!(err, BackendError::VaultNotFound { .. }), "got: {err:?}");
}

#[tokio::test]
async fn list_vaults_finds_all_markers() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::VaultBackend;

    let rule = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("myproj-kv/.xv-vault").build())
            .secret_list(SecretListEntry::builder().name("staging-kv/.xv-vault").build())
            .build()
    });
    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_vault_backend(client);

    let vaults = backend.list_vaults().await.unwrap();
    let names: Vec<String> = vaults.iter().map(|v| v.name.clone()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"myproj-kv".to_string()));
    assert!(names.contains(&"staging-kv".to_string()));
}
```

- [ ] **Step 4: Verify and commit**

```
cargo test --features aws --test aws_backend_tests get_vault_returns_vault_not_found_when_marker_missing list_vaults_finds_all_markers
```

```bash
git add src/backend/aws/vaults.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): get_vault + list_vaults via marker secrets"
```

---

## Task 25: Implement `delete_vault` (refusal logic + force) + tests

**Files:**
- Modify: `src/backend/aws/vaults.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Add a `delete_vault_with_force` helper**

The `VaultBackend` trait method `delete_vault(&self, name: &str)` is the public surface; we wire `--force` through the existing CLI flag plumbing later. The refusal logic lives in the backend.

```rust
async fn delete_vault(&self, name: &str) -> Result<(), BackendError> {
    self.delete_vault_internal(name, false).await
}
```

Add to the `impl AwsVaultBackend` (separate `impl` block, not part of the trait):

```rust
impl AwsVaultBackend {
    pub async fn delete_vault_internal(
        &self,
        name: &str,
        force: bool,
    ) -> Result<(), BackendError> {
        use crate::backend::aws::encoding::{is_marker, marker_name, strip_prefix};
        use aws_sdk_secretsmanager::types::{Filter, FilterNameStringType};

        // Step 1: List non-marker secrets in this vault prefix
        let prefix = format!("{name}/");
        let out = self
            .client
            .list_secrets()
            .max_results(100)
            .filters(
                Filter::builder()
                    .key(FilterNameStringType::Name)
                    .values(prefix.clone())
                    .build(),
            )
            .send()
            .await
            .map_err(super::errors::from_list)?;

        let non_marker: Vec<String> = out
            .secret_list
            .unwrap_or_default()
            .iter()
            .filter_map(|e| e.name().map(|s| s.to_string()))
            .filter(|n| !is_marker(n) && strip_prefix(name, n).is_some())
            .collect();

        if !non_marker.is_empty() && !force {
            return Err(BackendError::Conflict(format!(
                "vault '{name}' contains {} secret(s); pass --force to delete them all",
                non_marker.len()
            )));
        }

        // Step 2: With force, delete all non-marker secrets
        if force {
            for full_name in &non_marker {
                self.client
                    .delete_secret()
                    .secret_id(full_name)
                    .recovery_window_in_days(30)
                    .send()
                    .await
                    .map_err(|e| super::errors::from_delete(full_name, e))?;
            }
        }

        // Step 3: Delete the marker
        let marker = marker_name(name);
        self.client
            .delete_secret()
            .secret_id(&marker)
            .force_delete_without_recovery(true)
            .send()
            .await
            .map_err(|e| super::errors::from_delete(&marker, e))?;

        Ok(())
    }
}
```

- [ ] **Step 2: Add tests**

```rust
#[tokio::test]
async fn delete_vault_refuses_when_secrets_exist() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::VaultBackend;
    use crosstache::backend::error::BackendError;

    let rule = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("myproj-kv/.xv-vault").build())
            .secret_list(SecretListEntry::builder().name("myproj-kv/db-password").build())
            .build()
    });
    let client = mock_client_from_rules(vec![rule]);
    let backend = aws_vault_backend(client);

    let err = backend.delete_vault("myproj-kv").await.unwrap_err();
    assert!(matches!(err, BackendError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn delete_vault_succeeds_when_only_marker_exists() {
    use aws_sdk_secretsmanager::operation::delete_secret::DeleteSecretOutput;
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::VaultBackend;

    let list = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("myproj-kv/.xv-vault").build())
            .build()
    });
    let delete = mock!(Client::delete_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/.xv-vault"))
        .then_output(|| DeleteSecretOutput::builder().build());

    let client = mock_client_from_rules(vec![list, delete]);
    let backend = aws_vault_backend(client);
    backend.delete_vault("myproj-kv").await.unwrap();
}
```

- [ ] **Step 3: Verify and commit**

```
cargo test --features aws --test aws_backend_tests delete_vault_refuses_when_secrets_exist delete_vault_succeeds_when_only_marker_exists
```

```bash
git add src/backend/aws/vaults.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): delete_vault with refusal logic + delete_vault_internal(force)"
```

---

## Task 26: Implement `update_vault` + test

**Files:**
- Modify: `src/backend/aws/vaults.rs`
- Modify: `tests/aws_backend_tests.rs`

- [ ] **Step 1: Implement**

```rust
async fn update_vault(
    &self,
    name: &str,
    request: VaultUpdateRequest,
) -> Result<VaultProperties, BackendError> {
    use crate::backend::aws::encoding::marker_name;
    use aws_sdk_secretsmanager::types::Tag;
    let marker = marker_name(name);

    if let Some(ref desc) = request.description {
        self.client
            .update_secret()
            .secret_id(&marker)
            .description(desc)
            .send()
            .await
            .map_err(|e| super::errors::from_update(name, e))?;
    }

    if let Some(ref new_tags) = request.tags {
        let tag_list: Vec<Tag> = new_tags
            .iter()
            .filter(|(k, _)| !k.starts_with("xv:"))
            .map(|(k, v)| Tag::builder().key(k).value(v).build())
            .collect();
        if !tag_list.is_empty() {
            self.client
                .tag_resource()
                .secret_id(&marker)
                .set_tags(Some(tag_list))
                .send()
                .await
                .map_err(super::errors::from_tag)?;
        }
    }

    self.get_vault(name).await
}
```

> Adjust `VaultUpdateRequest` field names against the canonical definition.

- [ ] **Step 2: Add a smoke test**

```rust
#[tokio::test]
async fn update_vault_updates_description() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::update_secret::UpdateSecretOutput;
    use crosstache::backend::VaultBackend;
    use crosstache::vault::models::VaultUpdateRequest;

    let update = mock!(Client::update_secret)
        .match_requests(|req| req.description() == Some("new description"))
        .then_output(|| UpdateSecretOutput::builder().build());
    let describe =
        mock!(Client::describe_secret).then_output(|| DescribeSecretOutput::builder().build());

    let client = mock_client_from_rules(vec![update, describe]);
    let backend = aws_vault_backend(client);

    let request = VaultUpdateRequest {
        description: Some("new description".into()),
        ..Default::default()
    };
    backend.update_vault("myproj-kv", request).await.unwrap();
}
```

- [ ] **Step 3: Verify and commit**

```
cargo test --features aws --test aws_backend_tests update_vault_updates_description
```

```bash
git add src/backend/aws/vaults.rs tests/aws_backend_tests.rs
git commit -m "feat(aws): update_vault writes description + tags to marker secret"
```

---

## Phase 6: CLI integration — flags, init wizard, migrate

## Task 27: Add `--aws-profile` and `--region` global CLI flags

**Files:**
- Modify: `src/cli/commands.rs` (Cli struct definition)
- Modify: `src/cli/mod.rs` or wherever the global config is built from CLI flags

- [ ] **Step 1: Add the flags to `Cli`**

Find the top-level `Cli` struct (search for `struct Cli` in `src/cli/commands.rs`). Add two flags:

```rust
/// Override the AWS profile for this invocation (only honored when active backend is aws)
#[arg(long, global = true)]
pub aws_profile: Option<String>,

/// Override the AWS region for this invocation (only honored when active backend is aws)
#[arg(long, global = true)]
pub region: Option<String>,
```

- [ ] **Step 2: Plumb into config**

Find where `Config` is finalized from `Cli` (likely in `Cli::execute` or similar). After existing assignments, add:

```rust
if let Some(ref p) = cli.aws_profile {
    if let Some(ref mut aws) = config.aws {
        aws.profile = Some(p.clone());
    } else {
        // Allow per-invocation profile override even if no [aws] block
        config.aws = Some(AwsConfig {
            profile: Some(p.clone()),
            ..Default::default()
        });
    }
}
if let Some(ref r) = cli.region {
    if let Some(ref mut aws) = config.aws {
        aws.region = Some(r.clone());
    } else {
        config.aws = Some(AwsConfig {
            region: Some(r.clone()),
            ..Default::default()
        });
    }
}
```

- [ ] **Step 3: Verify**

Run: `cargo build` (default features)
Run: `cargo build --features aws`
Expected: clean.

Run: `cargo run -- --help | grep -E 'aws-profile|region'`
Expected: both flags listed.

- [ ] **Step 4: Commit**

```bash
git add src/cli/commands.rs src/cli/mod.rs
git commit -m "feat(aws): add --aws-profile and --region global flags"
```

---

## Task 28: Wire AWS branch into `migrate_ops::create_backend`

**Files:**
- Modify: `src/cli/migrate_ops.rs:14-36`

- [ ] **Step 1: Add AWS arm**

Replace `create_backend` in `src/cli/migrate_ops.rs`:

```rust
fn create_backend(kind: BackendKind, config: &Config) -> Result<Arc<dyn Backend>> {
    match kind {
        BackendKind::Azure => {
            let auth_provider =
                BackendRegistry::create_azure_auth_provider(config).map_err(|e| {
                    CrosstacheError::Unknown(format!("Failed to create Azure auth: {e}"))
                })?;
            let backend =
                crate::backend::azure::AzureBackend::new(config, auth_provider).map_err(|e| {
                    CrosstacheError::Unknown(format!("Failed to create Azure backend: {e}"))
                })?;
            Ok(Arc::new(backend))
        }
        BackendKind::Local => {
            let backend =
                crate::backend::local::LocalBackend::new(config.local.as_ref()).map_err(|e| {
                    CrosstacheError::Unknown(format!("Failed to create local backend: {e}"))
                })?;
            Ok(Arc::new(backend))
        }
        #[cfg(feature = "aws")]
        BackendKind::Aws => {
            let aws_cfg = config.aws.as_ref().ok_or_else(|| {
                CrosstacheError::config(
                    "[aws] config block missing — set backend = \"aws\" or pass --aws-profile",
                )
            })?;
            // create_backend is sync; block on async constructor
            let backend = tokio::runtime::Handle::current()
                .block_on(crate::backend::aws::AwsBackend::new(aws_cfg, None, None))
                .map_err(|e| CrosstacheError::Unknown(format!("Failed to create AWS backend: {e}")))?;
            Ok(Arc::new(backend))
        }
        #[cfg(not(feature = "aws"))]
        BackendKind::Aws => Err(CrosstacheError::Unknown(
            "AWS backend not compiled in: rebuild with --features aws".into(),
        )),
    }
}
```

- [ ] **Step 2: Verify**

Run: `cargo build --features aws`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/cli/migrate_ops.rs
git commit -m "feat(aws): wire BackendKind::Aws into migrate create_backend helper"
```

---

## Task 29: Wire AWS branch into `xv init` wizard

**Files:**
- Modify: `src/config/init.rs`

- [ ] **Step 1: Find the backend-selection prompt**

Run: `grep -n "interactive_setup\|backend.*choice\|init_local\|init_azure" src/config/init.rs | head -20`

Find the section that asks the user to pick backend (`azure` vs `local`). Add a third option for `aws`.

- [ ] **Step 2: Add the AWS branch**

Implement `init_aws_backend(&self, init_config: &mut InitConfig) -> Result<()>` that:

1. Prompts for AWS region (default from `AWS_REGION` env, fall through to `us-east-1`).
2. Prompts for AWS profile (default from `AWS_PROFILE` env, fall through to `"default"`).
3. Optionally tests connectivity by calling `STS GetCallerIdentity` (lightweight; no IAM permission required other than implicit). For v0.10, **skip** the live test to keep init hermetic — just collect config and let the first real call validate.
4. Writes `Config.aws = Some(AwsConfig { region, profile, default_vault: None, endpoint_url: None })`.
5. Asks for default vault name with the same UX as Azure default vault prompt.

Concrete shape:

```rust
async fn init_aws_backend(&self, init_config: &mut InitConfig) -> Result<()> {
    use dialoguer::Input;

    let region: String = Input::new()
        .with_prompt("AWS region")
        .default(
            std::env::var("AWS_REGION")
                .unwrap_or_else(|_| "us-east-1".to_string()),
        )
        .interact_text()
        .map_err(|e| CrosstacheError::config(format!("Region prompt failed: {e}")))?;

    let profile: String = Input::new()
        .with_prompt("AWS profile")
        .default(
            std::env::var("AWS_PROFILE")
                .unwrap_or_else(|_| "default".to_string()),
        )
        .interact_text()
        .map_err(|e| CrosstacheError::config(format!("Profile prompt failed: {e}")))?;

    let default_vault: String = Input::new()
        .with_prompt("Default vault (prefix)")
        .default("default".to_string())
        .interact_text()
        .map_err(|e| CrosstacheError::config(format!("Vault prompt failed: {e}")))?;

    init_config.aws_region = Some(region);
    init_config.aws_profile = Some(profile);
    init_config.aws_default_vault = Some(default_vault);
    init_config.backend_choice = "aws".to_string();

    Ok(())
}
```

Add corresponding fields to `InitConfig`:

```rust
pub struct InitConfig {
    // ... existing fields
    pub backend_choice: String,
    pub aws_region: Option<String>,
    pub aws_profile: Option<String>,
    pub aws_default_vault: Option<String>,
}
```

In `build_config`, add an arm that pulls AWS fields into `Config.aws` when `backend_choice == "aws"`.

- [ ] **Step 3: Update the backend-choice prompt**

In `run_interactive_setup`, find the existing two-option `Select` for backend choice (likely uses `dialoguer::Select`). Replace the items list to include "aws":

```rust
let backend_options = &["azure", "local", "aws"];
```

After the user picks, dispatch to `init_azure_backend` / `init_local_backend` / `init_aws_backend` accordingly.

- [ ] **Step 4: Verify**

Run: `cargo build --features aws`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/config/init.rs
git commit -m "feat(aws): xv init wizard gains AWS branch (region, profile, default vault)"
```

---

## Phase 7: `xv migrate` hardening (the headline)

## Task 30: Add `--on-conflict` flag (replaces `--overwrite`)

**Files:**
- Modify: `src/cli/commands.rs:690-709` (Commands::Migrate)
- Modify: `src/cli/migrate_ops.rs` (execute_migrate signature)

- [ ] **Step 1: Add the enum**

In `src/cli/commands.rs`, add (near the top of the file or just above `Commands`):

```rust
#[derive(Debug, Clone, clap::ValueEnum, PartialEq, Eq)]
pub enum OnConflict {
    /// Skip secrets that already exist in the target (default)
    Skip,
    /// Overwrite the target value, replacing the metadata
    Replace,
    /// Abort the migration on first conflict
    Fail,
}

impl Default for OnConflict {
    fn default() -> Self {
        Self::Skip
    }
}
```

- [ ] **Step 2: Update `Commands::Migrate`**

Replace the existing `Migrate` variant in `Commands`:

```rust
/// Migrate secrets between backends
Migrate {
    /// Source backend (azure, local, aws)
    #[arg(long)]
    from: String,
    /// Target backend (azure, local, aws)
    #[arg(long)]
    to: String,
    /// Only migrate secrets from this vault
    #[arg(long)]
    vault: Option<String>,
    /// Filter secrets by glob pattern (e.g., "db-*", "api-*")
    #[arg(long)]
    filter: Option<String>,
    /// Preview what would be migrated without making changes
    #[arg(long)]
    dry_run: bool,
    /// Behavior when a secret already exists in the target
    #[arg(long, value_enum, default_value_t = OnConflict::Skip)]
    on_conflict: OnConflict,
    /// Ignore migration tags and replace targets unconditionally
    #[arg(long)]
    force_replace: bool,
    /// Concurrent transfers (default 8)
    #[arg(long, default_value = "8")]
    concurrency: usize,
    /// DEPRECATED: use --on-conflict replace instead
    #[arg(long, hide = true)]
    overwrite: bool,
},
```

- [ ] **Step 3: Update `execute_migrate` signature**

In `src/cli/migrate_ops.rs`, change the function signature:

```rust
pub(crate) async fn execute_migrate(
    from: String,
    to: String,
    vault: Option<String>,
    filter: Option<String>,
    dry_run: bool,
    on_conflict: crate::cli::commands::OnConflict,
    force_replace: bool,
    concurrency: usize,
    legacy_overwrite: bool,
    config: Config,
) -> Result<()> {
    // Compatibility shim: --overwrite -> --on-conflict replace + warn
    let on_conflict = if legacy_overwrite {
        eprintln!("warning: --overwrite is deprecated; use --on-conflict replace");
        crate::cli::commands::OnConflict::Replace
    } else {
        on_conflict
    };
    // ... rest of function
}
```

- [ ] **Step 4: Update the dispatch site**

In `src/cli/commands.rs:1470-1490` (around the `Commands::Migrate` arm), update the call:

```rust
Commands::Migrate {
    from, to, vault, filter, dry_run, on_conflict, force_replace, concurrency, overwrite,
} => {
    crate::cli::migrate_ops::execute_migrate(
        from, to, vault, filter, dry_run,
        on_conflict, force_replace, concurrency, overwrite,
        config,
    ).await
}
```

- [ ] **Step 5: Verify**

Run: `cargo build`
Expected: clean.

Run: `cargo run -- migrate --help`
Expected: shows `--on-conflict <skip|replace|fail>`.

- [ ] **Step 6: Commit**

```bash
git add src/cli/commands.rs src/cli/migrate_ops.rs
git commit -m "feat(migrate): add --on-conflict flag (replaces --overwrite with deprecation)"
```

---

## Task 31: Add pre-flight diff and summary table

**Files:**
- Modify: `src/cli/migrate_ops.rs`

- [ ] **Step 1: Add a diff helper**

Above `execute_migrate`, add:

```rust
struct MigrationDiff {
    to_migrate: Vec<String>,
    conflicts: Vec<String>,
}

async fn compute_diff(
    source: &Arc<dyn Backend>,
    target: &Arc<dyn Backend>,
    vault: &str,
    filter: Option<&str>,
) -> Result<MigrationDiff> {
    let source_secrets = source
        .secrets()
        .list_secrets(vault, None)
        .await
        .map_err(|e| {
            CrosstacheError::Unknown(format!(
                "Failed to list secrets from {} backend: {e}",
                source.name()
            ))
        })?;

    let filtered: Vec<String> = match filter {
        Some(pattern) => {
            let glob = globset::Glob::new(pattern)
                .map_err(|e| CrosstacheError::invalid_argument(format!("Invalid glob pattern: {e}")))?
                .compile_matcher();
            source_secrets
                .into_iter()
                .filter(|s| glob.is_match(&s.name))
                .map(|s| s.name)
                .collect()
        }
        None => source_secrets.into_iter().map(|s| s.name).collect(),
    };

    let mut to_migrate = Vec::new();
    let mut conflicts = Vec::new();

    for name in filtered {
        match target.secrets().secret_exists(vault, &name).await {
            Ok(true) => conflicts.push(name),
            Ok(false) => to_migrate.push(name),
            Err(e) => {
                tracing::debug!("secret_exists check failed for {name}: {e}; assuming new");
                to_migrate.push(name);
            }
        }
    }
    Ok(MigrationDiff { to_migrate, conflicts })
}

fn print_diff_summary(
    diff: &MigrationDiff,
    source_name: &str,
    target_name: &str,
    vault: &str,
    on_conflict: &crate::cli::commands::OnConflict,
    dry_run: bool,
) {
    println!();
    println!("Source: {}:{}", source_name, vault);
    println!("Target: {}:{}", target_name, vault);
    println!();
    println!("  to migrate:    {} secret(s)", diff.to_migrate.len());
    println!("  conflict:      {} secret(s) (target already has same name)", diff.conflicts.len());
    println!();
    println!("On conflict: {:?}", on_conflict);
    println!("Dry run? {}", if dry_run { "yes" } else { "no" });
    println!();
}
```

- [ ] **Step 2: Wire into `execute_migrate`**

After resolving the source/target backends and `vault_name`, call:

```rust
let diff = compute_diff(&source, &target, &vault_name, filter.as_deref()).await?;
print_diff_summary(&diff, source.name(), target.name(), &vault_name, &on_conflict, dry_run);

if dry_run {
    return Ok(());
}

// Honor --on-conflict fail
if !diff.conflicts.is_empty() && on_conflict == crate::cli::commands::OnConflict::Fail {
    return Err(CrosstacheError::Unknown(format!(
        "{} conflict(s) detected; aborting (--on-conflict fail)",
        diff.conflicts.len()
    )));
}
```

Then iterate over `diff.to_migrate` for fresh transfers and `diff.conflicts` (only when `OnConflict::Replace`) for overwrites.

- [ ] **Step 3: Verify**

Run: `cargo build`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src/cli/migrate_ops.rs
git commit -m "feat(migrate): pre-flight diff + summary table + --on-conflict fail abort"
```

---

## Task 32: Add idempotency tags (`xv:migrated_from`, `xv:migrated_at`)

**Files:**
- Modify: `src/cli/migrate_ops.rs`

- [ ] **Step 1: Add idempotency tag constants**

Near the top of `migrate_ops.rs`:

```rust
const TAG_MIGRATED_FROM: &str = "xv:migrated_from";
const TAG_MIGRATED_AT: &str = "xv:migrated_at";
```

- [ ] **Step 2: Modify the per-secret transfer loop**

In `execute_migrate`, when transferring each secret to the target, after fetching `source_props` (with value), build the `SecretRequest` with the migration tags injected:

```rust
let migrated_from = format!("{}:{}:{}", source.name(), vault_name, source_props.version);
let migrated_at = chrono::Utc::now().to_rfc3339();

let mut tags = source_props.tags.clone();
tags.insert(TAG_MIGRATED_FROM.into(), migrated_from);
tags.insert(TAG_MIGRATED_AT.into(), migrated_at);

let request = SecretRequest {
    name: source_props.name.clone(),
    value: Zeroizing::new(
        source_props.value.as_ref()
            .map(|v| v.as_str().to_string())
            .unwrap_or_default(),
    ),
    tags,
    note: source_props.note.clone(),
    folder: source_props.folder.clone(),
    groups: source_props.groups.clone(),
    expires_on: source_props.expires_on,
    content_type: source_props.content_type.clone(),
    ..Default::default()
};
```

- [ ] **Step 3: Add idempotency-skip logic**

Before transferring, check if the target already has matching migration tags (when `on_conflict == Skip` and `force_replace == false`):

```rust
if !force_replace {
    if let Ok(existing) = target.secrets().get_secret(&vault_name, &name, false).await {
        if let Some(prev_from) = existing.tags.get(TAG_MIGRATED_FROM) {
            let expected = format!("{}:{}:{}", source.name(), vault_name, source_props.version);
            if prev_from == &expected {
                println!("  [skip] {} — already migrated (same source version)", name);
                skipped += 1;
                continue;
            }
        }
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo build`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/cli/migrate_ops.rs
git commit -m "feat(migrate): idempotency via xv:migrated_from + xv:migrated_at tags"
```

---

## Task 33: Bounded concurrency + retry/backoff

**Files:**
- Modify: `src/cli/migrate_ops.rs`

- [ ] **Step 1: Replace serial loop with concurrent transfers**

Convert the per-secret transfer loop into a `futures::stream::iter(...).buffer_unordered(concurrency)` pipeline:

```rust
use futures::stream::{self, StreamExt};

let names_to_migrate: Vec<String> = diff.to_migrate.clone();
let source_clone = source.clone();
let target_clone = target.clone();
let vault_clone = vault_name.clone();

let results = stream::iter(names_to_migrate.into_iter().map(|name| {
    let source = source_clone.clone();
    let target = target_clone.clone();
    let vault = vault_clone.clone();
    async move { migrate_one(&source, &target, &vault, &name).await }
}))
.buffer_unordered(concurrency)
.collect::<Vec<_>>()
.await;

let mut migrated = 0usize;
let mut errors: Vec<(String, CrosstacheError)> = Vec::new();
for r in results {
    match r {
        Ok(name) => {
            migrated += 1;
            println!("  [ok] {}", name);
        }
        Err((name, e)) => errors.push((name, e)),
    }
}
```

Add the helper:

```rust
async fn migrate_one(
    source: &Arc<dyn Backend>,
    target: &Arc<dyn Backend>,
    vault: &str,
    name: &str,
) -> std::result::Result<String, (String, CrosstacheError)> {
    let props = source
        .secrets()
        .get_secret(vault, name, true)
        .await
        .map_err(|e| (name.to_string(), CrosstacheError::Unknown(format!("get_secret: {e}"))))?;

    let request = build_request_from_props(&props, source.name(), vault);

    // Retry with exponential backoff on RateLimited
    let mut attempt = 0;
    loop {
        match target.secrets().set_secret(vault, request.clone()).await {
            Ok(_) => return Ok(name.to_string()),
            Err(BackendError::RateLimited { retry_after_secs }) if attempt < 5 => {
                let wait = retry_after_secs
                    .map(std::time::Duration::from_secs)
                    .unwrap_or_else(|| {
                        std::time::Duration::from_millis(500 * 2u64.pow(attempt as u32))
                    });
                tokio::time::sleep(wait).await;
                attempt += 1;
            }
            Err(e) => {
                return Err((name.to_string(), CrosstacheError::Unknown(format!("set_secret: {e}"))))
            }
        }
    }
}

fn build_request_from_props(
    props: &SecretProperties,
    source_name: &str,
    vault: &str,
) -> SecretRequest {
    let mut tags = props.tags.clone();
    tags.insert(
        TAG_MIGRATED_FROM.into(),
        format!("{}:{}:{}", source_name, vault, props.version),
    );
    tags.insert(TAG_MIGRATED_AT.into(), chrono::Utc::now().to_rfc3339());

    SecretRequest {
        name: props.name.clone(),
        value: Zeroizing::new(
            props.value.as_ref()
                .map(|v| v.as_str().to_string())
                .unwrap_or_default(),
        ),
        tags,
        note: props.note.clone(),
        folder: props.folder.clone(),
        groups: props.groups.clone(),
        expires_on: props.expires_on,
        content_type: props.content_type.clone(),
        ..Default::default()
    }
}
```

- [ ] **Step 2: Verify**

Run: `cargo build`
Expected: clean.

Run: `cargo test --lib`
Expected: existing tests still pass.

- [ ] **Step 3: Commit**

```bash
git add src/cli/migrate_ops.rs
git commit -m "feat(migrate): bounded concurrency (--concurrency) + exponential backoff retry"
```

---

## Phase 8: LocalStack and migration round-trip tests

## Task 34: LocalStack-gated integration tests

**Files:**
- Create: `tests/aws_localstack_tests.rs`

- [ ] **Step 1: Write the gated test harness**

```rust
//! LocalStack integration tests. Skipped silently when LocalStack is unavailable.
//!
//! Enable with: AWS_INTEGRATION_TESTS=1 (and a running LocalStack at AWS_ENDPOINT_URL).
//!
//! LocalStack docker quickstart:
//!   docker run -d --rm --name localstack -p 4566:4566 localstack/localstack
//!   export AWS_ENDPOINT_URL=http://localhost:4566
//!   export AWS_ACCESS_KEY_ID=test
//!   export AWS_SECRET_ACCESS_KEY=test
//!   export AWS_REGION=us-east-1

#![cfg(feature = "aws")]

use crosstache::backend::aws::AwsBackend;
use crosstache::backend::SecretBackend;
use crosstache::config::settings::AwsConfig;
use crosstache::secret::manager::SecretRequest;
use zeroize::Zeroizing;

fn skip_unless_enabled() -> bool {
    if std::env::var("AWS_INTEGRATION_TESTS").is_err() {
        eprintln!("AWS_INTEGRATION_TESTS not set — skipping");
        return true;
    }
    if std::env::var("AWS_ENDPOINT_URL").is_err() {
        eprintln!("AWS_ENDPOINT_URL not set — skipping (LocalStack required)");
        return true;
    }
    false
}

async fn build_backend() -> AwsBackend {
    let cfg = AwsConfig {
        region: Some(
            std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
        ),
        endpoint_url: Some(std::env::var("AWS_ENDPOINT_URL").unwrap()),
        ..Default::default()
    };
    AwsBackend::new(&cfg, None, None).await.unwrap()
}

#[tokio::test]
async fn localstack_set_get_round_trip() {
    if skip_unless_enabled() {
        return;
    }
    let backend = build_backend().await;

    // Use a unique vault per test run to avoid cross-contamination
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    let request = SecretRequest {
        name: "round-trip-test".into(),
        value: Zeroizing::new("test-value-42".into()),
        groups: vec!["test".into()],
        ..Default::default()
    };
    backend.secrets().set_secret(&vault, request).await.unwrap();

    let got = backend
        .secrets()
        .get_secret(&vault, "round-trip-test", true)
        .await
        .unwrap();
    assert_eq!(
        got.value.as_ref().map(|v| v.as_str().to_string()),
        Some("test-value-42".to_string())
    );
    assert_eq!(got.groups, vec!["test"]);

    // Cleanup
    backend
        .secrets()
        .purge_secret(&vault, "round-trip-test")
        .await
        .unwrap();
}

#[tokio::test]
async fn localstack_list_paginates() {
    if skip_unless_enabled() {
        return;
    }
    let backend = build_backend().await;
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    // Write 5 secrets
    for i in 0..5 {
        let request = SecretRequest {
            name: format!("test-{}", i),
            value: Zeroizing::new(format!("value-{}", i)),
            ..Default::default()
        };
        backend.secrets().set_secret(&vault, request).await.unwrap();
    }

    let listed = backend.secrets().list_secrets(&vault, None).await.unwrap();
    let names: Vec<String> = listed.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names.len(), 5);
    for i in 0..5 {
        assert!(names.contains(&format!("test-{}", i)));
    }

    // Cleanup
    for i in 0..5 {
        backend
            .secrets()
            .purge_secret(&vault, &format!("test-{}", i))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn localstack_vault_marker_create_list_delete() {
    if skip_unless_enabled() {
        return;
    }
    use crosstache::backend::VaultBackend;
    use crosstache::vault::models::VaultCreateRequest;

    let backend = build_backend().await;
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    backend
        .vaults()
        .unwrap()
        .create_vault(VaultCreateRequest {
            name: vault.clone(),
            ..Default::default()
        })
        .await
        .unwrap();

    let listed = backend.vaults().unwrap().list_vaults().await.unwrap();
    let names: Vec<String> = listed.iter().map(|v| v.name.clone()).collect();
    assert!(names.contains(&vault));

    backend.vaults().unwrap().delete_vault(&vault).await.unwrap();
}
```

- [ ] **Step 2: Add `uuid` to dev-dependencies if not present**

Check `Cargo.toml`:
```bash
grep "^uuid" Cargo.toml
```

If `uuid` is not already in `[dev-dependencies]` (it's already in `[dependencies]`), this is fine — dev tests can use the dependency-tree dep.

- [ ] **Step 3: Verify**

Run (without LocalStack):
```bash
cargo test --features aws --test aws_localstack_tests
```
Expected: 3 tests run, all skip silently with stderr "AWS_INTEGRATION_TESTS not set — skipping".

Run with LocalStack:
```bash
docker run -d --rm --name localstack -p 4566:4566 localstack/localstack
sleep 5
AWS_INTEGRATION_TESTS=1 \
  AWS_ENDPOINT_URL=http://localhost:4566 \
  AWS_ACCESS_KEY_ID=test \
  AWS_SECRET_ACCESS_KEY=test \
  AWS_REGION=us-east-1 \
  cargo test --features aws --test aws_localstack_tests
docker stop localstack
```
Expected: 3 PASS.

- [ ] **Step 4: Commit**

```bash
git add tests/aws_localstack_tests.rs
git commit -m "test(aws): LocalStack-gated integration tests for set/get/list/vaults"
```

---

## Task 35: Migration round-trip tests (Local↔AWS)

**Files:**
- Create: `tests/migration_round_trip_tests.rs`

- [ ] **Step 1: Write the test**

```rust
//! Cross-backend migration round-trip tests.
//!
//! Tests Local↔AWS today (LocalStack-gated). Azure↔AWS deferred to live tests.

#![cfg(feature = "aws")]

use crosstache::backend::aws::AwsBackend;
use crosstache::backend::local::LocalBackend;
use crosstache::backend::{Backend, SecretBackend};
use crosstache::config::settings::{AwsConfig, LocalConfig};
use crosstache::secret::manager::SecretRequest;
use std::sync::Arc;
use tempfile::TempDir;
use zeroize::Zeroizing;

fn skip_unless_enabled() -> bool {
    std::env::var("AWS_INTEGRATION_TESTS").is_err()
        || std::env::var("AWS_ENDPOINT_URL").is_err()
}

#[tokio::test]
async fn local_to_aws_round_trip() {
    if skip_unless_enabled() {
        return;
    }
    let tmp = TempDir::new().unwrap();

    // Build local backend
    let local_cfg = LocalConfig {
        store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
        key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
        default_vault: Some("test-vault".into()),
    };
    let local: Arc<dyn Backend> = Arc::new(LocalBackend::new(Some(&local_cfg)).unwrap());

    // Build AWS backend
    let aws_cfg = AwsConfig {
        region: Some("us-east-1".into()),
        endpoint_url: Some(std::env::var("AWS_ENDPOINT_URL").unwrap()),
        ..Default::default()
    };
    let aws: Arc<dyn Backend> = Arc::new(AwsBackend::new(&aws_cfg, None, None).await.unwrap());

    let vault = format!("xv-rt-{}", uuid::Uuid::new_v4());

    // Write 3 secrets to local
    for (n, v) in [("a", "1"), ("b", "2"), ("c", "3")] {
        let request = SecretRequest {
            name: n.into(),
            value: Zeroizing::new(v.into()),
            groups: vec!["roundtrip".into()],
            ..Default::default()
        };
        local.secrets().set_secret(&vault, request).await.unwrap();
    }

    // Read from local + write to AWS (manual migrate equivalent)
    for n in ["a", "b", "c"] {
        let props = local.secrets().get_secret(&vault, n, true).await.unwrap();
        let request = SecretRequest {
            name: props.name.clone(),
            value: Zeroizing::new(
                props.value.as_ref().map(|v| v.as_str().to_string()).unwrap_or_default(),
            ),
            groups: props.groups.clone(),
            ..Default::default()
        };
        aws.secrets().set_secret(&vault, request).await.unwrap();
    }

    // Verify on AWS side
    for (n, expected) in [("a", "1"), ("b", "2"), ("c", "3")] {
        let got = aws.secrets().get_secret(&vault, n, true).await.unwrap();
        assert_eq!(
            got.value.as_ref().map(|v| v.as_str().to_string()),
            Some(expected.to_string())
        );
        assert_eq!(got.groups, vec!["roundtrip"]);
    }

    // Cleanup AWS side
    for n in ["a", "b", "c"] {
        aws.secrets().purge_secret(&vault, n).await.unwrap();
    }
}
```

- [ ] **Step 2: Verify**

```bash
AWS_INTEGRATION_TESTS=1 \
  AWS_ENDPOINT_URL=http://localhost:4566 \
  AWS_ACCESS_KEY_ID=test \
  AWS_SECRET_ACCESS_KEY=test \
  AWS_REGION=us-east-1 \
  cargo test --features aws --test migration_round_trip_tests
```
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/migration_round_trip_tests.rs
git commit -m "test(migrate): Local<->AWS round-trip with LocalStack"
```

---

## Task 36: End-to-end CLI integration test for `xv migrate --from local --to aws --dry-run`

**Files:**
- Modify: `tests/cli_integration_tests.rs`

- [ ] **Step 1: Add the test**

```rust
#[test]
#[cfg(feature = "aws")]
fn migrate_dry_run_against_local_to_aws_shows_summary() {
    use std::process::Command;
    use tempfile::TempDir;

    if std::env::var("AWS_INTEGRATION_TESTS").is_err() {
        eprintln!("skipping: AWS_INTEGRATION_TESTS not set");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let xv = env!("CARGO_BIN_EXE_xv");

    // Set up local store with one secret
    Command::new(xv)
        .args(["--backend", "local", "set", "test-secret", "value123"])
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .output()
        .expect("set should succeed");

    // Run migrate dry-run
    let out = Command::new(xv)
        .args([
            "migrate", "--from", "local", "--to", "aws",
            "--vault", "default", "--dry-run",
        ])
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .env("AWS_ENDPOINT_URL", std::env::var("AWS_ENDPOINT_URL").unwrap())
        .env("AWS_REGION", "us-east-1")
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .output()
        .expect("migrate should run");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Source:"), "expected summary; got:\n{stdout}");
    assert!(stdout.contains("Target:"), "expected summary; got:\n{stdout}");
    assert!(stdout.contains("to migrate"), "expected counts; got:\n{stdout}");
}
```

- [ ] **Step 2: Verify**

Run with LocalStack up:
```bash
AWS_INTEGRATION_TESTS=1 \
  AWS_ENDPOINT_URL=http://localhost:4566 \
  AWS_ACCESS_KEY_ID=test \
  AWS_SECRET_ACCESS_KEY=test \
  AWS_REGION=us-east-1 \
  cargo test --features aws --test cli_integration_tests migrate_dry_run_against_local_to_aws_shows_summary
```
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/cli_integration_tests.rs
git commit -m "test(migrate): end-to-end dry-run smoke test for local->aws migration"
```

---

## Phase 9: Documentation and release

## Task 37: Write `docs/migration.md`

**Files:**
- Create: `docs/migration.md`

- [ ] **Step 1: Write the doc**

```markdown
# Cross-cloud migration with `xv migrate`

`xv migrate` copies secrets from one backend to another while preserving metadata. Phase 3 (v0.10) hardens this command for cross-cloud use as a marquee feature.

## Quick reference

```bash
# Azure -> AWS
xv migrate --from azure --to aws --vault myproj-kv

# AWS -> Azure
xv migrate --from aws --to azure --vault myproj-kv

# Filter
xv migrate --from azure --to aws --vault myproj-kv --filter "db-*"

# Dry run
xv migrate --from azure --to aws --vault myproj-kv --dry-run

# Conflict modes
xv migrate --from azure --to aws --vault myproj-kv --on-conflict skip      # default
xv migrate --from azure --to aws --vault myproj-kv --on-conflict replace
xv migrate --from azure --to aws --vault myproj-kv --on-conflict fail

# Force replace (ignore migration tags)
xv migrate --from azure --to aws --vault myproj-kv --force-replace

# Tune concurrency
xv migrate --from azure --to aws --vault myproj-kv --concurrency 4
```

## Prerequisites

### Azure source / target

You need:
- A logged-in Azure session (`az login` or env-based credentials).
- `Key Vault Secrets User` role on the source vault (for read).
- `Key Vault Secrets Officer` role on the target vault (for write).

### AWS source / target

You need:
- AWS credentials configured (env, profile, SSO, or instance role).
- `secretsmanager:ListSecrets`, `secretsmanager:GetSecretValue`, `secretsmanager:DescribeSecret` on the source.
- `secretsmanager:CreateSecret`, `secretsmanager:PutSecretValue`, `secretsmanager:UpdateSecret`, `secretsmanager:TagResource`, `secretsmanager:UntagResource` on the target.

Minimal AWS IAM policy for the target:

\`\`\`json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "secretsmanager:CreateSecret",
        "secretsmanager:PutSecretValue",
        "secretsmanager:UpdateSecret",
        "secretsmanager:TagResource",
        "secretsmanager:UntagResource",
        "secretsmanager:DescribeSecret",
        "secretsmanager:ListSecrets"
      ],
      "Resource": "*"
    }
  ]
}
\`\`\`

### Local source / target

No prerequisites beyond a configured local backend (`xv init --backend local`).

## How it works

Pre-flight: `xv migrate` enumerates source and target secrets, computes a diff, and prints a summary. In dry-run mode, the run stops here.

Per-secret transfer: each secret is `get_secret`'d from source (with value) and `set_secret`'d on target. Bounded by `--concurrency` (default 8). Throttling errors trigger exponential backoff with jitter.

Idempotency: each migrated secret carries `xv:migrated_from=<source>:<vault>:<source-version-id>` and `xv:migrated_at=<timestamp>` tags on the target. Re-running `xv migrate` with `--on-conflict skip` (the default) detects these and skips entries where the source version matches.

Interruption safety: a run interrupted with Ctrl-C leaves no partial-state damage. Each transfer is atomic. Re-run to resume.

## Metadata mapping

| Source field | Azure → AWS | AWS → Azure |
|---|---|---|
| `groups` | tag `xv:groups` (comma-joined) | tag `groups` |
| `note` | AWS `Description` field | tag `note` |
| `folder` | tag `xv:folder` | tag `folder` |
| `expiry` | tag `xv:expires_at` | native attribute |
| `original_name` | tag `xv:original_name` | tag `original_name` |
| `created_by` | tag `xv:created_by` | tag `created_by` |
| `content_type` | tag `xv:content_type` | native attribute |
| version history | current value only | current value only |

## Performance

A 100-secret migration completes in <60 s on a warm credential cache and `--concurrency 8`, assuming no throttling. For larger migrations, monitor AWS CloudWatch / Azure Monitor for rate-limit events and lower `--concurrency` if needed.

## Troubleshooting

- **`Error: vault 'X' not found`**: target vault doesn't exist. Run `xv vault create X --backend <target>` first, or rely on auto-create (currently only for the source's default vault).
- **`Error: ThrottlingException`**: AWS rate-limit hit. Lower `--concurrency`. Backoff is automatic.
- **`Error: AccessDeniedException`**: missing IAM permissions on AWS, or missing role on Azure. See "Prerequisites".
- **Migrate tags on the target make rollback messy**: pass `--force-replace` to overwrite without honoring migration tags.

## Limitations (Phase 3)

- Only the current value is transferred. Full version history transfer is deferred (`--with-history` not yet implemented).
- IAM resource policies on AWS source/target secrets are not preserved.
- Cross-region AWS migrations require running `xv migrate` once per source/target region pair, using `[named_backends.*]` config.
```

- [ ] **Step 2: Commit**

```bash
git add docs/migration.md
git commit -m "docs: add cross-cloud migration guide for xv migrate"
```

---

## Task 38: Update README and `docs/FEATURES.md`

**Files:**
- Modify: `README.md`
- Modify: `docs/FEATURES.md`

- [ ] **Step 1: Add AWS section to README**

In `README.md`, find the section that documents Azure / local backend (search for `## Backends` or `### Local backend`). Add:

```markdown
### AWS Secrets Manager backend (v0.10)

Use AWS Secrets Manager as the underlying secret store.

```bash
xv init  # pick "aws" when prompted
# or edit ~/.config/xv/xv.conf:
# backend = "aws"
# [aws]
# region = "us-east-1"
# profile = "default"
# default_vault = "myproj-kv"
```

Multi-region:

```toml
backend = "aws-east"
[named_backends.aws-east]
type = "aws"
region = "us-east-1"
[named_backends.aws-west]
type = "aws"
region = "us-west-2"
```

`xv share` and `xv audit` are not supported on AWS in v0.10 (see [docs/migration.md](docs/migration.md) for details). Use AWS IAM and CloudTrail directly for those needs.

### Cross-cloud migration

```bash
# Move secrets from Azure to AWS
xv migrate --from azure --to aws --vault myproj-kv

# Preview first
xv migrate --from azure --to aws --vault myproj-kv --dry-run
```

See [docs/migration.md](docs/migration.md) for the full guide.
```

- [ ] **Step 2: Update `docs/FEATURES.md`**

Find the existing migration entry; replace it with:

```markdown
### Cross-cloud migration (v0.10)

`xv migrate --from <source> --to <target>` copies secrets between backends. Supports Azure ↔ AWS ↔ Local in any combination. Hardening features:

- `--on-conflict skip|replace|fail` — controls behavior when target secret exists
- `--dry-run` — preview without changes
- `--filter "<glob>"` — restrict to matching names
- `--concurrency N` — bounded parallel transfers (default 8)
- Idempotent: re-runs detect previously-migrated secrets via `xv:migrated_from` tag
- Exponential backoff on rate limiting

See [migration.md](migration.md) for the full guide.
```

- [ ] **Step 3: Commit**

```bash
git add README.md docs/FEATURES.md
git commit -m "docs: add AWS backend section + update migration entry"
```

---

## Task 39: Bump version to 0.10.0-rc.1 and verify full build

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Bump version**

```toml
[package]
version = "0.10.0-rc.1"
```

- [ ] **Step 2: Full verification**

Run all of these and ensure each passes:

```bash
# Default features (no AWS)
cargo build
cargo test --lib
cargo test --tests

# AWS feature
cargo build --features aws
cargo test --features aws

# All features
cargo build --all-features
cargo test --all-features

# Clippy
cargo clippy --all-targets -- -W clippy::all
cargo clippy --features aws --all-targets -- -W clippy::all

# Format check
cargo fmt --check
```

Expected: all pass with no warnings.

- [ ] **Step 3: Run binary smoke checks**

```bash
cargo run --features aws -- --help
cargo run --features aws -- migrate --help
cargo run --features aws -- --backend aws --help 2>&1 | head -20
```

Expected: AWS-related help text is present and correct.

- [ ] **Step 4: Binary size check**

```bash
cargo build --release
ls -lh target/release/xv

cargo build --release --features aws
ls -lh target/release/xv
```

Expected: AWS-feature binary is at most 1.5 MB larger than default. Document the actual delta in the release notes.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.10.0-rc.1"
```

- [ ] **Step 6: Tag the rc**

```bash
git tag v0.10.0-rc.1
```

(Push deferred until release time; do not push automatically.)

---

## Task 40: Cut v0.10.0 final after rc soak

**Files:**
- Modify: `Cargo.toml`
- Modify: `CHANGELOG.md` (if present) or release notes file

- [ ] **Step 1: After rc soak window passes (~1 week)**

If no rc.2 needed:

```toml
[package]
version = "0.10.0"
```

If issues surfaced during soak: cut rc.2 first, repeat soak, then v0.10.0.

- [ ] **Step 2: Write release notes**

Create / update release notes (path conventions vary; check `git log --oneline | grep -i 'release\|notes'` for prior precedent):

```markdown
## v0.10.0 — AWS Secrets Manager backend

### Added
- AWS Secrets Manager as a third backend (`xv --backend aws ...`), behind `aws` Cargo feature flag.
- `[aws]` config block (region, profile, endpoint_url, default_vault).
- `[named_backends.*]` config map for multi-region setups (`aws-east`, `aws-west`).
- `--aws-profile` and `--region` global CLI flags.
- `xv migrate` hardening:
  - `--on-conflict skip|replace|fail` flag
  - `--concurrency` flag (default 8)
  - Pre-flight diff and summary table
  - Idempotency via `xv:migrated_from` / `xv:migrated_at` tags
  - Bounded parallelism with exponential backoff on throttling
- `xv init` wizard now offers AWS as a backend option.
- Documentation: `docs/migration.md`.
- Test coverage: hermetic mock tests, LocalStack-gated integration tests, migration round-trip tests.

### Changed
- `--overwrite` on `xv migrate` is deprecated. Use `--on-conflict replace` instead. The flag still works (with a warning) for one minor version.

### Capabilities (AWS backend)
- ✅ Secrets CRUD, versioning, soft-delete, restore, purge
- ✅ Vaults via prefix-based virtual vaults with marker secrets
- ✅ Groups, folders, notes, expiry (in tags)
- ❌ RBAC (`xv share`) — use AWS IAM directly
- ❌ Audit (`xv audit`) — use AWS CloudTrail
- ❌ Native rotation — `xv rotate` writes new versions
- ❌ File storage (S3) — deferred to a future phase

### Migration guide
- Existing Azure or local users: no action required. Default behavior unchanged.
- New AWS users: run `xv init` and pick "aws".

### Performance
- 100-secret cross-cloud migration completes in <60 s on a warm credential cache.

### Binary size
- Default-features binary: unchanged.
- `--features aws` binary: +<X> MB (fill in actual measurement from Task 39 Step 4).
```

- [ ] **Step 3: Commit and tag**

```bash
git add Cargo.toml CHANGELOG.md
git commit -m "chore: bump version to 0.10.0 and finalize release notes"
git tag v0.10.0
```

---

## Self-review checklist

This plan should now be checked against the spec at `docs/superpowers/specs/2026-05-09-aws-backend-phase-3-design.md`. The agent executing this plan should run the following before declaring complete:

- [ ] Spec §3 (capability matrix) — every flag matches what `AwsBackend::capabilities()` returns in Task 11.
- [ ] Spec §4.4 (config schema) — `Config.aws` and `Config.named_backends` exist and parse from TOML (Tasks 5, 6).
- [ ] Spec §5 (vault marker scheme) — marker name, reserved namespace, vault create/list/delete via marker (Tasks 9, 23, 24, 25).
- [ ] Spec §6 (per-method mapping table) — every row has a corresponding task (Tasks 14–22).
- [ ] Spec §7 (auth & config) — AWS SDK default credential chain in Task 7; config schema in Tasks 5, 6.
- [ ] Spec §8 (migrate hardening) — Tasks 30–33 cover conflict modes, pre-flight diff, idempotency, concurrency.
- [ ] Spec §9 (capability negotiation) — `has_rbac=false`, `has_audit=false` in capabilities (Task 11) cause CLI commands like `xv share` to produce `xv-unsupported` via the existing capability-check pattern.
- [ ] Spec §11 (testing) — hermetic mocks (Task 13–22), LocalStack (Task 34), round-trip (Task 35).
- [ ] Spec §13 (success criteria) — all listed commands work on `xv --backend aws`.

If any spec section has no covering task, return to the spec, identify the missing work, add a task, and re-verify.

---

## Plan complete — execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-09-aws-backend-phase-3-plan.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**








