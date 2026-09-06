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

#[test]
fn generic_copy_opt_in_transfers_attachment_and_preserves_source() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "source", "--value", "fixture"])
        .output()
        .unwrap()
        .status
        .success());
    std::fs::write(temp.path().join("proof.txt"), b"attachment-proof").unwrap();
    assert!(common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["attach", "source", "proof.txt"])
        .output()
        .unwrap()
        .status
        .success());
    let before = store_bytes(&temp.path().join("store"));
    let missing_offline = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "copy",
            "source",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "copied",
            "--with-attachments",
        ])
        .arg("--recovery-dir")
        .arg(temp.path().join("recovery"))
        .output()
        .unwrap();
    assert!(!missing_offline.status.success());
    assert!(String::from_utf8_lossy(&missing_offline.stderr).contains("offline"));
    assert_eq!(before, store_bytes(&temp.path().join("store")));
    assert!(!temp.path().join("recovery").exists());
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "copy",
            "source",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "copied",
            "--with-attachments",
            "--offline",
        ])
        .arg("--recovery-dir")
        .arg(temp.path().join("recovery"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["source", "copied"] {
        let output = common::xv_existing_isolated_local(temp.path(), temp.path())
            .args([
                "attachments",
                name,
                "--get",
                "proof.txt",
                "--output",
                &format!("{name}.txt"),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(temp.path().join(format!("{name}.txt"))).unwrap(),
            b"attachment-proof"
        );
    }
}

#[test]
fn mixed_migration_to_missing_vault_refuses_without_creating_it() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "a-plain", "--value", "plain"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["set", "z-attached", "--value", "attached"])
        .output()
        .unwrap()
        .status
        .success());
    std::fs::write(temp.path().join("proof.txt"), b"proof").unwrap();
    assert!(common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["attach", "z-attached", "proof.txt"])
        .output()
        .unwrap()
        .status
        .success());
    let before = store_bytes(&temp.path().join("store"));
    let copy = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "copy",
            "z-attached",
            "--from",
            "default",
            "--to",
            "missing",
            "--with-attachments",
            "--offline",
            "--to-key-id",
            "missing-key",
        ])
        .arg("--recovery-dir")
        .arg(temp.path().join("recovery"))
        .output()
        .unwrap();
    assert!(!copy.status.success());
    assert!(
        String::from_utf8_lossy(&copy.stderr).contains("initialize"),
        "{}",
        String::from_utf8_lossy(&copy.stderr)
    );
    assert_eq!(before, store_bytes(&temp.path().join("store")));
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "migrate",
            "--from",
            "local:default",
            "--to",
            "local:missing",
            "--with-attachments",
            "--offline",
            "--to-key-id",
            "missing-key",
        ])
        .arg("--recovery-dir")
        .arg(temp.path().join("recovery"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("initialize"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(before, store_bytes(&temp.path().join("store")));
    assert!(!temp.path().join("store/vaults/missing").exists());
    assert!(!temp.path().join("recovery").exists());
}

fn run_ok(temp: &Path, args: &[&str]) -> Vec<u8> {
    let output = common::xv_existing_isolated_local(temp, temp)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn run_vault_ok(temp: &Path, vault: &str, args: &[&str]) -> Vec<u8> {
    let output = common::xv_existing_isolated_local(temp, temp)
        .env("DEFAULT_VAULT", vault)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn independent_key_destination(temp: &Path) -> String {
    run_ok(temp, &["vault", "create", "stage"]);
    let bytes = run_ok(
        temp,
        &[
            "--format",
            "json",
            "attachment-key",
            "initialize",
            "--vault",
            "stage",
            "--apply",
            "--offline",
        ],
    );
    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["report"]["active_key_id"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn cross_vault_copy_move_and_folder_resume_preserve_plaintext() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args([
            "set",
            "source",
            "--value",
            "secret-value",
            "--folder",
            "original"
        ])
        .output()
        .unwrap()
        .status
        .success());
    std::fs::write(temp.path().join("proof.txt"), b"cross-vault-plaintext").unwrap();
    run_ok(temp.path(), &["attach", "source", "proof.txt"]);
    let key = independent_key_destination(temp.path());
    let recovery = temp.path().join("recovery");
    let recovery = recovery.to_str().unwrap();
    run_ok(
        temp.path(),
        &[
            "copy",
            "source",
            "--from",
            "default",
            "--to",
            "stage",
            "--new-name",
            "copied",
            "--with-attachments",
            "--offline",
            "--to-key-id",
            &key,
            "--recovery-dir",
            recovery,
        ],
    );
    run_vault_ok(
        temp.path(),
        "stage",
        &[
            "attachments",
            "copied",
            "--get",
            "proof.txt",
            "--output",
            "copied.txt",
        ],
    );
    assert_eq!(
        std::fs::read(temp.path().join("copied.txt")).unwrap(),
        b"cross-vault-plaintext"
    );
    assert_eq!(
        String::from_utf8_lossy(&run_ok(temp.path(), &["get", "source", "--raw"])).trim(),
        "secret-value"
    );
    run_ok(
        temp.path(),
        &[
            "move",
            "copied",
            "--from",
            "stage",
            "--to",
            "stage",
            "--new-name",
            "generic-moved",
            "--with-attachments",
            "--offline",
            "--force",
            "--recovery-dir",
            recovery,
        ],
    );
    run_vault_ok(
        temp.path(),
        "stage",
        &[
            "attachments",
            "generic-moved",
            "--get",
            "proof.txt",
            "--output",
            "generic-moved.txt",
        ],
    );
    assert_eq!(
        std::fs::read(temp.path().join("generic-moved.txt")).unwrap(),
        b"cross-vault-plaintext"
    );
    assert!(!temp
        .path()
        .join("store/vaults/stage/secrets/copied.meta.json")
        .exists());
    let report = run_ok(
        temp.path(),
        &[
            "transfer",
            "source",
            "--from",
            "default",
            "--to",
            "stage",
            "--new-name",
            "moved",
            "--move",
            "--apply",
            "--offline",
            "--to-key-id",
            &key,
            "--to-folder",
            "/",
            "--recovery-dir",
            recovery,
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&report).unwrap();
    let id = report["id"].as_str().unwrap();
    let before = store_bytes(&temp.path().join("store"));
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "transfer",
            "source",
            "--from",
            "default",
            "--to",
            "stage",
            "--new-name",
            "moved",
            "--move",
            "--resume",
            id,
            "--offline",
            "--to-key-id",
            &key,
            "--recovery-dir",
            recovery,
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "resume must repeat explicit root intent"
    );
    assert_eq!(before, store_bytes(&temp.path().join("store")));
    run_ok(
        temp.path(),
        &[
            "transfer",
            "source",
            "--from",
            "default",
            "--to",
            "stage",
            "--new-name",
            "moved",
            "--move",
            "--resume",
            id,
            "--offline",
            "--to-key-id",
            &key,
            "--to-folder",
            "/",
            "--recovery-dir",
            recovery,
        ],
    );
    run_vault_ok(
        temp.path(),
        "stage",
        &[
            "attachments",
            "moved",
            "--get",
            "proof.txt",
            "--output",
            "moved.txt",
        ],
    );
    assert_eq!(
        std::fs::read(temp.path().join("moved.txt")).unwrap(),
        b"cross-vault-plaintext"
    );
    let moved_metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            temp.path()
                .join("store/vaults/stage/secrets/moved.meta.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        moved_metadata["folder"].is_null(),
        "explicit root must clear source folder"
    );
    let preserved_metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            temp.path()
                .join("store/vaults/stage/secrets/generic-moved.meta.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preserved_metadata["folder"], "original");
}

#[test]
fn migration_later_attachment_collision_leaves_plain_secret_unwritten() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "a-plain", "--value", "plain"])
        .output()
        .unwrap()
        .status
        .success());
    run_ok(temp.path(), &["set", "z-attached", "--value", "source"]);
    std::fs::write(temp.path().join("proof.txt"), b"proof").unwrap();
    run_ok(temp.path(), &["attach", "z-attached", "proof.txt"]);
    let key = independent_key_destination(temp.path());
    run_vault_ok(
        temp.path(),
        "stage",
        &["set", "z-attached", "--value", "destination"],
    );
    let before = store_bytes(&temp.path().join("store"));
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "migrate",
            "--from",
            "local:default",
            "--to",
            "local:stage",
            "--on-conflict",
            "replace",
            "--force-replace",
            "--with-attachments",
            "--offline",
            "--to-key-id",
            &key,
        ])
        .arg("--recovery-dir")
        .arg(temp.path().join("recovery"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(before, store_bytes(&temp.path().join("store")));
    assert!(!temp.path().join("recovery").exists());
}

#[test]
fn workspace_mv_saves_destination_folder_and_generic_move_cleans_source() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "source", "--value", "fixture"])
        .output()
        .unwrap()
        .status
        .success());
    std::fs::write(temp.path().join("proof.txt"), b"workspace-proof").unwrap();
    run_ok(temp.path(), &["attach", "source", "proof.txt"]);
    let key = independent_key_destination(temp.path());
    run_ok(
        temp.path(),
        &["cx", "add", "default", "--backend", "local", "--as", "work"],
    );
    run_ok(
        temp.path(),
        &["cx", "add", "stage", "--backend", "local", "--as", "stage"],
    );
    let recovery = temp.path().join("recovery");
    let recovery = recovery.to_str().unwrap();
    let before = store_bytes(&temp.path().join("store"));
    run_ok(
        temp.path(),
        &[
            "mv",
            "work:source",
            "stage:archive/moved",
            "--with-attachments",
            "--to-key-id",
            &key,
            "--dry-run",
            "--recovery-dir",
            recovery,
        ],
    );
    assert_eq!(before, store_bytes(&temp.path().join("store")));
    assert!(!Path::new(recovery).exists());
    run_ok(
        temp.path(),
        &[
            "mv",
            "work:source",
            "stage:archive/moved",
            "--with-attachments",
            "--to-key-id",
            &key,
            "--offline",
            "--recovery-dir",
            recovery,
        ],
    );
    let metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            temp.path()
                .join("store/vaults/stage/secrets/moved.meta.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["folder"], "archive");
    assert!(!temp
        .path()
        .join("store/vaults/default/secrets/source.meta.json")
        .exists());
    run_ok(
        temp.path(),
        &[
            "attachments",
            "stage:moved",
            "--get",
            "proof.txt",
            "--output",
            "workspace.txt",
        ],
    );
    assert_eq!(
        std::fs::read(temp.path().join("workspace.txt")).unwrap(),
        b"workspace-proof"
    );
}

#[test]
fn mixed_migration_copies_plain_and_attached_entries_after_preflight() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "a-plain", "--value", "plain"])
        .output()
        .unwrap()
        .status
        .success());
    run_ok(temp.path(), &["set", "z-attached", "--value", "source"]);
    std::fs::write(temp.path().join("proof.txt"), b"migrated-proof").unwrap();
    run_ok(temp.path(), &["attach", "z-attached", "proof.txt"]);
    let key = independent_key_destination(temp.path());
    assert!(!temp.path().join("store/vaults/stage/files").exists());
    let before = store_bytes(&temp.path().join("store"));
    let recovery = temp.path().join("recovery");
    let recovery = recovery.to_str().unwrap();
    run_ok(
        temp.path(),
        &[
            "migrate",
            "--from",
            "local:default",
            "--to",
            "local:stage",
            "--with-attachments",
            "--to-key-id",
            &key,
            "--dry-run",
            "--recovery-dir",
            recovery,
        ],
    );
    assert_eq!(before, store_bytes(&temp.path().join("store")));
    assert!(!Path::new(recovery).exists());
    run_ok(
        temp.path(),
        &[
            "migrate",
            "--from",
            "local:default",
            "--to",
            "local:stage",
            "--with-attachments",
            "--to-key-id",
            &key,
            "--offline",
            "--recovery-dir",
            recovery,
        ],
    );
    assert_eq!(
        String::from_utf8_lossy(&run_vault_ok(
            temp.path(),
            "stage",
            &["get", "a-plain", "--raw"]
        ))
        .trim(),
        "plain"
    );
    run_vault_ok(
        temp.path(),
        "stage",
        &[
            "attachments",
            "z-attached",
            "--get",
            "proof.txt",
            "--output",
            "migrated.txt",
        ],
    );
    assert_eq!(
        std::fs::read(temp.path().join("migrated.txt")).unwrap(),
        b"migrated-proof"
    );
    run_ok(temp.path(), &["get", "z-attached", "--raw"]);
}

#[test]
fn attachment_preflight_never_bootstraps_a_fresh_local_store() {
    let cases: &[&[&str]] = &[
        &["transfer", "source", "--from", "default", "--to", "stage"],
        &[
            "copy",
            "source",
            "--from",
            "default",
            "--to",
            "stage",
            "--with-attachments",
            "--dry-run",
        ],
        &[
            "migrate",
            "--from",
            "local:default",
            "--to",
            "local:stage",
            "--with-attachments",
            "--dry-run",
        ],
        &["attachment-key", "initialize", "--vault", "default"],
    ];
    for args in cases {
        let (mut command, temp) = common::xv_isolated_local();
        std::fs::remove_dir(temp.path().join("store")).unwrap();
        let output = command.args(*args).output().unwrap();
        assert!(
            !output.status.success(),
            "unexpected fresh-store success: {args:?}"
        );
        assert!(
            !temp.path().join("store").exists(),
            "preflight created store for {args:?}"
        );
        assert!(!temp.path().join("key.txt").exists());
        assert!(!temp.path().join("recipients.txt").exists());
    }
}
