#![cfg(feature = "file-ops")]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn store_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, files);
            } else {
                files.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, &mut files);
    files
}

#[test]
fn attached_transfer_preview_is_json_and_preserves_the_store() {
    let (mut command, temp) = common::xv_isolated_local();
    let output = command
        .args(["set", "cert", "--value", "private-secret-value"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(
        temp.path().join("certificate.pem"),
        b"private-attachment-content",
    )
    .unwrap();
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["attach", "cert", "certificate.pem"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before = store_bytes(&temp.path().join("store"));
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "transfer",
            "cert",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "certificate",
            "--move",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let preview: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(preview["attachment_count"], 1);
    assert_eq!(preview["execution_supported"], false);
    assert_eq!(preview["intent"]["destination_name"], "certificate");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("private-secret-value"));
    assert!(!text.contains("private-attachment-content"));
    assert_eq!(before, store_bytes(&temp.path().join("store")));
}
