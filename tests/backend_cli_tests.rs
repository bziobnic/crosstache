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
