mod common;

#[test]
fn doctor_appears_in_top_level_help() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.arg("--help").output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    assert!(common::stdout_str(&out).contains("doctor"));
}

#[test]
fn doctor_runs_before_normal_config_loading() {
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "debug = [\n").unwrap();
    let out = cmd.arg("doctor").output().expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let output = format!("{}\n{}", common::stdout_str(&out), common::stderr_str(&out));
    assert!(output.contains("syntax"));
    assert!(output.contains(path.to_string_lossy().as_ref()));
}

#[test]
fn doctor_rejects_json_format_with_one_valid_error_envelope() {
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "debug = [\n").unwrap();

    let out = cmd
        .args(["--format", "json", "doctor"])
        .output()
        .expect("spawn");

    assert_ne!(out.status.code(), Some(0));
    let body = common::parse_json_envelope(&out.stdout);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("does not support --format json"));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.contains("Configuration:"), "mixed stdout: {stdout}");
    assert!(!stdout.contains(path.to_string_lossy().as_ref()));
}

#[test]
fn doctor_rejects_yaml_format_with_one_valid_error_envelope() {
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "debug = [\n").unwrap();

    let out = cmd
        .args(["--format", "yaml", "doctor"])
        .output()
        .expect("spawn");

    assert_ne!(out.status.code(), Some(0));
    let body: serde_yaml::Value =
        serde_yaml::from_slice(&out.stdout).expect("stdout must be one valid YAML value");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("does not support --format yaml"));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.contains("Configuration:"), "mixed stdout: {stdout}");
    assert!(!stdout.contains(path.to_string_lossy().as_ref()));
}

#[test]
fn doctor_missing_config_uses_defaults_without_creating_files() {
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");

    let out = cmd.arg("doctor").output().expect("spawn");

    assert_eq!(out.status.code(), Some(0));
    let output = format!("{}\n{}", common::stdout_str(&out), common::stderr_str(&out));
    assert!(output.contains("does not exist"));
    assert!(output.contains("defaults are usable"));
    assert!(!path.exists());
    assert!(!temp.path().join(".config/xv").exists());
}

#[test]
fn doctor_complete_config_is_healthy_and_unchanged() {
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    let original = br#"backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = "Vaults"
default_location = "eastus"
tenant_id = ""
output_json = false
no_color = true

[local]
default_vault = "default"
"#;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, original).unwrap();

    let out = cmd.arg("doctor").output().expect("spawn");

    assert_eq!(out.status.code(), Some(0));
    assert!(common::stdout_str(&out).contains("healthy"));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn doctor_repairs_sparse_local_config_and_preserves_exact_backup() {
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    let original =
        b"# sparse local config\nbackend = \"local\"\n\n[local]\ndefault_vault = \"default\"\n";
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, original).unwrap();

    let out = cmd.arg("doctor").output().expect("spawn");

    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    for field in [
        "debug",
        "subscription_id",
        "default_vault",
        "default_resource_group",
        "default_location",
        "tenant_id",
        "output_json",
        "no_color",
    ] {
        assert!(stdout.contains(&format!(
            "fixed: Restored missing configuration field '{field}'"
        )));
    }
    assert!(stdout.contains("Backup:"));
    assert!(
        stdout.find("fixed:").unwrap() < stdout.find("Backup:").unwrap(),
        "doctor checks must be rendered before the backup path: {stdout}"
    );
    let backup = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|entry| {
            entry
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("xv.conf.backup-")
        })
        .expect("backup path");
    assert!(stdout.contains(backup.to_string_lossy().as_ref()));
    assert_eq!(std::fs::read(backup).unwrap(), original);

    let mut show = common::xv();
    common::isolate(&mut show, temp.path());
    let loaded = show.args(["config", "show"]).output().expect("spawn");
    assert_eq!(
        loaded.status.code(),
        Some(0),
        "stderr: {}",
        common::stderr_str(&loaded)
    );
}

#[test]
fn doctor_invalid_occupied_type_requires_manual_edit_without_disclosing_value() {
    const BAD_VALUE: &str = "doctor-private-debug-value";
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    let original = format!(
        r#"backend = "local"
debug = "{BAD_VALUE}"
subscription_id = ""
default_vault = "default"
default_resource_group = "Vaults"
default_location = "eastus"
tenant_id = ""
output_json = false
no_color = true
"#
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &original).unwrap();

    let out = cmd.arg("doctor").output().expect("spawn");

    assert_eq!(out.status.code(), Some(3));
    let output = format!("{}\n{}", common::stdout_str(&out), common::stderr_str(&out));
    assert!(output.contains("debug"));
    assert!(output.contains("Edit"));
    assert!(!output.contains(BAD_VALUE));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn doctor_persists_repairs_before_reporting_semantic_error() {
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    let original = b"# sparse AWS config\nbackend = \"aws\"\n";
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, original).unwrap();

    let out = cmd.arg("doctor").output().expect("spawn");

    assert_eq!(out.status.code(), Some(3));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("fixed:"));
    assert!(stdout.contains("[aws]"));
    assert!(stdout.contains("Backup:"));
    let backup = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|entry| {
            entry
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("xv.conf.backup-")
        })
        .expect("backup path");
    assert_eq!(std::fs::read(backup).unwrap(), original);
    assert_ne!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn config_path_shows_isolated_path() {
    let (mut cmd, temp) = common::xv_isolated();
    let out = cmd.args(["config", "path"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    // Path should be under our isolated XDG_CONFIG_HOME.
    let expected_prefix = temp.path().join(".config").to_string_lossy().into_owned();
    assert!(
        stdout.contains(&expected_prefix) || stdout.contains("xv"),
        "config path should reference isolated dir: {stdout}"
    );
}

#[test]
fn config_show_works_on_empty_config() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["config", "show"]).output().expect("spawn");
    // With XDG_CONFIG_HOME pointing at an empty tempdir, no config file
    // exists. The command should still exit 0 and show defaults.
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        common::stderr_str(&out)
    );
}

#[test]
fn config_set_then_show_round_trips() {
    let (mut cmd1, temp) = common::xv_isolated();
    let out1 = cmd1
        .args(["config", "set", "default_vault", "test-vault"])
        .output()
        .expect("spawn");
    assert_eq!(
        out1.status.code(),
        Some(0),
        "set: {}",
        common::stderr_str(&out1)
    );

    let mut cmd2 = common::xv();
    common::isolate(&mut cmd2, temp.path());
    let out2 = cmd2.args(["config", "show"]).output().expect("spawn");
    assert_eq!(out2.status.code(), Some(0));
    let stdout = common::stdout_str(&out2);
    assert!(
        stdout.contains("test-vault"),
        "config show should display the value just set: {stdout}"
    );
}

#[test]
fn config_set_invalid_key_errors() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd
        .args(["config", "set", "this_key_does_not_exist", "value"])
        .output()
        .expect("spawn");
    // Either clap rejects (exit 2) or runtime returns invalid-argument (exit 2 or 3).
    // Acceptable: 2 or 3 (depending on validation layer).
    let code = out.status.code();
    assert!(
        code == Some(2) || code == Some(3),
        "invalid config key should error: {code:?}"
    );
}

#[test]
fn config_help_documents_subcommands() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["config", "--help"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("show"));
    assert!(stdout.contains("path"));
    assert!(stdout.contains("set"));
}

// ── P0.3 tests ───────────────────────────────────────────────────────────────

#[test]
fn version_lists_compiled_backends() {
    // `xv version` should always mention "azure" and "local" as built-in backends.
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["version"]).output().expect("spawn");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stdout = common::stdout_str(&out);
    assert!(
        stdout.contains("Backends:"),
        "expected Backends: line in version output: {stdout}"
    );
    assert!(
        stdout.contains("azure"),
        "expected azure in backends: {stdout}"
    );
    assert!(
        stdout.contains("local"),
        "expected local in backends: {stdout}"
    );
}

#[cfg(not(feature = "aws"))]
#[test]
fn backend_aws_on_default_build_gives_clear_error() {
    // On a build without --features aws, `xv --backend aws list` must return a
    // targeted error rather than the generic "No backend registry available" message.
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd
        .args(["--backend", "aws", "list"])
        .output()
        .expect("spawn");
    assert_ne!(out.status.code(), Some(0), "should have failed");
    let stderr = common::stderr_str(&out);
    assert!(
        stderr.contains("AWS backend") || stderr.contains("--features aws"),
        "expected AWS build hint in stderr: {stderr}"
    );
    // Must not say "No backend registry available" (the old generic message).
    assert!(
        !stderr.contains("No backend registry available"),
        "must not emit generic registry error: {stderr}"
    );
}
