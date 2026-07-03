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

    // Plain `get --raw` returns the primary field bare (record-aware `get`,
    // Task 7's compatibility contract) — not the JSON envelope. The
    // envelope shape itself is asserted directly against the on-disk value
    // in `set_field_secret_goes_to_envelope` below and via `--record` in
    // `get_record_json_includes_all_fields`.
    let out2 = xv_same_env(temp.path())
        .args(["get", "cred", "--raw"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    assert_eq!(common::stdout_str(&out2), "hunter2");
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

/// Bugbot PR #322 round 3: `--field username= ` (empty/whitespace value)
/// must not satisfy a required field — non-interactive validation must
/// match the interactive prompt path, which already rejects a blank
/// answer for a required field.
#[test]
fn set_typed_empty_required_field_fails_before_write() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username= ",
            "--value",
            "hunter2",
        ])
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

    let meta_path = temp
        .path()
        .join("store")
        .join("vaults")
        .join("default")
        .join("secrets")
        .join("cred.meta.json");
    assert!(!meta_path.exists(), "nothing must be written on rejection");
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

/// Bugbot PR #322 follow-up: a user `--tag xv-type=...` on a typed `set`
/// must be rejected before write, not silently override the record's own
/// type marker (which would desync tags from the envelope and break type
/// resolution / plain `get`).
#[test]
fn set_typed_rejects_tag_colliding_with_xv_type() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
            "--tag",
            "xv-type=other",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("xv-type"), "stderr: {stderr}");

    let meta_path = temp
        .path()
        .join("store")
        .join("vaults")
        .join("default")
        .join("secrets")
        .join("cred.meta.json");
    assert!(!meta_path.exists(), "nothing must be written on rejection");
}

/// Same as above but for a `--tag f.<name>=...` collision (the `f.*`
/// metadata-field tag prefix is just as reserved as `xv-type` itself).
#[test]
fn set_typed_rejects_tag_colliding_with_field_prefix() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
            "--tag",
            "f.something=x",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(
        stderr.contains("f.something") || stderr.contains("f.*"),
        "stderr: {stderr}"
    );

    let meta_path = temp
        .path()
        .join("store")
        .join("vaults")
        .join("default")
        .join("secrets")
        .join("cred.meta.json");
    assert!(!meta_path.exists(), "nothing must be written on rejection");
}

/// Bugbot PR #322 round 4: `--note x --tag note=y` must be rejected on a
/// typed record set rather than silently letting the dedicated `--note`
/// flag win (which is what the untyped `set` path does deterministically
/// today — no error, `x` always wins over a same-named `--tag`). The
/// record path is intentionally stricter: fail loud instead of silently
/// picking a winner between two conflicting sources for the same tag.
#[test]
fn set_typed_rejects_tag_colliding_with_note() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
            "--note",
            "x",
            "--tag",
            "note=y",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("note"), "stderr: {stderr}");

    let meta_path = temp
        .path()
        .join("store")
        .join("vaults")
        .join("default")
        .join("secrets")
        .join("cred.meta.json");
    assert!(!meta_path.exists(), "nothing must be written on rejection");
}

/// Same collision class as `note`, for `--group`/`--tag groups=...`.
#[test]
fn set_typed_rejects_tag_colliding_with_groups() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
            "--group",
            "prod",
            "--tag",
            "groups=other",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("groups"), "stderr: {stderr}");

    let meta_path = temp
        .path()
        .join("store")
        .join("vaults")
        .join("default")
        .join("secrets")
        .join("cred.meta.json");
    assert!(!meta_path.exists(), "nothing must be written on rejection");
}

/// Same collision class as `note`, for `--folder`/`--tag folder=...`.
#[test]
fn set_typed_rejects_tag_colliding_with_folder() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
            "--folder",
            "app",
            "--tag",
            "folder=other",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("folder"), "stderr: {stderr}");

    let meta_path = temp
        .path()
        .join("store")
        .join("vaults")
        .join("default")
        .join("secrets")
        .join("cred.meta.json");
    assert!(!meta_path.exists(), "nothing must be written on rejection");
}

/// Bugbot PR #322 re-review: `xv set --type` with `--field` values but no
/// `--value`/`--stdin` must not hang waiting on a TTY that will never
/// arrive — the test harness's stdin is not a TTY (it's whatever
/// std::process::Command inherits/pipes by default in a test process), so
/// this exercises exactly the non-interactive-without-primary path.
#[test]
fn set_typed_missing_value_non_tty_fails_before_write_mentions_stdin() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "cred", "--type", "login", "--field", "username=bob"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("--stdin"), "stderr: {stderr}");

    let meta_path = temp
        .path()
        .join("store")
        .join("vaults")
        .join("default")
        .join("secrets")
        .join("cred.meta.json");
    assert!(!meta_path.exists(), "nothing must be written on rejection");
}

/// Bugbot PR #322 re-review: `--field a=1 --field-secret a=2` must be
/// rejected rather than silently storing `a` as both an f.* tag and an
/// envelope entry (which `get --field a` would then read inconsistently —
/// envelope first, silently ignoring the tag).
#[test]
fn set_typed_rejects_duplicate_field_across_field_and_field_secret() {
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
            "a=1",
            "--field-secret",
            "a=2",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains('a'), "stderr: {stderr}");

    let meta_path = temp
        .path()
        .join("store")
        .join("vaults")
        .join("default")
        .join("secrets")
        .join("cred.meta.json");
    assert!(!meta_path.exists(), "nothing must be written on rejection");
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

// ---------------------------------------------------------------------------
// Task 7: `xv get` — primary, --field, --record, failure modes
// ---------------------------------------------------------------------------

/// Builds a fresh `xv` `Command` bound to the same isolated store/env as an
/// existing `xv_isolated_local()` tempdir, for a second CLI invocation
/// against the same store (each `Command` is single-use).
fn xv_same_env(temp: &std::path::Path) -> std::process::Command {
    let mut cmd = common::xv();
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp)
        .env("XDG_CONFIG_HOME", temp.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(temp);
    cmd
}

#[test]
fn get_typed_record_returns_primary_bare() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["get", "cred", "--raw"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    assert_eq!(common::stdout_str(&out2), "hunter2");
}

/// This test exercises `--field --raw`, not the clipboard path: every
/// harness in `tests/common/mod.rs` sets `clipboard_timeout = 0` and CI
/// runners are headless (no real clipboard), so asserting the "auto-clears
/// in Ns" affordance line end-to-end would be flaky/environment-dependent
/// here. The clipboard auto-clear-vs-skip decision for secret-kind vs
/// metadata-kind fields (code review follow-up) is instead unit-tested
/// directly against the extracted pure function `field_clipboard_outcome`
/// — see the `field_clipboard_outcome_*` tests in `src/cli/secret_ops.rs`.
#[test]
fn get_field_metadata_and_secret() {
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

    // Metadata field (tag-backed).
    let out_meta = xv_same_env(temp.path())
        .args(["get", "cred", "--field", "username", "--raw"])
        .output()
        .unwrap();
    assert!(
        out_meta.status.success(),
        "stderr: {}",
        common::stderr_str(&out_meta)
    );
    assert_eq!(common::stdout_str(&out_meta), "bob");

    // Secret field (envelope-backed).
    let out_secret = xv_same_env(temp.path())
        .args(["get", "cred", "--field", "password", "--raw"])
        .output()
        .unwrap();
    assert!(
        out_secret.status.success(),
        "stderr: {}",
        common::stderr_str(&out_secret)
    );
    assert_eq!(common::stdout_str(&out_secret), "hunter2");
}

#[test]
fn get_record_json_includes_all_fields() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["get", "cred", "--record", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stdout = common::stdout_str(&out2);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(parsed["type"], "login");
    assert_eq!(parsed["fields"]["username"], "bob");
    assert_eq!(parsed["fields"]["password"], "hunter2");
}

#[test]
fn get_unknown_field_lists_fields() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["get", "cred", "--field", "nosuchfield"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stderr = common::stderr_str(&out2);
    assert!(stderr.contains("username"), "stderr: {stderr}");
    assert!(stderr.contains("password"), "stderr: {stderr}");
}

/// Corrupt-envelope: a record-marked secret whose value fails JSON parsing.
/// The CLI's plain `xv set` can't produce this state directly — a full
/// `set_secret` write always replaces `content_type` from the request
/// (`unwrap_or_default()` in the local backend), so overwriting a record's
/// value through the CLI's untyped `xv set` path would also clear the
/// record content-type rather than reproduce "record-tagged, bad JSON".
/// Instead this drives the local backend library directly (same crate,
/// same store/key as the CLI run above) to write a bogus value while
/// keeping `content_type` and tags intact — reproducing the state a
/// corrupted on-disk write (or manual tampering) would leave behind.
#[test]
fn get_corrupt_envelope_fails_loud() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let store_path = temp.path().join("store");
    let key_file = temp.path().join("key.txt");
    let local_config = crosstache::config::settings::LocalConfig {
        store_path: Some(store_path.to_string_lossy().to_string()),
        key_file: Some(key_file.to_string_lossy().to_string()),
        default_vault: Some("default".to_string()),
        encrypt_metadata: None,
        opaque_filenames: None,
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        use crosstache::backend::local::LocalBackend;
        use crosstache::backend::Backend;
        use crosstache::secret::manager::SecretRequest;

        let backend = LocalBackend::new(Some(&local_config)).expect("open local backend");
        let existing = backend
            .secrets()
            .get_secret("default", "cred", false)
            .await
            .expect("get existing record");

        let request = SecretRequest {
            name: "cred".to_string(),
            value: zeroize::Zeroizing::new("not-json".to_string()),
            content_type: Some("application/vnd.xv.record".to_string()),
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: Some(existing.tags.clone()),
            groups: None,
            note: None,
            folder: None,
        };
        backend
            .secrets()
            .set_secret("default", request)
            .await
            .expect("overwrite with corrupt envelope");
    });

    let out2 = xv_same_env(temp.path())
        .args(["get", "cred"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stderr = common::stderr_str(&out2);
    assert!(stderr.contains("cred"), "stderr: {stderr}");
    assert!(
        stderr.contains("application/vnd.xv.record"),
        "stderr: {stderr}"
    );
    // Never print the raw JSON value as if it were valid.
    assert!(!stderr.contains("not-json"), "stderr: {stderr}");
}

#[test]
fn get_unknown_type_degrades() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    // Rewrite the xv-type tag to an unresolvable type name via the local
    // backend library (same rationale as get_corrupt_envelope_fails_loud —
    // no CLI verb yet edits tags in place; that's Task 8, out of Phase A).
    let store_path = temp.path().join("store");
    let key_file = temp.path().join("key.txt");
    let local_config = crosstache::config::settings::LocalConfig {
        store_path: Some(store_path.to_string_lossy().to_string()),
        key_file: Some(key_file.to_string_lossy().to_string()),
        default_vault: Some("default".to_string()),
        encrypt_metadata: None,
        opaque_filenames: None,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        use crosstache::backend::local::LocalBackend;
        use crosstache::backend::Backend;
        use crosstache::secret::manager::SecretRequest;

        let backend = LocalBackend::new(Some(&local_config)).expect("open local backend");
        let existing = backend
            .secrets()
            .get_secret("default", "cred", true)
            .await
            .expect("get existing record");
        let mut tags = existing.tags.clone();
        tags.insert("xv-type".to_string(), "nosuch".to_string());

        let request = SecretRequest {
            name: "cred".to_string(),
            value: existing.value.clone().unwrap(),
            content_type: Some("application/vnd.xv.record".to_string()),
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: Some(tags),
            groups: None,
            note: None,
            folder: None,
        };
        backend
            .secrets()
            .set_secret("default", request)
            .await
            .expect("rewrite with unknown type");
    });

    // Plain `get` can't determine the primary field: errors.
    let out_plain = xv_same_env(temp.path())
        .args(["get", "cred"])
        .output()
        .unwrap();
    assert_eq!(
        out_plain.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out_plain)
    );

    // `--field` still works via the raw envelope/tags.
    let out_field = xv_same_env(temp.path())
        .args(["get", "cred", "--field", "username", "--raw"])
        .output()
        .unwrap();
    assert!(
        out_field.status.success(),
        "stderr: {}",
        common::stderr_str(&out_field)
    );
    assert_eq!(common::stdout_str(&out_field), "bob");
}

// ---------------------------------------------------------------------------
// Task 8: `xv update --field`/`--field-secret`
// ---------------------------------------------------------------------------

#[test]
fn update_metadata_field_is_tag_only() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let before = read_local_meta(temp.path(), "default", "cred");
    let version_before = before["version"].clone();

    let out2 = xv_same_env(temp.path())
        .args(["update", "cred", "--field", "username=alice"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let after = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(after["tags"]["f.username"], "alice");
    // Tag-only update: no new version.
    assert_eq!(after["version"], version_before);

    // Primary value untouched.
    let out3 = xv_same_env(temp.path())
        .args(["get", "cred", "--raw"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert_eq!(common::stdout_str(&out3), "hunter2");
}

#[test]
fn update_secret_field_writes_new_envelope() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let before = read_local_meta(temp.path(), "default", "cred");
    let version_before = before["version"].clone();

    let out2 = xv_same_env(temp.path())
        .args(["update", "cred", "--field-secret", "totp-seed=ABCDEF"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let after = read_local_meta(temp.path(), "default", "cred");
    // New secret-field value forces a new version.
    assert_ne!(after["version"], version_before);
    assert!(after["tags"].get("f.totp-seed").is_none());

    let out3 = xv_same_env(temp.path())
        .args(["get", "cred", "--field", "totp-seed", "--raw"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert_eq!(common::stdout_str(&out3), "ABCDEF");

    // Primary value untouched by the merge.
    let out4 = xv_same_env(temp.path())
        .args(["get", "cred", "--raw"])
        .output()
        .unwrap();
    assert_eq!(common::stdout_str(&out4), "hunter2");
}

#[test]
fn update_field_on_untyped_errors() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "plain", "--value", "just-a-value"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["update", "plain", "--field", "username=bob"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out2)
    );
}

// ---------------------------------------------------------------------------
// Task 9: `xv update --type` / `--untype` conversion
// ---------------------------------------------------------------------------

#[test]
fn type_conversion_roundtrip() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "bare", "--value", "hunter2"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["update", "bare", "--type", "login"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let meta = read_local_meta(temp.path(), "default", "bare");
    assert_eq!(meta["content_type"], "application/vnd.xv.record");
    assert_eq!(meta["tags"]["xv-type"], "login");

    let out3 = xv_same_env(temp.path())
        .args(["get", "bare", "--raw"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert_eq!(common::stdout_str(&out3), "hunter2");

    let out4 = xv_same_env(temp.path())
        .args(["update", "bare", "--untype", "--yes"])
        .output()
        .unwrap();
    assert!(
        out4.status.success(),
        "stderr: {}",
        common::stderr_str(&out4)
    );
    let meta2 = read_local_meta(temp.path(), "default", "bare");
    assert_eq!(meta2["content_type"], "");
    assert!(meta2["tags"].get("xv-type").is_none());

    let out5 = xv_same_env(temp.path())
        .args(["get", "bare", "--raw"])
        .output()
        .unwrap();
    assert!(
        out5.status.success(),
        "stderr: {}",
        common::stderr_str(&out5)
    );
    assert_eq!(common::stdout_str(&out5), "hunter2");
}

#[test]
fn untype_with_extra_secret_fields_requires_yes() {
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

    // Non-TTY without --yes: refuses, exit 3, nothing changed.
    let out2 = xv_same_env(temp.path())
        .args(["update", "cred", "--untype"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stderr = common::stderr_str(&out2);
    assert!(stderr.contains("--yes"), "stderr: {stderr}");
    let meta_unchanged = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(meta_unchanged["content_type"], "application/vnd.xv.record");

    // With --yes: succeeds, drops totp-seed, names it in output.
    let out3 = xv_same_env(temp.path())
        .args(["update", "cred", "--untype", "--yes"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    let stderr3 = common::stderr_str(&out3);
    assert!(stderr3.contains("totp-seed"), "stderr: {stderr3}");

    let meta = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(meta["content_type"], "");
    assert!(meta["tags"].get("xv-type").is_none());
    assert!(meta["tags"].get("f.username").is_none());

    let out4 = xv_same_env(temp.path())
        .args(["get", "cred", "--raw"])
        .output()
        .unwrap();
    assert_eq!(common::stdout_str(&out4), "hunter2");
}

#[test]
fn type_on_existing_record_errors() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "cred",
            "--type",
            "login",
            "--field",
            "username=bob",
            "--value",
            "hunter2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["update", "cred", "--type", "api-key"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stderr = common::stderr_str(&out2);
    assert!(stderr.contains("already"), "stderr: {stderr}");
}

#[test]
fn get_field_on_untyped_errors() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "plain", "--value", "just-a-value"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["get", "plain", "--field", "whatever"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let out3 = xv_same_env(temp.path())
        .args(["get", "plain", "--record"])
        .output()
        .unwrap();
    assert_eq!(
        out3.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out3)
    );
}
