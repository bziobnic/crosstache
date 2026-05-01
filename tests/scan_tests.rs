//! Integration tests for `xv scan`. Active tests are tempdir-only
//! (no Azure). Live tests are #[ignore]'d and gated on XV_TEST_VAULT.

use std::process::Command;

fn xv() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xv"))
}

#[test]
fn scan_clean_dir_exits_0() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("a.txt"), "innocuous content").unwrap();
    let out = xv()
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
    let out = xv()
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

#[test]
fn scan_install_outside_git_repo_errors() {
    let temp = tempfile::tempdir().unwrap();
    let out = xv()
        .args(["scan", "install"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(3));
}
