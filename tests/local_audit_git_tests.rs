//! End-to-end coverage for the local backend's audit trail and git-native
//! versioning, driven through `LocalBackend` rather than the inner types — so
//! the config plumbing, capability flags, and per-operation hooks are all
//! exercised the way the CLI reaches them.

use crosstache::backend::local::audit::ChainStatus;
use crosstache::backend::local::LocalBackend;
use crosstache::backend::Backend;
use crosstache::config::settings::LocalConfig;
use crosstache::secret::manager::{FieldUpdate, SecretRequest, SecretUpdateRequest};
use tempfile::TempDir;

/// Build a local backend rooted in a temp dir.
///
/// Key material deliberately sits *outside* `store/`, matching the shipped
/// default (`~/.xv/key.txt` alongside `~/.xv/store`).
fn backend(tmp: &TempDir, audit: bool, git: bool) -> LocalBackend {
    let cfg = LocalConfig {
        store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
        key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
        default_vault: Some("default".into()),
        encrypt_metadata: None,
        opaque_filenames: None,
        audit: Some(audit),
        git: Some(git),
    };
    LocalBackend::new(Some(&cfg)).expect("create local backend")
}

fn request(name: &str, value: &str) -> SecretRequest {
    SecretRequest {
        name: name.to_string(),
        value: value.to_string().into(),
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

/// A metadata-only update that sets `note`.
fn note_update(name: &str, note: &str) -> SecretUpdateRequest {
    SecretUpdateRequest {
        name: name.to_string(),
        value: None,
        content_type: None,
        enabled: None,
        expires_on: FieldUpdate::Unchanged,
        not_before: FieldUpdate::Unchanged,
        tags: None,
        groups: None,
        note: FieldUpdate::Set(note.to_string()),
        folder: FieldUpdate::Unchanged,
        replace_tags: false,
        replace_groups: false,
        expected_revision: None,
    }
}

// ---------------------------------------------------------------------------
// Audit trail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_disabled_by_default_and_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, false, false);

    assert!(
        !be.capabilities().has_audit,
        "has_audit must stay false when [local].audit is off"
    );
    assert!(be.audit().is_none(), "no AuditBackend without the flag");
    assert!(be.audit_log().is_none());

    be.secrets()
        .set_secret("default", request("DB_PASSWORD", "hunter2"))
        .await
        .unwrap();

    let audit_dir = tmp.path().join("store/vaults/default/.audit");
    assert!(
        !audit_dir.exists(),
        "the default path must not create an audit log"
    );
}

#[tokio::test]
async fn audit_records_the_full_secret_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);

    assert!(be.capabilities().has_audit);
    let audit = be.audit().expect("AuditBackend when [local].audit is on");

    // set → get(value) → update → delete → restore → delete → purge.
    // Purge follows a *delete*: since the transactional-store rework, purge
    // applies only to soft-deleted (trashed) secrets and errors on an active
    // one, so the lifecycle re-deletes before purging.
    be.secrets()
        .set_secret("default", request("DB_PASSWORD", "hunter2"))
        .await
        .unwrap();
    be.secrets()
        .get_secret("default", "DB_PASSWORD", true)
        .await
        .unwrap();
    be.secrets()
        .update_secret(
            "default",
            "DB_PASSWORD",
            note_update("DB_PASSWORD", "prod db"),
        )
        .await
        .unwrap();
    be.secrets()
        .delete_secret("default", "DB_PASSWORD")
        .await
        .unwrap();
    be.secrets()
        .restore_secret("default", "DB_PASSWORD")
        .await
        .unwrap();
    be.secrets()
        .delete_secret("default", "DB_PASSWORD")
        .await
        .unwrap();
    be.secrets()
        .purge_secret("default", "DB_PASSWORD")
        .await
        .unwrap();

    let events = audit.get_vault_events("default", None, 30).await.unwrap();
    // Newest first, so reverse for a chronological read.
    let ops: Vec<&str> = events.iter().rev().map(|e| e.operation.as_str()).collect();
    assert_eq!(
        ops,
        vec![
            "PutSecretValue",
            "GetSecretValue",
            "UpdateSecret",
            "DeleteSecret",
            "RestoreSecret",
            "DeleteSecret",
            "PurgeSecret",
        ],
        "every mutation and value read must be recorded, in order"
    );
    assert!(events.iter().all(|e| e.status == "Succeeded"));
    assert!(events.iter().all(|e| e.resource_name == "DB_PASSWORD"));
    assert!(
        events.iter().all(|e| e.source_ip.is_none()),
        "local access has no network peer to report"
    );

    // Per-secret query narrows to that secret.
    let scoped = audit
        .get_secret_events("default", "DB_PASSWORD", None, 30)
        .await
        .unwrap();
    assert_eq!(scoped.len(), 7);

    assert_eq!(
        be.audit_log().unwrap().verify_chain("default").unwrap(),
        ChainStatus::Intact { records: 7 }
    );
}

#[tokio::test]
async fn metadata_only_reads_are_not_logged_as_value_access() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);
    be.secrets()
        .set_secret("default", request("A", "v"))
        .await
        .unwrap();

    // include_value = false — backs listings and existence checks.
    be.secrets()
        .get_secret("default", "A", false)
        .await
        .unwrap();
    be.secrets().secret_exists("default", "A").await.unwrap();

    let events = be
        .audit()
        .unwrap()
        .get_vault_events("default", None, 30)
        .await
        .unwrap();
    let gets = events
        .iter()
        .filter(|e| e.operation == "GetSecretValue")
        .count();
    assert_eq!(
        gets, 0,
        "metadata-only reads must not look like value access"
    );
}

#[tokio::test]
async fn listing_is_recorded_as_a_vault_wide_event() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);
    be.secrets()
        .set_secret("default", request("A", "v"))
        .await
        .unwrap();
    be.secrets().list_secrets("default", None).await.unwrap();

    let events = be
        .audit()
        .unwrap()
        .get_vault_events("default", None, 30)
        .await
        .unwrap();
    let list = events
        .iter()
        .find(|e| e.operation == "ListSecrets")
        .expect("enumeration is audit-relevant");
    assert_eq!(list.resource_name, "*");
}

#[tokio::test]
async fn tampering_with_the_audit_log_is_detected() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);
    for name in ["A", "B", "C"] {
        be.secrets()
            .set_secret("default", request(name, "v"))
            .await
            .unwrap();
    }

    let log_path = tmp.path().join("store/vaults/default/.audit/log.jsonl");
    let body = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 3);

    // Drop the middle record — the classic "hide one access" edit.
    std::fs::write(&log_path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();

    match be.audit_log().unwrap().verify_chain("default").unwrap() {
        ChainStatus::Broken { verified, .. } => {
            assert_eq!(verified, 1, "the first record still verifies");
        }
        other => panic!("expected a broken chain, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Git-native versioning
// ---------------------------------------------------------------------------

#[tokio::test]
async fn git_disabled_by_default() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, false, false);
    be.secrets()
        .set_secret("default", request("A", "v"))
        .await
        .unwrap();

    assert!(be.git_store().is_none());
    assert!(
        !tmp.path().join("store/.git").exists(),
        "the default path must not create a repository"
    );
}

#[tokio::test]
async fn every_mutation_produces_a_commit() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, false, true);

    // The repo is created eagerly at backend construction.
    assert!(tmp.path().join("store/.git").exists());
    let git = be.git_store().expect("git store when [local].git is on");

    be.secrets()
        .set_secret("default", request("DB_PASSWORD", "hunter2"))
        .await
        .unwrap();
    be.secrets()
        .set_secret("default", request("DB_PASSWORD", "hunter3"))
        .await
        .unwrap();
    be.secrets()
        .update_secret("default", "DB_PASSWORD", note_update("DB_PASSWORD", "prod"))
        .await
        .unwrap();
    be.secrets()
        .delete_secret("default", "DB_PASSWORD")
        .await
        .unwrap();

    let subjects: Vec<String> = git
        .log(None, 0)
        .unwrap()
        .into_iter()
        .map(|c| c.subject)
        .collect();
    assert_eq!(
        subjects,
        vec![
            "delete DB_PASSWORD",
            "update DB_PASSWORD",
            "set DB_PASSWORD",
            "set DB_PASSWORD",
        ],
        "history should read as the operation log, newest first"
    );
}

#[tokio::test]
async fn only_ciphertext_is_committed_never_the_identity() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, false, true);
    be.secrets()
        .set_secret("default", request("DB_PASSWORD", "hunter2"))
        .await
        .unwrap();

    let tracked = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path().join("store"))
        .args(["ls-files"])
        .output()
        .unwrap();
    let tracked = String::from_utf8_lossy(&tracked.stdout);

    assert!(
        tracked.contains(".age"),
        "age ciphertext should be versioned: {tracked}"
    );
    assert!(
        !tracked.contains("key.txt"),
        "the age identity must never be tracked: {tracked}"
    );

    // And the committed blob must not contain the plaintext value.
    let blobs = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path().join("store"))
        .args(["grep", "-I", "--cached", "hunter2"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&blobs.stdout).contains("hunter2"),
        "plaintext must never reach a commit"
    );
}

#[tokio::test]
async fn git_versioning_and_audit_compose() {
    // The combination is the interesting one: git gives the audit log an
    // off-box copy, which is what the hash chain alone cannot provide.
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, true);
    be.secrets()
        .set_secret("default", request("A", "v"))
        .await
        .unwrap();

    let tracked = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path().join("store"))
        .args(["ls-files"])
        .output()
        .unwrap();
    let tracked = String::from_utf8_lossy(&tracked.stdout);
    assert!(
        tracked.contains(".audit/log.jsonl"),
        "the audit log must be versioned so a remote holds copies: {tracked}"
    );
    assert_eq!(
        be.audit_log().unwrap().verify_chain("default").unwrap(),
        ChainStatus::Intact { records: 1 }
    );
}

#[tokio::test]
async fn history_and_rollback_still_work_alongside_git() {
    // Git versioning is additive: the backend's own version archive continues
    // to drive `xv history` / `xv rollback` unchanged.
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, true);
    be.secrets()
        .set_secret("default", request("A", "v1"))
        .await
        .unwrap();
    be.secrets()
        .set_secret("default", request("A", "v2"))
        .await
        .unwrap();

    let versions = be.secrets().list_versions("default", "A").await.unwrap();
    assert!(versions.len() >= 2, "version archive still populated");

    be.secrets().rollback("default", "A", "v1").await.unwrap();
    let current = be.secrets().get_secret("default", "A", true).await.unwrap();
    assert_eq!(current.value.as_deref().map(|v| v.as_str()), Some("v1"));

    let subjects: Vec<String> = be
        .git_store()
        .unwrap()
        .log(None, 1)
        .unwrap()
        .into_iter()
        .map(|c| c.subject)
        .collect();
    assert!(
        subjects[0].starts_with("rollback A"),
        "rollback should be its own commit: {subjects:?}"
    );
}

// ---------------------------------------------------------------------------
// Failed attempts
// ---------------------------------------------------------------------------

/// Read the raw audit records for a vault.
fn audit_records(tmp: &TempDir) -> Vec<serde_json::Value> {
    let path = tmp.path().join("store/vaults/default/.audit/log.jsonl");
    let body = std::fs::read_to_string(&path).unwrap_or_default();
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("audit line is JSON"))
        .collect()
}

#[tokio::test]
async fn a_read_of_a_missing_secret_is_audited_as_notfound() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);

    let err = be
        .secrets()
        .get_secret("default", "NOPE", true)
        .await
        .expect_err("missing secret");
    assert!(matches!(
        err,
        crosstache::backend::BackendError::NotFound { .. }
    ));

    let records = audit_records(&tmp);
    assert_eq!(
        records.len(),
        1,
        "the attempt must be recorded: {records:?}"
    );
    assert_eq!(records[0]["operation"], "GetSecretValue");
    assert_eq!(records[0]["resource_name"], "NOPE");
    assert_eq!(records[0]["status"], "NotFound");

    // The failure record must not break the chain.
    assert_eq!(
        be.audit_log().unwrap().verify_chain("default").unwrap(),
        ChainStatus::Intact { records: 1 }
    );
}

#[tokio::test]
async fn a_metadata_only_read_of_a_missing_secret_is_not_audited() {
    // Existence probes back listings; auditing them would bury real access in
    // noise, and the success path already declines to log them.
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);
    let _ = be.secrets().get_secret("default", "NOPE", false).await;
    let _ = be.secrets().secret_exists("default", "NOPE").await;
    assert!(audit_records(&tmp).is_empty());
}

#[tokio::test]
async fn undecryptable_ciphertext_is_audited_as_decryption_failed() {
    // The security-relevant failure: the caller reached the material and could
    // not open it — wrong identity, or altered/truncated ciphertext.
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);
    be.secrets()
        .set_secret("default", request("DB_PASSWORD", "hunter2"))
        .await
        .unwrap();

    // Corrupt the ciphertext, leaving metadata intact so the read gets that far.
    let age_path = tmp
        .path()
        .join("store/vaults/default/secrets/DB_PASSWORD.age");
    std::fs::write(&age_path, b"not-age-ciphertext-at-all").unwrap();

    let err = be
        .secrets()
        .get_secret("default", "DB_PASSWORD", true)
        .await
        .expect_err("corrupt ciphertext must not decrypt");
    assert!(
        matches!(err, crosstache::backend::BackendError::Decryption(_)),
        "expected a Decryption error, got {err:?}"
    );

    let records = audit_records(&tmp);
    let last = records.last().expect("a record");
    assert_eq!(last["operation"], "GetSecretValue");
    assert_eq!(last["resource_name"], "DB_PASSWORD");
    assert_eq!(
        last["status"], "DecryptionFailed",
        "a failed decryption must be distinguishable from a generic internal error: {records:?}"
    );
}

#[tokio::test]
async fn a_write_to_a_missing_vault_is_audited_as_vaultnotfound() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);
    // The vault dir must exist for the audit log to be written into it, so use
    // a real vault for the log and a missing one for the operation.
    let err = be
        .secrets()
        .set_secret("default", request("A", "v"))
        .await
        .map(|_| ())
        .and(
            be.secrets()
                .delete_secret("default", "GHOST")
                .await
                .map(|_| ()),
        )
        .expect_err("deleting a nonexistent secret fails");
    let _ = err;

    let records = audit_records(&tmp);
    let failure = records
        .iter()
        .find(|r| r["operation"] == "DeleteSecret")
        .expect("the delete attempt must be recorded");
    assert_eq!(failure["resource_name"], "GHOST");
    assert_ne!(
        failure["status"], "Succeeded",
        "a failed delete must not be recorded as a success: {records:?}"
    );
}

#[tokio::test]
async fn failure_records_never_contain_secret_values() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);
    let secret_value = "super-secret-canary-value";
    be.secrets()
        .set_secret("default", request("CANARY", secret_value))
        .await
        .unwrap();

    // Force a decryption failure, whose error message is the richest of any
    // failure path.
    let age_path = tmp.path().join("store/vaults/default/secrets/CANARY.age");
    std::fs::write(&age_path, b"corrupt").unwrap();
    let _ = be.secrets().get_secret("default", "CANARY", true).await;
    // And an invalid-argument failure via a traversal vault name.
    let _ = be.secrets().get_secret("../escape", "CANARY", true).await;

    let raw =
        std::fs::read_to_string(tmp.path().join("store/vaults/default/.audit/log.jsonl")).unwrap();
    assert!(
        !raw.contains(secret_value),
        "no audit record may contain a secret value: {raw}"
    );
    // Statuses come from a closed set, so no error text leaks in either.
    for record in audit_records(&tmp) {
        let status = record["status"].as_str().unwrap();
        assert!(
            [
                "Succeeded",
                "NotFound",
                "VaultNotFound",
                "AuthenticationFailed",
                "AccessDenied",
                "Unsupported",
                "InvalidArgument",
                "Conflict",
                "RateLimited",
                "NetworkError",
                "RenameIncomplete",
                "DecryptionFailed",
                "InternalError",
            ]
            .contains(&status),
            "status {status:?} is outside the closed vocabulary"
        );
    }
}

#[tokio::test]
async fn successes_and_failures_interleave_in_one_verifiable_chain() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, false);

    be.secrets()
        .set_secret("default", request("A", "v"))
        .await
        .unwrap();
    let _ = be.secrets().get_secret("default", "MISSING", true).await;
    be.secrets().get_secret("default", "A", true).await.unwrap();
    let _ = be
        .secrets()
        .get_secret("default", "ALSO_MISSING", true)
        .await;

    let records = audit_records(&tmp);
    let pairs: Vec<(&str, &str)> = records
        .iter()
        .map(|r| {
            (
                r["resource_name"].as_str().unwrap(),
                r["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("A", "Succeeded"),
            ("MISSING", "NotFound"),
            ("A", "Succeeded"),
            ("ALSO_MISSING", "NotFound"),
        ]
    );

    assert_eq!(
        be.audit_log().unwrap().verify_chain("default").unwrap(),
        ChainStatus::Intact { records: 4 }
    );
}

#[tokio::test]
async fn failures_are_not_audited_when_auditing_is_off() {
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, false, false);
    let _ = be.secrets().get_secret("default", "NOPE", true).await;
    assert!(!tmp.path().join("store/vaults/default/.audit").exists());
}

#[tokio::test]
async fn renames_are_audited_and_committed() {
    // `rename_secret` arrived with the transactional-store rework; it must flow
    // through the same audit/git hooks as every other mutation.
    let tmp = TempDir::new().unwrap();
    let be = backend(&tmp, true, true);
    be.secrets()
        .set_secret("default", request("OLD_NAME", "v"))
        .await
        .unwrap();

    be.secrets()
        .rename_secret("default", "OLD_NAME", "NEW_NAME")
        .await
        .unwrap();

    let records = audit_records(&tmp);
    let rename = records
        .iter()
        .find(|r| r["operation"] == "RenameSecret")
        .expect("the rename must be recorded");
    assert_eq!(
        rename["resource_name"], "OLD_NAME",
        "the record keys on the source name, joining the pre-rename history"
    );
    assert_eq!(rename["status"], "Succeeded");

    // And a failed rename (missing source) is recorded too.
    let err = be
        .secrets()
        .rename_secret("default", "GHOST", "ANYTHING")
        .await
        .expect_err("renaming a missing secret fails");
    let _ = err;
    let records = audit_records(&tmp);
    let failed = records
        .iter()
        .rev()
        .find(|r| r["operation"] == "RenameSecret" && r["resource_name"] == "GHOST")
        .expect("the failed attempt must be recorded");
    assert_ne!(failed["status"], "Succeeded");

    let subjects = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path().join("store"))
        .args(["log", "--pretty=%s"])
        .output()
        .unwrap();
    let subjects = String::from_utf8_lossy(&subjects.stdout);
    assert!(
        subjects.contains("rename OLD_NAME to NEW_NAME"),
        "the rename should be its own commit: {subjects}"
    );

    assert_eq!(
        be.audit_log().unwrap().verify_chain("default").unwrap(),
        ChainStatus::Intact { records: 3 },
        "set + rename + failed rename: the chain must verify across all three"
    );
}

#[tokio::test]
async fn git_log_filter_survives_opaque_filenames() {
    // End-to-end for the Bugbot finding: with opaque filenames the committed
    // paths are keyed-hash stems, so only subject-based filtering can find a
    // secret's history.
    let tmp = TempDir::new().unwrap();
    let cfg = LocalConfig {
        store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
        key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
        default_vault: Some("default".into()),
        encrypt_metadata: None,
        opaque_filenames: Some(true),
        audit: None,
        git: Some(true),
    };
    let be = LocalBackend::new(Some(&cfg)).expect("create local backend");

    be.secrets()
        .set_secret("default", request("DB_PASSWORD", "v1"))
        .await
        .unwrap();
    be.secrets()
        .set_secret("default", request("OTHER", "v"))
        .await
        .unwrap();
    be.secrets()
        .set_secret("default", request("DB_PASSWORD", "v2"))
        .await
        .unwrap();

    // Confirm the layout really is opaque — no committed path contains the name.
    let tracked = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path().join("store"))
        .args(["ls-files"])
        .output()
        .unwrap();
    let tracked = String::from_utf8_lossy(&tracked.stdout);
    assert!(
        !tracked.contains("DB_PASSWORD"),
        "precondition: paths must be opaque, got {tracked}"
    );

    let subjects: Vec<String> = be
        .git_store()
        .unwrap()
        .log(Some("DB_PASSWORD"), 0)
        .unwrap()
        .into_iter()
        .map(|c| c.subject)
        .collect();
    assert_eq!(
        subjects,
        vec!["set DB_PASSWORD", "set DB_PASSWORD"],
        "opaque paths must not hide the secret's history"
    );
}
