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
fn attached_rename_applies_offline_and_resumes_after_process_restart() {
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
    std::fs::write(temp.path().join("proof.txt"), b"private-attachment-content").unwrap();
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["attach", "cert", "proof.txt"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recovery = temp.path().join("recovery");
    let base = [
        "transfer",
        "cert",
        "--from",
        "default",
        "--to",
        "default",
        "--new-name",
        "renamed",
        "--move",
    ];
    let before = store_bytes(&temp.path().join("store"));
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(base)
        .arg("--apply")
        .arg("--recovery-dir")
        .arg(&recovery)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(before, store_bytes(&temp.path().join("store")));
    assert!(!recovery.exists());

    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(base)
        .args(["--apply", "--offline"])
        .arg("--recovery-dir")
        .arg(&recovery)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["complete"], true);
    let id = report["id"].as_str().unwrap();
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["get", "renamed", "--raw"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "private-secret-value"
    );
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["get", "cert", "--raw"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "attachments",
            "renamed",
            "--get",
            "proof.txt",
            "--output",
            "proof-restored.txt",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(temp.path().join("proof-restored.txt")).unwrap(),
        b"private-attachment-content"
    );
    let after = store_bytes(&temp.path().join("store"));
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(base)
        .args(["--resume", id, "--offline"])
        .arg("--recovery-dir")
        .arg(&recovery)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(after, store_bytes(&temp.path().join("store")));
    for content in store_bytes(&recovery).values() {
        assert!(!content
            .windows(b"private-secret-value".len())
            .any(|part| part == b"private-secret-value"));
        assert!(!content
            .windows(b"private-attachment-content".len())
            .any(|part| part == b"private-attachment-content"));
    }
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
    assert_eq!(preview["execution_supported"], true);
    assert_eq!(preview["intent"]["destination_name"], "certificate");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("private-secret-value"));
    assert!(!text.contains("private-attachment-content"));
    assert_eq!(before, store_bytes(&temp.path().join("store")));
}

#[test]
fn generic_transfers_refuse_attachments_before_changing_secrets() {
    let (mut command, temp) = common::xv_isolated_local();
    let output = command
        .args(["set", "cert", "--value", "source-value"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(temp.path().join("certificate.pem"), b"attachment-content").unwrap();
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

    let cases: &[&[&str]] = &[
        &[
            "copy",
            "cert",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "copied",
        ],
        &[
            "move",
            "cert",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "moved",
            "--force",
        ],
        &[
            "update",
            "cert",
            "--rename",
            "renamed",
            "--note",
            "must-not-change",
            "--yes",
        ],
        &["mv", "cert", "archive/renamed", "--yes"],
        &["mv", "cert", "archive/renamed", "--dry-run"],
    ];
    for args in cases {
        let output = common::xv_existing_isolated_local(temp.path(), temp.path())
            .args(*args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "unexpected success for {args:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("attachment"),
            "wrong refusal for {args:?}: {error}"
        );
        assert_eq!(
            before,
            store_bytes(&temp.path().join("store")),
            "mutated store for {args:?}"
        );
    }
}

#[test]
fn forced_move_preserves_an_attached_destination() {
    let (mut command, temp) = common::xv_isolated_local();
    let output = command
        .args(["set", "source", "--value", "source-value"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["set", "destination", "--value", "destination-value"])
        .output()
        .unwrap();
    assert!(output.status.success());
    std::fs::write(temp.path().join("certificate.pem"), b"attachment-content").unwrap();
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["attach", "destination", "certificate.pem"])
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
            "move",
            "source",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "destination",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("attachment"));
    assert_eq!(before, store_bytes(&temp.path().join("store")));
}

#[test]
fn local_case_alias_move_preserves_attached_source() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "source", "--value", "fixture"])
        .output()
        .unwrap()
        .status
        .success());
    std::fs::write(temp.path().join("proof.txt"), b"proof").unwrap();
    assert!(common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["attach", "source", "proof.txt"])
        .output()
        .unwrap()
        .status
        .success());
    let secrets = temp.path().join("store/vaults/default/secrets");
    // Reproduce filesystem alias lookup deterministically on case-sensitive CI.
    for suffix in ["meta.json", "age"] {
        let alias = secrets.join(format!("SOURCE.{suffix}"));
        if !alias.exists() {
            std::fs::hard_link(secrets.join(format!("source.{suffix}")), alias).unwrap();
        }
    }
    let before = store_bytes(&temp.path().join("store"));
    for verb in ["move", "transfer"] {
        let mut command = common::xv_existing_isolated_local(temp.path(), temp.path());
        command.args([
            verb,
            "SOURCE",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "destination",
        ]);
        if verb == "move" {
            command.arg("--force");
        }
        let output = command.output().unwrap();
        assert!(
            !output.status.success(),
            "{verb} silently accepted a physical secret alias"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("attachment"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(before, store_bytes(&temp.path().join("store")));
    }
}

#[test]
fn local_case_alias_force_preserves_attached_destination() {
    local_case_alias_force_destination(false);
}
#[test]
fn local_case_alias_force_preserves_orphan_destination() {
    local_case_alias_force_destination(true);
}
fn local_case_alias_force_destination(orphan: bool) {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "source", "--value", "fixture"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["set", "destination", "--value", "fixture"])
        .output()
        .unwrap()
        .status
        .success());
    std::fs::write(temp.path().join("proof.txt"), b"proof").unwrap();
    assert!(common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["attach", "destination", "proof.txt"])
        .output()
        .unwrap()
        .status
        .success());
    let secrets = temp.path().join("store/vaults/default/secrets");
    if orphan {
        for suffix in ["meta.json", "age"] {
            std::fs::remove_file(secrets.join(format!("destination.{suffix}"))).unwrap();
        }
        let files = temp.path().join("store/vaults/default/files");
        for suffix in ["meta.json", "age"] {
            let alias = files.join(format!("attachments%2FDESTINATION%2Fproof.txt.{suffix}"));
            if !alias.exists() {
                std::fs::hard_link(
                    files.join(format!("attachments%2Fdestination%2Fproof.txt.{suffix}")),
                    alias,
                )
                .unwrap();
            }
        }
    } else {
        for suffix in ["meta.json", "age"] {
            let alias = secrets.join(format!("DESTINATION.{suffix}"));
            if !alias.exists() {
                std::fs::hard_link(secrets.join(format!("destination.{suffix}")), alias).unwrap();
            }
        }
    }
    let before = store_bytes(&temp.path().join("store"));
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "move",
            "source",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "DESTINATION",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "force accepted destination physical alias (orphan={orphan})"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("attachment"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(before, store_bytes(&temp.path().join("store")));
}
