#![cfg(feature = "file-ops")]
use crosstache::secret::attachment_transfer_execution::{RecoveryStore, TransferSummary};

#[test]
fn default_feature_consumers_can_list_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let entries: Vec<TransferSummary> = RecoveryStore::new(dir.path().join("absent"))
        .list()
        .unwrap();
    assert!(entries.is_empty());
}

#[test]
fn environment_override_resolves_relative_paths_outside_git() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(work.join(".git")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "environment_override_child", "--nocapture"])
        .current_dir(&work)
        .env("XV_RECOVERY_TEST_CHILD", dir.path().canonicalize().unwrap())
        .env("XV_TRANSFER_RECOVERY_DIR", "../recovery")
        .env("XDG_CONFIG_HOME", &work)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!dir.path().join("recovery").exists());
}

#[test]
fn environment_override_child() {
    let Some(root) = std::env::var_os("XV_RECOVERY_TEST_CHILD") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let resolved = RecoveryStore::default_path().unwrap();
    assert_eq!(resolved.parent().unwrap().canonicalize().unwrap(), root);
    assert_eq!(resolved.file_name().unwrap(), "recovery");
    assert!(RecoveryStore::new(root.join("explicit"))
        .list()
        .unwrap()
        .is_empty());
    assert!(!root.join("explicit").exists());
}
