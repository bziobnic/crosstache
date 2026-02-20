//! Tests for clipboard functionality: arboard integration, configurable timeout,
//! and detached process clipboard clearing.
//!
//! NOTE: Clipboard tests must run single-threaded to avoid segfaults from
//! concurrent clipboard access:
//!   cargo test --test clipboard_tests -- --test-threads=1

use std::process::Command;
use std::time::{Duration, Instant};

/// Helper: read current clipboard text via pbcopy/pbpaste (macOS).
/// Returns None if clipboard access fails.
#[cfg(target_os = "macos")]
fn get_clipboard() -> Option<String> {
    Command::new("pbpaste")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
}

/// Helper: set clipboard text via pbcopy (macOS).
#[cfg(target_os = "macos")]
fn set_clipboard(text: &str) {
    use std::io::Write;
    let mut child = Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("pbcopy should be available on macOS");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    child.wait().unwrap();
}

// ─── arboard basic read/write ───────────────────────────────────────────────

#[test]
fn test_arboard_set_and_get_text() {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping clipboard test (no display?): {e}");
            return;
        }
    };

    let sentinel = format!("crosstache-test-{}", std::process::id());

    clipboard.set_text(&sentinel).expect("set_text should work");

    let got = clipboard.get_text().expect("get_text should work");
    assert_eq!(got, sentinel, "clipboard round-trip should preserve text");

    // Clean up
    let _ = clipboard.set_text(String::new());
}

#[test]
fn test_arboard_set_empty_string() {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping clipboard test (no display?): {e}");
            return;
        }
    };

    // Set something first, then clear
    clipboard.set_text("temp").expect("set_text should work");
    clipboard
        .set_text("")
        .expect("setting empty string should work");

    let got = clipboard.get_text().expect("get_text should work");
    assert!(
        got.is_empty(),
        "clipboard should be empty after setting empty string"
    );
}

// ─── Detached process clipboard clear ───────────────────────────────────────

/// Test that the detached clear process actually clears the clipboard.
/// Uses a short timeout (2s) so the test doesn't take forever.
#[cfg(target_os = "macos")]
#[test]
fn test_detached_clear_process_clears_clipboard() {
    let sentinel = format!("crosstache-clear-test-{}", std::process::id());
    set_clipboard(&sentinel);

    // Verify it was set
    assert_eq!(get_clipboard().unwrap(), sentinel);

    // Spawn the same detached clear command used in production, but with 2s timeout
    let child = Command::new("sh")
        .args(["-c", "sleep 2 && printf '' | pbcopy"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    assert!(child.is_ok(), "detached clear process should spawn");

    // Clipboard should still have our value immediately
    assert_eq!(
        get_clipboard().unwrap(),
        sentinel,
        "clipboard should not be cleared immediately"
    );

    // Wait for the clear to fire
    std::thread::sleep(Duration::from_secs(3));

    let after = get_clipboard().unwrap();
    assert!(
        after.is_empty() || after != sentinel,
        "clipboard should be cleared after timeout, got: '{after}'"
    );
}

/// Verify the spawned process is truly detached — it should get its own PID
/// and not block the parent.
#[cfg(target_os = "macos")]
#[test]
fn test_detached_process_does_not_block() {
    let start = Instant::now();

    let child = Command::new("sh")
        .args(["-c", "sleep 30 && printf '' | pbcopy"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    let elapsed = start.elapsed();

    assert!(child.is_ok(), "spawn should succeed");
    assert!(
        elapsed < Duration::from_secs(1),
        "spawn should return immediately, took {:?}",
        elapsed
    );

    // Kill the child so we don't leave a 30s sleeper
    if let Ok(mut c) = child {
        let _ = c.kill();
    }
}

// ─── Config: clipboard_timeout ──────────────────────────────────────────────

#[test]
fn test_config_default_clipboard_timeout() {
    let config = crosstache::config::settings::Config::default();
    assert_eq!(
        config.clipboard_timeout, 30,
        "default clipboard_timeout should be 30 seconds"
    );
}

#[test]
fn test_config_clipboard_timeout_deserialization() {
    // Simulate a config without clipboard_timeout — serde default should kick in
    let json = r#"{
        "debug": false,
        "subscription_id": "",
        "default_vault": "",
        "default_resource_group": "Vaults",
        "default_location": "eastus",
        "tenant_id": "",
        "function_app_url": "",
        "cache_ttl": { "secs": 300, "nanos": 0 },
        "output_json": false,
        "no_color": false,
        "azure_credential_priority": "default"
    }"#;

    let config: crosstache::config::settings::Config =
        serde_json::from_str(json).expect("should deserialize without clipboard_timeout field");
    assert_eq!(
        config.clipboard_timeout, 30,
        "missing field should use serde default of 30"
    );
}

#[test]
fn test_config_clipboard_timeout_custom_value() {
    let json = r#"{
        "debug": false,
        "subscription_id": "",
        "default_vault": "",
        "default_resource_group": "Vaults",
        "default_location": "eastus",
        "tenant_id": "",
        "function_app_url": "",
        "cache_ttl": { "secs": 300, "nanos": 0 },
        "output_json": false,
        "no_color": false,
        "azure_credential_priority": "default",
        "clipboard_timeout": 60
    }"#;

    let config: crosstache::config::settings::Config =
        serde_json::from_str(json).expect("should deserialize with custom clipboard_timeout");
    assert_eq!(config.clipboard_timeout, 60);
}

#[test]
fn test_config_clipboard_timeout_zero_disables() {
    let json = r#"{
        "debug": false,
        "subscription_id": "",
        "default_vault": "",
        "default_resource_group": "Vaults",
        "default_location": "eastus",
        "tenant_id": "",
        "function_app_url": "",
        "cache_ttl": { "secs": 300, "nanos": 0 },
        "output_json": false,
        "no_color": false,
        "azure_credential_priority": "default",
        "clipboard_timeout": 0
    }"#;

    let config: crosstache::config::settings::Config =
        serde_json::from_str(json).expect("should deserialize with clipboard_timeout=0");
    assert_eq!(
        config.clipboard_timeout, 0,
        "0 should be valid (means disabled)"
    );
}

#[test]
fn test_config_clipboard_timeout_serialization_roundtrip() {
    let mut config = crosstache::config::settings::Config::default();
    config.clipboard_timeout = 45;

    let serialized = serde_json::to_string(&config).expect("should serialize");
    assert!(
        serialized.contains("\"clipboard_timeout\":45"),
        "serialized config should contain clipboard_timeout"
    );

    let deserialized: crosstache::config::settings::Config =
        serde_json::from_str(&serialized).expect("should deserialize");
    assert_eq!(deserialized.clipboard_timeout, 45);
}
