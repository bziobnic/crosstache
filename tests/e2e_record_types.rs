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
    // Record identity must survive a secret-field edit (Bugbot BLOCKER:
    // an Azure-specific bug relied on backend merge-on-None semantics for
    // content_type/tags on this exact write shape — content_type: None
    // and, when no --field accompanies --field-secret, tags: None too —
    // which the Azure full-PUT path takes literally rather than treating
    // as "leave unchanged". Local never had this bug (it does treat None
    // as unchanged), so this assertion is a regression guard for the fix
    // itself, not a reproduction of the original Azure bug.
    assert_eq!(after["content_type"], "application/vnd.xv.record");
    assert_eq!(after["tags"]["xv-type"], "login");
    assert_eq!(after["tags"]["f.username"], "bob");

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

/// Bugbot MEDIUM review, round 3: `SecretProperties.tags` (as returned by
/// `get_secret`) is DENORMALIZED for display — groups/note/folder are
/// folded into plain tag keys. The record write-back paths built their
/// `replace_tags: true` map directly from `secret.tags.clone()`, so those
/// denormalized keys rode along into the write. On local this means they'd
/// persist into `SecretMeta.tags` even though the real values live in the
/// dedicated `.groups`/`.note`/`.folder` fields — the exact bug class the
/// #315 copy/move review caught. This test pins the fix: group/note/folder
/// metadata survives a secret-field edit, AND `meta.tags` (the raw on-disk
/// tag map) never contains `groups`/`note`/`folder` keys.
#[test]
fn update_secret_field_preserves_group_note_folder_without_denormalizing_into_tags() {
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
            "--group",
            "team-a",
            "--note",
            "rotate monthly",
            "--folder",
            "app/db",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let before = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(before["groups"], serde_json::json!(["prod", "team-a"]));
    assert_eq!(before["note"], "rotate monthly");
    assert_eq!(before["folder"], "app/db");
    // Never denormalized into tags in the first place (set path).
    assert!(before["tags"].get("groups").is_none());
    assert!(before["tags"].get("note").is_none());
    assert!(before["tags"].get("folder").is_none());

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
    // Dedicated metadata fields intact.
    assert_eq!(after["groups"], serde_json::json!(["prod", "team-a"]));
    assert_eq!(after["note"], "rotate monthly");
    assert_eq!(after["folder"], "app/db");
    // Never leaked into the raw tag map by the write-back.
    assert!(
        after["tags"].get("groups").is_none(),
        "meta.tags: {:?}",
        after["tags"]
    );
    assert!(
        after["tags"].get("note").is_none(),
        "meta.tags: {:?}",
        after["tags"]
    );
    assert!(
        after["tags"].get("folder").is_none(),
        "meta.tags: {:?}",
        after["tags"]
    );
    // Record identity and the field edit itself still landed correctly.
    assert_eq!(after["content_type"], "application/vnd.xv.record");
    assert_eq!(after["tags"]["xv-type"], "login");
    assert_eq!(after["tags"]["f.username"], "bob");
}

/// Same denormalization guard as above, for `--type` conversion.
#[test]
fn type_conversion_preserves_group_note_folder_without_denormalizing_into_tags() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "bare",
            "--value",
            "hunter2",
            "--group",
            "prod",
            "--group",
            "team-a",
            "--note",
            "rotate monthly",
            "--folder",
            "app/db",
        ])
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

    let after = read_local_meta(temp.path(), "default", "bare");
    assert_eq!(after["groups"], serde_json::json!(["prod", "team-a"]));
    assert_eq!(after["note"], "rotate monthly");
    assert_eq!(after["folder"], "app/db");
    assert!(
        after["tags"].get("groups").is_none(),
        "meta.tags: {:?}",
        after["tags"]
    );
    assert!(
        after["tags"].get("note").is_none(),
        "meta.tags: {:?}",
        after["tags"]
    );
    assert!(
        after["tags"].get("folder").is_none(),
        "meta.tags: {:?}",
        after["tags"]
    );
}

/// Same denormalization guard as above, for `--untype`.
#[test]
fn untype_preserves_group_note_folder_without_denormalizing_into_tags() {
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
            "--group",
            "team-a",
            "--note",
            "rotate monthly",
            "--folder",
            "app/db",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["update", "cred", "--untype", "--yes"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let after = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(after["groups"], serde_json::json!(["prod", "team-a"]));
    assert_eq!(after["note"], "rotate monthly");
    assert_eq!(after["folder"], "app/db");
    assert!(
        after["tags"].get("groups").is_none(),
        "meta.tags: {:?}",
        after["tags"]
    );
    assert!(
        after["tags"].get("note").is_none(),
        "meta.tags: {:?}",
        after["tags"]
    );
    assert!(
        after["tags"].get("folder").is_none(),
        "meta.tags: {:?}",
        after["tags"]
    );
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

/// Bugbot MEDIUM review: combining a record-path flag (`--field`,
/// `--field-secret`, `--type`, `--untype`) with a classic update flag
/// (`--note`, `--tags`, `--folder`, the positional value, etc.) used to
/// early-return after applying only the record edit, silently ignoring the
/// classic flag while still reporting success. Now a clap usage error
/// (exit 2) before anything runs, and nothing is changed.
#[test]
fn update_field_with_classic_flag_is_a_usage_error() {
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

    let out2 = xv_same_env(temp.path())
        .args(["update", "cred", "--field", "username=alice", "--note", "n"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(2),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    // Nothing changed — neither the field edit nor the note was applied.
    let after = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(before, after);
}

/// Bugbot MEDIUM review: `xv update <name> --type <t>` converting AND
/// replacing the primary value in the same command (a positional VALUE
/// arg alongside `--type`) is now a usage error — convert first, then set
/// the primary via `xv update <name> --field-secret <primary>=<value>`.
#[test]
fn update_type_with_positional_value_is_a_usage_error() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "bare", "--value", "hunter2"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["update", "bare", "--type", "login", "new-value"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(2),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stderr = common::stderr_str(&out2);
    assert!(stderr.contains("--type"), "stderr: {stderr}");

    // Nothing changed — the secret is still untyped with its original value.
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

// ---------------------------------------------------------------------------
// Task 10: `ls` — type column, f.* fields in JSON, --type filter
// ---------------------------------------------------------------------------

#[test]
fn ls_shows_type_column_when_typed_present() {
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
    let out_plain = xv_same_env(temp.path())
        .args(["set", "plain", "--value", "just-a-value"])
        .output()
        .unwrap();
    assert!(
        out_plain.status.success(),
        "stderr: {}",
        common::stderr_str(&out_plain)
    );

    let out2 = xv_same_env(temp.path())
        .args(["ls", "--format", "table"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stdout = common::stdout_str(&out2);
    assert!(stdout.contains("Type"), "stdout: {stdout}");
    assert!(stdout.contains("login"), "stdout: {stdout}");
}

#[test]
fn ls_untyped_only_output_unchanged() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "plain", "--value", "just-a-value"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["ls", "--format", "table"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stdout = common::stdout_str(&out2);
    // Byte-compare the table header line against the pre-Task-10 shape:
    // TableFormatter drops all-empty columns, so this fixture's table only
    // ever showed Name/Updated — the point of this test is that adding
    // record-types support doesn't introduce a Type column (or any other
    // column) for an untyped-only listing.
    let header_line = stdout
        .lines()
        .find(|l| l.contains("Name"))
        .unwrap_or_else(|| panic!("no header line in: {stdout}"));
    assert_eq!(
        header_line.trim(),
        "│ Name  │  Updated   │",
        "stdout: {stdout}"
    );
    assert!(!stdout.contains("Type"), "stdout: {stdout}");

    // Byte-compare a DATA row too, not just the header — the date portion
    // is the only part that legitimately varies run to run, so extract it
    // from the actual row and rebuild the expected line around it; every
    // other byte (name padding, column widths, box-drawing characters)
    // must still match exactly.
    let data_line = stdout
        .lines()
        .find(|l| l.contains("plain"))
        .unwrap_or_else(|| panic!("no data row in: {stdout}"));
    let date = data_line
        .trim()
        .split('│')
        .nth(2)
        .unwrap_or_else(|| panic!("row has no second column: {data_line}"))
        .trim();
    assert_eq!(
        data_line.trim(),
        format!("│ plain │ {date} │"),
        "stdout: {stdout}"
    );
}

#[test]
fn ls_json_lifts_fields_and_type() {
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
        .args(["ls", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stdout = common::stdout_str(&out2);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let entries = parsed.as_array().expect("array");
    let cred = entries
        .iter()
        .find(|e| e["name"] == "cred")
        .expect("cred entry present");
    assert_eq!(cred["record_type"], "login");
    assert_eq!(cred["fields"]["username"], "bob");
}

#[test]
fn ls_type_filter() {
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
    let out_plain = xv_same_env(temp.path())
        .args(["set", "plain", "--value", "just-a-value"])
        .output()
        .unwrap();
    assert!(
        out_plain.status.success(),
        "stderr: {}",
        common::stderr_str(&out_plain)
    );

    let out2 = xv_same_env(temp.path())
        .args(["ls", "--type", "login", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stdout = common::stdout_str(&out2);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let entries = parsed.as_array().expect("array");
    assert_eq!(entries.len(), 1, "stdout: {stdout}");
    assert_eq!(entries[0]["name"], "cred");
}

/// Bugbot LOW review, round 3: `DeletedSecretSummary` (the shape returned
/// by `ls --deleted` on every backend) has no `tags` field at all — deleted
/// listings never carry `xv-type`, so `--type` can't be threaded through
/// and filtered without a bigger cross-backend change to start fetching
/// tags for every deleted secret. Rather than a silent no-op (the filter
/// looking like it did nothing), `--deleted --type` is a hard clap usage
/// error in both flag orders.
#[test]
fn ls_deleted_with_type_filter_is_a_usage_error() {
    let (mut cmd, _temp) = common::xv_isolated_local();
    let out = cmd
        .args(["ls", "--deleted", "--type", "login"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        common::stderr_str(&out)
    );
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("cannot be used with"), "{stderr}");

    let (mut cmd2, _temp2) = common::xv_isolated_local();
    let out2 = cmd2
        .args(["ls", "--type", "login", "--deleted"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(2),
        "stderr: {}",
        common::stderr_str(&out2)
    );
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

// ---------------------------------------------------------------------------
// Task 12: `inject` field syntax + `xv run` primary-field guard
// ---------------------------------------------------------------------------

/// Creates a `login` record named `cred` (username `bob`, primary/password
/// `hunter2`) in the isolated store rooted at `temp`.
fn set_cred_login_record(temp: &std::path::Path) {
    let out = xv_same_env(temp)
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
}

#[test]
fn inject_field_syntax_renders() {
    let (_cmd, temp) = common::xv_isolated_local();
    set_cred_login_record(temp.path());

    let template_path = temp.path().join("template.txt");
    std::fs::write(&template_path, "user: {{ secret:cred.username }}\n").unwrap();
    let out_path = temp.path().join("out.txt");

    let out = xv_same_env(temp.path())
        .args([
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let rendered = std::fs::read_to_string(&out_path).unwrap();
    assert!(rendered.contains("bob"), "rendered: {rendered}");
    assert!(!rendered.contains("{{ secret:"), "rendered: {rendered}");
}

#[test]
fn inject_bare_name_renders_primary() {
    let (_cmd, temp) = common::xv_isolated_local();
    set_cred_login_record(temp.path());

    let template_path = temp.path().join("template.txt");
    std::fs::write(&template_path, "pw: {{ secret:cred }}\n").unwrap();
    let out_path = temp.path().join("out.txt");

    let out = xv_same_env(temp.path())
        .args([
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let rendered = std::fs::read_to_string(&out_path).unwrap();
    assert!(rendered.contains("hunter2"), "rendered: {rendered}");
    // Never the raw JSON envelope.
    assert!(!rendered.contains("\"password\""), "rendered: {rendered}");
}

/// An untyped secret literally named `a.b` must resolve as itself via the
/// exact-name-first match rule, never as field `b` of a record named `a`
/// (record-types spec/plan Task 12 disambiguation requirement).
#[test]
fn inject_dot_name_exact_match_wins() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "a.b", "--value", "plain-dotted-value"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let template_path = temp.path().join("template.txt");
    std::fs::write(&template_path, "v: {{ secret:a.b }}\n").unwrap();
    let out_path = temp.path().join("out.txt");

    let out2 = xv_same_env(temp.path())
        .args([
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let rendered = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        rendered.contains("plain-dotted-value"),
        "rendered: {rendered}"
    );
}

#[test]
fn inject_unknown_field_aborts() {
    let (_cmd, temp) = common::xv_isolated_local();
    set_cred_login_record(temp.path());

    let template_path = temp.path().join("template.txt");
    std::fs::write(&template_path, "v: {{ secret:cred.nosuchfield }}\n").unwrap();
    let out_path = temp.path().join("out.txt");

    let out = xv_same_env(temp.path())
        .args([
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out)
    );
    // Distinguish from a plain "secret not found" (the pre-Task-12 failure
    // mode for any dotted name): the message must come from field
    // resolution, naming the record's actual known fields.
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("username"), "stderr: {stderr}");
    assert!(
        !out_path.exists(),
        "output file must NOT be created when a field reference fails to resolve"
    );
}

#[test]
fn inject_field_on_untyped_secret_aborts() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "plain", "--value", "just-a-value"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let template_path = temp.path().join("template.txt");
    std::fs::write(&template_path, "v: {{ secret:plain.somefield }}\n").unwrap();
    let out_path = temp.path().join("out.txt");

    let out2 = xv_same_env(temp.path())
        .args([
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    assert!(!out_path.exists());
}

#[test]
fn inject_uri_fragment_field() {
    let (_cmd, temp) = common::xv_isolated_local();
    set_cred_login_record(temp.path());

    let template_path = temp.path().join("template.txt");
    std::fs::write(&template_path, "v: xv://default/cred#username\n").unwrap();
    let out_path = temp.path().join("out.txt");

    let out = xv_same_env(temp.path())
        .args([
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let rendered = std::fs::read_to_string(&out_path).unwrap();
    assert!(rendered.contains("bob"), "rendered: {rendered}");
    assert!(!rendered.contains("xv://"), "rendered: {rendered}");
}

/// Bugbot review (round on #321 Phase C): a bare `xv://vault/name` is a
/// strict prefix of its own `xv://vault/name#field` form. Substituting via
/// `HashMap` iteration order (nondeterministic) could rewrite part of the
/// longer `#field` reference before it was ever matched whole, mangling the
/// output. Referencing a record's primary *and* one of its fields in the
/// same template is exactly the workflow this feature invites, so both
/// forms must always resolve correctly regardless of iteration order — run
/// several times to guard against order-dependent flakiness.
#[test]
fn inject_bare_and_fragment_uri_for_same_secret_both_resolve() {
    let (_cmd, temp) = common::xv_isolated_local();
    set_cred_login_record(temp.path());

    let template_path = temp.path().join("template.txt");
    std::fs::write(
        &template_path,
        "primary: xv://default/cred\nusername: xv://default/cred#username\n",
    )
    .unwrap();
    let out_path = temp.path().join("out.txt");

    for _ in 0..10 {
        let out = xv_same_env(temp.path())
            .args([
                "inject",
                "--template",
                template_path.to_str().unwrap(),
                "--out",
                out_path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

        let rendered = std::fs::read_to_string(&out_path).unwrap();
        assert!(
            rendered.contains("primary: hunter2"),
            "rendered: {rendered}"
        );
        assert!(rendered.contains("username: bob"), "rendered: {rendered}");
        assert!(!rendered.contains('#'), "rendered: {rendered}");
        assert!(!rendered.contains("xv://"), "rendered: {rendered}");
    }
}

#[test]
fn inject_field_best_effort_renders_with_warning() {
    let (_cmd, temp) = common::xv_isolated_local();
    set_cred_login_record(temp.path());

    let template_path = temp.path().join("template.txt");
    std::fs::write(&template_path, "v: {{ secret:cred.nosuchfield }}\n").unwrap();
    let out_path = temp.path().join("out.txt");

    let out = xv_same_env(temp.path())
        .args([
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
            "--best-effort",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("nosuchfield"), "stderr: {stderr}");

    // The unresolved placeholder is left in the output verbatim, matching
    // the pre-existing --best-effort contract for other unresolved
    // references (#319).
    let rendered = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        rendered.contains("{{ secret:cred.nosuchfield }}"),
        "rendered: {rendered}"
    );
}

/// Guard test (spec §9: `xv run` gets no per-field expansion in v1) proving
/// `xv run` on a typed record injects the *primary* field value under the
/// record's name — not the raw JSON envelope — exactly like plain `get`.
#[test]
fn run_typed_record_injects_primary() {
    let (_cmd, temp) = common::xv_isolated_local();
    set_cred_login_record(temp.path());

    let marker = temp.path().join("run_primary_marker.txt");
    let out = xv_same_env(temp.path())
        .args([
            "run",
            "--",
            "sh",
            "-c",
            &format!("printf '%s' \"$CRED\" > '{}'", marker.display()),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let injected = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(injected, "hunter2", "stderr: {}", common::stderr_str(&out));
}

// ---------------------------------------------------------------------------
// Bugbot round 2 on #321 Phase C: `xv run`/`xv inject` must resolve record
// types LAZILY (only when a fetched secret is actually a record), not
// eagerly before fetching anything. An all-untyped selection must succeed
// even with a broken `[types.*]` config block that no referenced secret
// actually uses; a typed record under the same broken config must still
// surface the resolution error, since resolving types is fail-closed for
// the whole config (Task 13 docs).
// ---------------------------------------------------------------------------

/// A `.xv.toml` with a `[types.*]` block with two primaries — invalid per
/// `RecordType::validate()` (exactly one `primary` per type) — so any
/// command that actually needs to resolve types under this config fails.
/// Carries a minimal `default_env` pointing at the same "default" vault the
/// global `xv.conf` already uses, purely so vault resolution doesn't itself
/// error on "no env selected" now that the file has env profiles at all —
/// unrelated to the type-resolution behavior under test.
const BROKEN_TYPES_TOML: &str = r#"
default_env = "default"

[env.default]
vault = "default"

[types.bad]
fields = [
  { name = "a", kind = "secret", primary = true },
  { name = "b", kind = "secret", primary = true },
]
"#;

/// An `xv` command using ONLY the global (valid, no custom types) config
/// written by `xv_isolated_local_with_profile` — cwd is the temp root,
/// outside the `project/` subdirectory that holds the broken `.xv.toml`, so
/// project-config discovery finds nothing and only built-in record types
/// resolve. Used to create a typed record before switching to the
/// broken-config project directory to exercise `run`/`inject` against it.
fn xv_temp_root_env(temp: &std::path::Path) -> std::process::Command {
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

/// The same isolated store as `xv_isolated_local_with_profile`'s harness,
/// but a fresh `Command` with cwd in the `project/` subdirectory so the
/// (deliberately broken) `.xv.toml` written there is discovered.
fn xv_project_env(temp: &std::path::Path) -> std::process::Command {
    let mut cmd = common::xv();
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp)
        .env("XDG_CONFIG_HOME", temp.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(temp.join("project"));
    cmd
}

#[test]
fn run_untyped_secrets_unaffected_by_broken_types_config() {
    let (_cmd, temp) = common::xv_isolated_local_with_profile(BROKEN_TYPES_TOML);

    // Created from the temp root (no project config in scope), so this
    // plain `set` never touches record-type resolution at all.
    let out = xv_temp_root_env(temp.path())
        .args(["set", "plain-secret", "--value", "plain-value"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    // Run from the project directory, where the broken `[types.bad]` block
    // IS in scope. The only secret in the vault is untyped, so `xv run`
    // must succeed exactly as it would without the record-types feature.
    let marker = temp.path().join("run_untyped_marker.txt");
    let out2 = xv_project_env(temp.path())
        .args([
            "run",
            "--",
            "sh",
            "-c",
            &format!("printf '%s' \"$PLAIN_SECRET\" > '{}'", marker.display()),
        ])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "plain-value");
}

#[test]
fn inject_untyped_template_unaffected_by_broken_types_config() {
    let (_cmd, temp) = common::xv_isolated_local_with_profile(BROKEN_TYPES_TOML);

    let out = xv_temp_root_env(temp.path())
        .args(["set", "plain-secret", "--value", "plain-value"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let project_dir = temp.path().join("project");
    let template_path = project_dir.join("template.txt");
    std::fs::write(&template_path, "v: {{ secret:plain-secret }}\n").unwrap();
    let out_path = project_dir.join("out.txt");

    let out2 = xv_project_env(temp.path())
        .args([
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let rendered = std::fs::read_to_string(&out_path).unwrap();
    assert!(rendered.contains("plain-value"), "rendered: {rendered}");
}

#[test]
fn run_and_inject_typed_record_fails_under_broken_types_config() {
    let (_cmd, temp) = common::xv_isolated_local_with_profile(BROKEN_TYPES_TOML);

    // Created from the temp root: only built-in types are in scope there,
    // so creating a `login` record succeeds even though the project's
    // `.xv.toml` (not in scope here) has a broken custom type.
    let out = xv_temp_root_env(temp.path())
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

    // From the project directory the broken `[types.bad]` block IS in
    // scope. `cred` is a record, so both `run` and `inject` must now
    // actually attempt type resolution and fail — resolving types is
    // fail-closed for the whole config, so the broken custom block breaks
    // resolution even though `cred`'s own type (`login`) is a valid
    // built-in.
    let out2 = xv_project_env(temp.path())
        .args(["run", "--", "sh", "-c", "true"])
        .output()
        .unwrap();
    assert_eq!(
        out2.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let project_dir = temp.path().join("project");
    let template_path = project_dir.join("template.txt");
    std::fs::write(&template_path, "v: {{ secret:cred }}\n").unwrap();
    let out_path = project_dir.join("out.txt");
    let out3 = xv_project_env(temp.path())
        .args([
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        out3.status.code(),
        Some(3),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert!(
        !out_path.exists(),
        "output file must NOT be created when type resolution fails"
    );
}

// ---------------------------------------------------------------------------
// #330: bare-value `update`/`--stdin`/`rotate` set a record's primary field
// instead of corrupting the envelope.
// ---------------------------------------------------------------------------

/// `xv update <record> <value>` (positional) on a typed record: the primary
/// field changes inside the envelope, every other envelope field and every
/// metadata field/tag/groups/note/folder stays intact, and `get --record`
/// still parses. On unpatched code this instead overwrote the whole value
/// with the bare string while `content_type` stayed
/// `application/vnd.xv.record`, so `get --record` failed
/// `xv-config-invalid` afterwards — this test fails on that code.
#[test]
fn update_positional_value_on_record_sets_primary_field() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "prod-db",
            "--type",
            "database",
            "--field",
            "host=h",
            "--field",
            "username=u",
            "--value",
            "pw1",
            "--group",
            "prod",
            "--note",
            "primary db",
            "--folder",
            "app/db",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let before = read_local_meta(temp.path(), "default", "prod-db");
    let version_before = before["version"].clone();

    let out2 = xv_same_env(temp.path())
        .args(["update", "prod-db", "pw2"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let after = read_local_meta(temp.path(), "default", "prod-db");
    assert_ne!(after["version"], version_before);
    assert_eq!(after["content_type"], "application/vnd.xv.record");
    assert_eq!(after["tags"]["xv-type"], "database");
    assert_eq!(after["tags"]["f.host"], "h");
    assert_eq!(after["tags"]["f.username"], "u");
    assert_eq!(after["groups"], serde_json::json!(["prod"]));
    assert_eq!(after["note"], "primary db");
    assert_eq!(after["folder"], "app/db");

    // Plain `get --raw` returns the new primary.
    let out3 = xv_same_env(temp.path())
        .args(["get", "prod-db", "--raw"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert_eq!(common::stdout_str(&out3), "pw2");

    // `get --record` still parses and shows the non-primary fields intact.
    let out4 = xv_same_env(temp.path())
        .args(["get", "prod-db", "--record", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        out4.status.success(),
        "stderr: {}",
        common::stderr_str(&out4)
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&common::stdout_str(&out4)).expect("valid json");
    assert_eq!(parsed["fields"]["password"], "pw2");
    assert_eq!(parsed["fields"]["host"], "h");
    assert_eq!(parsed["fields"]["username"], "u");
}

/// Same as `update_positional_value_on_record_sets_primary_field` but via
/// `--stdin`.
#[test]
fn update_stdin_value_on_record_sets_primary_field() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "prod-db",
            "--type",
            "database",
            "--field",
            "host=h",
            "--field",
            "username=u",
            "--value",
            "pw1",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let before = read_local_meta(temp.path(), "default", "prod-db");
    let version_before = before["version"].clone();

    let mut child = xv_same_env(temp.path())
        .args(["update", "prod-db", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"pw2-stdin").unwrap();
    let out2 = child.wait_with_output().unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let after = read_local_meta(temp.path(), "default", "prod-db");
    assert_ne!(after["version"], version_before);
    assert_eq!(after["content_type"], "application/vnd.xv.record");
    assert_eq!(after["tags"]["f.host"], "h");
    assert_eq!(after["tags"]["f.username"], "u");

    let out3 = xv_same_env(temp.path())
        .args(["get", "prod-db", "--raw"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert_eq!(common::stdout_str(&out3), "pw2-stdin");
}

/// `xv rotate <record>`: the generated value becomes the new primary,
/// non-primary fields/tags stay intact, and `--show-value` prints the
/// generated value. On unpatched code `rotate` called `set_secret` with the
/// bare generated string as the value while leaving `content_type` as
/// `application/vnd.xv.record` — the same corruption as bare `update`.
#[test]
fn rotate_on_record_sets_primary_field() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args([
            "set",
            "prod-db",
            "--type",
            "database",
            "--field",
            "host=h",
            "--field",
            "username=u",
            "--value",
            "pw1",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));
    let before = read_local_meta(temp.path(), "default", "prod-db");
    let version_before = before["version"].clone();

    let out2 = xv_same_env(temp.path())
        .args(["rotate", "prod-db", "--force", "--show-value"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stdout = common::stdout_str(&out2);
    let generated = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Generated value: "))
        .expect("printed generated value")
        .trim()
        .to_string();
    assert_ne!(generated, "pw1");

    let after = read_local_meta(temp.path(), "default", "prod-db");
    assert_ne!(after["version"], version_before);
    assert_eq!(after["content_type"], "application/vnd.xv.record");
    assert_eq!(after["tags"]["xv-type"], "database");
    assert_eq!(after["tags"]["f.host"], "h");
    assert_eq!(after["tags"]["f.username"], "u");

    let out3 = xv_same_env(temp.path())
        .args(["get", "prod-db", "--raw"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert_eq!(common::stdout_str(&out3), generated);
}

/// A corrupt-envelope record + bare-value `update`: must fail loud (same
/// error as plain `get`) and write nothing — the corrupt value is not
/// silently replaced, and the envelope is not touched.
#[test]
fn update_positional_value_on_corrupt_envelope_fails_loud_without_writing() {
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
    let before = read_local_meta(temp.path(), "default", "cred");
    let version_before = before["version"].clone();

    let out2 = xv_same_env(temp.path())
        .args(["update", "cred", "pw2"])
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

    // Nothing was written: version and raw value are untouched.
    let after = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(after["version"], version_before);
}

/// A record with an unknown `xv-type` + bare-value `update`: the primary is
/// unknowable, so this must error with guidance rather than guess or
/// silently corrupt the envelope by writing the bare value under a field
/// name that might not even exist.
#[test]
fn update_positional_value_on_unknown_type_record_errors_without_writing() {
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
    let before = read_local_meta(temp.path(), "default", "cred");
    let version_before = before["version"].clone();

    let out2 = xv_same_env(temp.path())
        .args(["update", "cred", "pw2"])
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
    assert!(stderr.contains("nosuch"), "stderr: {stderr}");

    let after = read_local_meta(temp.path(), "default", "cred");
    assert_eq!(after["version"], version_before);
}

/// Guard: positional-value `update` on an untyped secret is unaffected —
/// byte-identical to pre-#330 behavior (plain overwrite, no envelope
/// machinery involved).
#[test]
fn update_positional_value_on_untyped_secret_unchanged() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "plain", "--value", "v1"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["update", "plain", "v2"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let after = read_local_meta(temp.path(), "default", "plain");
    assert!(after["content_type"].is_null() || after["content_type"] == "");

    let out3 = xv_same_env(temp.path())
        .args(["get", "plain", "--raw"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert_eq!(common::stdout_str(&out3), "v2");
}

/// Guard: `--stdin` update on an untyped secret is unaffected.
#[test]
fn update_stdin_value_on_untyped_secret_unchanged() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "plain", "--value", "v1"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let mut child = xv_same_env(temp.path())
        .args(["update", "plain", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"v2-stdin").unwrap();
    let out2 = child.wait_with_output().unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );

    let out3 = xv_same_env(temp.path())
        .args(["get", "plain", "--raw"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert_eq!(common::stdout_str(&out3), "v2-stdin");
}

/// Guard: `rotate` on an untyped secret is unaffected — still the classic
/// `set_secret` overwrite path.
#[test]
fn rotate_on_untyped_secret_unchanged() {
    let (mut cmd, temp) = common::xv_isolated_local();
    let out = cmd
        .args(["set", "plain", "--value", "v1"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", common::stderr_str(&out));

    let out2 = xv_same_env(temp.path())
        .args(["rotate", "plain", "--force", "--show-value"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "stderr: {}",
        common::stderr_str(&out2)
    );
    let stdout = common::stdout_str(&out2);
    let generated = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Generated value: "))
        .expect("printed generated value")
        .trim()
        .to_string();
    assert_ne!(generated, "v1");

    let after = read_local_meta(temp.path(), "default", "plain");
    assert!(after["content_type"].is_null() || after["content_type"] == "");

    let out3 = xv_same_env(temp.path())
        .args(["get", "plain", "--raw"])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "stderr: {}",
        common::stderr_str(&out3)
    );
    assert_eq!(common::stdout_str(&out3), generated);
}
