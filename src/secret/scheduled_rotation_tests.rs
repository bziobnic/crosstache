//! Service-level tests for the due-rotation service.
//!
//! These run against a **real** local age-encrypted store in a temp directory —
//! no backend mock — so a passing test means secrets really were re-encrypted
//! and re-stamped. Nothing here mutates process-global state (no `set_var`, no
//! `set_current_dir`), because the whole suite runs in parallel.

use super::*;

use std::collections::HashMap;

use crate::backend::local::LocalBackend;
use crate::config::settings::LocalConfig;
use crate::secret::manager::SecretRequest;
use crate::secret::rotation::{TAG_ROTATED_AT, TAG_ROTATE_EVERY};
use zeroize::Zeroizing;

/// Vault name used by every test. Deliberately not "default": the rotation
/// helper touches the *user's* context file only when the context's vault name
/// matches, and this one cannot collide with a real vault.
const VAULT: &str = "xv-scheduled-rotation-suite";

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Arc<dyn Backend>) {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let backend = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(store.display().to_string()),
        key_file: Some(dir.path().join("identity").display().to_string()),
        default_vault: Some(VAULT.to_string()),
        ..Default::default()
    }))
    .unwrap();
    (dir, store, Arc::new(backend))
}

/// A config that never touches the user's cache directory.
fn test_config() -> Config {
    Config {
        cache_enabled: false,
        ..Default::default()
    }
}

fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

async fn seed(backend: &Arc<dyn Backend>, name: &str, policy_tags: HashMap<String, String>) {
    backend
        .secrets()
        .set_secret(
            VAULT,
            SecretRequest {
                name: name.to_string(),
                value: Zeroizing::new(format!("initial-value-for-{name}")),
                content_type: None,
                enabled: Some(true),
                expires_on: None,
                not_before: None,
                tags: Some(policy_tags),
                groups: None,
                note: None,
                folder: None,
            },
        )
        .await
        .unwrap();
}

/// Tags for a secret whose policy came due two days ago.
fn due_policy() -> HashMap<String, String> {
    tags(&[
        (TAG_ROTATE_EVERY, "1d"),
        (
            TAG_ROTATED_AT,
            &(chrono::Utc::now() - chrono::Duration::days(3)).to_rfc3339(),
        ),
    ])
}

/// Tags for a secret with a policy that is nowhere near due.
fn fresh_policy() -> HashMap<String, String> {
    tags(&[
        (TAG_ROTATE_EVERY, "30d"),
        (TAG_ROTATED_AT, &chrono::Utc::now().to_rfc3339()),
    ])
}

/// Tags carrying an unparseable interval.
fn invalid_policy() -> HashMap<String, String> {
    tags(&[
        (TAG_ROTATE_EVERY, "banana"),
        (TAG_ROTATED_AT, &chrono::Utc::now().to_rfc3339()),
    ])
}

async fn value_of(backend: &Arc<dyn Backend>, name: &str) -> String {
    backend
        .secrets()
        .get_secret(VAULT, name, true)
        .await
        .unwrap()
        .value
        .as_deref()
        .unwrap()
        .to_string()
}

/// Corrupt the ciphertext of one secret, leaving its plaintext `.meta.json`
/// intact. The secret still appears in a listing (metadata-only) but cannot be
/// read back, so rotation — which verifies the secret first — fails for that
/// one secret and no other.
fn corrupt_ciphertext(store: &std::path::Path, name: &str) {
    let dir = store.join("vaults").join(VAULT).join("secrets");
    let mut stem = None;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let Some(file) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        let Some(candidate) = file.strip_suffix(".meta.json") else {
            continue;
        };
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if meta["name"] == serde_json::Value::String(name.to_string()) {
            stem = Some(candidate.to_string());
            break;
        }
    }
    let stem = stem.unwrap_or_else(|| panic!("no metadata file for {name}"));
    std::fs::write(dir.join(format!("{stem}.age")), b"not an age file").unwrap();
}

/// Observer that records every event, with a configurable plan decision.
#[derive(Default)]
struct Recorder {
    abort: bool,
    plans: Vec<DueRotationPlan>,
    rotated: Vec<String>,
    failures: Vec<(String, DueRotationFailureCategory)>,
}

impl DueRotationObserver for Recorder {
    fn on_plan(&mut self, plan: &DueRotationPlan) -> Result<PlanDecision> {
        self.plans.push(plan.clone());
        Ok(if self.abort {
            PlanDecision::Abort
        } else {
            PlanDecision::Proceed
        })
    }

    fn on_rotated(&mut self, name: &str) {
        self.rotated.push(name.to_string());
    }

    fn on_failure(
        &mut self,
        name: &str,
        category: DueRotationFailureCategory,
        _error: &CrosstacheError,
    ) {
        self.failures.push((name.to_string(), category));
    }
}

async fn run(
    config: &Config,
    backend: &Arc<dyn Backend>,
    observer: &mut Recorder,
) -> Result<DueRotationSummary> {
    run_due_rotation_with_backend(
        config,
        backend.clone(),
        "local",
        VAULT,
        &DueRotationOptions::default(),
        observer,
    )
    .await
}

#[tokio::test]
async fn nothing_due_counts_policies_and_writes_nothing() {
    let (_dir, _store, backend) = fixture();
    seed(&backend, "fresh-a", fresh_policy()).await;
    seed(&backend, "fresh-b", fresh_policy()).await;
    seed(&backend, "unmanaged", HashMap::new()).await;
    let before = value_of(&backend, "fresh-a").await;

    let mut observer = Recorder::default();
    let summary = run(&test_config(), &backend, &mut observer).await.unwrap();

    assert_eq!(
        summary.policy_managed, 2,
        "unmanaged secrets are out of scope"
    );
    assert_eq!(summary.due, 0);
    assert_eq!(summary.rotated, 0);
    assert_eq!(summary.failed, 0);
    assert!(summary.failures.is_empty());
    assert!(observer.rotated.is_empty());
    assert_eq!(value_of(&backend, "fresh-a").await, before);
}

#[tokio::test]
async fn every_due_secret_rotates() {
    let (_dir, _store, backend) = fixture();
    seed(&backend, "due-a", due_policy()).await;
    seed(&backend, "due-b", due_policy()).await;
    seed(&backend, "fresh", fresh_policy()).await;
    let before_a = value_of(&backend, "due-a").await;
    let before_fresh = value_of(&backend, "fresh").await;

    let mut observer = Recorder::default();
    let summary = run(&test_config(), &backend, &mut observer).await.unwrap();

    assert_eq!(summary.policy_managed, 3);
    assert_eq!(summary.due, 2);
    assert_eq!(summary.rotated, 2);
    assert_eq!(summary.failed, 0);
    assert_eq!(observer.rotated, vec!["due-a".to_string(), "due-b".into()]);
    assert_ne!(value_of(&backend, "due-a").await, before_a);
    assert_eq!(
        value_of(&backend, "fresh").await,
        before_fresh,
        "a secret that is not due must not be touched"
    );

    // The rotation stamp is refreshed, so a second run has nothing to do.
    let mut second = Recorder::default();
    let summary = run(&test_config(), &backend, &mut second).await.unwrap();
    assert_eq!(summary.due, 0);
}

#[tokio::test]
async fn abort_from_the_plan_writes_nothing() {
    let (_dir, _store, backend) = fixture();
    seed(&backend, "due-a", due_policy()).await;
    let before = value_of(&backend, "due-a").await;

    let mut observer = Recorder {
        abort: true,
        ..Default::default()
    };
    let summary = run(&test_config(), &backend, &mut observer).await.unwrap();

    assert_eq!(summary.due, 1);
    assert_eq!(summary.rotated, 0);
    assert_eq!(summary.failed, 0);
    assert_eq!(observer.plans.len(), 1);
    assert_eq!(value_of(&backend, "due-a").await, before);
}

#[tokio::test]
async fn an_error_from_the_plan_propagates() {
    struct Refuse;
    impl DueRotationObserver for Refuse {
        fn on_plan(&mut self, _plan: &DueRotationPlan) -> Result<PlanDecision> {
            Err(CrosstacheError::InvalidArgument("refused".into()))
        }
    }

    let (_dir, _store, backend) = fixture();
    seed(&backend, "due-a", due_policy()).await;
    let before = value_of(&backend, "due-a").await;

    let err = run_due_rotation_with_backend(
        &test_config(),
        backend.clone(),
        "local",
        VAULT,
        &DueRotationOptions::default(),
        &mut Refuse,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, CrosstacheError::InvalidArgument(_)));
    assert_eq!(value_of(&backend, "due-a").await, before);
}

#[tokio::test]
async fn an_invalid_policy_is_counted_while_the_rest_still_rotate() {
    let (_dir, _store, backend) = fixture();
    seed(&backend, "broken", invalid_policy()).await;
    seed(&backend, "due-a", due_policy()).await;
    let before = value_of(&backend, "broken").await;

    let mut observer = Recorder::default();
    let summary = run(&test_config(), &backend, &mut observer).await.unwrap();

    assert_eq!(summary.policy_managed, 2);
    assert_eq!(summary.due, 1);
    assert_eq!(summary.rotated, 1);
    assert_eq!(summary.failed, 1);
    assert_eq!(
        summary.failures,
        vec![DueRotationFailure {
            category: DueRotationFailureCategory::InvalidPolicy
        }]
    );
    assert_eq!(summary.failures[0].code(), "invalid-policy");
    assert_eq!(observer.plans[0].invalid, vec!["broken".to_string()]);
    assert_eq!(
        observer.failures,
        vec![(
            "broken".to_string(),
            DueRotationFailureCategory::InvalidPolicy
        )]
    );
    assert_eq!(
        value_of(&backend, "broken").await,
        before,
        "an unevaluated secret is never rotated blind"
    );
}

#[tokio::test]
async fn a_single_unrotatable_secret_does_not_stop_the_batch() {
    let (_dir, store, backend) = fixture();
    seed(&backend, "due-a", due_policy()).await;
    seed(&backend, "due-broken", due_policy()).await;
    corrupt_ciphertext(&store, "due-broken");
    let before_a = value_of(&backend, "due-a").await;

    let mut observer = Recorder::default();
    let summary = run(&test_config(), &backend, &mut observer).await.unwrap();

    assert_eq!(summary.due, 2);
    assert_eq!(summary.rotated, 1);
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(observer.rotated, vec!["due-a".to_string()]);
    assert_eq!(observer.failures.len(), 1);
    assert_eq!(observer.failures[0].0, "due-broken");
    assert_ne!(value_of(&backend, "due-a").await, before_a);
}

#[tokio::test]
async fn a_total_discovery_failure_is_an_error_not_a_summary() {
    let (_dir, _store, backend) = fixture();
    seed(&backend, "due-a", due_policy()).await;
    let registry = BackendRegistry::new(backend);

    let err = run_due_rotation(
        &test_config(),
        &registry,
        "no-such-backend",
        VAULT,
        &DueRotationOptions::default(),
        &mut SilentObserver,
    )
    .await
    .unwrap_err();

    // No partial summary: the caller cannot tell "nothing due" from
    // "never looked", so a scheduled run must not record a green outcome.
    assert!(
        err.to_string().contains("no-such-backend"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn the_summary_carries_no_secret_names_or_error_bodies() {
    let (_dir, store, backend) = fixture();
    seed(&backend, "due-a", due_policy()).await;
    seed(&backend, "due-broken", due_policy()).await;
    seed(&backend, "broken-policy", invalid_policy()).await;
    corrupt_ciphertext(&store, "due-broken");

    let summary = run(&test_config(), &backend, &mut Recorder::default())
        .await
        .unwrap();

    let debug = format!("{summary:?}");
    let json = serde_json::to_string(&summary).unwrap();
    for rendered in [&debug, &json] {
        for name in ["due-a", "due-broken", "broken-policy", VAULT] {
            assert!(
                !rendered.contains(name),
                "summary leaked '{name}': {rendered}"
            );
        }
        assert!(
            !rendered.contains("not an age file"),
            "summary leaked an error body"
        );
    }
    assert_eq!(summary.failed, 2);
}

#[tokio::test]
async fn silent_observer_proceeds_by_default() {
    let (_dir, _store, backend) = fixture();
    seed(&backend, "due-a", due_policy()).await;

    let summary = run_due_rotation_with_backend(
        &test_config(),
        backend.clone(),
        "local",
        VAULT,
        &DueRotationOptions::default(),
        &mut SilentObserver,
    )
    .await
    .unwrap();

    assert_eq!(summary.rotated, 1);
    assert_eq!(summary.failed, 0);
}
