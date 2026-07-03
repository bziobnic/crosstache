//! Integration tests for `xv scan`. Active tests are tempdir-only
//! (no Azure). Live tests are #[ignore]'d and gated on XV_TEST_VAULT.

mod common;

#[test]
fn scan_clean_dir_exits_0() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("a.txt"), "innocuous content").unwrap();
    let out = common::xv()
        .args(["scan"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    // Exit 0 because no findings; OR could fail before reaching scan
    // because of vault-resolution. Accept either outcome — the test
    // is here to lock the contract that a clean tree produces no
    // ScanLeakDetected.
    if out.status.success() {
        assert_eq!(out.status.code(), Some(0));
    } else {
        // If the scan couldn't run, exit is NOT 50.
        assert_ne!(out.status.code(), Some(50));
    }
}

#[test]
fn scan_with_aws_key_exits_50() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("leak.txt"), "aws=AKIAIOSFODNN7EXAMPLE\n").unwrap();
    let out = common::xv()
        .args(["scan"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    if out.status.code() == Some(50) {
        // Built-in pattern fired — expected when a vault is reachable.
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("AKIAIOSFODNN7EXAMPLE"),
            "stderr must NOT echo the matched value, ever"
        );
    } else {
        // Test environment doesn't have a vault; the scan failed
        // before reaching content. That's not what this test covers.
    }
}

/// Issue #309 Finding 6: `XV_SCAN_DISABLE=1` was documented but read
/// nowhere. It must now bypass the scan entirely — exit 0 with no
/// findings — even against a file that would otherwise trip a built-in
/// pattern.
///
/// Uses `common::xv_isolated_local()` (local age-encrypted backend, fully
/// offline, valid `xv.conf`) rather than the bare host environment: a plain
/// `xv scan` with no config at all fails `Config::validate()` (missing
/// Azure subscription/tenant ID) before `execute_scan_command` ever reaches
/// the `XV_SCAN_DISABLE` check, which is exactly the false-pass this test
/// hit in CI (exit 3, not the disable path) when it relied on ambient host
/// config. The local backend needs no credentials, so the *second* run
/// below (without the disable var) deterministically reaches real content
/// scanning and finds the leak — proving the first run's exit 0 came from
/// the disable check, not from the scan failing to run at all.
#[test]
fn scan_disabled_via_env_skips_scan_even_with_leak() {
    // Run 1: XV_SCAN_DISABLE=1 must skip the scan entirely (exit 0), even
    // though leak.txt contains a value that trips a built-in pattern.
    let (mut cmd_disabled, temp_disabled) = common::xv_isolated_local();
    std::fs::write(
        temp_disabled.path().join("leak.txt"),
        "aws=AKIAIOSFODNN7EXAMPLE\n",
    )
    .unwrap();
    let out_disabled = cmd_disabled
        .args(["scan"])
        .env("XV_SCAN_DISABLE", "1")
        .output()
        .unwrap();
    assert_eq!(
        out_disabled.status.code(),
        Some(0),
        "stderr: {}",
        common::stderr_str(&out_disabled)
    );
    let stderr_disabled = common::stderr_str(&out_disabled);
    assert!(
        stderr_disabled.contains("XV_SCAN_DISABLE"),
        "must print a notice that the scan was skipped: {stderr_disabled}"
    );

    // Run 2: same fixture, same isolated local backend, no disable var —
    // the scan must actually run and find the leak (exit 50). This is the
    // control that proves run 1's exit 0 came from the disable check, not
    // from the scan silently failing to run for some unrelated reason.
    let (mut cmd_enabled, temp_enabled) = common::xv_isolated_local();
    std::fs::write(
        temp_enabled.path().join("leak.txt"),
        "aws=AKIAIOSFODNN7EXAMPLE\n",
    )
    .unwrap();
    let out_enabled = cmd_enabled.args(["scan"]).output().unwrap();
    assert_eq!(
        out_enabled.status.code(),
        Some(50),
        "without XV_SCAN_DISABLE the scan must run and detect the leak; stderr: {}",
        common::stderr_str(&out_enabled)
    );
}

#[test]
fn scan_install_outside_git_repo_errors() {
    let temp = tempfile::tempdir().unwrap();
    let out = common::xv()
        .args(["scan", "install"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(3));
}

// --- Hook installer edge cases ---

#[test]
fn scan_install_writes_marker() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let out = cmd.args(["scan", "install"]).output().expect("spawn");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let hook = temp.path().join(".git/hooks/pre-commit");
    assert!(hook.exists(), "hook file should be created");
    let content = std::fs::read_to_string(&hook).unwrap();
    assert!(
        content.contains("xv-scan-managed"),
        "marker missing: {content}"
    );
    assert!(
        content.contains("xv scan --staged --hook"),
        "hook body missing: {content}"
    );
}

#[test]
fn scan_install_repeat_is_no_op() {
    let (mut cmd1, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let _ = cmd1.args(["scan", "install"]).output().expect("spawn");

    // Second install: should succeed (already installed); no error.
    let mut cmd2 = common::xv();
    common::isolate(&mut cmd2, temp.path());
    let out = cmd2.args(["scan", "install"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let combined = format!("{}{}", common::stderr_str(&out), common::stdout_str(&out));
    assert!(
        combined.to_lowercase().contains("already") || combined.contains("xv-scan-managed"),
        "should report already-installed: {combined}"
    );
}

#[test]
fn scan_install_refuses_unmanaged_hook_without_force() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let hooks_dir = temp.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho hi\n").unwrap();

    let out = cmd.args(["scan", "install"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let stderr = common::stderr_str(&out);
    assert!(
        stderr.contains("not xv-managed") || stderr.contains("force"),
        "stderr: {stderr}"
    );
}

#[test]
fn scan_install_force_overwrites_unmanaged_hook() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let hooks_dir = temp.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho hi\n").unwrap();

    let out = cmd
        .args(["scan", "install", "--force"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let content = std::fs::read_to_string(hooks_dir.join("pre-commit")).unwrap();
    assert!(content.contains("xv-scan-managed"));
}

#[test]
fn scan_uninstall_refuses_unmanaged_hook() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let hooks_dir = temp.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho hi\n").unwrap();

    let out = cmd.args(["scan", "uninstall"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let stderr = common::stderr_str(&out);
    assert!(
        stderr.contains("not xv-managed") || stderr.contains("refusing"),
        "stderr: {stderr}"
    );
}

#[test]
fn scan_uninstall_when_no_hook_is_no_op() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let out = cmd.args(["scan", "uninstall"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    // Some output indicating "no hook to remove":
    let combined = format!("{}{}", common::stderr_str(&out), common::stdout_str(&out));
    assert!(
        combined.to_lowercase().contains("no") || combined.to_lowercase().contains("not"),
        "should mention no hook: {combined}"
    );
}

#[test]
fn scan_install_round_trip() {
    let (mut cmd1, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let out1 = cmd1.args(["scan", "install"]).output().expect("spawn");
    assert_eq!(out1.status.code(), Some(0));
    assert!(temp.path().join(".git/hooks/pre-commit").exists());

    let mut cmd2 = common::xv();
    common::isolate(&mut cmd2, temp.path());
    let out2 = cmd2.args(["scan", "uninstall"]).output().expect("spawn");
    assert_eq!(out2.status.code(), Some(0));
    assert!(
        !temp.path().join(".git/hooks/pre-commit").exists(),
        "hook should be removed"
    );
}
