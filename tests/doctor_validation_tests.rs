mod common;

fn write_complete_local_config(path: &std::path::Path) -> Vec<u8> {
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
"#
    .to_vec();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, &original).unwrap();
    original
}

#[test]
fn doctor_rejects_unknown_environment_backend_without_disclosing_its_value() {
    const BACKEND_SENTINEL: &str = "doctor-private-environment-backend";
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    let original = write_complete_local_config(&path);

    let out = cmd
        .env("XV_BACKEND", BACKEND_SENTINEL)
        .arg("doctor")
        .output()
        .expect("spawn");

    assert_eq!(out.status.code(), Some(3));
    let output = format!("{}\n{}", common::stdout_str(&out), common::stderr_str(&out));
    assert!(output.to_ascii_lowercase().contains("backend"));
    assert!(!output.contains(BACKEND_SENTINEL));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn doctor_reports_sanitized_toml_reason_and_location_without_source_values() {
    const SOURCE_SENTINEL: &str = "doctor-private-malformed-array";
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    let original = format!("backend = [\"{SOURCE_SENTINEL}\"\n");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &original).unwrap();

    let out = cmd.arg("doctor").output().expect("spawn");

    assert_eq!(out.status.code(), Some(3));
    let output = format!("{}\n{}", common::stdout_str(&out), common::stderr_str(&out));
    assert!(output.contains("array syntax"), "{output}");
    assert!(output.contains("line "), "{output}");
    assert!(output.contains("column "), "{output}");
    assert!(!output.contains(SOURCE_SENTINEL));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn doctor_uses_environment_for_validation_without_persisting_it() {
    const SUBSCRIPTION_SENTINEL: &str = "doctor-private-production-subscription";
    const TENANT_SENTINEL: &str = "doctor-private-production-tenant";
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    let original = b"# sparse Azure config\nbackend = \"azure\"\n";
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, original).unwrap();

    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", SUBSCRIPTION_SENTINEL)
        .env("AZURE_TENANT_ID", TENANT_SENTINEL)
        .arg("doctor")
        .output()
        .expect("spawn");

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stdout = common::stdout_str(&out);
    let fixed = stdout.find("fixed:").expect("fixed check");
    let backup_label = stdout.find("Backup:").expect("backup label");
    assert!(fixed < backup_label, "stdout: {stdout}");
    assert!(!stdout.contains(SUBSCRIPTION_SENTINEL));
    assert!(!stdout.contains(TENANT_SENTINEL));

    let persisted = std::fs::read_to_string(&path).unwrap();
    assert!(!persisted.contains(SUBSCRIPTION_SENTINEL));
    assert!(!persisted.contains(TENANT_SENTINEL));
    let config: toml::Value = toml::from_str(&persisted).unwrap();
    assert_eq!(config["subscription_id"].as_str(), Some(""));
    assert_eq!(config["tenant_id"].as_str(), Some(""));

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
}

#[test]
fn doctor_rejects_invalid_enum_without_disclosing_or_replacing_it() {
    const ENUM_SENTINEL: &str = "doctor-private-invalid-credential-enum";
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    let original = format!(
        r#"backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = "Vaults"
default_location = "eastus"
tenant_id = ""
output_json = false
no_color = true
azure_credential_priority = "{ENUM_SENTINEL}"

[local]
default_vault = "default"
"#
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &original).unwrap();

    let out = cmd.arg("doctor").output().expect("spawn");

    assert_eq!(out.status.code(), Some(3));
    let output = format!("{}\n{}", common::stdout_str(&out), common::stderr_str(&out));
    assert!(output.contains("azure_credential_priority"), "{output}");
    assert!(output.contains("valid configuration value"), "{output}");
    assert!(!output.contains(ENUM_SENTINEL));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn doctor_reports_missing_aws_region_in_an_environment_cleared_child() {
    let (mut cmd, temp) = common::xv_isolated();
    let path = temp.path().join(".config/xv/xv.conf");
    let original = br#"backend = "aws"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = "Vaults"
default_location = "eastus"
tenant_id = ""
output_json = false
no_color = true

[aws]
profile = "doctor-test"
"#;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, original).unwrap();

    let out = cmd.arg("doctor").output().expect("spawn");

    assert_eq!(out.status.code(), Some(3));
    let output = format!("{}\n{}", common::stdout_str(&out), common::stderr_str(&out));
    assert!(output.contains("AWS region is required"), "{output}");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
}
