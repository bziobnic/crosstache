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
