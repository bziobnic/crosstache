//! `xv backend` CLI surface. Uses an isolated config dir so the developer's
//! real ~/.config/xv is never read (see the e2e host-isolation convention).

use std::process::Command;

fn xv(args: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_xv"))
        .args(args)
        .env("XDG_CONFIG_HOME", home)
        .env("HOME", home)
        // Pin the context store explicitly. Without this, ContextManager::load
        // checks `cwd/.xv/context` first and would read whatever context the
        // test process happens to be sitting next to.
        .env("XV_CONTEXT_DIR", home.join("xv"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("NO_COLOR", "1")
        .output()
        .expect("xv should run")
}

/// The context store path used by the `xv` helper above. Note there is no
/// `.json` extension — the file is literally named `context`.
///
/// Unused by Task 5 (`ls` doesn't touch context); Task 7 (`backend rm`) is
/// expected to use it.
#[allow(dead_code)]
fn context_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join("xv").join("context")
}

#[test]
fn backend_ls_reports_nothing_configured_on_a_fresh_config() {
    let home = tempfile::tempdir().unwrap();
    let out = xv(&["backend", "ls"], home.path());
    // Guidance/chrome (no data to show) goes to stderr, matching every
    // sibling list-style empty-state message in this codebase (output::info).
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("No backends configured"),
        "unexpected output: {text}"
    );
}

#[test]
fn backend_ls_lists_a_configured_local_backend_and_marks_it_active() {
    let home = tempfile::tempdir().unwrap();
    let conf_dir = home.path().join("xv");
    std::fs::create_dir_all(&conf_dir).unwrap();
    // Config deserializes straight from this TOML (see `load_from_file`), so
    // every field lacking `#[serde(default)]` on `Config` must be present —
    // matching the fixture pattern in tests/e2e_backend_resolution.rs.
    std::fs::write(
        conf_dir.join("xv.conf"),
        r#"
backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0

[local]
store_path = "/tmp/xv-store"
key_file = "/tmp/xv-key.txt"
default_vault = "default"
"#,
    )
    .unwrap();

    let out = xv(&["backend", "ls"], home.path());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("local"), "should list local: {text}");
    assert!(
        text.contains("active"),
        "should mark the active backend: {text}"
    );
}

#[test]
fn backend_add_rejects_an_unknown_backend_name() {
    let home = tempfile::tempdir().unwrap();
    let out = xv(&["backend", "add", "postgres"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "unknown backend must fail");
    assert!(
        text.contains("local, azure, aws"),
        "error should list the valid backends: {text}"
    );
}

#[test]
fn backend_add_refuses_to_reconfigure_without_confirmation_in_non_tty() {
    let home = tempfile::tempdir().unwrap();
    let conf_dir = home.path().join("xv");
    std::fs::create_dir_all(&conf_dir).unwrap();
    // Every field lacking `#[serde(default)]` on `Config` must be present —
    // matching the fixture pattern used above and in
    // tests/e2e_backend_resolution.rs.
    std::fs::write(
        conf_dir.join("xv.conf"),
        r#"
backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true

[local]
store_path = "/tmp/xv-store"
key_file = "/tmp/xv-key.txt"
default_vault = "default"
"#,
    )
    .unwrap();

    let out = xv(&["backend", "add", "local"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "reconfigure needs confirmation");
    assert!(text.contains("--yes"), "should name the skip flag: {text}");
}

// The CRITICAL regression (main.rs's unconditional dispatch-config resolution
// leaking into a saved config) is covered by a Rust-level test in
// src/cli/backend_ops.rs (`execute_backend_add_inner_ignores_the_dispatch_configs_backend`)
// rather than here: reaching the success path through this binary requires a
// real terminal (dialoguer's `Input` refuses to run without one), which a
// piped subprocess test cannot provide.

/// Writes a config with both local and aws configured, local active. Every
/// field lacking `#[serde(default)]` on `Config` must be present — matching
/// the fixture pattern used above and in tests/e2e_backend_resolution.rs.
fn two_backend_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let conf_dir = home.path().join("xv");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("xv.conf"),
        r#"
backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true

[local]
store_path = "/tmp/xv-store"
key_file = "/tmp/xv-key.txt"
default_vault = "default"

[aws]
region = "us-east-1"
default_vault = "default"
"#,
    )
    .unwrap();
    home
}

#[test]
fn backend_rm_refuses_to_remove_the_active_backend_when_others_remain() {
    let home = two_backend_home();
    let out = xv(&["backend", "rm", "local"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "removing the active backend must fail"
    );
    assert!(
        text.contains("xv config set backend"),
        "should say how to switch: {text}"
    );

    let saved = std::fs::read_to_string(home.path().join("xv/xv.conf")).unwrap();
    assert!(
        saved.contains("[local]"),
        "config must be untouched: {saved}"
    );
}

#[test]
fn backend_rm_removes_an_inactive_backend() {
    let home = two_backend_home();
    let out = xv(&["backend", "rm", "aws", "--yes"], home.path());
    assert!(
        out.status.success(),
        "removing an inactive backend should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let saved = std::fs::read_to_string(home.path().join("xv/xv.conf")).unwrap();
    assert!(
        !saved.contains("[aws]"),
        "aws block should be gone: {saved}"
    );
    assert!(saved.contains("[local]"), "local must survive: {saved}");
}

#[test]
fn backend_rm_errors_when_the_backend_is_not_configured() {
    let home = two_backend_home();
    let out = xv(&["backend", "rm", "azure", "--yes"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    assert!(
        text.contains("local") && text.contains("aws"),
        "should name what is configured: {text}"
    );
}

#[test]
fn backend_rm_drops_workspace_entries_for_the_removed_backend() {
    let home = two_backend_home();
    // Two attached vaults: the local one is the workspace default, the aws
    // one is not — so removing aws must not trip the default-stranding guard.
    std::fs::write(
        context_path(home.path()),
        r#"{
  "recent": [],
  "workspace": {
    "entries": [
      {"vault": "default", "backend": "local", "alias": "home", "default": true},
      {"vault": "default", "backend": "aws", "alias": "work"}
    ]
  }
}"#,
    )
    .unwrap();

    let out = xv(&["backend", "rm", "aws", "--yes"], home.path());
    assert!(
        out.status.success(),
        "should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let ctx = std::fs::read_to_string(context_path(home.path())).unwrap();
    assert!(!ctx.contains("\"work\""), "aws entry should be gone: {ctx}");
    assert!(ctx.contains("\"home\""), "local entry must survive: {ctx}");
}

#[test]
fn backend_rm_refuses_when_removal_would_strand_the_workspace_default() {
    let home = two_backend_home();
    // Here the *aws* entry is the workspace default, and a local entry
    // survives — so removing aws would leave the workspace without a default.
    let ctx_json = r#"{
  "recent": [],
  "workspace": {
    "entries": [
      {"vault": "default", "backend": "local", "alias": "home"},
      {"vault": "default", "backend": "aws", "alias": "work", "default": true}
    ]
  }
}"#;
    std::fs::write(context_path(home.path()), ctx_json).unwrap();
    let conf_before = std::fs::read_to_string(home.path().join("xv/xv.conf")).unwrap();

    let out = xv(&["backend", "rm", "aws", "--yes"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "must refuse to strand the default");
    assert!(
        text.contains("xv cx default"),
        "should say how to fix it: {text}"
    );

    let saved = std::fs::read_to_string(home.path().join("xv/xv.conf")).unwrap();
    assert!(saved.contains("[aws]"), "config must be untouched: {saved}");
    assert_eq!(
        saved, conf_before,
        "a refused rm must not touch the config file"
    );

    let ctx_after = std::fs::read_to_string(context_path(home.path())).unwrap();
    assert_eq!(
        ctx_after, ctx_json,
        "a refused rm must not touch the context file"
    );
}

#[test]
fn backend_rm_rejects_purge_for_non_local_backends() {
    let home = two_backend_home();
    let out = xv(&["backend", "rm", "aws", "--purge", "--yes"], home.path());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "--purge is local-only");
    assert!(
        text.contains("local"),
        "should explain the restriction: {text}"
    );

    let saved = std::fs::read_to_string(home.path().join("xv/xv.conf")).unwrap();
    assert!(
        saved.contains("[aws]"),
        "nothing removed on refusal: {saved}"
    );
}
