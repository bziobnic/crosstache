//! End-to-end CLI tests for `xv totp` against hermetic encrypted local stores.

mod common;

use std::path::Path;
use std::process::{Command, Output};

const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

fn xv_again(temp: &Path) -> Command {
    common::xv_existing_isolated_local(temp, temp)
}

fn set_record(temp: &Path, name: &str, field: &str, material: &str) -> Output {
    xv_again(temp)
        .args([
            "set",
            name,
            "--type",
            "login",
            "--field",
            "username=alice",
            "--field-secret",
            &format!("{field}={material}"),
            "--value",
            "password",
        ])
        .output()
        .unwrap()
}

fn assert_numeric_code(output: &Output, digits: usize) {
    assert!(
        output.status.success(),
        "stderr: {}",
        common::stderr_str(output)
    );
    let stdout = common::stdout_str(output);
    assert_eq!(stdout.len(), digits, "stdout: {stdout:?}");
    assert!(
        stdout.bytes().all(|byte| byte.is_ascii_digit()),
        "stdout: {stdout:?}"
    );
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        common::stderr_str(output)
    );
}

#[test]
fn canonical_bare_seed_generates_raw_code_only() {
    let (_cmd, temp) = common::xv_isolated_local();
    let set = set_record(temp.path(), "github", "one-time-code", SEED);
    assert!(set.status.success(), "stderr: {}", common::stderr_str(&set));
    let output = xv_again(temp.path())
        .args(["totp", "github", "--raw"])
        .output()
        .unwrap();
    assert_numeric_code(&output, 6);
}

#[test]
fn keeper_uri_honors_eight_digits_and_sha256() {
    let (_cmd, temp) = common::xv_isolated_local();
    let uri = format!(
        "otpauth://totp/GitHub:alice?secret={SEED}&issuer=GitHub&algorithm=SHA256&digits=8&period=60"
    );
    let set = set_record(temp.path(), "github", "one-time-code", &uri);
    assert!(set.status.success(), "stderr: {}", common::stderr_str(&set));
    let output = xv_again(temp.path())
        .args(["totp", "github", "-r"])
        .output()
        .unwrap();
    assert_numeric_code(&output, 8);
}

#[test]
fn explicit_field_override_uses_only_the_named_field() {
    let (_cmd, temp) = common::xv_isolated_local();
    let set = xv_again(temp.path())
        .args([
            "set",
            "github",
            "--type",
            "login",
            "--field",
            "username=alice",
            "--field-secret",
            "one-time-code=invalid",
            "--field-secret",
            &format!("authenticator-seed={SEED}"),
            "--value",
            "password",
        ])
        .output()
        .unwrap();
    assert!(set.status.success(), "stderr: {}", common::stderr_str(&set));
    let output = xv_again(temp.path())
        .args(["totp", "github", "--field", "authenticator-seed", "--raw"])
        .output()
        .unwrap();
    assert_numeric_code(&output, 6);
}

#[test]
fn untyped_secret_is_rejected_without_echoing_value() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let set = cmd
        .args(["set", "raw-seed", "--value", SEED])
        .output()
        .unwrap();
    assert!(set.status.success());
    let output = xv_again(temp.path())
        .args(["totp", "raw-seed", "--raw"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = common::stderr_str(&output);
    assert!(stderr.contains("typed record"), "{stderr}");
    assert!(!stderr.contains(SEED), "{stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn metadata_seed_is_rejected_with_field_secret_hint() {
    let (_cmd, temp) = common::xv_isolated_local();
    let set = xv_again(temp.path())
        .args([
            "set",
            "metadata-seed",
            "--type",
            "login",
            "--field",
            "username=alice",
            "--field",
            &format!("one-time-code={SEED}"),
            "--value",
            "password",
        ])
        .output()
        .unwrap();
    assert!(set.status.success());
    let output = xv_again(temp.path())
        .args(["totp", "metadata-seed", "--raw"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = common::stderr_str(&output);
    assert!(stderr.contains("--field-secret"), "{stderr}");
    assert!(!stderr.contains(SEED), "{stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn missing_and_malformed_material_fail_without_disclosure() {
    let (_cmd, temp) = common::xv_isolated_local();
    let missing = set_record(temp.path(), "missing", "backup-seed", SEED);
    assert!(missing.status.success());
    let missing_output = xv_again(temp.path())
        .args(["totp", "missing", "--raw"])
        .output()
        .unwrap();
    assert_eq!(missing_output.status.code(), Some(3));
    assert!(common::stderr_str(&missing_output).contains("one-time-code"));

    let bad_uri = "otpauth://hotp/Test?secret=SENTINEL-SEED&counter=1";
    let malformed = set_record(temp.path(), "malformed", "one-time-code", bad_uri);
    assert!(malformed.status.success());
    let malformed_output = xv_again(temp.path())
        .args(["totp", "malformed", "--raw"])
        .output()
        .unwrap();
    assert_eq!(malformed_output.status.code(), Some(3));
    let stderr = common::stderr_str(&malformed_output);
    assert!(!stderr.contains("SENTINEL-SEED"), "{stderr}");
    assert!(!stderr.contains(bad_uri), "{stderr}");
}

#[test]
fn workspace_alias_qualified_totp_uses_read_resolution() {
    let profile = r#"
default_env = "dev"

[env.dev]
vaults = [
  { vault = "default", backend = "local", alias = "work", default = true },
]
"#;
    let (_cmd, temp) = common::xv_isolated_local_with_profile(profile);
    let cwd = temp.path().join("project");
    let set = common::xv_existing_isolated_local(temp.path(), &cwd)
        .args([
            "set",
            "work:github",
            "--type",
            "login",
            "--field",
            "username=alice",
            "--field-secret",
            &format!("one-time-code={SEED}"),
            "--value",
            "password",
        ])
        .output()
        .unwrap();
    assert!(set.status.success(), "stderr: {}", common::stderr_str(&set));
    let output = common::xv_existing_isolated_local(temp.path(), &cwd)
        .args(["totp", "work:github", "--raw"])
        .output()
        .unwrap();
    assert_numeric_code(&output, 6);
}

#[test]
fn generating_a_code_does_not_mutate_or_version_the_record() {
    let (_cmd, temp) = common::xv_isolated_local();
    let set = set_record(temp.path(), "github", "one-time-code", SEED);
    assert!(set.status.success());
    let secrets = temp.path().join("store/vaults/default/secrets");
    let meta_path = secrets.join("github.meta.json");
    let value_path = secrets.join("github.age");
    let meta_before = std::fs::read(&meta_path).unwrap();
    let value_before = std::fs::read(&value_path).unwrap();

    let output = xv_again(temp.path())
        .args(["totp", "github", "--raw"])
        .output()
        .unwrap();
    assert_numeric_code(&output, 6);

    assert_eq!(std::fs::read(meta_path).unwrap(), meta_before);
    assert_eq!(std::fs::read(value_path).unwrap(), value_before);
}
