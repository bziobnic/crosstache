//! End-to-end **authenticated** tests for the AWS Secrets Manager backend.
//!
//! Unlike `aws_backend_tests.rs` (hermetic, mocked) and `aws_localstack_tests.rs`
//! (LocalStack), these tests exercise `AwsBackend` against the **real AWS
//! Secrets Manager API** using the credentials already configured for the
//! `aws` CLI (default credential chain — env vars, `~/.aws/credentials`,
//! SSO, instance profile, …).
//!
//! Requirements:
//! - A working AWS identity (`aws sts get-caller-identity` succeeds)
//! - Permission to create/read/update/delete secrets in Secrets Manager
//! - Region `us-east-1` (override with `AWS_REGION`)
//! - Internet connection
//!
//! Run with:
//!   cargo test --features aws --test e2e_aws_backend -- --ignored --nocapture --test-threads=1
//!
//! Safety:
//! - Every test uses a unique, timestamped vault prefix (`xv-e2e-aws-<ts>`),
//!   so created secrets never collide with anything else in the account.
//! - All secrets created here are **force-purged** during cleanup.
//! - The pre-existing live secret `claude-api-key` lives at the account root
//!   (no `vault/` prefix), so the prefix-scoped operations below never see,
//!   touch, or delete it.

#![cfg(feature = "aws")]

use crosstache::backend::aws::AwsBackend;
use crosstache::backend::Backend;
use crosstache::config::settings::AwsConfig;
use crosstache::secret::manager::{FieldUpdate, SecretRequest, SecretUpdateRequest};
use crosstache::vault::models::VaultCreateRequest;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

/// The live, pre-existing secret that tests must never disturb.
const PROTECTED_SECRET: &str = "claude-api-key";

fn aws_region() -> String {
    std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_string())
}

/// Bridge AWS CLI credentials into the process environment so the Rust
/// `aws-config` default chain can pick them up.
///
/// AWS CLI v2.22+ introduced the `aws login` sign-in flow, which caches
/// credentials under `~/.aws/login/cache/` — a format the SDK credential
/// chain does **not** understand. `aws configure export-credentials`
/// resolves whatever the CLI is using (SSO, `aws login`, static keys,
/// assumed roles, …) and emits plain env-var assignments. We parse those
/// and set them on the current process before building the SDK client.
///
/// This is idempotent and a no-op if standard env credentials already exist.
fn bridge_aws_cli_credentials() {
    if std::env::var("AWS_ACCESS_KEY_ID").is_ok() && std::env::var("AWS_SESSION_TOKEN").is_ok() {
        return; // already populated (e.g. by CI)
    }
    let output = std::process::Command::new("aws")
        .args(["configure", "export-credentials", "--format", "env"])
        .output()
        .expect("failed to run `aws configure export-credentials` — is the aws CLI installed?");
    assert!(
        output.status.success(),
        "`aws configure export-credentials` failed (is the aws CLI authenticated?): {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        // Lines look like: `export AWS_ACCESS_KEY_ID=AKIA...`
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if let Some((key, value)) = line.split_once('=') {
            if key.starts_with("AWS_") {
                std::env::set_var(key.trim(), value.trim());
            }
        }
    }
}

/// Build an `AwsBackend` from the default credential chain, bridging the
/// AWS CLI's credentials into the environment first.
async fn aws_backend() -> AwsBackend {
    bridge_aws_cli_credentials();
    let cfg = AwsConfig {
        region: Some(aws_region()),
        ..Default::default()
    };
    AwsBackend::new(
        &cfg,
        None,
        None,
        crosstache::backend::aws::TransferConfig::default(),
    )
    .await
    .expect("failed to build AwsBackend — is the aws CLI authenticated?")
}

/// Unique vault prefix for one test run.
fn unique_vault(tag: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("xv-e2e-aws-{tag}-{ts}")
}

fn make_request(name: &str, value: &str) -> SecretRequest {
    SecretRequest {
        name: name.to_string(),
        value: Zeroizing::new(value.to_string()),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    }
}

/// Poll `list_secrets` until `predicate` holds or the attempt budget is
/// exhausted. AWS Secrets Manager `ListSecrets` is *eventually consistent* —
/// a secret (or vault marker) created via `CreateSecret` can take a few
/// seconds to surface in `ListSecrets`. Returns the final secret-name vec.
async fn poll_list_secrets<F>(
    backend: &AwsBackend,
    vault: &str,
    predicate: F,
    what: &str,
) -> Vec<String>
where
    F: Fn(&[String]) -> bool,
{
    const MAX_ATTEMPTS: u32 = 12;
    let mut names = Vec::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let listed = backend
            .secrets()
            .list_secrets(vault, None)
            .await
            .expect("list_secrets should succeed");
        names = listed.into_iter().map(|s| s.name).collect();
        if predicate(&names) {
            return names;
        }
        if attempt < MAX_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    panic!("list_secrets never satisfied condition ({what}) after {MAX_ATTEMPTS} attempts; last seen: {names:?}");
}

/// Poll `list_vaults` until `vault` appears (eventual consistency, as above).
async fn poll_list_vaults_contains(backend: &AwsBackend, vault: &str) {
    const MAX_ATTEMPTS: u32 = 12;
    let vaults = backend.vaults().expect("AWS backend exposes vaults");
    for attempt in 1..=MAX_ATTEMPTS {
        let all = vaults
            .list_vaults(None)
            .await
            .expect("list_vaults should succeed");
        if all.iter().any(|v| v.name == vault) {
            return;
        }
        if attempt < MAX_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    panic!("vault '{vault}' never appeared in list_vaults after {MAX_ATTEMPTS} attempts");
}

/// Best-effort cleanup: force-purge every named secret, then drop the vault marker.
async fn cleanup(backend: &AwsBackend, vault: &str, secrets: &[&str]) {
    for name in secrets {
        let _ = backend.secrets().purge_secret(vault, name).await;
    }
    // Give AWS a moment so the marker-only delete_vault sees an empty prefix.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    if let Some(vaults) = backend.vaults() {
        let _ = vaults.delete_vault(vault, None).await;
    }
}

#[tokio::test]
#[ignore]
async fn e2e_aws_health_check() {
    let backend = aws_backend().await;
    backend
        .health_check()
        .await
        .expect("AWS health check (list_secrets) should succeed against live API");
}

#[tokio::test]
#[ignore]
async fn e2e_aws_secret_full_lifecycle() {
    let backend = aws_backend().await;
    let vault = unique_vault("lc");
    let secret = "db-password";

    // --- VAULT CREATE (writes the marker secret) ---
    let vaults = backend.vaults().expect("AWS backend exposes vaults");
    vaults
        .create_vault(VaultCreateRequest {
            name: vault.clone(),
            ..Default::default()
        })
        .await
        .expect("create_vault should succeed");

    // --- SET (create) ---
    let v1_value = "secret-value-v1";
    let r1 = backend
        .secrets()
        .set_secret(&vault, make_request(secret, v1_value))
        .await
        .expect("set_secret (create) should succeed");
    assert_eq!(r1.name, secret);
    let v1_version = r1.version.clone();
    assert!(!v1_version.is_empty(), "create should return a version id");

    // --- GET (with value) ---
    let got = backend
        .secrets()
        .get_secret(&vault, secret, true)
        .await
        .expect("get_secret with value should succeed");
    assert_eq!(
        got.value.as_ref().map(|v| v.as_str()),
        Some(v1_value),
        "round-tripped value must match"
    );

    // --- GET (metadata only) ---
    let meta = backend
        .secrets()
        .get_secret(&vault, secret, false)
        .await
        .expect("get_secret metadata-only should succeed");
    assert_eq!(meta.name, secret);
    assert!(
        meta.value.is_none(),
        "value must be absent when include_value=false"
    );

    // --- EXISTS ---
    assert!(
        backend
            .secrets()
            .secret_exists(&vault, secret)
            .await
            .expect("secret_exists should succeed"),
        "secret should exist after creation"
    );

    // --- LIST (excludes the vault marker) — eventually consistent ---
    let names = poll_list_secrets(
        &backend,
        &vault,
        |names| names.iter().any(|n| n == secret),
        "secret appears in list",
    )
    .await;
    assert!(
        names.contains(&secret.to_string()),
        "list should include our secret, got: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with(".xv")),
        "list must exclude the vault marker, got: {names:?}"
    );

    // --- SET (update → new version) ---
    let v2_value = "secret-value-v2";
    let r2 = backend
        .secrets()
        .set_secret(&vault, make_request(secret, v2_value))
        .await
        .expect("set_secret (update) should succeed");
    assert_ne!(
        r2.version, v1_version,
        "update should produce a new version id"
    );
    let got2 = backend
        .secrets()
        .get_secret(&vault, secret, true)
        .await
        .expect("get after update should succeed");
    assert_eq!(got2.value.as_ref().map(|v| v.as_str()), Some(v2_value));

    // --- LIST VERSIONS ---
    let versions = backend
        .secrets()
        .list_versions(&vault, secret)
        .await
        .expect("list_versions should succeed");
    assert!(
        versions.len() >= 2,
        "expected at least 2 versions after one update, got {}",
        versions.len()
    );

    // --- UPDATE METADATA (note + multiple groups) ---
    // Groups are stored in the AWS `xv:groups` tag using a `+` separator
    // (commas are rejected by the AWS tagging service), so multi-group
    // updates round-trip correctly.
    let update = SecretUpdateRequest {
        name: secret.to_string(),
        expected_revision: None,
        value: None,
        content_type: None,
        enabled: None,
        expires_on: FieldUpdate::Unchanged,
        not_before: FieldUpdate::Unchanged,
        tags: None,
        groups: Some(vec!["e2e".to_string(), "prod".to_string()]),
        note: FieldUpdate::Set("updated by e2e test".to_string()),
        folder: FieldUpdate::Unchanged,
        replace_tags: false,
        replace_groups: true,
    };
    backend
        .secrets()
        .update_secret(&vault, secret, update)
        .await
        .expect("update_secret should succeed");
    let after_meta = backend
        .secrets()
        .get_secret(&vault, secret, false)
        .await
        .expect("get_secret after metadata update should succeed");
    assert_eq!(
        after_meta.tags.get("note").map(String::as_str),
        Some("updated by e2e test"),
        "note should be persisted, tags: {:?}",
        after_meta.tags
    );
    // Both groups must round-trip — the AWS tag is exposed back to callers
    // as the canonical comma-separated form.
    let groups = after_meta
        .tags
        .get("groups")
        .map(String::as_str)
        .unwrap_or("");
    assert!(
        groups.split(',').any(|g| g == "e2e") && groups.split(',').any(|g| g == "prod"),
        "both groups should round-trip, got groups tag: {groups:?}"
    );

    // --- ROLLBACK (AWSCURRENT → v1) ---
    backend
        .secrets()
        .rollback(&vault, secret, &v1_version)
        .await
        .expect("rollback to v1 should succeed");
    let rolled = backend
        .secrets()
        .get_secret(&vault, secret, true)
        .await
        .expect("get after rollback should succeed");
    assert_eq!(
        rolled.value.as_ref().map(|v| v.as_str()),
        Some(v1_value),
        "rollback should restore the v1 value"
    );

    // --- DELETE (soft, 30-day recovery window) ---
    backend
        .secrets()
        .delete_secret(&vault, secret)
        .await
        .expect("delete_secret should succeed");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let deleted = backend
        .secrets()
        .list_deleted_secrets(&vault)
        .await
        .expect("list_deleted_secrets should succeed");
    assert!(
        deleted.iter().any(|s| s.name == secret),
        "soft-deleted secret should appear in list_deleted_secrets, got: {:?}",
        deleted.iter().map(|s| &s.name).collect::<Vec<_>>()
    );

    // --- RESTORE ---
    backend
        .secrets()
        .restore_secret(&vault, secret)
        .await
        .expect("restore_secret should succeed");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let restored = backend
        .secrets()
        .get_secret(&vault, secret, true)
        .await
        .expect("get after restore should succeed");
    assert!(
        restored.value.is_some(),
        "restored secret should be readable again"
    );

    // --- CLEANUP (force-purge + drop marker) ---
    cleanup(&backend, &vault, &[secret]).await;
}

#[tokio::test]
#[ignore]
async fn e2e_aws_bulk_set_and_list() {
    let backend = aws_backend().await;
    let vault = unique_vault("bulk");

    let vaults = backend.vaults().expect("AWS backend exposes vaults");
    vaults
        .create_vault(VaultCreateRequest {
            name: vault.clone(),
            ..Default::default()
        })
        .await
        .expect("create_vault should succeed");

    let entries = [("alpha", "a-val"), ("beta", "b-val"), ("gamma", "g-val")];
    for (name, value) in entries {
        backend
            .secrets()
            .set_secret(&vault, make_request(name, value))
            .await
            .unwrap_or_else(|e| panic!("set_secret {name} failed: {e:?}"));
    }

    for (name, value) in entries {
        let got = backend
            .secrets()
            .get_secret(&vault, name, true)
            .await
            .unwrap_or_else(|e| panic!("get_secret {name} failed: {e:?}"));
        assert_eq!(got.value.as_ref().map(|v| v.as_str()), Some(value));
    }

    // List should report exactly the 3 secrets — eventually consistent.
    let names = poll_list_secrets(
        &backend,
        &vault,
        |names| names.len() == 3,
        "all 3 bulk secrets appear in list",
    )
    .await;
    assert_eq!(
        names.len(),
        3,
        "expected exactly 3 secrets in the test vault, got {}: {names:?}",
        names.len()
    );

    cleanup(&backend, &vault, &["alpha", "beta", "gamma"]).await;
}

#[tokio::test]
#[ignore]
async fn e2e_aws_vault_create_list_delete() {
    let backend = aws_backend().await;
    let vault = unique_vault("vault");
    let vaults = backend.vaults().expect("AWS backend exposes vaults");

    vaults
        .create_vault(VaultCreateRequest {
            name: vault.clone(),
            ..Default::default()
        })
        .await
        .expect("create_vault should succeed");

    // list_vaults is eventually consistent — poll until the marker shows up.
    poll_list_vaults_contains(&backend, &vault).await;

    // Empty vault (marker only) — delete_vault should succeed without --force.
    vaults
        .delete_vault(&vault, None)
        .await
        .expect("delete_vault of an empty vault should succeed");

    // delete_vault drops the marker, which is also eventually consistent.
    let mut gone = false;
    for attempt in 1..=12 {
        let after = vaults
            .list_vaults(None)
            .await
            .expect("list_vaults after delete should succeed");
        if !after.iter().any(|v| v.name == vault) {
            gone = true;
            break;
        }
        if attempt < 12 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    assert!(gone, "deleted vault should no longer be listed");
}

#[tokio::test]
#[ignore]
async fn e2e_aws_protected_secret_untouched() {
    // Sanity guard: confirm the prefix-scoped backend never reports the
    // account-root `claude-api-key` secret as part of any test vault.
    let backend = aws_backend().await;
    let vault = unique_vault("guard");

    let vaults = backend.vaults().expect("AWS backend exposes vaults");
    vaults
        .create_vault(VaultCreateRequest {
            name: vault.clone(),
            ..Default::default()
        })
        .await
        .expect("create_vault should succeed");

    let listed = backend
        .secrets()
        .list_secrets(&vault, None)
        .await
        .expect("list_secrets should succeed");
    assert!(
        !listed.iter().any(|s| s.name == PROTECTED_SECRET),
        "the live '{PROTECTED_SECRET}' secret must never appear inside a test vault"
    );

    cleanup(&backend, &vault, &[]).await;
}
