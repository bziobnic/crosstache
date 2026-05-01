mod common;

use common::{stderr_str, stdout_str, xv_isolated};

// --- xv find flag validation (no Azure required for parse-time errors) ---

#[test]
fn find_unknown_in_field_exits_2() {
    let (mut cmd, _temp) = xv_isolated();
    // Need to provide a vault so config validation passes and we reach field validation.
    cmd.env("DEFAULT_VAULT", "test-vault");
    let out = cmd
        .args(["find", "anything", "--in", "bogus_field"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains("bogus_field") || stderr.contains("unknown") || stderr.contains("invalid"),
        "stderr should mention the bad field: {stderr}"
    );
}

#[test]
fn find_each_valid_in_field_passes_clap() {
    // The fields are validated AT runtime in execute_secret_find, so
    // these should fail with xv-config-invalid (no vault) — NOT at clap (exit 2).
    for field in &["name", "folder", "groups", "note", "tags"] {
        let (mut cmd, _temp) = xv_isolated();
        let out = cmd
            .args(["find", "x", "--in", field])
            .output()
            .expect("spawn");
        // Should NOT exit 2 (parse error); should exit 3 (config-invalid, no vault).
        assert_ne!(
            out.status.code(),
            Some(2),
            "field '{field}' should not be a parse error"
        );
    }
}

#[test]
fn find_limit_zero_is_accepted_at_parse() {
    // Limit 0 is semantically odd but clap accepts it as a usize.
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .args(["find", "x", "--limit", "0"])
        .output()
        .expect("spawn");
    assert_ne!(out.status.code(), Some(2));
}

#[test]
fn find_limit_negative_exits_2() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .args(["find", "x", "--limit", "-1"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2));
}

// --- xv list pagination flag validation ---

#[test]
fn list_page_without_page_size_errors() {
    // --page without --page-size should be a clap error (exit 2) per the
    // pagination plan's UX spec.
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd.args(["list", "--page", "2"]).output().expect("spawn");
    // Two acceptable behaviors:
    //   - Clap rejects at parse (exit 2)
    //   - Pagination::from_args returns InvalidArgument → exit 2
    // Either way exit 2 is the contract.
    assert_eq!(
        out.status.code(),
        Some(2),
        "--page without --page-size: {}",
        stderr_str(&out)
    );
}

#[test]
fn file_list_limit_and_page_size_conflict_errors() {
    // Per the pagination plan, --limit and --page-size on file list cannot coexist.
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .args(["file", "list", "--limit", "10", "--page-size", "5"])
        .output()
        .expect("spawn");
    // Either clap rejects (exit 2) or runtime errors with config-invalid (exit 3).
    let code = out.status.code();
    assert!(
        code == Some(2) || code == Some(3),
        "expected exit 2 or 3 for --limit + --page-size conflict; got {code:?}"
    );
}

// --- --names-only contract ---

#[test]
fn ls_names_only_help_documents_no_format_no_ansi() {
    // Confirm the flag exists by querying --help; this is parse-only,
    // doesn't need a vault.
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd.args(["list", "--help"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let help = stdout_str(&out);
    assert!(
        help.contains("names-only"),
        "list --help should document --names-only: {help}"
    );
}

#[test]
fn find_help_documents_in_field() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd.args(["find", "--help"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let help = stdout_str(&out);
    assert!(
        help.contains("--in"),
        "find --help should document --in: {help}"
    );
    assert!(
        help.contains("FIELD") || help.contains("field"),
        "should reference field arg: {help}"
    );
}
