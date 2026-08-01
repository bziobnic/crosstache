//! End-to-end coverage for rotation policies (`xv update --rotate-every`,
//! `xv rotate --every/--due/--check`) on the local backend.
//!
//! The policy is backend-agnostic metadata, so exercising it on the local
//! backend covers the same code path Azure and AWS take; only `--native`
//! remains AWS-specific and is untouched here.

mod common;

use common::xv_isolated_local_with_opts;

/// Rebuild a command against an existing isolated store.
fn xv_cmd_for(store: &std::path::Path) -> std::process::Command {
    let root = store.parent().expect("store has a parent");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_xv"));
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(root);
    cmd
}

/// Parse the first JSON document on stdout.
///
/// With an explicit `--format json`, `xv` also writes a machine-readable error
/// envelope to stdout when the command exits non-zero (see
/// `print_user_friendly_error`). So a `--check` run that finds due secrets emits
/// the rows *and* the envelope — the same shape `xv scan --format json` has
/// produced since it shipped. Read only the leading document.
fn first_json_doc(stdout: &str) -> serde_json::Value {
    let mut stream = serde_json::Deserializer::from_str(stdout).into_iter::<serde_json::Value>();
    stream
        .next()
        .unwrap_or_else(|| panic!("expected a JSON document on stdout: {stdout}"))
        .unwrap_or_else(|e| panic!("invalid JSON on stdout ({e}): {stdout}"))
}

/// `xv rotate --check --format json` rows plus whether the command succeeded.
fn check_rows(store: &std::path::Path) -> (bool, Vec<serde_json::Value>) {
    let out = xv_cmd_for(store)
        .args(["rotate", "--check", "--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = first_json_doc(&stdout);
    let rows = rows
        .as_array()
        .unwrap_or_else(|| panic!("--check must emit an array: {stdout}"))
        .clone();
    (out.status.success(), rows)
}

/// A secret's tag map, read straight from the local store's metadata file.
///
/// `xv ls --format json` emits a curated row shape without the raw tag map, so
/// on-disk metadata is the direct way to assert on rotation bookkeeping. Test
/// names here are plain ASCII, which the local backend stores verbatim.
fn tags_of(store: &std::path::Path, name: &str) -> serde_json::Value {
    let path = store
        .join("vaults/default/secrets")
        .join(format!("{name}.meta.json"));
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let meta: serde_json::Value = serde_json::from_str(&body).unwrap();
    meta["tags"].clone()
}

// ---------------------------------------------------------------------------
// Setting and clearing a policy
// ---------------------------------------------------------------------------

#[test]
fn setting_a_policy_does_not_rotate_and_starts_the_clock() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "DB_PASSWORD", "--value", "original"])
        .status()
        .unwrap();

    let out = xv_cmd_for(&store)
        .args(["update", "DB_PASSWORD", "--rotate-every", "90d"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The value must be untouched — setting a policy is not a rotation.
    let value = xv_cmd_for(&store)
        .args(["get", "DB_PASSWORD", "--raw"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&value.stdout).trim(),
        "original",
        "setting a policy must not change the value"
    );

    // Both tags are written, so the secret is not immediately due.
    let tags = tags_of(&store, "DB_PASSWORD");
    assert_eq!(tags["xv:rotate_every"], "90d");
    assert!(
        tags["xv:rotated_at"].is_string(),
        "the clock must start at policy-set time: {tags}"
    );

    let (ok, rows) = check_rows(&store);
    assert!(ok, "a fresh policy must not be reported as due");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "ok");
    assert_eq!(rows[0]["name"], "DB_PASSWORD");
}

#[test]
fn clearing_a_policy_removes_both_tags_and_keeps_others() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args([
        "set",
        "A",
        "--value",
        "v",
        "--tag",
        "owner=platform",
        "--note",
        "keep me",
    ])
    .status()
    .unwrap();
    xv_cmd_for(&store)
        .args(["update", "A", "--rotate-every", "30d"])
        .status()
        .unwrap();

    let out = xv_cmd_for(&store)
        .args(["update", "A", "--clear-rotate-every"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let tags = tags_of(&store, "A");
    assert!(tags.get("xv:rotate_every").is_none(), "{tags}");
    assert!(tags.get("xv:rotated_at").is_none(), "{tags}");
    assert_eq!(
        tags["owner"], "platform",
        "unrelated tags must survive the authoritative rewrite: {tags}"
    );

    // Note lives outside the tag map on some backends; confirm it survived too.
    let listed = xv_cmd_for(&store)
        .args(["ls", "--format", "json"])
        .output()
        .unwrap();
    let rows = first_json_doc(&String::from_utf8_lossy(&listed.stdout));
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "A")
        .unwrap();
    assert_eq!(row["note"], "keep me", "{row}");

    let (ok, rows) = check_rows(&store);
    assert!(ok);
    assert!(rows.is_empty(), "no policies remain: {rows:?}");
}

#[test]
fn clearing_a_missing_policy_is_a_no_op() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "A", "--value", "v"]).status().unwrap();

    let out = xv_cmd_for(&store)
        .args(["update", "A", "--clear-rotate-every"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("no rotation policy"), "{combined}");
}

#[test]
fn invalid_intervals_are_rejected_before_any_write() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "A", "--value", "v"]).status().unwrap();

    for bad in ["90", "abc", "0d", "9999w"] {
        let out = xv_cmd_for(&store)
            .args(["update", "A", "--rotate-every", bad])
            .output()
            .unwrap();
        assert!(!out.status.success(), "interval {bad:?} should be rejected");
    }

    // No partial policy should have been written by the failed attempts.
    let tags = tags_of(&store, "A");
    assert!(tags.get("xv:rotate_every").is_none(), "{tags}");
}

// ---------------------------------------------------------------------------
// --check / --due
// ---------------------------------------------------------------------------

/// Force a due state by back-dating the rotation timestamp.
fn make_due(store: &std::path::Path, name: &str) {
    let status = xv_cmd_for(store)
        .args([
            "update",
            name,
            "--tag",
            "xv:rotate_every=30d",
            "--tag",
            "xv:rotated_at=2020-01-01T00:00:00Z",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "failed to back-date {name}");
}

#[test]
fn check_exits_51_when_a_secret_is_due() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "STALE", "--value", "v"]).status().unwrap();
    make_due(&store, "STALE");

    let out = xv_cmd_for(&store)
        .args(["rotate", "--check", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(51),
        "a due secret must exit 51 so CI can gate: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let rows = first_json_doc(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows[0]["status"], "due");
}

#[test]
fn check_ignores_secrets_without_a_policy() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "UNMANAGED", "--value", "v"])
        .status()
        .unwrap();

    let (ok, rows) = check_rows(&store);
    assert!(ok, "an unmanaged secret is out of scope, not overdue");
    assert!(rows.is_empty(), "{rows:?}");
}

#[test]
fn due_rotates_only_due_secrets_and_restamps_them() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "STALE", "--value", "old-value"])
        .status()
        .unwrap();
    xv_cmd_for(&store)
        .args(["set", "FRESH", "--value", "fresh-value"])
        .status()
        .unwrap();
    xv_cmd_for(&store)
        .args(["set", "UNMANAGED", "--value", "untouched"])
        .status()
        .unwrap();

    make_due(&store, "STALE");
    xv_cmd_for(&store)
        .args(["update", "FRESH", "--rotate-every", "90d"])
        .status()
        .unwrap();

    let out = xv_cmd_for(&store)
        .args(["rotate", "--due", "--force"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Only STALE rotated.
    let stale = xv_cmd_for(&store)
        .args(["get", "STALE", "--raw"])
        .output()
        .unwrap();
    assert_ne!(
        String::from_utf8_lossy(&stale.stdout).trim(),
        "old-value",
        "the due secret should have a new value"
    );

    for (name, expected) in [("FRESH", "fresh-value"), ("UNMANAGED", "untouched")] {
        let got = xv_cmd_for(&store)
            .args(["get", name, "--raw"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&got.stdout).trim(),
            expected,
            "{name} must not have been rotated"
        );
    }

    // The rotation restamped the clock, so nothing is due any more — this is
    // what stops --due from re-rotating the same secret on every run.
    let (ok, rows) = check_rows(&store);
    assert!(ok, "after rotating, --check should be clean: {rows:?}");
    assert_eq!(rows.len(), 2, "both policies still tracked: {rows:?}");
    assert!(rows.iter().all(|r| r["status"] == "ok"), "{rows:?}");

    // And the policy interval survived the rotation.
    let tags = tags_of(&store, "STALE");
    assert_eq!(tags["xv:rotate_every"], "30d", "{tags}");
}

#[test]
fn due_is_a_no_op_when_nothing_is_due() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "A", "--value", "v"]).status().unwrap();
    xv_cmd_for(&store)
        .args(["update", "A", "--rotate-every", "90d"])
        .status()
        .unwrap();

    let out = xv_cmd_for(&store)
        .args(["rotate", "--due", "--force"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("Nothing due"), "{combined}");

    let value = xv_cmd_for(&store)
        .args(["get", "A", "--raw"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&value.stdout).trim(), "v");
}

#[test]
fn an_unparseable_policy_fails_due_rather_than_being_skipped() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "BROKEN", "--value", "v"])
        .status()
        .unwrap();
    // A hand-edited or externally-written tag that xv cannot interpret.
    xv_cmd_for(&store)
        .args(["update", "BROKEN", "--tag", "xv:rotate_every=ninety days"])
        .status()
        .unwrap();

    let out = xv_cmd_for(&store)
        .args(["rotate", "--due", "--force"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "an uninterpretable policy must fail the run, not be silently skipped"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("BROKEN"), "{stderr}");

    // --check surfaces it as invalid rather than pretending it is fine.
    let out = xv_cmd_for(&store)
        .args(["rotate", "--check", "--format", "json"])
        .output()
        .unwrap();
    let rows = first_json_doc(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows[0]["status"], "invalid", "{rows:?}");
}

#[test]
fn rotate_every_sets_the_policy_while_rotating() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "A", "--value", "original"])
        .status()
        .unwrap();

    let out = xv_cmd_for(&store)
        .args(["rotate", "A", "--every", "2w", "--force"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let value = xv_cmd_for(&store)
        .args(["get", "A", "--raw"])
        .output()
        .unwrap();
    assert_ne!(
        String::from_utf8_lossy(&value.stdout).trim(),
        "original",
        "--every should still rotate the value"
    );

    let tags = tags_of(&store, "A");
    assert_eq!(tags["xv:rotate_every"], "2w", "{tags}");
    assert!(tags["xv:rotated_at"].is_string(), "{tags}");
}

#[test]
fn plain_rotate_restamps_an_existing_policy_without_redefining_it() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    cmd.args(["set", "A", "--value", "v"]).status().unwrap();
    make_due(&store, "A");

    // No --every: the existing 30d policy must survive, and the timestamp must
    // move forward so the secret stops being due.
    let out = xv_cmd_for(&store)
        .args(["rotate", "A", "--force"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let tags = tags_of(&store, "A");
    assert_eq!(
        tags["xv:rotate_every"], "30d",
        "policy must persist: {tags}"
    );
    assert_ne!(
        tags["xv:rotated_at"], "2020-01-01T00:00:00Z",
        "timestamp must be refreshed: {tags}"
    );

    let (ok, _rows) = check_rows(&store);
    assert!(ok, "the secret should no longer be due");
}

#[test]
fn rotate_requires_a_name_or_a_mode_flag() {
    let (mut cmd, _tmp, _store) = xv_isolated_local_with_opts(false, false);
    let out = cmd.args(["rotate"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--due") && stderr.contains("--check"),
        "the error should name the alternatives: {stderr}"
    );
}

#[test]
fn every_and_due_are_mutually_exclusive() {
    let (mut cmd, _tmp, _store) = xv_isolated_local_with_opts(false, false);
    let out = cmd
        .args(["rotate", "--due", "--every", "30d"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "clap should reject the combination");
}

// ---------------------------------------------------------------------------
// Composition with the other new features
// ---------------------------------------------------------------------------

#[test]
fn due_rotation_is_audited_and_committed() {
    // Rotation must flow through the same hooks a manual write does.
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(true, true);
    cmd.args(["set", "STALE", "--value", "v"]).status().unwrap();
    make_due(&store, "STALE");

    xv_cmd_for(&store)
        .args(["rotate", "--due", "--force"])
        .status()
        .unwrap();

    let out = xv_cmd_for(&store)
        .args(["audit", "--vault", "default", "--format", "json"])
        .output()
        .unwrap();
    let rows = first_json_doc(&String::from_utf8_lossy(&out.stdout));
    let rows = rows.as_array().unwrap();
    let puts = rows
        .iter()
        .filter(|r| r["operation"] == "PutSecretValue" && r["resource"] == "STALE")
        .count();
    assert!(puts >= 2, "the rotation write should be audited: {rows:?}");

    let log = xv_cmd_for(&store).args(["git", "log"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&log.stdout);
    assert!(
        stdout.contains("set STALE"),
        "rotation should commit: {stdout}"
    );

    let verify = xv_cmd_for(&store)
        .args(["audit", "--verify"])
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "chain must stay intact across rotation"
    );
}
