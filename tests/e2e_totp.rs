//! End-to-end CLI tests for `xv totp` against hermetic encrypted local stores.

mod common;

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_secs()
}

fn sha256_totp_at(seed: &str, unix_seconds: u64, period: u64, digits: u32) -> String {
    let key = BASE32_NOPAD
        .decode(seed.as_bytes())
        .expect("valid Base32 seed");
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts key");
    mac.update(&(unix_seconds / period).to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let binary = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    format!(
        "{:0width$}",
        binary % 10_u32.pow(digits),
        width = digits as usize
    )
}

fn stable_sixty_second_window() -> u64 {
    loop {
        let now = unix_seconds();
        if 60 - (now % 60) > 10 {
            return now;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn snapshot_tree(root: &Path) -> Vec<(std::path::PathBuf, Option<Vec<u8>>)> {
    fn visit(
        root: &Path,
        relative: &Path,
        entries: &mut Vec<(std::path::PathBuf, Option<Vec<u8>>)>,
    ) {
        let mut children = std::fs::read_dir(root.join(relative))
            .expect("read store tree")
            .map(|entry| entry.expect("read store entry"))
            .collect::<Vec<_>>();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let child_relative = relative.join(child.file_name());
            let path = root.join(&child_relative);
            if child.file_type().expect("read entry type").is_dir() {
                entries.push((child_relative.clone(), None));
                visit(root, &child_relative, entries);
            } else {
                entries.push((
                    child_relative,
                    Some(std::fs::read(path).expect("read store file")),
                ));
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, Path::new(""), &mut entries);
    entries
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
fn keeper_uri_honors_sha256_and_sixty_second_period() {
    let (_cmd, temp) = common::xv_isolated_local();
    let uri = format!(
        "otpauth://totp/GitHub:alice?secret={SEED}&issuer=GitHub&algorithm=SHA256&digits=8&period=60"
    );
    let set = set_record(temp.path(), "github", "one-time-code", &uri);
    assert!(set.status.success(), "stderr: {}", common::stderr_str(&set));
    for _ in 0..2 {
        let before = stable_sixty_second_window();
        let output = xv_again(temp.path())
            .args(["totp", "github", "-r"])
            .output()
            .unwrap();
        let after = unix_seconds();
        if before / 60 == after / 60 {
            assert_numeric_code(&output, 8);
            assert_eq!(
                common::stdout_str(&output),
                sha256_totp_at(SEED, before, 60, 8),
                "URI TOTP must use SHA-256 and its 60-second period"
            );
            return;
        }
    }
    panic!("TOTP invocation crossed a 60-second boundary twice");
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
fn explicit_field_override_does_not_fall_back_to_another_secret_field() {
    let (_cmd, temp) = common::xv_isolated_local();
    let set = set_record(temp.path(), "github", "authenticator-seed", SEED);
    assert!(set.status.success(), "stderr: {}", common::stderr_str(&set));
    let output = xv_again(temp.path())
        .args(["totp", "github", "--field", "one-time-code", "--raw"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = common::stderr_str(&output);
    assert!(stderr.contains("one-time-code"), "{stderr}");
    assert!(stderr.contains("authenticator-seed"), "{stderr}");
    assert!(!stderr.contains(SEED), "{stderr}");
    assert!(output.stdout.is_empty());
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
    let tree_before = snapshot_tree(&secrets);

    let output = xv_again(temp.path())
        .args(["totp", "github", "--raw"])
        .output()
        .unwrap();
    assert_numeric_code(&output, 6);

    assert_eq!(snapshot_tree(&secrets), tree_before);
}
