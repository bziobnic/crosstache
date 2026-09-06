mod common;

#[test]
fn forced_plain_self_move_keeps_source_without_files_directory() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "source", "--value", "must-survive"])
        .output()
        .unwrap()
        .status
        .success());
    let files = temp.path().join("store/vaults/default/files");
    if files.exists() {
        std::fs::remove_dir_all(&files).unwrap();
    }
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "move", "source", "--from", "default", "--to", "default", "--force",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "self move must be refused before writes"
    );
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["get", "source", "--raw"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "must-survive"
    );
    assert!(!files.exists());
}

#[test]
fn plain_case_alias_force_move_is_refused() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "source", "--value", "must-survive"])
        .output()
        .unwrap()
        .status
        .success());
    let secrets = temp.path().join("store/vaults/default/secrets");
    for suffix in ["meta.json", "age"] {
        let alias = secrets.join(format!("SOURCE.{suffix}"));
        if !alias.exists() {
            std::fs::hard_link(secrets.join(format!("source.{suffix}")), alias).unwrap();
        }
    }
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "move",
            "SOURCE",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "different",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["get", "source", "--raw"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "must-survive"
    );
}

#[test]
fn workspace_alias_plain_self_move_is_refused() {
    let (mut command, temp) = common::xv_isolated_local();
    assert!(command
        .args(["set", "source", "--value", "must-survive"])
        .output()
        .unwrap()
        .status
        .success());
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["cx", "add", "default", "--backend", "local", "--as", "work"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args([
            "move", "source", "--from", "work", "--to", "default", "--force",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = common::xv_existing_isolated_local(temp.path(), temp.path())
        .args(["get", "source", "--raw"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "must-survive"
    );
}
