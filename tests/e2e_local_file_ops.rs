//! CLI-level behavior-lock tests for `xv file` on the **local** backend.
//! These exercise the whole binary end-to-end against a fully hermetic,
//! isolated local (age-encrypted file) store — no Azure/AWS credentials or
//! network.
//!
//! Scope (distinct from the trait-level `local_backend_integration.rs`): these
//! are CLI locks for `xv file` dispatching through the workspace default
//! entry's `Backend::files()`. They cover:
//!   1. the upload/list/info/download/delete round-trip,
//!   2. file ops landing in the workspace default entry's vault (and the
//!      degenerate no-workspace case targeting the configured default vault),
//!   3. `xv file sync` up/down on local, and
//!   4. the capability gate when a backend has no file storage, plus the
//!      AWS-`sync` resolved-kind gate (aws feature).

#![cfg(feature = "file-ops")]

mod common;

use common::xv;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// Isolated local-backend environment that also exposes the on-disk store so
/// tests can assert *where* files land (`store/vaults/<vault>/files/`).
struct FileEnv {
    temp: TempDir,
}

impl FileEnv {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let cfg_dir = temp.path().join(".config").join("xv");
        std::fs::create_dir_all(&cfg_dir).expect("config dir");
        std::fs::create_dir_all(temp.path().join("store")).expect("store dir");
        std::fs::write(cfg_dir.join("xv.conf"), local_config(temp.path())).expect("write config");
        Self { temp }
    }

    fn path(&self) -> &Path {
        self.temp.path()
    }

    /// `store/vaults/<vault>/files/` for on-disk placement assertions.
    fn files_dir(&self, vault: &str) -> PathBuf {
        self.temp
            .path()
            .join("store")
            .join("vaults")
            .join(vault)
            .join("files")
    }

    fn cmd(&self) -> Command {
        let mut c = xv();
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join(".config"))
            .env("XV_NO_PARENT_CONFIG", "1")
            .env("XV_BACKEND", "local")
            .env("NO_COLOR", "1")
            .current_dir(self.path());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().expect("spawn xv")
    }

    /// Run and assert success, returning stdout as a String.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "expected success for {args:?}\n--stdout--\n{}\n--stderr--\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// The full `xv.conf` `xv_isolated_local` writes, rooted at `root`.
fn local_config(root: &Path) -> String {
    format!(
        r#"backend = "local"
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
store_path = "{store}"
key_file = "{key}"
default_vault = "default"
"#,
        store = common::toml_path(root.join("store")),
        key = common::toml_path(root.join("key.txt")),
    )
}

/// Count `*.age` payload files in a vault's files dir (0 if the dir is absent).
/// Robust against the on-disk name encoding — we assert *how many* files a
/// vault holds, not their encoded stems.
fn count_age_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("age"))
                .count()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 1. Round-trip: upload -> list -> info -> download -> delete -> list
// ---------------------------------------------------------------------------

#[test]
fn file_roundtrip_upload_list_info_download_delete_on_local() {
    let env = FileEnv::new();
    let payload: &[u8] = b"hello-file-content\nsecond line\n";
    std::fs::write(env.path().join("payload.bin"), payload).unwrap();

    // upload
    let up = env.run(&["file", "upload", "payload.bin", "--name", "greeting.txt"]);
    let progress = String::from_utf8_lossy(&up.stderr);
    assert!(up.status.success(), "upload failed: {progress}");
    assert!(
        progress.contains("greeting.txt"),
        "upload progress: {progress}"
    );
    // lands on disk under the default vault
    assert_eq!(
        count_age_files(&env.files_dir("default")),
        1,
        "exactly one payload file should exist after upload"
    );

    // list (flat + recursive) surfaces the file
    assert!(
        env.ok(&["file", "list"]).contains("greeting.txt"),
        "list should show the uploaded file"
    );
    assert!(
        env.ok(&["file", "list", "--recursive"])
            .contains("greeting.txt"),
        "recursive list should show the uploaded file"
    );

    // info reports name + size
    let info = env.ok(&["file", "info", "greeting.txt"]);
    assert!(info.contains("greeting.txt"), "info: {info}");
    assert!(
        info.contains(&payload.len().to_string()),
        "info should report the byte size {}: {info}",
        payload.len()
    );

    // download and verify byte-equality
    env.ok(&["file", "download", "greeting.txt", "-o", "out.bin"]);
    let got = std::fs::read(env.path().join("out.bin")).expect("downloaded file");
    assert_eq!(
        got, payload,
        "downloaded bytes must equal the uploaded bytes"
    );

    // delete, then the listing is empty and the payload is gone from disk
    env.ok(&["file", "delete", "greeting.txt", "--force"]);
    assert!(
        !env.ok(&["file", "list"]).contains("greeting.txt"),
        "deleted file must not appear in the listing"
    );
    assert_eq!(
        count_age_files(&env.files_dir("default")),
        0,
        "payload file should be removed from disk after delete"
    );
}

#[cfg(unix)]
#[test]
fn single_download_rejects_symlink_destination() {
    use std::os::unix::fs::symlink;

    let env = FileEnv::new();
    std::fs::write(env.path().join("payload.bin"), b"remote-content").unwrap();
    env.ok(&["file", "upload", "payload.bin", "--name", "remote.txt"]);

    let outside = env.path().join("outside.txt");
    std::fs::write(&outside, b"outside-original").unwrap();
    let destination = env.path().join("download.txt");
    symlink(&outside, &destination).unwrap();

    let out = env.run(&[
        "file",
        "download",
        "remote.txt",
        "--output",
        destination.to_str().unwrap(),
        "--force",
    ]);

    assert!(
        !out.status.success(),
        "symlink destination must be rejected"
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside-original");
}

#[cfg(unix)]
#[test]
fn recursive_download_rejects_symlink_parent_component() {
    use std::os::unix::fs::symlink;

    let env = FileEnv::new();
    std::fs::create_dir_all(env.path().join("source")).unwrap();
    std::fs::write(env.path().join("source/file.txt"), b"remote-content").unwrap();
    env.ok(&[
        "file",
        "upload",
        "source/file.txt",
        "--name",
        "nested/file.txt",
    ]);

    let output = env.path().join("downloads");
    let outside = env.path().join("outside");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, output.join("nested")).unwrap();

    let out = env.run(&[
        "file",
        "download",
        "nested",
        "--recursive",
        "--output",
        output.to_str().unwrap(),
        "--force",
    ]);

    assert!(!out.status.success(), "symlinked parent must be rejected");
    assert!(!outside.join("file.txt").exists());
}

// ---------------------------------------------------------------------------
// 2. Default-entry targeting
// ---------------------------------------------------------------------------

#[test]
fn files_land_in_workspace_default_entry_vault() {
    let env = FileEnv::new();
    // A second local vault, made the workspace default via `cx add --default`.
    env.ok(&["vault", "create", "project"]);
    env.ok(&["cx", "add", "project", "--backend", "local", "--default"]);

    std::fs::write(env.path().join("f.txt"), b"in-workspace").unwrap();
    env.ok(&["file", "upload", "f.txt", "--name", "inws.txt"]);

    // The upload targets the default *entry's* vault (project), not the
    // config's default_vault.
    assert_eq!(
        count_age_files(&env.files_dir("project")),
        1,
        "file should land in the workspace default entry's vault"
    );
    assert_eq!(
        count_age_files(&env.files_dir("default")),
        0,
        "file must NOT land in the config default vault when a workspace default is set"
    );
}

#[test]
fn files_land_in_default_vault_without_workspace() {
    let env = FileEnv::new();
    std::fs::write(env.path().join("f.txt"), b"degenerate").unwrap();
    env.ok(&["file", "upload", "f.txt", "--name", "solo.txt"]);

    // Degenerate workspace-of-one: the configured default vault is the target.
    assert_eq!(
        count_age_files(&env.files_dir("default")),
        1,
        "with no workspace, files target the configured default vault"
    );
}

// ---------------------------------------------------------------------------
// 2b. The 10-tag cap is Azure-only
// ---------------------------------------------------------------------------

/// The 10-tag limit is an Azure Blob index-tag constraint and must not apply to
/// other backends. A local upload with more than 10 tags succeeds — guarding
/// against a refactor silently re-globalizing the Azure cap in the now-shared
/// upload handler.
#[test]
fn local_upload_accepts_more_than_ten_tags() {
    let env = FileEnv::new();
    std::fs::write(env.path().join("f.txt"), b"tagged").unwrap();

    let mut args = vec!["file", "upload", "f.txt", "--name", "tagged.txt"];
    // 11 tags — one past the Azure limit.
    for t in [
        "a=1", "b=2", "c=3", "d=4", "e=5", "f=6", "g=7", "h=8", "i=9", "j=10", "k=11",
    ] {
        args.push("-t");
        args.push(t);
    }
    env.ok(&args);

    assert_eq!(
        count_age_files(&env.files_dir("default")),
        1,
        "local backend must accept >10 tags (the 10-tag cap is Azure-only)"
    );
}

// ---------------------------------------------------------------------------
// 3. Sync up/down round-trip
// ---------------------------------------------------------------------------

#[test]
fn file_sync_up_down_roundtrip_on_local() {
    let env = FileEnv::new();
    let data = env.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("a.txt"), b"alpha-payload").unwrap();
    std::fs::write(data.join("b.txt"), b"beta-payload").unwrap();

    // sync up
    env.ok(&["file", "sync", "data", "--direction", "up"]);
    let listed = env.ok(&["file", "list", "--recursive"]);
    assert!(
        listed.contains("data/a.txt"),
        "sync-up should push a.txt: {listed}"
    );
    assert!(
        listed.contains("data/b.txt"),
        "sync-up should push b.txt: {listed}"
    );

    // wipe local, then sync down reconstructs both files byte-for-byte
    std::fs::remove_file(data.join("a.txt")).unwrap();
    std::fs::remove_file(data.join("b.txt")).unwrap();
    assert!(
        std::fs::read_dir(&data).unwrap().next().is_none(),
        "local data cleared"
    );

    env.ok(&["file", "sync", "data", "--direction", "down"]);
    assert_eq!(
        std::fs::read(data.join("a.txt")).unwrap(),
        b"alpha-payload",
        "sync-down should restore a.txt content"
    );
    assert_eq!(
        std::fs::read(data.join("b.txt")).unwrap(),
        b"beta-payload",
        "sync-down should restore b.txt content"
    );
}

#[cfg(unix)]
#[test]
fn file_sync_down_rejects_symlink_parent_component() {
    use std::os::unix::fs::symlink;

    let env = FileEnv::new();
    let data = env.path().join("data");
    std::fs::create_dir_all(data.join("nested")).unwrap();
    std::fs::write(data.join("nested/file.txt"), b"remote-content").unwrap();
    env.ok(&["file", "sync", "data", "--direction", "up"]);

    std::fs::remove_file(data.join("nested/file.txt")).unwrap();
    std::fs::remove_dir(data.join("nested")).unwrap();
    let outside = env.path().join("outside-sync");
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, data.join("nested")).unwrap();

    let out = env.run(&["file", "sync", "data", "--direction", "down"]);

    assert!(
        !out.status.success(),
        "sync-down symlinked parent must be rejected"
    );
    assert!(!outside.join("file.txt").exists());
}

#[test]
fn file_sync_dry_run_uploads_nothing() {
    let env = FileEnv::new();
    let data = env.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("c.txt"), b"gamma").unwrap();

    env.ok(&["file", "sync", "data", "--direction", "up", "--dry-run"]);

    // Nothing was actually uploaded.
    assert!(
        !env.ok(&["file", "list", "--recursive"])
            .contains("data/c.txt"),
        "--dry-run must not upload anything"
    );
    assert_eq!(
        count_age_files(&env.files_dir("default")),
        0,
        "--dry-run must not write any payload to the store"
    );
}

// ---------------------------------------------------------------------------
// 4. Capability gate + AWS sync gate
// ---------------------------------------------------------------------------

/// A backend with no file storage configured must fail `xv file` with the
/// actionable capability-gate message and the stable `InvalidArgument` exit
/// code (2) — never a panic, silent success, or network round-trip. Uses the
/// Azure backend with an empty `storage_account`, resolved fully offline.
#[test]
fn file_ops_capability_gated_when_no_file_storage() {
    let temp = tempfile::tempdir().unwrap();
    let cfg_dir = temp.path().join(".config").join("xv");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let config = r#"backend = "azure"
debug = false
subscription_id = "00000000-0000-0000-0000-000000000000"
default_vault = "testvault"
default_resource_group = "rg"
default_location = "eastus"
tenant_id = "00000000-0000-0000-0000-000000000000"
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0
"#;
    std::fs::write(cfg_dir.join("xv.conf"), config).unwrap();

    let mut c = xv();
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("NO_COLOR", "1")
        .current_dir(temp.path());
    let out = c.args(["file", "list"]).output().expect("spawn xv");

    assert_eq!(
        out.status.code(),
        Some(2),
        "capability gate must exit 2 (InvalidArgument)\nstdout:{}\nstderr:{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no file storage configured"),
        "expected capability-gate message, got: {stderr}"
    );
    assert!(
        stderr.contains("storage account"),
        "expected the actionable Azure hint, got: {stderr}"
    );
}

/// `xv file sync` stays blocked on the AWS backend via a gate on the *resolved*
/// backend kind (not a probe), with the original error text and exit code 2.
/// Fully offline: the gate fires before any S3 call, so a fake bucket + creds
/// suffice.
#[cfg(feature = "aws")]
#[test]
fn aws_file_sync_gated_on_resolved_kind() {
    let temp = tempfile::tempdir().unwrap();
    let cfg_dir = temp.path().join(".config").join("xv");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::create_dir_all(temp.path().join("syncdir")).unwrap();
    std::fs::write(temp.path().join("syncdir").join("a.txt"), b"x").unwrap();
    let config = r#"backend = "aws"
debug = false
subscription_id = ""
default_vault = "testvault"
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0

[aws]
region = "us-east-1"
s3_bucket = "fake-bucket-xyz"
default_vault = "testvault"
"#;
    std::fs::write(cfg_dir.join("xv.conf"), config).unwrap();

    let mut c = xv();
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("NO_COLOR", "1")
        .env("AWS_ACCESS_KEY_ID", "fake")
        .env("AWS_SECRET_ACCESS_KEY", "fake")
        .env("AWS_REGION", "us-east-1")
        .current_dir(temp.path());
    let out = c
        .args(["file", "sync", "syncdir", "--direction", "up"])
        .output()
        .expect("spawn xv");

    assert_eq!(
        out.status.code(),
        Some(2),
        "AWS sync gate must exit 2\nstdout:{}\nstderr:{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not yet supported on the AWS backend"),
        "expected the AWS sync gate message, got: {stderr}"
    );
}

#[test]
fn attachment_integrity_failure_has_structured_cli_error_and_no_plaintext_output() {
    let env = FileEnv::new();
    std::fs::write(env.path().join("payload.bin"), b"DO-NOT-RETURN-PAYLOAD").unwrap();
    env.ok(&["file", "upload", "payload.bin", "--name", "managed.txt"]);
    let metadata_path = std::fs::read_dir(env.files_dir("default"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with(".meta.json"))
        .unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
    metadata["metadata"]["xv_encrypted"] = serde_json::json!("age");
    std::fs::write(metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    for format in ["json", "plain"] {
        let output = env.run(&[
            "file",
            "download",
            "managed.txt",
            "-o",
            "result.txt",
            "--format",
            format,
        ]);
        assert_eq!(output.status.code(), Some(2));
        assert!(!env.path().join("result.txt").exists());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stdout.contains("DO-NOT-RETURN-PAYLOAD"));
        assert!(!stderr.contains("DO-NOT-RETURN-PAYLOAD"));
        if format == "json" {
            let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(body["error"]["code"], "xv-attachment-not-ciphertext");
            assert_eq!(body["error"]["exit_code"], 2);
        } else {
            assert!(stdout.is_empty());
            assert!(stderr.contains("xv-attachment-not-ciphertext"));
        }
    }
}

#[test]
fn attachment_upload_failure_keeps_cli_json_free_of_progress_text() {
    use crosstache::backend::{local::LocalBackend, Backend};
    use crosstache::config::settings::LocalConfig;
    use crosstache::secret::manager::SecretRequest;
    let env = FileEnv::new();
    std::fs::write(env.path().join("payload.bin"), b"PRIVATE-UPLOAD-CONTENT").unwrap();
    let backend = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(env.path().join("store").display().to_string()),
        key_file: Some(env.path().join("key.txt").display().to_string()),
        default_vault: Some("default".into()),
        ..Default::default()
    }))
    .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        backend
            .attachment_keys()
            .set_secret(
                "default",
                SecretRequest {
                    name: "xv-attachment-key".into(),
                    value: zeroize::Zeroizing::new("INVALID-POINTER-CONTENT".into()),
                    content_type: None,
                    enabled: None,
                    expires_on: None,
                    not_before: None,
                    tags: None,
                    groups: None,
                    note: None,
                    folder: None,
                },
            )
            .await
            .unwrap();
    });
    let output = env.run(&[
        "file",
        "upload",
        "payload.bin",
        "--encrypt",
        "--format",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let body: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("only JSON belongs on stdout");
    assert_eq!(body["error"]["code"], "xv-attachment-pointer-invalid");
    assert_eq!(count_age_files(&env.files_dir("default")), 0);
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("PRIVATE-UPLOAD-CONTENT"));
        assert!(!text.contains("INVALID-POINTER-CONTENT"));
    }
}

#[test]
fn attachment_key_observations_are_structured_read_only_and_vault_scoped() {
    let env = FileEnv::new();
    let status = || -> serde_json::Value {
        serde_json::from_str(&env.ok(&["attachment-key", "status", "--format", "json"])).unwrap()
    };
    let empty = status();
    assert_eq!(empty["backend"], "local");
    assert_eq!(empty["vault"], "default");
    assert_eq!(empty["report"]["mode"], "absent");

    std::fs::write(
        env.path().join("private.bin"),
        b"NEVER-PRINT-INVENTORY-PAYLOAD",
    )
    .unwrap();
    env.ok(&[
        "file",
        "upload",
        "private.bin",
        "--name",
        "encrypted.bin",
        "--encrypt",
    ]);
    env.ok(&["file", "upload", "private.bin", "--name", "ordinary.bin"]);
    let active = status();
    assert_eq!(active["report"]["mode"], "v2");
    assert!(active["report"]["problem_code"].is_null());
    assert!(active["report"]["active_key_id"]
        .as_str()
        .unwrap()
        .starts_with("ak1-"));
    let inventory = env.ok(&["attachment-key", "inventory", "--format", "json"]);
    assert!(!inventory.contains("NEVER-PRINT-INVENTORY-PAYLOAD"));
    assert!(!inventory.contains("AGE-SECRET-KEY"));
    let inventory: serde_json::Value = serde_json::from_str(&inventory).unwrap();
    assert_eq!(inventory["report"]["observation"], "metadata_only");
    let rows = inventory["report"]["files"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "encrypted.bin");
    assert_eq!(rows[0]["classification"], "schema1");
    assert_eq!(rows[0]["key_id"], active["report"]["active_key_id"]);
    assert_eq!(
        rows[0]["provider_version"],
        active["report"]["active_version"]
    );
    assert_eq!(rows[1]["classification"], "unmanaged");
    assert_eq!(
        status(),
        active,
        "observations must not rotate or rewrite the pointer"
    );
    let yaml = env.ok(&["attachment-key", "status", "--format", "yaml"]);
    let yaml: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(yaml["report"]["mode"].as_str(), Some("v2"));

    env.ok(&["vault", "create", "elsewhere"]);
    let other: serde_json::Value = serde_json::from_str(&env.ok(&[
        "attachment-key",
        "status",
        "--vault",
        "elsewhere",
        "--format",
        "json",
    ]))
    .unwrap();
    assert_eq!(other["vault"], "elsewhere");
    assert_eq!(other["report"]["mode"], "absent");
    let csv = env.run(&["attachment-key", "inventory", "--format", "csv"]);
    assert_eq!(csv.status.code(), Some(2));
}

#[test]
fn attachment_key_inventory_uses_workspace_default_entry() {
    let env = FileEnv::new();
    env.ok(&["vault", "create", "project"]);
    env.ok(&[
        "cx",
        "add",
        "project",
        "--backend",
        "local",
        "--as",
        "project-alias",
        "--default",
    ]);
    std::fs::write(env.path().join("file.bin"), b"workspace-payload").unwrap();
    env.ok(&["file", "upload", "file.bin", "--encrypt"]);
    for command in ["status", "inventory"] {
        let body: serde_json::Value =
            serde_json::from_str(&env.ok(&["attachment-key", command, "--format", "json"]))
                .unwrap();
        let alias: serde_json::Value = serde_json::from_str(&env.ok(&[
            "attachment-key",
            command,
            "--vault",
            "project-alias",
            "--format",
            "json",
        ]))
        .unwrap();
        assert_eq!(
            alias, body,
            "explicit alias must select the same backend/vault as default"
        );
        assert_eq!(body["backend"], "local");
        assert_eq!(body["vault"], "project");
        if command == "status" {
            assert_eq!(body["report"]["mode"], "v2");
        } else {
            assert_eq!(body["report"]["files"].as_array().unwrap().len(), 1);
            assert_eq!(body["report"]["files"][0]["classification"], "schema1");
        }
    }
}

#[test]
fn attachment_key_status_reports_broken_pointer_without_exposing_or_replacing_it() {
    use crosstache::backend::{local::LocalBackend, Backend};
    use crosstache::config::settings::LocalConfig;
    use crosstache::secret::manager::SecretRequest;
    let env = FileEnv::new();
    let backend = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(env.path().join("store").display().to_string()),
        key_file: Some(env.path().join("key.txt").display().to_string()),
        default_vault: Some("default".into()),
        ..Default::default()
    }))
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let original = runtime
        .block_on(backend.attachment_keys().set_secret(
            "default",
            SecretRequest {
                name: "xv-attachment-key".into(),
                value: zeroize::Zeroizing::new("PRIVATE-BROKEN-POINTER".into()),
                content_type: None,
                enabled: None,
                expires_on: None,
                not_before: None,
                tags: None,
                groups: None,
                note: None,
                folder: None,
            },
        ))
        .unwrap();
    for format in ["json", "yaml", "plain"] {
        let output = env.ok(&["attachment-key", "status", "--format", format]);
        assert!(output.contains("xv-attachment-pointer-invalid"));
        assert!(!output.contains("PRIVATE-BROKEN-POINTER"));
        if format == "json" {
            let body: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(body["report"]["mode"], "invalid");
        }
    }
    // File-reference inventory must not depend on a readable/valid key.
    let inventory: serde_json::Value =
        serde_json::from_str(&env.ok(&["attachment-key", "inventory", "--format", "json"]))
            .unwrap();
    assert!(inventory["report"]["files"].as_array().unwrap().is_empty());
    let after = runtime
        .block_on(
            backend
                .attachment_keys()
                .get_secret("default", "xv-attachment-key", true),
        )
        .unwrap();
    assert_eq!(after.version, original.version);
    assert_eq!(after.value.unwrap().as_str(), "PRIVATE-BROKEN-POINTER");
}

#[test]
fn attachment_key_lifecycle_cli_previews_applies_and_recovers_without_losing_files() {
    use age::secrecy::ExposeSecret;
    use crosstache::backend::{local::LocalBackend, Backend};
    use crosstache::config::settings::LocalConfig;
    use crosstache::secret::{attachment_key as key, manager::SecretRequest};
    let env = FileEnv::new();
    let backend = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(env.path().join("store").display().to_string()),
        key_file: Some(env.path().join("key.txt").display().to_string()),
        default_vault: Some("default".into()),
        ..Default::default()
    }))
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let identity = age::x25519::Identity::generate();
    let id = key::AttachmentKeyId::derive(&identity.to_public().to_string());
    let req = |value: &str| SecretRequest {
        name: key::ACTIVE_POINTER_SECRET.into(),
        value: zeroize::Zeroizing::new(value.into()),
        content_type: None,
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    };
    runtime
        .block_on(
            backend
                .attachment_keys()
                .set_secret("default", req(identity.to_string().expose_secret())),
        )
        .unwrap();
    let json = |args: &[&str]| -> serde_json::Value {
        let text = env.ok(args);
        assert!(!text.contains("AGE-SECRET-KEY"));
        assert!(!text.contains("PRIVATE-LIFECYCLE-POINTER"));
        serde_json::from_str(&text).unwrap()
    };
    std::fs::write(env.path().join("old.bin"), b"pinned-before-upgrade").unwrap();
    env.ok(&["file", "upload", "old.bin", "--encrypt"]);
    assert_eq!(
        json(&["attachment-key", "keys", "--format", "json"])["report"]["keys"],
        serde_json::json!([])
    );
    assert_eq!(
        json(&["attachment-key", "upgrade", "--format", "json"])["report"]["outcome"],
        "ready"
    );
    assert_eq!(
        json(&["attachment-key", "status", "--format", "json"])["report"]["mode"],
        "v1"
    );
    assert_eq!(
        env.run(&["attachment-key", "upgrade", "--apply"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        json(&[
            "attachment-key",
            "upgrade",
            "--apply",
            "--offline",
            "--format",
            "json"
        ])["report"]["outcome"],
        "applied"
    );
    let enumerated = json(&["attachment-key", "keys", "--format", "json"]);
    assert_eq!(
        enumerated["report"]["observation"],
        "visible_retained_records"
    );
    assert_eq!(enumerated["report"]["keys"].as_array().unwrap().len(), 1);
    assert_eq!(enumerated["report"]["keys"][0]["key_id"], id.as_str());
    env.ok(&["file", "download", "old.bin", "-o", "after-upgrade.bin"]);
    assert_eq!(
        std::fs::read(env.path().join("after-upgrade.bin")).unwrap(),
        b"pinned-before-upgrade"
    );

    runtime
        .block_on(
            backend
                .attachment_keys()
                .set_secret("default", req("PRIVATE-LIFECYCLE-POINTER")),
        )
        .unwrap();
    let base = [
        "attachment-key",
        "recover",
        "--key-id",
        id.as_str(),
        "--legacy-key-id",
        id.as_str(),
        "--format",
        "json",
    ];
    assert_eq!(json(&base)["report"]["outcome"], "ready");
    assert_eq!(
        json(&["attachment-key", "status", "--format", "json"])["report"]["mode"],
        "invalid"
    );
    let mut apply = base.to_vec();
    apply.extend(["--apply", "--offline"]);
    assert_eq!(json(&apply)["report"]["outcome"], "applied");
    assert_eq!(json(&apply)["report"]["outcome"], "unchanged");
    env.ok(&["file", "download", "old.bin", "-o", "after-recovery.bin"]);
    assert_eq!(
        std::fs::read(env.path().join("after-recovery.bin")).unwrap(),
        b"pinned-before-upgrade"
    );
    assert_eq!(
        env.run(&["attachment-key", "recover", "--key-id", id.as_str()])
            .status
            .code(),
        Some(2)
    );
}
