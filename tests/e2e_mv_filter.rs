//! End-to-end CLI tests for `--filter <GLOB>` on `xv mv` (2026-07-03 design
//! doc: docs/superpowers/specs/2026-07-03-mv-filter-design.md).
//!
//! A dedicated file rather than an extension of `e2e_filter_glob.rs`: `mv
//! --filter` needs its own folder/dest setup (bulk-move confirmation, TTY
//! refusal, collision, already-in-dest skip) that doesn't share fixtures
//! with the `ls`/`find --filter` tests, and keeping it separate mirrors how
//! `mv`'s own grammar/bulk-machinery tests already live apart from
//! `ls`/`find` in `e2e_local_backend.rs` vs `e2e_filter_glob.rs`. It reuses
//! the same hermetic harness (`tests/common/mod.rs`) and either-name glob
//! predicate (`compile_name_glob` / `glob_matches_either_name` in
//! `src/utils/helpers.rs`, already exhaustively unit-tested) as #326.
//!
//! Hermetic: every test uses the isolated local-backend harness from
//! `tests/common/mod.rs` — no Azure credentials or network access required.
//!
//! Run with:
//!   cargo test --test e2e_mv_filter

mod common;

/// Builds a fresh `xv` `Command` bound to the same isolated store/env as an
/// existing `xv_isolated_local()` tempdir, for a second (or third, ...) CLI
/// invocation against the same store (each `Command` is single-use). Mirrors
/// `xv_same_env` in `e2e_filter_glob.rs` / `e2e_record_types.rs`.
fn xv_same_env(temp: &std::path::Path) -> std::process::Command {
    let mut cmd = common::xv();
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp)
        .env("XDG_CONFIG_HOME", temp.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(temp);
    cmd
}

/// Set a secret with a plain value via `xv set NAME --value VALUE`, optionally
/// scoped to a folder.
fn set_secret(temp: &std::path::Path, name: &str, value: &str, folder: Option<&str>) {
    let mut args = vec!["set", name, "--value", value];
    if let Some(f) = folder {
        args.push("--folder");
        args.push(f);
    }
    let out = xv_same_env(temp)
        .args(&args)
        .output()
        .expect("execute xv set");
    assert!(
        out.status.success(),
        "xv set {name} failed: stderr: {}",
        common::stderr_str(&out)
    );
}

/// `xv ls --format json` as a parsed value, for folder/name assertions.
fn ls_json(temp: &std::path::Path) -> serde_json::Value {
    let out = xv_same_env(temp)
        .args(["ls", "--format", "json"])
        .output()
        .expect("execute xv ls");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    serde_json::from_str(&common::stdout_str(&out)).expect("ls --format json output")
}

/// `folder` tag for the secret named `name` in a parsed `ls --format json`
/// value, or `None` if unset / not found.
fn folder_of<'a>(json: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    json.as_array()
        .expect("json array")
        .iter()
        .find(|v| v["name"].as_str() == Some(name))
        .and_then(|v| v["folder"].as_str())
}

// ===========================================================================
// `xv mv --filter` — bulk filtered move
// ===========================================================================

#[test]
fn mv_filter_relocates_only_matches() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-a", "v1", None);
    set_secret(temp.path(), "test-b", "v2", None);
    set_secret(temp.path(), "latest-x", "v3", None);

    let out = cmd
        .args(["mv", "--filter", "test-*", "archive/", "--yes"])
        .output()
        .expect("execute xv mv --filter");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let json = ls_json(temp.path());
    assert_eq!(folder_of(&json, "test-a"), Some("archive"), "{json}");
    assert_eq!(folder_of(&json, "test-b"), Some("archive"), "{json}");
    assert_eq!(
        folder_of(&json, "latest-x"),
        None,
        "non-matching secret must not move: {json}"
    );
}

#[test]
fn mv_filter_dest_must_be_folder_errors() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-a", "v1", None);

    let out = cmd
        .args(["mv", "--filter", "test-*", "notafolder", "--yes"])
        .output()
        .expect("execute xv mv --filter");
    assert!(
        !out.status.success(),
        "non-folder dest should fail: stdout: {}",
        common::stdout_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(
        stderr.contains("folder moves require a folder destination ending in /"),
        "{stderr}"
    );
    assert!(stderr.contains("did you mean 'notafolder/'"), "{stderr}");
}

#[test]
fn mv_filter_source_and_filter_together_is_usage_error() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-a", "v1", None);

    // Both orders: --filter before the trailing positionals, and SOURCE
    // supplied ahead of --filter.
    let out = cmd
        .args(["mv", "--filter", "test-*", "some-source", "archive/"])
        .output()
        .expect("execute xv mv");
    assert!(
        !out.status.success(),
        "stdout: {}",
        common::stdout_str(&out)
    );
    assert!(
        common::stderr_str(&out).contains("either SOURCE or --filter"),
        "{}",
        common::stderr_str(&out)
    );

    let out2 = xv_same_env(temp.path())
        .args(["mv", "some-source", "--filter", "test-*", "archive/"])
        .output()
        .expect("execute xv mv");
    assert!(
        !out2.status.success(),
        "stdout: {}",
        common::stdout_str(&out2)
    );
    assert!(
        common::stderr_str(&out2).contains("either SOURCE or --filter"),
        "{}",
        common::stderr_str(&out2)
    );
}

#[test]
fn mv_filter_neither_source_nor_filter_is_usage_error() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-a", "v1", None);

    // Only DEST given: no SOURCE, no --filter.
    let out = cmd
        .args(["mv", "archive/"])
        .output()
        .expect("execute xv mv");
    assert!(
        !out.status.success(),
        "stdout: {}",
        common::stdout_str(&out)
    );
}

#[test]
fn mv_filter_dry_run_previews_without_moving() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-a", "v1", None);
    set_secret(temp.path(), "test-b", "v2", None);
    set_secret(temp.path(), "latest-x", "v3", None);

    let out = cmd
        .args(["mv", "--filter", "test-*", "archive/", "--dry-run"])
        .output()
        .expect("execute xv mv --filter --dry-run");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(
        stdout.contains("test-a -> archive/test-a"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("test-b -> archive/test-b"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("latest-x"),
        "dry-run must not list non-matches: {stdout}"
    );

    let json = ls_json(temp.path());
    assert_eq!(
        folder_of(&json, "test-a"),
        None,
        "dry-run must not mutate: {json}"
    );
    assert_eq!(
        folder_of(&json, "test-b"),
        None,
        "dry-run must not mutate: {json}"
    );
}

#[test]
fn mv_filter_non_tty_without_yes_refuses() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-a", "v1", None);
    set_secret(temp.path(), "test-b", "v2", None);

    // Tests run with piped stdin (not a TTY): without --yes this must refuse.
    let out = cmd
        .args(["mv", "--filter", "test-*", "archive/"])
        .output()
        .expect("execute xv mv --filter");
    assert!(
        !out.status.success(),
        "stdout: {}",
        common::stdout_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("--yes"), "should mention --yes: {stderr}");

    let json = ls_json(temp.path());
    assert_eq!(
        folder_of(&json, "test-a"),
        None,
        "refusal must not mutate: {json}"
    );
}

#[test]
fn mv_filter_yes_bypasses_confirmation() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-a", "v1", None);

    let out = cmd
        .args(["mv", "--filter", "test-*", "archive/", "--yes"])
        .output()
        .expect("execute xv mv --filter --yes");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let json = ls_json(temp.path());
    assert_eq!(folder_of(&json, "test-a"), Some("archive"), "{json}");
}

#[test]
fn mv_filter_already_in_dest_is_skipped_not_error() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-already", "v1", Some("archive"));
    set_secret(temp.path(), "test-new", "v2", None);

    let out = cmd
        .args(["mv", "--filter", "test-*", "archive/", "--yes"])
        .output()
        .expect("execute xv mv --filter --yes");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    let stderr = common::stderr_str(&out);
    assert!(
        stderr.contains("already in") || stdout.contains("already in"),
        "already-in-dest secret should be noted, not silently ignored: stdout={stdout} stderr={stderr}"
    );

    let json = ls_json(temp.path());
    assert_eq!(folder_of(&json, "test-already"), Some("archive"), "{json}");
    assert_eq!(folder_of(&json, "test-new"), Some("archive"), "{json}");
}

#[test]
fn mv_filter_all_matches_already_in_dest_is_a_noop_not_error() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-already", "v1", Some("archive"));

    let out = cmd
        .args(["mv", "--filter", "test-*", "archive/"])
        .output()
        .expect("execute xv mv --filter");
    assert!(
        out.status.success(),
        "all-already-in-dest must succeed with nothing to move: stderr: {}",
        common::stderr_str(&out)
    );

    let json = ls_json(temp.path());
    assert_eq!(folder_of(&json, "test-already"), Some("archive"), "{json}");
}

#[test]
fn mv_filter_zero_matches_fails_loud() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "latest-x", "v1", None);

    let out = cmd
        .args(["mv", "--filter", "test-*", "archive/", "--yes"])
        .output()
        .expect("execute xv mv --filter --yes");
    assert!(
        !out.status.success(),
        "zero matches must fail loud: stdout: {}",
        common::stdout_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(
        stderr.contains("no secrets matched --filter 'test-*'"),
        "{stderr}"
    );
}

#[test]
fn mv_filter_invalid_glob_errors_before_listing() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-a", "v1", None);

    let out = cmd
        .args(["mv", "--filter", "test-[", "archive/"])
        .output()
        .expect("execute xv mv --filter");
    assert!(
        !out.status.success(),
        "invalid glob should fail: stdout: {}",
        common::stdout_str(&out)
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "invalid_argument exit code is 2: stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("Invalid glob pattern"), "stderr: {stderr}");

    // Nothing was moved: the (only) secret is untouched (still no folder).
    let json = ls_json(temp.path());
    assert_eq!(folder_of(&json, "test-a"), None, "{json}");
}
