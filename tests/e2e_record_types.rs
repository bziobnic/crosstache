//! End-to-end CLI tests for record types (`xv type`, `xv set --type`,
//! `xv get --field`/`--record`). Hermetic: every test uses the isolated
//! local-backend harness from `tests/common/mod.rs` — no Azure credentials
//! or network access required.
//!
//! Run with:
//!   cargo test --test e2e_record_types

mod common;

// ---------------------------------------------------------------------------
// Task 4: `xv type list` / `xv type show`
// ---------------------------------------------------------------------------

#[test]
fn type_list_shows_builtins() {
    let (mut cmd, _temp) = common::xv_isolated_local();
    let out = cmd
        .args(["type", "list", "--format", "table"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("login"), "stdout: {stdout}");
    assert!(stdout.contains("api-key"), "stdout: {stdout}");
    assert!(stdout.contains("database"), "stdout: {stdout}");
    assert!(stdout.contains("built-in"), "stdout: {stdout}");
}

#[test]
fn type_list_ls_alias_works() {
    let (mut cmd, _temp) = common::xv_isolated_local();
    let out = cmd
        .args(["type", "ls", "--format", "table"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("login"), "stdout: {stdout}");
}

#[test]
fn type_show_login_fields() {
    let (mut cmd, _temp) = common::xv_isolated_local();
    let out = cmd.args(["type", "show", "login"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("username"), "stdout: {stdout}");
    assert!(stdout.contains("url"), "stdout: {stdout}");
    assert!(stdout.contains("password"), "stdout: {stdout}");
}

#[test]
fn type_list_includes_project_custom_type() {
    let xv_toml = r#"
default_env = "dev"

[env.dev]
vault = "default"
resource_group = "test-rg"

[types.smtp]
fields = [
  { name = "host" },
  { name = "port" },
  { name = "username", required = true },
  { name = "password", kind = "secret", primary = true },
]
"#;
    let (mut cmd, _temp) = common::xv_isolated_local_with_profile(xv_toml);
    let out = cmd
        .args(["type", "list", "--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let types = parsed.as_array().expect("array");
    let smtp = types
        .iter()
        .find(|t| t["name"] == "smtp")
        .expect("smtp type present");
    assert_eq!(smtp["source"], "project");
}

// ---------------------------------------------------------------------------
// Task 6: `xv set --type` (non-interactive)
// ---------------------------------------------------------------------------

/// Reads the on-disk `.meta.json` for `name` in the `default` vault of the
/// isolated local-backend store created by `common::xv_isolated_local()`.
/// There is no CLI surface yet (Phase A stops at Task 7) that exposes a
/// record's tags directly, so these tests read the store layout the local
/// backend itself defines (`<store>/vaults/<vault>/secrets/<name>.meta.json`
/// — un-opaque by default) to assert on tags. This is a deliberate,
/// documented deviation for Task 6 only; Task 10 (`ls` JSON field lifting,
/// out of Phase A scope) will give these assertions a CLI path.
fn read_local_meta(temp_dir: &std::path::Path, vault: &str, name: &str) -> serde_json::Value {
    let path = temp_dir
        .join("store")
        .join("vaults")
        .join(vault)
        .join("secrets")
        .join(format!("{name}.meta.json"));
    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&content).expect("valid meta json")
}

#[test]
fn set_typed_record_stores_envelope_and_tags() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--field",
            "url=https://example.com",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let meta = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(meta["content_type"], "application/vnd.xv.record");
    assert_eq!(meta["tags"]["xv-type"], "login");
    assert_eq!(meta["tags"]["f.username"], "bob");
    assert_eq!(meta["tags"]["f.url"], "https://example.com");
    // The primary field never appears as a tag.
    assert!(meta["tags"].get("f.password").is_none());

    // The stored value is the JSON envelope, not the bare primary — proven
    // via --raw (Task 7 hasn't landed record-aware `get` yet, so --raw
    // returns the stored value verbatim). Reuses the same store/env as the
    // `set` above (can't reuse `cmd`/xv_isolated_local() itself: each
    // Command is single-use and a fresh xv_isolated_local() call would
    // create a brand-new, empty store).
    let out2 = common::xv()
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(temp.path())
        .args(["get", "cred", "--raw"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stdout2 = common::stdout_str(&out2);
    let envelope: serde_json::Value = serde_json::from_str(&stdout2).expect("valid json envelope");
    assert_eq!(envelope["password"], "hunter2");
    assert_eq!(envelope.as_object().unwrap().len(), 1);
}

#[test]
fn set_typed_missing_required_field_fails_before_write() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "cred", "--type", "login", "--value", "hunter2"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("username"), "stderr: {stderr}");

    // No secret was created.
    let meta_path = temp
        .path()
        .join("store")
        .join("vaults")
        .join("default")
        .join("secrets")
        .join("cred.meta.json");
    assert!(!meta_path.exists());
}

#[test]
fn set_unknown_type_errors_listing_types() {
    let (mut cmd, _temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "cred", "--type", "nosuch", "--value", "x"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("login"), "stderr: {stderr}");
}

#[test]
fn set_adhoc_field_allowed() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--field",
            "custom-note=hello",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let meta = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(meta["tags"]["f.custom-note"], "hello");
}

#[test]
fn set_field_secret_goes_to_envelope() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--field-secret",
            "totp-seed=ABCDEF",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let meta = read_local_meta(temp.path(), "default", "cred");
    assert!(
        meta["tags"].get("f.totp-seed").is_none(),
        "secret field must not appear as a tag: {meta}"
    );
}

#[test]
fn type_show_unknown_errors() {
    let (mut cmd, _temp) = common::xv_isolated_local();
    let out = cmd.args(["type", "show", "nosuch"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(
        stderr.contains("login"),
        "stderr should list known types: {stderr}"
    );
    assert!(
        stderr.contains("api-key"),
        "stderr should list known types: {stderr}"
    );
    assert!(
        stderr.contains("database"),
        "stderr should list known types: {stderr}"
    );
}
