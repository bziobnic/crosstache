//! End-to-end CLI tests for `--filter <GLOB>` on `xv ls` and `xv find`
//! (2026-07-03 design doc: docs/superpowers/specs/2026-07-03-ls-find-filter-design.md).
//!
//! Hermetic: every test uses the isolated local-backend harness from
//! `tests/common/mod.rs` — no Azure credentials or network access required.
//!
//! Run with:
//!   cargo test --test e2e_filter_glob

mod common;

/// Builds a fresh `xv` `Command` bound to the same isolated store/env as an
/// existing `xv_isolated_local()` tempdir, for a second (or third, ...) CLI
/// invocation against the same store (each `Command` is single-use). Mirrors
/// `xv_same_env` in `tests/e2e_record_types.rs`.
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

/// Set a secret with a plain value via `xv set NAME --value VALUE`.
fn set_secret(temp: &std::path::Path, name: &str, value: &str) {
    let out = xv_same_env(temp)
        .args(["set", name, "--value", value])
        .output()
        .expect("execute xv set");
    assert!(
        out.status.success(),
        "xv set {name} failed: stderr: {}",
        common::stderr_str(&out)
    );
}

// ===========================================================================
// `xv ls --filter`
// ===========================================================================

#[test]
fn ls_filter_prefix_anchoring() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-db", "v1");
    set_secret(temp.path(), "latest-db", "v2");

    let out = cmd
        .args(["ls", "--filter", "test-*", "--names-only"])
        .output()
        .expect("execute xv ls");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(
        stdout.contains("test-db"),
        "filter 'test-*' should match test-db: {stdout}"
    );
    assert!(
        !stdout.contains("latest-db"),
        "filter 'test-*' must NOT match latest-db (prefix anchoring): {stdout}"
    );
}

/// Either-name matching (design doc's "matching rule"): a filter matches
/// either the user-facing display name or the backend (sanitized) name.
/// The local backend never sanitizes (name == original_name always — see
/// `set_secret` in `src/backend/local/secrets.rs`), so this test can only
/// exercise the display-name side end-to-end; the backend-name side of the
/// predicate is covered directly by
/// `crate::utils::helpers::tests::test_glob_matches_either_name` in
/// `src/utils/helpers.rs`, with a synthetic summary whose `name` and
/// `original_name` differ.
#[test]
fn ls_filter_matches_display_name() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "display-thing", "v1");

    let out = cmd
        .args(["ls", "--filter", "display-*", "--names-only"])
        .output()
        .expect("execute xv ls");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("display-thing"), "stdout: {stdout}");
}

#[test]
fn ls_filter_glob_question_mark() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "abc", "v1");
    set_secret(temp.path(), "abcd", "v2");

    let out = cmd
        .args(["ls", "--filter", "ab?", "--names-only"])
        .output()
        .expect("execute xv ls");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(
        stdout.contains("abc"),
        "'?' should match one char: {stdout}"
    );
    assert!(
        !stdout.contains("abcd"),
        "'?' must not match two chars: {stdout}"
    );
}

#[test]
fn ls_filter_glob_char_class() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "fao", "v1");
    set_secret(temp.path(), "fbo", "v2");
    set_secret(temp.path(), "fco", "v3");

    let out = cmd
        .args(["ls", "--filter", "f[ab]o", "--names-only"])
        .output()
        .expect("execute xv ls");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("fao"), "stdout: {stdout}");
    assert!(stdout.contains("fbo"), "stdout: {stdout}");
    assert!(!stdout.contains("fco"), "stdout: {stdout}");
}

#[test]
fn ls_filter_composes_with_type() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = xv_same_env(temp.path())
        .args([
            "set",
            "test-login",
            "--type",
            "login",
            "--value",
            "hunter2",
            "--field",
            "username=alice",
        ])
        .output()
        .expect("execute xv set --type");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    set_secret(temp.path(), "test-plain", "v1");
    set_secret(temp.path(), "other-login-shape", "v2");

    let out = cmd
        .args([
            "ls",
            "--filter",
            "test-*",
            "--type",
            "login",
            "--names-only",
        ])
        .output()
        .expect("execute xv ls");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(
        stdout.contains("test-login"),
        "should match glob + type: {stdout}"
    );
    assert!(
        !stdout.contains("test-plain"),
        "type filter should exclude untyped secret: {stdout}"
    );
    assert!(
        !stdout.contains("other-login-shape"),
        "glob filter should exclude non-matching name: {stdout}"
    );
}

#[test]
fn ls_filter_composes_with_folder_scope() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = xv_same_env(temp.path())
        .args(["set", "test-db", "--value", "v1", "--folder", "prod"])
        .output()
        .expect("execute xv set");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    set_secret(temp.path(), "test-other", "v2"); // root, not in 'prod'

    let out = cmd
        .args(["ls", "prod", "--filter", "test-*", "--names-only"])
        .output()
        .expect("execute xv ls");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("test-db"), "stdout: {stdout}");
    assert!(
        !stdout.contains("test-other"),
        "folder scope should exclude root secret: {stdout}"
    );
}

#[test]
fn ls_filter_composes_with_deleted() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-deleted", "v1");
    set_secret(temp.path(), "latest-deleted", "v2");

    let out = xv_same_env(temp.path())
        .args(["rm", "test-deleted", "--force"])
        .output()
        .expect("execute xv rm");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["rm", "latest-deleted", "--force"])
        .output()
        .expect("execute xv rm");
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let out = cmd
        .args(["ls", "--deleted", "--filter", "test-*", "--names-only"])
        .output()
        .expect("execute xv ls --deleted");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("test-deleted"), "stdout: {stdout}");
    assert!(
        !stdout.contains("latest-deleted"),
        "prefix anchoring should exclude latest-deleted: {stdout}"
    );
}

#[test]
fn ls_filter_invalid_glob_errors_before_listing() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-db", "v1");

    let out = cmd
        .args(["ls", "--filter", "test-["])
        .output()
        .expect("execute xv ls");
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
}

// ===========================================================================
// `xv find --filter`
// ===========================================================================

/// Placement matters, not just presence: a filter applied *after* fuzzy
/// scoring can produce the exact same `--names-only` output as a correct
/// pre-filter whenever `--min-score`'s relative cutoff happens to land in
/// the same place either way (e.g. with the default 0.3 and no excluded
/// candidate scoring anywhere near the top). To make the placement itself
/// observable, this test includes a filtered-OUT candidate ("alpha", bare)
/// that is the single highest fuzzy scorer for the pattern "alpha" — higher
/// even than "test-alpha", which the glob *does* admit — and pairs it with
/// an explicit `--min-score` tuned so the two placements disagree on
/// whether a third, low (but real) scorer survives:
///
/// Scores against pattern "alpha" (verified via `--format json`): bare
/// "alpha" = 140, "latest-alpha" = "test-alpha" = 128, "test-a1l2p3h4a" = 84
/// (gapped match), "test-zzz" = no match at all (dropped unconditionally).
///
/// - Correct (pre-filter): candidate set is glob-restricted to
///   {test-alpha, test-a1l2p3h4a, test-zzz} *before* scoring, so the top
///   score used for the relative cutoff is test-alpha's 128. At
///   `--min-score 0.62`, cutoff = ceil(128 * 0.62) = 80, and 84 >= 80, so
///   test-a1l2p3h4a survives.
/// - Buggy (post-scoring filter): the cutoff is computed against the full,
///   unfiltered candidate set, so bare "alpha" (140) sets the top score
///   instead. cutoff = ceil(140 * 0.62) = 87, and 84 < 87 — test-a1l2p3h4a
///   is dropped before the glob filter even gets a chance to run, even
///   though it belongs in the answer. (Empirically confirmed against a
///   temporarily bug-simulated build during development of this test: the
///   post-scoring placement returns only `test-alpha`, missing
///   `test-a1l2p3h4a`.)
///
/// So this test fails under a post-scoring filter placement but passes
/// under the correct pre-filter placement — the earlier bare "excludes
/// latest-alpha" assertion alone could not tell the two apart.
#[test]
fn find_filter_prefilters_before_fuzzy_scoring() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "alpha", "v0"); // excluded by filter; top fuzzy scorer
    set_secret(temp.path(), "test-alpha", "v1");
    set_secret(temp.path(), "test-beta", "v2");
    set_secret(temp.path(), "latest-alpha", "v3");
    set_secret(temp.path(), "test-a1l2p3h4a", "v4"); // included; low (gapped) score
    set_secret(temp.path(), "test-zzz", "v5"); // included; no fuzzy match at all

    // "alpha" fuzzy-matches "alpha", "latest-alpha", and "test-alpha", but
    // --filter 'test-*' hard-excludes "alpha" and "latest-alpha" before
    // scoring even starts.
    let out = cmd
        .args(["find", "alpha", "--filter", "test-*", "--names-only"])
        .output()
        .expect("execute xv find");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("test-alpha"), "stdout: {stdout}");
    assert!(
        !stdout.contains("latest-alpha"),
        "hard pre-filter should exclude latest-alpha even though it fuzzy-matches 'alpha': {stdout}"
    );
    assert!(
        !stdout.contains("test-beta"),
        "'test-beta' does not fuzzy-match 'alpha': {stdout}"
    );

    // The placement-sensitive case: --min-score's cutoff must be computed
    // relative to the top score *within the glob-filtered set* (128, from
    // test-alpha), not the unfiltered set (140, from the excluded "alpha").
    let out2 = xv_same_env(temp.path())
        .args([
            "find",
            "alpha",
            "--filter",
            "test-*",
            "--min-score",
            "0.62",
            "--format",
            "json",
        ])
        .output()
        .expect("execute xv find --format json");
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let json: serde_json::Value =
        serde_json::from_str(&common::stdout_str(&out2)).expect("find --format json output");
    let names: Vec<&str> = json
        .as_array()
        .expect("json array")
        .iter()
        .map(|v| v["name"].as_str().expect("name field"))
        .collect();
    assert!(
        names.contains(&"test-alpha"),
        "top-of-filtered-set survivor should always pass: {names:?}"
    );
    assert!(
        names.contains(&"test-a1l2p3h4a"),
        "test-a1l2p3h4a (score 84) must survive a cutoff computed from the \
         filtered set's top score (128 * 0.62 = 80) — a post-scoring filter \
         would instead compute the cutoff from the excluded 'alpha' (140 * \
         0.62 = 87) and wrongly drop it: {names:?}"
    );
    assert_eq!(
        names.len(),
        2,
        "only the two glob-admitted, score-surviving secrets should appear: {names:?}"
    );
}

#[test]
fn find_filter_no_pattern_is_unranked_list() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-one", "v1");
    set_secret(temp.path(), "test-two", "v2");
    set_secret(temp.path(), "other", "v3");

    let out = cmd
        .args(["find", "--filter", "test-*", "--names-only"])
        .output()
        .expect("execute xv find");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("test-one"), "stdout: {stdout}");
    assert!(stdout.contains("test-two"), "stdout: {stdout}");
    assert!(!stdout.contains("other"), "stdout: {stdout}");
}

#[test]
fn find_filter_names_only_is_pipe_friendly() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-a", "v1");
    set_secret(temp.path(), "test-b", "v2");
    set_secret(temp.path(), "skip-me", "v3");

    let out = cmd
        .args(["find", "--filter", "test-*", "--names-only"])
        .output()
        .expect("execute xv find");
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 2, "bare names only, one per line: {stdout}");
    assert!(lines.contains(&"test-a"), "stdout: {stdout}");
    assert!(lines.contains(&"test-b"), "stdout: {stdout}");
}

#[test]
fn find_filter_invalid_glob_errors_before_listing() {
    let (mut cmd, temp) = common::xv_isolated_local();
    set_secret(temp.path(), "test-db", "v1");

    let out = cmd
        .args(["find", "--filter", "test-["])
        .output()
        .expect("execute xv find");
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
}
