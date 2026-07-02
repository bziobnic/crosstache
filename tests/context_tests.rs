mod common;

use common::{parse_json_envelope, stderr_str, stdout_str, write_xv_toml, xv_isolated};

/// Fake Azure creds that satisfy config validation without hitting any real endpoint.
/// Commands that only touch the filesystem (context init, env list) exit before
/// any network call, so these values never reach Azure.
const FAKE_SUB: &str = "00000000-0000-0000-0000-000000000001";
const FAKE_TENANT: &str = "00000000-0000-0000-0000-000000000002";

/// Write a minimal global `xv.conf` with `backend = "local"` for isolated tests.
fn write_local_global_config(temp: &std::path::Path) {
    let xv_dir = temp.join(".config/xv");
    std::fs::create_dir_all(&xv_dir).expect("config dir");
    let store = temp.join("local-store");
    let key = temp.join("local-key.txt");
    std::fs::create_dir_all(&store).expect("store dir");
    let content = format!(
        r#"backend = "local"
debug = false
subscription_id = "{FAKE_SUB}"
tenant_id = "{FAKE_TENANT}"
default_vault = "default"
default_resource_group = ""
default_location = ""
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0

[local]
store_path = "{store}"
key_file = "{key}"
default_vault = "default"
"#,
        store = store.display(),
        key = key.display(),
    );
    std::fs::write(xv_dir.join("xv.conf"), content).expect("write xv.conf");
}

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
    assert!(
        !content.contains("backend ="),
        "omit profile backend when --backend not passed: {content}"
    );
}

#[test]
fn context_init_non_interactive_inherits_global_backend_without_writing_profile_backend() {
    let (mut cmd, temp) = xv_isolated();
    write_local_global_config(temp.path());
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "context",
            "init",
            "--non-interactive",
            "--vault",
            "prod-prefix",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(
        !content.contains("backend ="),
        "profile should inherit global backend: {content}"
    );
    assert!(
        !content.contains("resource_group ="),
        "local backend should not require resource_group: {content}"
    );
}

#[test]
#[cfg(feature = "aws")]
fn context_init_non_interactive_aws_does_not_require_resource_group() {
    let (mut cmd, temp) = xv_isolated();
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "context",
            "init",
            "--non-interactive",
            "--backend",
            "aws",
            "--vault",
            "prod-prefix",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(content.contains("backend = \"aws\""), ".xv.toml: {content}");
    assert!(!content.contains("resource_group ="), ".xv.toml: {content}");
}

#[test]
fn context_init_non_interactive_azure_requires_resource_group() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "context",
            "init",
            "--non-interactive",
            "--backend",
            "azure",
            "--vault",
            "myvault",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_str(&out));
    assert!(
        stderr_str(&out).contains("--resource-group"),
        "stderr: {}",
        stderr_str(&out)
    );
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
fn context_envs_alias_is_removed() {
    // The `context envs` alias was removed outright; clap now rejects it as
    // an unrecognized subcommand. Equivalent listing coverage lives in
    // `env_list_shows_xv_toml_project_envs` / `_table_format` below.
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "vdev"), ("prod", "vprod")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["context", "envs"])
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "context envs unexpectedly succeeded");
}

#[test]
fn config_show_resolved_notes_env_fallback_layers() {
    let (mut cmd, temp) = xv_isolated();
    write_local_global_config(temp.path());
    std::fs::write(
        temp.path().join(".xv.toml"),
        r#"default_env = "dev"

[env.dev]
"#,
    )
    .expect("write .xv.toml");

    let out = cmd
        .args(["--format", "table", "config", "show", "--resolved"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(
        stdout.contains("resource_group  : --resource-group > .xv.toml profile.resource_group > context > global default_resource_group"),
        "resolved config should document the context fallback for resource_group: {stdout}"
    );
    assert!(
        stdout.contains("active env has no backend"),
        "resolved config should explain inherited backend: {stdout}"
    );
    assert!(
        stdout.contains("active env has no vault") && stdout.contains("global config"),
        "resolved config should explain vault fallback to global config: {stdout}"
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
        .args(["env", "list"])
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

// ── xv env list ──────────────────────────────────────────────────────────────

#[test]
fn env_list_shows_xv_toml_project_envs() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(
        temp.path(),
        "dev",
        &[("dev", "vault-dev"), ("prod", "vault-prod")],
    );
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "list", "--format", "json"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    let rows: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be JSON");
    let rows = rows.as_array().expect("rows must be a JSON array");
    let find = |name: &str| {
        rows.iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("expected env '{name}' in rows: {rows:?}"))
    };
    let dev = find("dev");
    assert_eq!(dev["vault"], "vault-dev");
    // Active env (default_env=dev) must be starred.
    assert_eq!(dev["active"], "*", "dev should be the active env: {dev:?}");
    let prod = find("prod");
    assert_eq!(prod["vault"], "vault-prod");
    assert_eq!(
        prod["active"], "",
        "prod should not be marked active: {prod:?}"
    );
}

#[test]
fn env_list_shows_xv_toml_project_envs_table_format() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(
        temp.path(),
        "dev",
        &[("dev", "vault-dev"), ("prod", "vault-prod")],
    );
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "list", "--format", "table"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("Name"), "expected table header: {stdout}");
    let dev_row = stdout
        .lines()
        .find(|line| line.contains("│ dev") && !line.contains("prod"))
        .unwrap_or_else(|| panic!("expected a 'dev' row in table output: {stdout}"));
    assert!(
        dev_row.contains('*'),
        "active env row should contain the '*' marker: {dev_row}"
    );
}

#[test]
fn env_list_without_xv_toml_prints_info() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "list"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let combined = format!("{}{}", stdout_str(&out), stderr_str(&out));
    assert!(
        combined.contains(".xv.toml"),
        "must mention .xv.toml: {combined}"
    );
    assert!(
        combined.contains("xv context init"),
        "must hint at context init: {combined}"
    );
}

// ── xv env use ───────────────────────────────────────────────────────────────

#[test]
fn env_use_rewrites_default_env() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(
        temp.path(),
        "dev",
        &[("dev", "vault-dev"), ("prod", "vault-prod")],
    );
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "use", "prod"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(
        content.contains("default_env = \"prod\""),
        "expected default_env=prod in .xv.toml: {content}"
    );
}

#[test]
fn env_use_errors_without_xv_toml() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "use", "dev"])
        .output()
        .expect("spawn");
    assert_ne!(out.status.code(), Some(0));
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains(".xv.toml"),
        "must mention .xv.toml: {stderr}"
    );
    assert!(
        stderr.contains("xv context init"),
        "must hint at context init: {stderr}"
    );
}

#[test]
fn env_use_errors_when_env_not_in_config() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "vault-dev")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "use", "nonexistent"])
        .output()
        .expect("spawn");
    assert_ne!(out.status.code(), Some(0));
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains("nonexistent"),
        "must name the missing env: {stderr}"
    );
    assert!(
        stderr.contains("Available"),
        "must list available envs: {stderr}"
    );
}

// ── xv env create ────────────────────────────────────────────────────────────

#[test]
fn env_create_adds_block_to_xv_toml() {
    let (mut cmd, temp) = xv_isolated();
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "env",
            "create",
            "stage",
            "--vault",
            "stage-vault",
            "--resource-group",
            "rg-stage",
            "--backend",
            "azure",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(content.contains("[env.stage]"), ".xv.toml: {content}");
    assert!(
        content.contains("vault = \"stage-vault\""),
        ".xv.toml: {content}"
    );
    assert!(
        content.contains("resource_group = \"rg-stage\""),
        ".xv.toml: {content}"
    );
}

#[test]
fn env_create_conflict_errors_without_force() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("stage", "existing-vault")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "env",
            "create",
            "stage",
            "--vault",
            "other",
            "--resource-group",
            "other",
        ])
        .output()
        .expect("spawn");
    assert_ne!(out.status.code(), Some(0));
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains("already exists"),
        "must mention conflict: {stderr}"
    );
    assert!(stderr.contains("--force"), "must hint --force: {stderr}");
}

#[test]
fn env_create_with_force_overwrites() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("stage", "old-vault")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "env",
            "create",
            "stage",
            "--vault",
            "new-vault",
            "--resource-group",
            "rg",
            "--force",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(content.contains("new-vault"), ".xv.toml: {content}");
}

#[test]
fn env_create_with_default_sets_default_env() {
    let (mut cmd, temp) = xv_isolated();
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args([
            "env",
            "create",
            "prod",
            "--vault",
            "prod-vault",
            "--resource-group",
            "rg-prod",
            "--default",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(
        content.contains("default_env = \"prod\""),
        ".xv.toml: {content}"
    );
}

// ── xv env delete ────────────────────────────────────────────────────────────

#[test]
fn env_delete_removes_block() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(
        temp.path(),
        "dev",
        &[("dev", "vault-dev"), ("prod", "vault-prod")],
    );
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "delete", "prod", "-f"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(
        !content.contains("[env.prod]"),
        "prod block must be gone: {content}"
    );
    assert!(
        content.contains("[env.dev]") || content.contains("vault-dev"),
        "dev must remain: {content}"
    );
}

#[test]
fn env_delete_clears_default_env_when_deleted() {
    let (mut cmd, temp) = xv_isolated();
    // default_env points at "dev" — deleting dev must also clear default_env.
    write_xv_toml(temp.path(), "dev", &[("dev", "vault-dev")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "delete", "dev", "-f"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(
        !content.contains("default_env"),
        "default_env must be cleared: {content}"
    );
}

// ── xv env show ──────────────────────────────────────────────────────────────

#[test]
fn env_show_prints_active_env() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "vault-dev")]);
    let out = cmd
        .env("AZURE_SUBSCRIPTION_ID", FAKE_SUB)
        .env("AZURE_TENANT_ID", FAKE_TENANT)
        .args(["env", "show"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(
        stdout.contains("Active env: dev"),
        "expected 'Active env: dev' in output: {stdout}"
    );
    assert!(
        stdout.contains("vault-dev"),
        "expected vault in output: {stdout}"
    );
}
