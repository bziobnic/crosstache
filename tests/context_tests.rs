mod common;

use common::{parse_json_envelope, stderr_str, stdout_str, write_xv_toml, xv_isolated};

/// Fake Azure creds that satisfy config validation without hitting any real endpoint.
/// Commands that only touch the filesystem (context init, context envs) exit before
/// any network call, so these values never reach Azure.
const FAKE_SUB: &str = "00000000-0000-0000-0000-000000000001";
const FAKE_TENANT: &str = "00000000-0000-0000-0000-000000000002";

#[test]
fn context_init_non_interactive_writes_xv_toml() {
    let (mut cmd, temp) = xv_isolated();
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "context",
            "init",
            "--non-interactive",
            "--vault",
            "myvault",
            "--resource-group",
            "myrg",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let path = temp.path().join(".xv.toml");
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("default_env = \"dev\""));
    assert!(content.contains("vault = \"myvault\""));
    assert!(content.contains("resource_group = \"myrg\""));
}

#[test]
fn context_init_refuses_existing_without_force() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "context",
            "init",
            "--non-interactive",
            "--vault",
            "v2",
            "--resource-group",
            "rg",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(3), "should be config-invalid");
    let stderr = stderr_str(&out);
    assert!(stderr.contains("already exists"), "stderr: {stderr}");
}

#[test]
fn context_init_force_overwrites() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "context",
            "init",
            "--non-interactive",
            "--force",
            "--vault",
            "v2",
            "--resource-group",
            "rg",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(content.contains("vault = \"v2\""));
}

#[test]
fn context_init_non_interactive_requires_vault() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "context",
            "init",
            "--non-interactive",
            "--resource-group",
            "rg",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2), "missing required --vault");
}

#[test]
fn context_envs_lists_envs() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "vdev"), ("prod", "vprod")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["context", "envs"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("dev"));
    assert!(stdout.contains("prod"));
    assert!(stdout.contains("vdev"));
    assert!(stdout.contains("vprod"));
    // Default env starred:
    assert!(
        stdout.contains("* dev"),
        "active env should be starred: {stdout}"
    );
}

#[test]
fn context_envs_no_xv_toml_warns() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["context", "envs"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stderr = stderr_str(&out);
    let stdout = stdout_str(&out);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains(".xv.toml") || combined.contains("no .xv.toml"),
        "must mention missing config: {combined}"
    );
}

#[test]
fn unknown_env_exits_3_with_env_not_defined_code() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1"), ("prod", "v2")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["--env", "staging", "list", "--format", "json"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let body = parse_json_envelope(&out.stdout);
    assert_eq!(body["error"]["code"], "xv-env-not-defined");
    assert_eq!(body["error"]["exit_code"], 3);
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("staging"));
    assert!(msg.contains("dev"));
    assert!(msg.contains("prod"));
}

#[test]
fn xv_env_overrides_default_env() {
    // We can't fully exercise the priority chain without a vault, but
    // we CAN confirm XV_ENV with a missing env produces the same
    // xv-env-not-defined error (mentioning the XV_ENV value, not default_env).
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .env("XV_ENV", "staging")
        .args(["list", "--format", "json"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let body = parse_json_envelope(&out.stdout);
    assert_eq!(body["error"]["code"], "xv-env-not-defined");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("staging"),
        "XV_ENV value should appear in message: {msg}"
    );
}

#[test]
fn xv_no_parent_config_disables_walkup() {
    // Place .xv.toml at parent dir; cwd is a child without one.
    // With XV_NO_PARENT_CONFIG=1 (set by isolate()), walk-up is disabled,
    // so the .xv.toml in parent should NOT be discovered.
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let child = temp.path().join("subproject");
    std::fs::create_dir_all(&child).unwrap();
    cmd.current_dir(&child); // override the harness's current_dir

    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["context", "envs"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = stdout_str(&out);
    let stderr = stderr_str(&out);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("no .xv.toml") || combined.contains(".xv.toml"),
        "should report no .xv.toml found because walk-up is disabled: {combined}"
    );
}

#[test]
fn xv_toml_in_ancestor_emits_cross_boundary_notice() {
    // Without XV_NO_PARENT_CONFIG, walk-up should find the ancestor
    // .xv.toml and emit a one-time stderr notice when vault resolution
    // is triggered (the notice fires inside resolve_vault_name).
    let temp = tempfile::tempdir().expect("tempdir");
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let child = temp.path().join("sub");
    std::fs::create_dir_all(&child).unwrap();
    let mut cmd = common::xv();
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join(".config"))
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        // explicitly DO NOT set XV_NO_PARENT_CONFIG
        .current_dir(&child);

    // list --format json triggers resolve_vault_name, which walks up to
    // find the ancestor .xv.toml and emits the cross-boundary notice on
    // stderr before failing with an auth or network error.
    let out = cmd
        .args(["list", "--format", "json"])
        .output()
        .expect("spawn");
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains("using config from") && stderr.contains(".xv.toml"),
        "cross-boundary notice expected on stderr: {stderr}"
    );
}

// ── P0.1 tests ───────────────────────────────────────────────────────────────

#[test]
fn context_use_rejects_xv_toml_env_name() {
    // `xv context use dev` when .xv.toml has [env.dev] must fail with a
    // targeted message instead of silently creating a vault named "dev".
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "real-dev-vault")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["context", "use", "dev"])
        .output()
        .expect("spawn");
    // Must be an error.
    assert_ne!(out.status.code(), Some(0), "should have failed");
    let stderr = stderr_str(&out);
    // Must mention that "dev" is an env profile, not a vault.
    assert!(
        stderr.contains("env profile") || stderr.contains("--env"),
        "expected targeted env-profile hint in stderr: {stderr}"
    );
    // Must not say "Switched to vault 'dev'".
    assert!(
        !stderr.contains("Switched to vault"),
        "must not have silently created vault context: {stderr}"
    );
}

#[test]
fn env_list_shows_xv_toml_project_envs() {
    // `xv env list` with a .xv.toml present should surface project envs.
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(
        temp.path(),
        "dev",
        &[("dev", "vault-dev"), ("prod", "vault-prod")],
    );
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "list"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    // Should show the .xv.toml profiles.
    assert!(stdout.contains("dev"), "expected 'dev' in output: {stdout}");
    assert!(
        stdout.contains("prod"),
        "expected 'prod' in output: {stdout}"
    );
    // Should indicate these are activated via --env / XV_ENV.
    assert!(
        stdout.contains("--env") || stdout.contains("XV_ENV"),
        "expected activation hint in output: {stdout}"
    );
}

#[test]
fn env_use_targeted_error_for_xv_toml_env() {
    // `xv env use dev` when dev exists only in .xv.toml should produce a
    // targeted hint rather than a bare "not found" error.
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "real-dev-vault")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "use", "dev"])
        .output()
        .expect("spawn");
    assert_ne!(out.status.code(), Some(0), "should have failed");
    let stderr = stderr_str(&out);
    // Must mention .xv.toml or --env as the correct path.
    assert!(
        stderr.contains(".xv.toml") || stderr.contains("--env") || stderr.contains("XV_ENV"),
        "expected targeted hint in stderr: {stderr}"
    );
    // Must not just say "not found" with no guidance.
    let bare_not_found = stderr.contains("not found")
        && !stderr.contains("--env")
        && !stderr.contains(".xv.toml")
        && !stderr.contains("XV_ENV");
    assert!(!bare_not_found, "bare not-found without hint: {stderr}");
}
