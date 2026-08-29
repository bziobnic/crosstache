# Backend Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `xv` configure more than one backend over time — `xv init` bootstraps one, and a new `xv backend add|rm|ls` group manages them afterwards.

**Architecture:** `build_setup_config` currently clears sibling backends on every call, so it is split into an additive `apply_backend` plus an unchanged exclusive wrapper. Azure gains a real `[azure]` config block so three backends can coexist without fighting over the single top-level `default_vault`. A `Prompter` trait puts a seam under the interactive prompts so the flows become testable.

**Tech Stack:** Rust 2021, `clap` derive, `dialoguer`, `toml`, `tokio`, `tempfile` for tests.

**Spec:** `docs/superpowers/specs/2026-08-09-backend-lifecycle-design.md`

## Global Constraints

- Backend types are exactly `local`, `azure`, `aws`. No named/multi-instance backends — `named_backends` and `NamedBackendEntry` are **not** modified by this plan.
- `build_setup_config`'s signature and behavior must not change. Its existing tests in `src/config/setup.rs` must pass **unmodified** — that is the back-compat proof for the desktop setup service at `src/config/setup.rs:123`.
- A config written before this change has no `[azure]` block and must behave identically. Azure read precedence is: `config.azure` when present, else the top-level fields.
- Every command in this plan either fully succeeds or writes nothing. There is no partial-write state and no exit code 53 — that design was withdrawn.
- `rm` never deletes cloud-side secrets. `--purge` applies to `local` only.
- Removal refuses rather than silently relocating writes. Follow the existing precedent in `execute_cx_rm` (`src/cli/config_ops.rs:1664`), which refuses to remove the workspace default while other entries remain.
- Run `cargo fmt` before every commit. CI runs `cargo clippy -- -D warnings`.

---

### Task 1: Split `build_setup_config` into an additive `apply_backend`

**Files:**
- Modify: `src/config/setup.rs:213-311`
- Test: `src/config/setup.rs` (the existing `#[cfg(test)] mod tests` at line 320)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pub fn apply_backend(request: &SetupRequest, base: &mut Config) -> Result<()>` — writes only the request's own backend block into `base`, never clearing siblings; runs the same field validation as today. `pub fn build_setup_config(request: &SetupRequest, base: Config) -> Result<Config>` keeps its exact current signature and behavior.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/config/setup.rs`:

```rust
/// `apply_backend` is additive: configuring a second backend must not erase
/// the first. This is the whole reason the function exists — `xv backend add`
/// would otherwise silently wipe an existing configuration.
#[test]
fn apply_backend_keeps_previously_configured_backends() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();

    apply_backend(&local_request(dir.path()), &mut config).unwrap();
    apply_backend(
        &SetupRequest::Aws {
            region: "us-east-1".into(),
            profile: Some("default".into()),
            vault_prefix: "default".into(),
        },
        &mut config,
    )
    .unwrap();

    assert!(
        config.local.is_some(),
        "local block must survive a subsequent aws apply"
    );
    assert!(config.aws.is_some(), "aws block must be written");
    assert_eq!(
        config.aws.as_ref().unwrap().region.as_deref(),
        Some("us-east-1")
    );
}

/// The exclusive wrapper keeps its old behavior: it clears siblings first.
#[test]
fn build_setup_config_still_clears_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let mut base = Config::default();
    apply_backend(&local_request(dir.path()), &mut base).unwrap();

    let config = build_setup_config(
        &SetupRequest::Aws {
            region: "us-east-1".into(),
            profile: None,
            vault_prefix: "default".into(),
        },
        base,
    )
    .unwrap();

    assert!(
        config.local.is_none(),
        "build_setup_config must remain mutually exclusive"
    );
    assert!(config.aws.is_some());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config::setup::tests::apply_backend_keeps_previously_configured_backends`
Expected: FAIL to compile — `cannot find function 'apply_backend' in this scope`.

- [ ] **Step 3: Write minimal implementation**

In `src/config/setup.rs`, replace the body of `build_setup_config` with a wrapper and move each match arm into `apply_backend`, deleting the sibling-clearing lines from the arms:

```rust
/// Reset every backend block, so exactly one backend is configured after the
/// following `apply_backend`. This preserves `build_setup_config`'s historic
/// mutually-exclusive contract for the desktop setup service.
fn clear_backend_blocks(base: &mut Config) {
    base.backend = None;
    base.local = None;
    base.aws = None;
    base.azure = None;
    base.subscription_id.clear();
    base.tenant_id.clear();
    base.default_resource_group.clear();
    base.default_location.clear();
    base.blob_config = None;
}

/// Validate `request` and write **only** its own backend block into `base`.
///
/// Additive by contract: a previously configured backend is left untouched, so
/// `xv backend add` can add a second backend without erasing the first. Use
/// [`build_setup_config`] when exactly one backend should survive.
pub fn apply_backend(request: &SetupRequest, base: &mut Config) -> Result<()> {
    match request {
        SetupRequest::Local {
            store_path,
            key_file,
            vault,
        } => {
            let store_path = persisted_path(store_path, "Local store path")?;
            let key_file = persisted_path(key_file, "Local key file")?;
            required(vault, "Local vault")?;

            let local = LocalConfig {
                store_path: Some(store_path),
                key_file: Some(key_file),
                default_vault: Some(vault.clone()),
                encrypt_metadata: None,
                opaque_filenames: None,
                audit: None,
                git: None,
            };
            crate::backend::local::config::ResolvedLocalConfig::from_raw(Some(&local))
                .validate()?;
            base.local = Some(local);
        }
        SetupRequest::Azure {
            subscription_id,
            tenant_id,
            vault,
            resource_group,
            location,
        } => {
            required(subscription_id, "Azure subscription ID")?;
            required(tenant_id, "Azure tenant ID")?;
            required(vault, "Azure vault")?;
            required(resource_group, "Azure resource group")?;
            required(location, "Azure location")?;

            base.subscription_id = subscription_id.clone();
            base.tenant_id = tenant_id.clone();
            base.default_resource_group = resource_group.clone();
            base.default_location = location.clone();
        }
        SetupRequest::Aws {
            region,
            profile,
            vault_prefix,
        } => {
            required(region, "AWS region")?;
            required(vault_prefix, "AWS vault prefix")?;
            if let Some(profile) = profile {
                required(profile, "AWS profile")?;
            }

            base.aws = Some(AwsConfig {
                region: Some(region.clone()),
                profile: profile.clone(),
                endpoint_url: None,
                default_vault: Some(vault_prefix.clone()),
                s3_bucket: None,
            });
        }
    }
    Ok(())
}

pub fn build_setup_config(request: &SetupRequest, mut base: Config) -> Result<Config> {
    clear_backend_blocks(&mut base);
    apply_backend(request, &mut base)?;

    // The active backend and the shared default_vault mirror belong to the
    // exclusive path only; `apply_backend` leaves both alone so an added
    // backend never steals the write target.
    match request {
        SetupRequest::Local { vault, .. } => {
            base.backend = Some("local".into());
            base.default_vault = vault.clone();
        }
        // Preserve the CLI initializer's legacy representation: no explicit
        // backend means Azure.
        SetupRequest::Azure { vault, .. } => {
            base.backend = None;
            base.default_vault = vault.clone();
        }
        SetupRequest::Aws { vault_prefix, .. } => {
            base.backend = Some("aws".into());
            base.default_vault = vault_prefix.clone();
        }
    }

    base.validate()?;
    Ok(base)
}
```

Note: `clear_backend_blocks` references `base.azure`, which does not exist until Task 2. For this task, omit that one line and add it in Task 2.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib config::setup`
Expected: PASS, including every pre-existing test in the module **unmodified**. If any pre-existing test needed editing, the split changed behavior — revert and fix rather than editing the test.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/config/setup.rs
git commit -m "refactor(config): split an additive apply_backend out of build_setup_config"
```

---

### Task 2: Add the `[azure]` config block

**Files:**
- Modify: `src/config/settings.rs` (add `AzureConfig`, add the `azure` field to `Config`, add to `Config::default()` near line 373)
- Modify: `src/config/setup.rs` (`apply_backend` Azure arm, `clear_backend_blocks`)
- Test: `src/config/settings.rs` test module (alongside `named_backends_deserializes_aws_entry` at line 1127)

**Interfaces:**
- Consumes: `apply_backend` from Task 1.
- Produces: `pub struct AzureConfig` with fields `subscription_id: Option<String>`, `tenant_id: Option<String>`, `default_vault: Option<String>`, `resource_group: Option<String>`, `location: Option<String>`. `Config.azure: Option<AzureConfig>`. `Config::azure_settings(&self) -> AzureConfig` resolving block-then-top-level precedence.

- [ ] **Step 1: Write the failing test**

Add to the test module in `src/config/settings.rs`:

```rust
/// A config written before the [azure] block existed must resolve exactly as
/// it always did. This is the back-compat guarantee for every config file in
/// the wild.
#[test]
fn azure_settings_fall_back_to_top_level_fields() {
    let cfg: Config = toml::from_str(
        r#"
subscription_id = "sub-123"
tenant_id = "tenant-456"
default_vault = "legacy-vault"
default_resource_group = "rg-legacy"
default_location = "eastus"
"#,
    )
    .unwrap();

    assert!(cfg.azure.is_none(), "no [azure] block in a legacy config");
    let azure = cfg.azure_settings();
    assert_eq!(azure.subscription_id.as_deref(), Some("sub-123"));
    assert_eq!(azure.tenant_id.as_deref(), Some("tenant-456"));
    assert_eq!(azure.default_vault.as_deref(), Some("legacy-vault"));
    assert_eq!(azure.resource_group.as_deref(), Some("rg-legacy"));
    assert_eq!(azure.location.as_deref(), Some("eastus"));
}

/// When the block is present it wins, so Azure keeps its own vault even when
/// another backend owns the shared top-level default_vault.
#[test]
fn azure_block_takes_precedence_over_top_level() {
    let cfg: Config = toml::from_str(
        r#"
backend = "local"
default_vault = "local-vault"

[azure]
subscription_id = "sub-999"
tenant_id = "tenant-999"
default_vault = "azure-vault"
resource_group = "rg-9"
location = "westus2"
"#,
    )
    .unwrap();

    let azure = cfg.azure_settings();
    assert_eq!(azure.default_vault.as_deref(), Some("azure-vault"));
    assert_eq!(azure.subscription_id.as_deref(), Some("sub-999"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config::settings::tests::azure_settings_fall_back_to_top_level_fields`
Expected: FAIL to compile — no field `azure` on `Config`, no method `azure_settings`.

- [ ] **Step 3: Write minimal implementation**

In `src/config/settings.rs`, add next to `LocalConfig`/`AwsConfig`:

```rust
/// Azure Key Vault settings.
///
/// Azure historically had no block of its own — its settings *are* the
/// top-level `subscription_id` / `tenant_id` / `default_vault` /
/// `default_resource_group` / `default_location` fields. That works for a
/// single-backend config but not once backends coexist, because
/// `default_vault` is global and single-valued. This block gives Azure its own
/// home; the top-level fields remain the legacy fallback and the mirror for
/// whichever backend is active.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AzureConfig {
    #[serde(default)]
    pub subscription_id: Option<String>,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub default_vault: Option<String>,
    #[serde(default)]
    pub resource_group: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
}
```

Add the field to `Config` beside `local` and `aws`:

```rust
    /// Configuration for the Azure Key Vault backend. Absent in configs
    /// written before this block existed — see [`Config::azure_settings`].
    #[tabled(skip)]
    #[serde(default)]
    pub azure: Option<AzureConfig>,
```

Add `azure: None,` to `Config::default()` beside `local`/`aws`.

Add the resolver to `impl Config`:

```rust
    /// Azure settings with block-then-top-level precedence.
    ///
    /// Returns the `[azure]` block when present; otherwise synthesizes one
    /// from the legacy top-level fields so pre-block configs behave
    /// identically. Empty top-level strings resolve to `None` rather than
    /// `Some("")`.
    pub fn azure_settings(&self) -> AzureConfig {
        if let Some(azure) = &self.azure {
            return azure.clone();
        }
        let non_empty = |value: &str| {
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        };
        AzureConfig {
            subscription_id: non_empty(&self.subscription_id),
            tenant_id: non_empty(&self.tenant_id),
            default_vault: non_empty(&self.default_vault),
            resource_group: non_empty(&self.default_resource_group),
            location: non_empty(&self.default_location),
        }
    }
```

In `src/config/setup.rs`, add `base.azure = None;` to `clear_backend_blocks`, and write the block in `apply_backend`'s Azure arm (after the `required(...)` checks, replacing the top-level-only writes):

```rust
            base.azure = Some(crate::config::settings::AzureConfig {
                subscription_id: Some(subscription_id.clone()),
                tenant_id: Some(tenant_id.clone()),
                default_vault: Some(vault.clone()),
                resource_group: Some(resource_group.clone()),
                location: Some(location.clone()),
            });
            base.subscription_id = subscription_id.clone();
            base.tenant_id = tenant_id.clone();
            base.default_resource_group = resource_group.clone();
            base.default_location = location.clone();
```

Import `AzureConfig` in `setup.rs`'s existing `use crate::config::settings::{...}` line.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib config::`
Expected: PASS, including the pre-existing `named_backends_deserializes_aws_entry` and all of `config::setup`.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/config/settings.rs src/config/setup.rs
git commit -m "feat(config): give Azure its own [azure] block so backends can coexist"
```

---

### Task 3: Put a `Prompter` seam under the interactive prompts

**Files:**
- Modify: `src/utils/interactive.rs`
- Modify: `src/config/init.rs:18-20` (the `ConfigInitializer` struct and `new`)
- Test: `src/utils/interactive.rs` (new `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pub trait Prompter` with `confirm(&self, message: &str, default: bool) -> Result<bool>`, `input_text(&self, message: &str, default: Option<&str>) -> Result<String>`, `select(&self, message: &str, options: &[String], default: Option<usize>) -> Result<usize>`. `InteractivePrompt` implements it. `pub struct ScriptedPrompter` (test-only, `#[cfg(any(test, feature = "test-support"))]`) with `ScriptedPrompter::new(answers: Vec<Answer>)` and `pub enum Answer { Confirm(bool), Text(String), Select(usize) }`.

Note: `input_text_validated` is deliberately **not** on the trait — it is generic over a closure, which is not object-safe. Callers that need validation keep using `InteractivePrompt` directly, or validate the returned `String` themselves.

- [ ] **Step 1: Write the failing test**

Add to `src/utils/interactive.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_prompter_returns_queued_answers_in_order() {
        let prompter = ScriptedPrompter::new(vec![
            Answer::Select(1),
            Answer::Text("my-vault".into()),
            Answer::Confirm(true),
        ]);

        assert_eq!(
            prompter
                .select("backend?", &["a".into(), "b".into()], Some(0))
                .unwrap(),
            1
        );
        assert_eq!(prompter.input_text("vault?", None).unwrap(), "my-vault");
        assert!(prompter.confirm("sure?", false).unwrap());
    }

    #[test]
    fn scripted_prompter_errors_when_the_script_runs_out() {
        let prompter = ScriptedPrompter::new(vec![]);
        assert!(prompter.confirm("sure?", false).is_err());
    }

    #[test]
    fn scripted_prompter_errors_on_answer_type_mismatch() {
        let prompter = ScriptedPrompter::new(vec![Answer::Text("oops".into())]);
        assert!(prompter.confirm("sure?", false).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib utils::interactive`
Expected: FAIL to compile — `cannot find struct 'ScriptedPrompter'`.

- [ ] **Step 3: Write minimal implementation**

In `src/utils/interactive.rs`:

```rust
/// The prompting surface used by setup flows.
///
/// Exists so `xv init` and `xv backend add` can be tested without a TTY —
/// mirroring `src/schedule/`, which is tested against a fake `CommandRunner`
/// so no test registers a real OS job. `input_text_validated` is absent
/// deliberately: it is generic over a closure and so not object-safe.
pub trait Prompter {
    fn confirm(&self, message: &str, default: bool) -> Result<bool>;
    fn input_text(&self, message: &str, default: Option<&str>) -> Result<String>;
    fn select(&self, message: &str, options: &[String], default: Option<usize>) -> Result<usize>;
}

impl Prompter for InteractivePrompt {
    fn confirm(&self, message: &str, default: bool) -> Result<bool> {
        InteractivePrompt::confirm(self, message, default)
    }
    fn input_text(&self, message: &str, default: Option<&str>) -> Result<String> {
        InteractivePrompt::input_text(self, message, default)
    }
    fn select(&self, message: &str, options: &[String], default: Option<usize>) -> Result<usize> {
        InteractivePrompt::select(self, message, options, default)
    }
}

/// A queued answer for [`ScriptedPrompter`].
#[cfg(test)]
#[derive(Debug, Clone)]
pub enum Answer {
    Confirm(bool),
    Text(String),
    Select(usize),
}

/// Test double that replays queued answers and records the prompts it saw.
#[cfg(test)]
pub struct ScriptedPrompter {
    answers: std::sync::Mutex<std::collections::VecDeque<Answer>>,
    pub seen: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl ScriptedPrompter {
    pub fn new(answers: Vec<Answer>) -> Self {
        Self {
            answers: std::sync::Mutex::new(answers.into()),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn next(&self, message: &str) -> Result<Answer> {
        self.seen.lock().unwrap().push(message.to_string());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| CrosstacheError::config(format!("scripted prompter exhausted at '{message}'")))
    }
}

#[cfg(test)]
impl Prompter for ScriptedPrompter {
    fn confirm(&self, message: &str, _default: bool) -> Result<bool> {
        match self.next(message)? {
            Answer::Confirm(value) => Ok(value),
            other => Err(CrosstacheError::config(format!(
                "expected Confirm at '{message}', got {other:?}"
            ))),
        }
    }
    fn input_text(&self, message: &str, _default: Option<&str>) -> Result<String> {
        match self.next(message)? {
            Answer::Text(value) => Ok(value),
            other => Err(CrosstacheError::config(format!(
                "expected Text at '{message}', got {other:?}"
            ))),
        }
    }
    fn select(&self, message: &str, _options: &[String], _default: Option<usize>) -> Result<usize> {
        match self.next(message)? {
            Answer::Select(value) => Ok(value),
            other => Err(CrosstacheError::config(format!(
                "expected Select at '{message}', got {other:?}"
            ))),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib utils::interactive`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/utils/interactive.rs
git commit -m "test(interactive): add a Prompter seam and scripted test double"
```

---

### Task 4: Extract the shared add-a-backend flow and route `xv init` through it

**Files:**
- Create: `src/config/backend_ops.rs`
- Modify: `src/config/mod.rs` (add `pub mod backend_ops;`)
- Modify: `src/config/init.rs` (`run_local_setup` / `run_aws_setup` call the shared flow)
- Test: `src/config/backend_ops.rs` (new test module)

**Interfaces:**
- Consumes: `apply_backend` (Task 1), `Config.azure` / `azure_settings` (Task 2), `Prompter` / `ScriptedPrompter` / `Answer` (Task 3).
- Produces:
  - `pub enum BackendType { Local, Azure, Aws }` with `fn as_str(&self) -> &'static str` returning `"local"` / `"azure"` / `"aws"`, and `impl std::str::FromStr`.
  - `pub fn configured_backends(config: &Config) -> Vec<BackendType>` — which blocks are populated.
  - `pub async fn add_backend(request: &SetupRequest, base: Config, make_active: bool) -> Result<Config>` — validates via `apply_backend`, initializes the backend for real, sets `Config.backend` plus the top-level mirror when `make_active`, and returns the new config **without** saving.

- [ ] **Step 1: Write the failing test**

Create `src/config/backend_ops.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::Config;

    fn local_request(root: &std::path::Path) -> SetupRequest {
        SetupRequest::Local {
            store_path: root.join("store"),
            key_file: root.join("key.txt"),
            vault: "default".into(),
        }
    }

    /// Adding a backend without making it active must not move the write
    /// target — the whole point of `xv backend add` versus `xv init`.
    #[tokio::test]
    async fn add_backend_without_make_active_leaves_the_active_backend_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut base = Config::default();
        base.backend = Some("aws".into());
        base.default_vault = "aws-vault".into();

        let config = add_backend(&local_request(dir.path()), base, false)
            .await
            .unwrap();

        assert_eq!(config.backend.as_deref(), Some("aws"));
        assert_eq!(config.default_vault, "aws-vault");
        assert!(config.local.is_some(), "local block was still written");
    }

    #[tokio::test]
    async fn add_backend_with_make_active_switches_the_write_target() {
        let dir = tempfile::tempdir().unwrap();
        let config = add_backend(&local_request(dir.path()), Config::default(), true)
            .await
            .unwrap();

        assert_eq!(config.backend.as_deref(), Some("local"));
        assert_eq!(config.default_vault, "default");
    }

    #[test]
    fn configured_backends_lists_only_populated_blocks() {
        let mut config = Config::default();
        assert!(configured_backends(&config).is_empty());

        config.local = Some(Default::default());
        assert_eq!(configured_backends(&config), vec![BackendType::Local]);
    }

    #[test]
    fn backend_type_round_trips_through_str() {
        for name in ["local", "azure", "aws"] {
            assert_eq!(name.parse::<BackendType>().unwrap().as_str(), name);
        }
        assert!("postgres".parse::<BackendType>().is_err());
    }

    /// A failed add must leave the caller's config exactly as it was, so
    /// nothing partial is ever written. Here the store path is a *file*, so
    /// creating the store directory fails during initialization.
    #[tokio::test]
    async fn failed_add_returns_err_and_never_mutates_the_callers_config() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("store");
        std::fs::write(&blocker, "not a directory").unwrap();

        let mut base = Config::default();
        base.backend = Some("aws".into());
        base.aws = Some(Default::default());
        let before = toml::to_string(&base).unwrap();

        let result = add_backend(
            &SetupRequest::Local {
                store_path: blocker,
                key_file: dir.path().join("key.txt"),
                vault: "default".into(),
            },
            base.clone(),
            true,
        )
        .await;

        assert!(result.is_err(), "a store path that is a file must fail");
        assert_eq!(
            toml::to_string(&base).unwrap(),
            before,
            "the caller's config must be untouched by a failed add"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config::backend_ops`
Expected: FAIL to compile — module not declared, `add_backend` / `BackendType` / `configured_backends` undefined.

- [ ] **Step 3: Write minimal implementation**

Add `pub mod backend_ops;` to `src/config/mod.rs`. Then at the top of `src/config/backend_ops.rs`:

```rust
//! Backend lifecycle shared by `xv init` and `xv backend add|rm|ls`.
//!
//! See `docs/superpowers/specs/2026-08-09-backend-lifecycle-design.md`.

use crate::config::settings::Config;
use crate::config::setup::{apply_backend, SetupRequest};
use crate::error::{CrosstacheError, Result};

/// The backend types `xv backend` manages. One instance per type; named
/// multi-instance backends (`named_backends`) are out of scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    Local,
    Azure,
    Aws,
}

impl BackendType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Azure => "azure",
            Self::Aws => "aws",
        }
    }

    pub const ALL: [BackendType; 3] = [BackendType::Local, BackendType::Azure, BackendType::Aws];
}

impl std::str::FromStr for BackendType {
    type Err = CrosstacheError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "local" => Ok(Self::Local),
            "azure" => Ok(Self::Azure),
            "aws" => Ok(Self::Aws),
            other => Err(CrosstacheError::invalid_argument(format!(
                "unknown backend '{other}'; expected one of: local, azure, aws"
            ))),
        }
    }
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which backend blocks are populated in `config`.
pub fn configured_backends(config: &Config) -> Vec<BackendType> {
    let mut found = Vec::new();
    if config.local.is_some() {
        found.push(BackendType::Local);
    }
    if config.azure.is_some() {
        found.push(BackendType::Azure);
    }
    if config.aws.is_some() {
        found.push(BackendType::Aws);
    }
    found
}

/// Validate `request`, stand the backend up for real, and fold it into `base`.
///
/// Returns the updated config **without** saving; the caller persists it. Any
/// failure returns `Err` with `base` unmodified, so a failed add writes nothing.
pub async fn add_backend(
    request: &SetupRequest,
    base: Config,
    make_active: bool,
) -> Result<Config> {
    let mut candidate = base;
    apply_backend(request, &mut candidate)?;
    initialize_backend(request, &candidate).await?;

    if make_active {
        match request {
            SetupRequest::Local { vault, .. } => {
                candidate.backend = Some("local".into());
                candidate.default_vault = vault.clone();
            }
            SetupRequest::Azure {
                vault,
                subscription_id,
                tenant_id,
                resource_group,
                location,
            } => {
                candidate.backend = None;
                candidate.default_vault = vault.clone();
                candidate.subscription_id = subscription_id.clone();
                candidate.tenant_id = tenant_id.clone();
                candidate.default_resource_group = resource_group.clone();
                candidate.default_location = location.clone();
            }
            SetupRequest::Aws { vault_prefix, .. } => {
                candidate.backend = Some("aws".into());
                candidate.default_vault = vault_prefix.clone();
            }
        }
        candidate.validate()?;
    }

    Ok(candidate)
}

/// Stand the backend up: create keys and directories, or probe credentials.
async fn initialize_backend(request: &SetupRequest, config: &Config) -> Result<()> {
    match request {
        SetupRequest::Local { .. } => {
            let local = config.local.as_ref().ok_or_else(|| {
                CrosstacheError::config("local setup did not produce a [local] block")
            })?;
            crate::backend::local::LocalBackend::new(Some(local)).map_err(|e| {
                CrosstacheError::config(format!("Failed to create local backend: {e}"))
            })?;
            Ok(())
        }
        // Azure and AWS validate their credentials lazily on first use; the
        // interactive flows already probe the CLI before building the request.
        SetupRequest::Azure { .. } | SetupRequest::Aws { .. } => Ok(()),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib config::backend_ops`
Expected: PASS, 4 tests.

- [ ] **Step 5: Route `xv init` through it**

In `src/config/init.rs`, `run_local_setup` currently calls `build_setup_config` then `LocalBackend::new`. Replace those two with the shared flow, loading any existing config as the base so init no longer discards other backends:

```rust
        let request = SetupRequest::Local {
            store_path: store_path.clone().into(),
            key_file: key_file.clone().into(),
            vault: default_vault.clone(),
        };
        let base = Config::load().await.unwrap_or_default();
        let progress = ProgressIndicator::new("Setting up local backend...");
        let config = crate::config::backend_ops::add_backend(&request, base, true).await?;
        progress.finish_success("Local backend initialized");
```

Apply the same substitution in `run_aws_setup`, replacing its `build_setup_config(&SetupRequest::Aws { .. }, Config::default())` call with `add_backend(&request, base, true).await?`.

Because `base` is now the loaded config rather than `Config::default()`, `xv init` must confirm before replacing an already-configured backend. Add this immediately after the backend selection in `run_interactive_setup`, before the per-backend flow runs:

```rust
        // Loading the existing config means init no longer silently discards
        // other backends — but it can still replace the selected one, so ask.
        let existing = Config::load().await.unwrap_or_default();
        if crate::config::backend_ops::configured_backends(&existing).contains(&chosen) {
            let proceed = self.prompt.confirm(
                &format!(
                    "Backend '{chosen}' is already configured; reconfigure it? \
                     Its existing settings will be replaced."
                ),
                false,
            )?;
            if !proceed {
                output::info("Aborted; no changes made.");
                return Ok(existing);
            }
        }
```

where `chosen` is the `BackendType` derived from the existing `backend_index` select (index 0 → `Azure`, 1 → `Local`, 2 → `Aws`, matching the current `backend_options` ordering at `src/config/init.rs:58-67`).

- [ ] **Step 6: Run the full suite**

Run: `cargo test --lib`
Expected: PASS. Then `cargo clippy -- -D warnings` — expected clean.

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add src/config/backend_ops.rs src/config/mod.rs src/config/init.rs
git commit -m "feat(config): extract the shared add-a-backend flow and route init through it"
```

---

### Task 5: `xv backend ls`

**Files:**
- Create: `src/cli/backend_ops.rs`
- Modify: `src/cli/mod.rs` (add `pub(crate) mod backend_ops;`)
- Modify: `src/cli/commands.rs` (add the `Backend` variant, the `BackendCommands` subcommand enum, and the dispatch arm beside `Commands::Init` at line 2377)
- Test: `tests/backend_cli_tests.rs`

**Interfaces:**
- Consumes: `BackendType`, `configured_backends` (Task 4).
- Produces: `pub(crate) async fn execute_backend_ls(config: Config) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Create `tests/backend_cli_tests.rs`:

```rust
//! `xv backend` CLI surface. Uses an isolated config dir so the developer's
//! real ~/.config/xv is never read (see the e2e host-isolation convention).

use std::process::Command;

fn xv(args: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_xv"))
        .args(args)
        .env("XDG_CONFIG_HOME", home)
        .env("HOME", home)
        // Pin the context store explicitly. Without this, ContextManager::load
        // checks `cwd/.xv/context` first and would read whatever context the
        // test process happens to be sitting next to.
        .env("XV_CONTEXT_DIR", home.join("xv"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("NO_COLOR", "1")
        .output()
        .expect("xv should run")
}

/// The context store path used by the `xv` helper above. Note there is no
/// `.json` extension — the file is literally named `context`.
fn context_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join("xv").join("context")
}

#[test]
fn backend_ls_reports_nothing_configured_on_a_fresh_config() {
    let home = tempfile::tempdir().unwrap();
    let out = xv(&["backend", "ls"], home.path());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("No backends configured"),
        "unexpected output: {text}"
    );
}

#[test]
fn backend_ls_lists_a_configured_local_backend_and_marks_it_active() {
    let home = tempfile::tempdir().unwrap();
    let conf_dir = home.path().join("xv");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("xv.conf"),
        r#"
backend = "local"
default_vault = "default"

[local]
store_path = "/tmp/xv-store"
key_file = "/tmp/xv-key.txt"
default_vault = "default"
"#,
    )
    .unwrap();

    let out = xv(&["backend", "ls"], home.path());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("local"), "should list local: {text}");
    assert!(
        text.contains("active"),
        "should mark the active backend: {text}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test backend_cli_tests`
Expected: FAIL — `xv backend` is an unknown subcommand, so stdout lacks the expected text.

- [ ] **Step 3: Write minimal implementation**

In `src/cli/commands.rs`, add beside the other top-level variants:

```rust
    /// Manage configured secret backends (add, remove, list)
    Backend {
        #[command(subcommand)]
        command: BackendCommands,
    },
```

And the subcommand enum next to the other `*Commands` enums:

```rust
#[derive(Subcommand, Debug)]
pub enum BackendCommands {
    /// List configured backends
    #[command(alias = "list")]
    Ls,
}
```

Dispatch beside `Commands::Init`:

```rust
            Commands::Backend { command } => match command {
                BackendCommands::Ls => {
                    crate::cli::backend_ops::execute_backend_ls(config).await
                }
            },
```

Create `src/cli/backend_ops.rs`:

```rust
//! `xv backend` — configured-backend lifecycle.

use crate::config::backend_ops::{configured_backends, BackendType};
use crate::config::settings::Config;
use crate::error::Result;
use crate::utils::output;

/// Where a backend's secrets live, for the `ls` listing.
fn location(backend: BackendType, config: &Config) -> String {
    match backend {
        BackendType::Local => config
            .local
            .as_ref()
            .and_then(|l| l.store_path.clone())
            .unwrap_or_else(|| "(default store path)".into()),
        BackendType::Azure => {
            let azure = config.azure_settings();
            azure
                .default_vault
                .unwrap_or_else(|| "(no vault configured)".into())
        }
        BackendType::Aws => config
            .aws
            .as_ref()
            .and_then(|a| a.region.clone())
            .unwrap_or_else(|| "(no region configured)".into()),
    }
}

pub(crate) async fn execute_backend_ls(config: Config) -> Result<()> {
    let configured = configured_backends(&config);
    if configured.is_empty() {
        output::info("No backends configured. Run `xv init` to configure one.");
        return Ok(());
    }

    let active = config.effective_backend_name();
    for backend in configured {
        let marker = if backend.as_str() == active {
            "  (active)"
        } else {
            ""
        };
        println!("{}\t{}{marker}", backend.as_str(), location(backend, &config));
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test backend_cli_tests`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/cli/backend_ops.rs src/cli/mod.rs src/cli/commands.rs tests/backend_cli_tests.rs
git commit -m "feat(cli): add xv backend ls"
```

---

### Task 6: `xv backend add`

**Files:**
- Modify: `src/cli/commands.rs` (add the `Add` variant to `BackendCommands` and its dispatch arm)
- Modify: `src/cli/backend_ops.rs`
- Modify: `src/config/init.rs` (expose the per-backend prompt flows for reuse)
- Test: `tests/backend_cli_tests.rs`

**Interfaces:**
- Consumes: `add_backend`, `BackendType`, `configured_backends` (Task 4); `execute_backend_ls` (Task 5).
- Produces: `pub(crate) async fn execute_backend_add(backend: String, yes: bool, config: Config) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Add to `tests/backend_cli_tests.rs`:

```rust
#[test]
fn backend_add_rejects_an_unknown_backend_name() {
    let home = tempfile::tempdir().unwrap();
    let out = xv(&["backend", "add", "postgres"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "unknown backend must fail");
    assert!(
        text.contains("local, azure, aws"),
        "error should list the valid backends: {text}"
    );
}

#[test]
fn backend_add_refuses_to_reconfigure_without_confirmation_in_non_tty() {
    let home = tempfile::tempdir().unwrap();
    let conf_dir = home.path().join("xv");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("xv.conf"),
        r#"
backend = "local"
default_vault = "default"

[local]
store_path = "/tmp/xv-store"
key_file = "/tmp/xv-key.txt"
default_vault = "default"
"#,
    )
    .unwrap();

    let out = xv(&["backend", "add", "local"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "reconfigure needs confirmation");
    assert!(text.contains("--yes"), "should name the skip flag: {text}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test backend_cli_tests::backend_add_rejects_an_unknown_backend_name`
Expected: FAIL — `add` is not a `BackendCommands` variant.

- [ ] **Step 3: Write minimal implementation**

In `src/cli/commands.rs`, extend `BackendCommands`:

```rust
    /// Configure a backend (local | azure | aws)
    Add {
        /// Backend type to configure
        backend: String,
        /// Skip the confirmation when reconfiguring an already-configured backend
        #[arg(long)]
        yes: bool,
    },
```

Dispatch:

```rust
                BackendCommands::Add { backend, yes } => {
                    crate::cli::backend_ops::execute_backend_add(backend, yes, config).await
                }
```

In `src/cli/backend_ops.rs`:

```rust
use crate::cli::helpers::confirm_proceed;
use crate::config::backend_ops::add_backend;
use crate::config::setup::atomic_save_config;

pub(crate) async fn execute_backend_add(
    backend: String,
    yes: bool,
    config: Config,
) -> Result<()> {
    let backend: BackendType = backend.parse()?;

    if configured_backends(&config).contains(&backend) {
        let prompt = format!(
            "Backend '{backend}' is already configured; reconfigure it? \
             Its existing settings will be replaced."
        );
        if !confirm_proceed(yes, &prompt, "--yes")? {
            output::info("Aborted; no changes made.");
            return Ok(());
        }
    }

    let initializer = crate::config::init::ConfigInitializer::new();
    let request = initializer.collect_backend_request(backend).await?;

    // `xv backend add` never moves the write target — that is `xv init`'s job.
    let updated = add_backend(&request, config, false).await?;
    atomic_save_config(&updated, &Config::get_config_path()?).await?;

    output::success(&format!("Configured backend '{backend}'"));
    output::info(&format!(
        "It is not the active backend. Switch with `xv config set backend {backend}`, \
         or attach one of its vaults with `xv cx add <vault> --backend {backend}`."
    ));
    Ok(())
}
```

In `src/config/init.rs`, expose the prompting that already exists, so `add` and `init` share it. Add to `impl ConfigInitializer`:

```rust
    /// Collect the interactive answers for one backend, producing a
    /// `SetupRequest`. Pure prompting: no files are written and no backend is
    /// contacted, so the caller decides whether to apply the result.
    pub async fn collect_backend_request(
        &self,
        backend: crate::config::backend_ops::BackendType,
    ) -> Result<SetupRequest> {
        use crate::config::backend_ops::BackendType;
        match backend {
            BackendType::Local => self.collect_local_request(),
            BackendType::Aws => self.collect_aws_request().await,
            BackendType::Azure => self.collect_azure_request().await,
        }
    }
```

Extract three collectors, each pure prompting with no writes. `collect_local_request` is the model — move the three prompt blocks currently at the top of `run_local_setup` (`src/config/init.rs:162-190`) verbatim into it:

```rust
    /// Prompt for the local backend's settings. No files are created.
    fn collect_local_request(&self) -> Result<SetupRequest> {
        println!();
        output::step("Step 1/3: Store Location");
        let default_store = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".xv")
            .join("store");
        let store_path = self.prompt.input_text(
            "Store path for encrypted secrets",
            Some(&default_store.to_string_lossy()),
        )?;

        println!();
        output::step("Step 2/3: Key File");
        let default_key = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".xv")
            .join("key.txt");
        let key_file = self
            .prompt
            .input_text("Age key file path", Some(&default_key.to_string_lossy()))?;

        println!();
        output::step("Step 3/3: Default Vault");
        let default_vault = self
            .prompt
            .input_text("Default vault name", Some("default"))?;

        Ok(SetupRequest::Local {
            store_path: store_path.into(),
            key_file: key_file.into(),
            vault: default_vault,
        })
    }
```

Apply the same treatment to the other two:

- `async fn collect_aws_request(&self) -> Result<SetupRequest>` — move the three `Input::new()` prompts from `init_aws_backend` (`src/config/init.rs:296-329`) and return `SetupRequest::Aws { region, profile: Some(profile), vault_prefix: default_vault }` instead of mutating an `InitConfig`.
- `async fn collect_azure_request(&self) -> Result<SetupRequest>` — move steps 1–6 of `run_interactive_setup` (`src/config/init.rs:79-131`: detect environment, subscription, resource group, location, resource-group creation, blob storage, vault) and return `SetupRequest::Azure { subscription_id, tenant_id, vault, resource_group, location }`. Keep the resource-group creation and blob-storage side effects where they are — they are Azure provisioning, not config writes, and moving them would change behavior.

`run_local_setup`, `run_aws_setup`, and the Azure branch of `run_interactive_setup` then each become: call the collector, call `add_backend(&request, base, true).await?`, save, print the summary. The `InitConfig` struct's `backend_choice` / `aws_*` fields become dead once all three are converted — delete them and any now-unused construction sites.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test backend_cli_tests && cargo test --lib config::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/cli/backend_ops.rs src/cli/commands.rs src/config/init.rs tests/backend_cli_tests.rs
git commit -m "feat(cli): add xv backend add"
```

---

### Task 7: `xv backend rm` (config-only, with refusals)

**Files:**
- Modify: `src/cli/commands.rs` (add `Rm` to `BackendCommands` and dispatch)
- Modify: `src/cli/backend_ops.rs`
- Test: `tests/backend_cli_tests.rs`

**Interfaces:**
- Consumes: `BackendType`, `configured_backends` (Task 4).
- Produces: `pub(crate) async fn execute_backend_rm(backend: String, purge: bool, yes: bool, config: Config) -> Result<()>`. Task 8 adds the `purge` behavior; this task accepts the flag and rejects it for non-local backends.

- [ ] **Step 1: Write the failing test**

Add to `tests/backend_cli_tests.rs`:

```rust
/// Writes a config with both local and aws configured, local active.
fn two_backend_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let conf_dir = home.path().join("xv");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("xv.conf"),
        r#"
backend = "local"
default_vault = "default"

[local]
store_path = "/tmp/xv-store"
key_file = "/tmp/xv-key.txt"
default_vault = "default"

[aws]
region = "us-east-1"
default_vault = "default"
"#,
    )
    .unwrap();
    home
}

#[test]
fn backend_rm_refuses_to_remove_the_active_backend_when_others_remain() {
    let home = two_backend_home();
    let out = xv(&["backend", "rm", "local"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "removing the active backend must fail");
    assert!(
        text.contains("xv config set backend"),
        "should say how to switch: {text}"
    );

    let saved = std::fs::read_to_string(home.path().join("xv/xv.conf")).unwrap();
    assert!(saved.contains("[local]"), "config must be untouched: {saved}");
}

#[test]
fn backend_rm_removes_an_inactive_backend() {
    let home = two_backend_home();
    let out = xv(&["backend", "rm", "aws", "--yes"], home.path());
    assert!(
        out.status.success(),
        "removing an inactive backend should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let saved = std::fs::read_to_string(home.path().join("xv/xv.conf")).unwrap();
    assert!(!saved.contains("[aws]"), "aws block should be gone: {saved}");
    assert!(saved.contains("[local]"), "local must survive: {saved}");
}

#[test]
fn backend_rm_errors_when_the_backend_is_not_configured() {
    let home = two_backend_home();
    let out = xv(&["backend", "rm", "azure", "--yes"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    assert!(
        text.contains("local") && text.contains("aws"),
        "should name what is configured: {text}"
    );
}

#[test]
fn backend_rm_drops_workspace_entries_for_the_removed_backend() {
    let home = two_backend_home();
    // Two attached vaults: the local one is the workspace default, the aws
    // one is not — so removing aws must not trip the default-stranding guard.
    std::fs::write(
        context_path(home.path()),
        r#"{
  "workspace": {
    "entries": [
      {"vault": "default", "backend": "local", "alias": "home", "default": true},
      {"vault": "default", "backend": "aws", "alias": "work"}
    ]
  }
}"#,
    )
    .unwrap();

    let out = xv(&["backend", "rm", "aws", "--yes"], home.path());
    assert!(
        out.status.success(),
        "should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let ctx = std::fs::read_to_string(context_path(home.path())).unwrap();
    assert!(!ctx.contains("\"work\""), "aws entry should be gone: {ctx}");
    assert!(ctx.contains("\"home\""), "local entry must survive: {ctx}");
}

#[test]
fn backend_rm_refuses_when_removal_would_strand_the_workspace_default() {
    let home = two_backend_home();
    // Here the *aws* entry is the workspace default, and a local entry
    // survives — so removing aws would leave the workspace without a default.
    std::fs::write(
        context_path(home.path()),
        r#"{
  "workspace": {
    "entries": [
      {"vault": "default", "backend": "local", "alias": "home"},
      {"vault": "default", "backend": "aws", "alias": "work", "default": true}
    ]
  }
}"#,
    )
    .unwrap();

    let out = xv(&["backend", "rm", "aws", "--yes"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "must refuse to strand the default");
    assert!(
        text.contains("xv cx default"),
        "should say how to fix it: {text}"
    );

    let saved = std::fs::read_to_string(home.path().join("xv/xv.conf")).unwrap();
    assert!(saved.contains("[aws]"), "config must be untouched: {saved}");
}

#[test]
fn backend_rm_rejects_purge_for_non_local_backends() {
    let home = two_backend_home();
    let out = xv(&["backend", "rm", "aws", "--purge", "--yes"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "--purge is local-only");
    assert!(text.contains("local"), "should explain the restriction: {text}");

    let saved = std::fs::read_to_string(home.path().join("xv/xv.conf")).unwrap();
    assert!(saved.contains("[aws]"), "nothing removed on refusal: {saved}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test backend_cli_tests::backend_rm_removes_an_inactive_backend`
Expected: FAIL — `rm` is not a `BackendCommands` variant.

- [ ] **Step 3: Write minimal implementation**

In `src/cli/commands.rs`, extend `BackendCommands`:

```rust
    /// Remove a configured backend (config only unless --purge)
    #[command(alias = "remove")]
    Rm {
        /// Backend type to remove
        backend: String,
        /// Also delete the local store and age key. Local backend only.
        /// This destroys every secret in that store permanently.
        #[arg(long)]
        purge: bool,
        /// Skip confirmation prompts
        #[arg(long)]
        yes: bool,
    },
```

Dispatch:

```rust
                BackendCommands::Rm {
                    backend,
                    purge,
                    yes,
                } => crate::cli::backend_ops::execute_backend_rm(backend, purge, yes, config).await,
```

In `src/cli/backend_ops.rs`:

```rust
use crate::config::context::ContextManager;
use crate::error::CrosstacheError;

pub(crate) async fn execute_backend_rm(
    backend: String,
    purge: bool,
    yes: bool,
    config: Config,
) -> Result<()> {
    let backend: BackendType = backend.parse()?;
    let configured = configured_backends(&config);

    if !configured.contains(&backend) {
        let names: Vec<&str> = configured.iter().map(|b| b.as_str()).collect();
        let listed = if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        };
        return Err(CrosstacheError::invalid_argument(format!(
            "backend '{backend}' is not configured; configured backends: {listed}"
        )));
    }

    if purge && backend != BackendType::Local {
        return Err(CrosstacheError::invalid_argument(format!(
            "--purge deletes an on-disk store and applies to the local backend only; \
             '{backend}' stores its secrets remotely. Remove the configuration with \
             `xv backend rm {backend}`, and delete remote data with `xv vault delete`."
        )));
    }

    // Refuse to silently relocate the write target.
    if config.effective_backend_name() == backend.as_str() && configured.len() > 1 {
        let others: Vec<&str> = configured
            .iter()
            .filter(|b| **b != backend)
            .map(|b| b.as_str())
            .collect();
        return Err(CrosstacheError::invalid_argument(format!(
            "'{backend}' is the active backend; switch first with \
             `xv config set backend {}`, then `xv backend rm {backend}`",
            others[0]
        )));
    }

    // Refuse when removal would strand the workspace's write target. Mirrors
    // `execute_cx_rm` in src/cli/config_ops.rs.
    let mut context_manager = ContextManager::load().await?;
    if let Some(ws) = context_manager.workspace.clone() {
        let doomed: Vec<_> = ws
            .entries
            .iter()
            .filter(|e| e.backend.as_deref() == Some(backend.as_str()))
            .collect();
        let removes_default = doomed.iter().any(|e| e.default);
        let survivors = ws.entries.len() - doomed.len();
        if removes_default && survivors > 0 {
            return Err(CrosstacheError::invalid_argument(format!(
                "removing '{backend}' would remove the workspace's default vault; \
                 choose a new default with `xv cx default <alias>` first"
            )));
        }
    }

    if !confirm_proceed(
        yes,
        &format!("Remove backend '{backend}' from the configuration?"),
        "--yes",
    )? {
        output::info("Aborted; no changes made.");
        return Ok(());
    }

    let mut updated = config;
    let data_location = match backend {
        BackendType::Local => {
            let where_it_lives = updated
                .local
                .as_ref()
                .and_then(|l| l.store_path.clone())
                .unwrap_or_else(|| "(default store path)".into());
            updated.local = None;
            where_it_lives
        }
        BackendType::Azure => {
            updated.azure = None;
            updated.subscription_id.clear();
            updated.tenant_id.clear();
            updated.default_resource_group.clear();
            updated.default_location.clear();
            "Azure Key Vault (unchanged)".to_string()
        }
        BackendType::Aws => {
            updated.aws = None;
            "AWS Secrets Manager (unchanged)".to_string()
        }
    };

    if updated.effective_backend_name() == backend.as_str() {
        updated.backend = None;
    }

    // Drop workspace entries that pointed at the removed backend.
    if let Some(mut ws) = context_manager.workspace.clone() {
        let before = ws.entries.len();
        ws.entries
            .retain(|e| e.backend.as_deref() != Some(backend.as_str()));
        if ws.entries.len() != before {
            context_manager.workspace = if ws.entries.is_empty() { None } else { Some(ws) };
            context_manager.save().await?;
        }
    }

    atomic_save_config(&updated, &Config::get_config_path()?).await?;
    output::success(&format!("Removed backend '{backend}' from the configuration"));
    output::info(&format!("Data was not deleted; it remains at: {data_location}"));
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test backend_cli_tests`
Expected: PASS, all tests including the 4 new ones.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/cli/backend_ops.rs src/cli/commands.rs tests/backend_cli_tests.rs
git commit -m "feat(cli): add xv backend rm with fail-closed refusals"
```

---

### Task 8: `--purge` for the local backend

**Files:**
- Modify: `src/cli/backend_ops.rs`
- Test: `tests/backend_cli_tests.rs`

**Interfaces:**
- Consumes: `execute_backend_rm` (Task 7).
- Produces: no new public API; adds purge behavior inside `execute_backend_rm`.

- [ ] **Step 1: Write the failing test**

Add to `tests/backend_cli_tests.rs`:

```rust
/// A store directory that looks like a real xv store, so the safety check
/// accepts it.
fn make_store(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let store = root.join("store");
    std::fs::create_dir_all(store.join("vaults")).unwrap();
    let key = root.join("key.txt");
    std::fs::write(&key, "AGE-SECRET-KEY-TEST\n").unwrap();
    (store, key)
}

#[test]
fn backend_rm_purge_deletes_the_store_and_key() {
    let home = tempfile::tempdir().unwrap();
    let (store, key) = make_store(home.path());
    let conf_dir = home.path().join("xv");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("xv.conf"),
        format!(
            "backend = \"local\"\ndefault_vault = \"default\"\n\n\
             [local]\nstore_path = {:?}\nkey_file = {:?}\ndefault_vault = \"default\"\n",
            store, key
        ),
    )
    .unwrap();

    let out = xv(&["backend", "rm", "local", "--purge", "--yes"], home.path());
    assert!(
        out.status.success(),
        "purge should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!store.exists(), "store directory should be deleted");
    assert!(!key.exists(), "age key should be deleted");
}

#[test]
fn backend_rm_purge_refuses_a_store_path_that_is_not_an_xv_store() {
    let home = tempfile::tempdir().unwrap();
    let bogus = home.path().join("important-documents");
    std::fs::create_dir_all(&bogus).unwrap();
    std::fs::write(bogus.join("taxes.pdf"), "keep me").unwrap();

    let conf_dir = home.path().join("xv");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("xv.conf"),
        format!(
            "backend = \"local\"\ndefault_vault = \"default\"\n\n\
             [local]\nstore_path = {:?}\ndefault_vault = \"default\"\n",
            bogus
        ),
    )
    .unwrap();

    let out = xv(&["backend", "rm", "local", "--purge", "--yes"], home.path());
    assert!(!out.status.success(), "must refuse a non-store path");
    assert!(bogus.join("taxes.pdf").exists(), "must not delete anything");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test backend_cli_tests::backend_rm_purge_deletes_the_store_and_key`
Expected: FAIL — the store still exists, because `--purge` is accepted but does nothing.

- [ ] **Step 3: Write minimal implementation**

In `src/cli/backend_ops.rs`, add the guard and the deletion, calling it from `execute_backend_rm` after the confirmation and before `atomic_save_config`:

```rust
/// A directory is only purgeable when it looks like an xv store: it must
/// contain a `vaults/` child (or be empty). This is the guard against a
/// misconfigured `store_path` pointing at `$HOME` or a documents folder.
fn assert_looks_like_store(store: &std::path::Path) -> Result<()> {
    if !store.exists() {
        return Ok(());
    }
    let has_vaults = store.join("vaults").is_dir();
    let is_empty = std::fs::read_dir(store)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    if has_vaults || is_empty {
        return Ok(());
    }
    Err(CrosstacheError::invalid_argument(format!(
        "refusing to purge {}: it does not look like an xv store (no 'vaults/' directory). \
         Check [local].store_path in your config.",
        store.display()
    )))
}

/// Delete the local store and age key. Destroys every secret in the store.
fn purge_local_store(config: &Config) -> Result<()> {
    let resolved =
        crate::backend::local::config::ResolvedLocalConfig::from_raw(config.local.as_ref());
    assert_looks_like_store(&resolved.store_path)?;

    if resolved.store_path.exists() {
        std::fs::remove_dir_all(&resolved.store_path).map_err(|e| {
            CrosstacheError::config(format!(
                "failed to delete store {}: {e}",
                resolved.store_path.display()
            ))
        })?;
    }
    if resolved.key_file.exists() {
        std::fs::remove_file(&resolved.key_file).map_err(|e| {
            CrosstacheError::config(format!(
                "failed to delete key file {}: {e}",
                resolved.key_file.display()
            ))
        })?;
    }
    Ok(())
}
```

Wire it in, replacing the plain confirmation for the purge case so the prompt states the stakes:

```rust
    if purge {
        let resolved =
            crate::backend::local::config::ResolvedLocalConfig::from_raw(config.local.as_ref());
        let prompt = format!(
            "PERMANENTLY DELETE every secret in {} and the age key at {}? \
             This cannot be undone — the key is deleted too, so the data is unrecoverable.",
            resolved.store_path.display(),
            resolved.key_file.display()
        );
        if !confirm_proceed(yes, &prompt, "--yes")? {
            output::info("Aborted; nothing deleted.");
            return Ok(());
        }
        purge_local_store(&config)?;
    } else if !confirm_proceed(
        yes,
        &format!("Remove backend '{backend}' from the configuration?"),
        "--yes",
    )? {
        output::info("Aborted; no changes made.");
        return Ok(());
    }
```

Adjust the closing message so purge does not claim the data survived:

```rust
    if purge {
        output::success(&format!(
            "Removed backend '{backend}' and deleted its store and key"
        ));
    } else {
        output::success(&format!("Removed backend '{backend}' from the configuration"));
        output::info(&format!("Data was not deleted; it remains at: {data_location}"));
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test backend_cli_tests`
Expected: PASS, all tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/cli/backend_ops.rs tests/backend_cli_tests.rs
git commit -m "feat(cli): add xv backend rm --purge with store-shape guard"
```

---

### Task 9: Documentation

**Files:**
- Create: `docs/backends.md`
- Modify: `README.md` (add `xv backend` to the command overview)
- Modify: `CHANGELOG.md` (add an `## Unreleased` section)
- Modify: `CLAUDE.md` (add to the implementation-status list)

- [ ] **Step 1: Write `docs/backends.md`**

Cover: the three backend types; `xv init` versus `xv backend add`; that one instance per type is supported and `named_backends` is for advanced multi-instance use; the `[azure]` block and its fallback to top-level fields; the full `rm` refusal table from the spec; and a prominent warning that `--purge` deletes the age key and is therefore unrecoverable.

- [ ] **Step 2: Update `README.md`, `CHANGELOG.md`, and `CLAUDE.md`**

In `CHANGELOG.md`, add above the `## v0.36.2` heading:

```markdown
## Unreleased

### Added

- **Backends can now be added and removed after setup.** `xv backend add`,
  `xv backend rm`, and `xv backend ls` manage configured backends, so a local
  store and a cloud vault can coexist instead of `xv init` replacing whichever
  was there. `xv backend rm` is config-only by default; `--purge` additionally
  deletes the local store and age key.
- **Azure has an `[azure]` config block.** Configs written before this change
  keep working unchanged — the top-level Azure fields remain the fallback.
```

- [ ] **Step 3: Run the full suite**

Run: `cargo test && cargo clippy -- -D warnings && cargo fmt --check`
Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add docs/backends.md README.md CHANGELOG.md CLAUDE.md
git commit -m "docs: document the xv backend lifecycle commands"
```

---

## Notes for the implementer

- **Task 1 is the one to get right.** Every arm of `build_setup_config` clears its siblings today; if `apply_backend` keeps any of that, `xv backend add aws` silently deletes a user's local configuration. The additive test in Task 1 is the guard.
- **Do not edit the pre-existing tests in `src/config/setup.rs`.** They passing unmodified is the proof that the desktop setup service is unaffected. If one fails, the split is wrong.
- **`ContextManager::load()`** is at `src/config/context.rs:114`; `save()` at line 232. `execute_cx_rm` at `src/cli/config_ops.rs:1664` is the precedent for workspace-entry removal and for refusing to strand the default.
- **Host isolation in CLI tests is mandatory.** Always set `XDG_CONFIG_HOME`, `HOME`, and `XV_NO_PARENT_CONFIG=1`, or the tests read the developer's real config and pass locally while failing in CI.
