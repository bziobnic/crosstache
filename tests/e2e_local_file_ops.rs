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
        store = root.join("store").display(),
        key = root.join("key.txt").display(),
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
    let up = env.ok(&["file", "upload", "payload.bin", "--name", "greeting.txt"]);
    assert!(up.contains("greeting.txt"), "upload output: {up}");
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
