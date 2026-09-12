//! Machine-output contract suite.
//!
//! Machine mode is `--format json|yaml|csv` given explicitly. In that mode
//! stdout must hold **exactly one** document for the whole run: either the
//! command's report, or the error envelope with the report attached under
//! `report`. Human narration (plan banners, per-item lines, summaries) stays
//! on stderr and is suppressed from stdout entirely.
//!
//! Isolation follows the `WorkspaceEnv`/`DisclosureEnv` pattern: `env_clear()`
//! plus an explicit allowlist (selective `env_remove()` leaks host vars into
//! the child), a private `HOME`/`XDG_CONFIG_HOME`, `XV_NO_PARENT_CONFIG=1` so
//! no ancestor `.xv.toml` is picked up, and a dedicated `XV_CACHE_DIR` so no
//! test touches the real OS cache.
//!
//! Later tasks in the machine-output contract append their commands here.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// Render a path for interpolation into a **double-quoted** TOML string.
/// TOML basic strings process backslash escapes, so an unescaped Windows
/// path makes the parser read `\U` as a unicode escape and reject the config.
fn toml_path(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// A hermetic single-local-backend environment. Vaults inside the one store
/// stand in for migration endpoints (`local:default` → `local:other`).
struct MachineEnv {
    _tmp: TempDir,
    home: PathBuf,
    config_dir: PathBuf,
    cache_dir: PathBuf,
}

impl MachineEnv {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().join("home");
        let config_dir = home.join(".config");
        let xv_dir = config_dir.join("xv");
        let store_dir = tmp.path().join("store");
        let cache_dir = tmp.path().join("cache");
        let key_file = tmp.path().join("key").join("key.txt");

        std::fs::create_dir_all(&xv_dir).expect("create config dir");
        std::fs::create_dir_all(&store_dir).expect("create store dir");
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");
        std::fs::create_dir_all(key_file.parent().unwrap()).expect("create key dir");

        let config_content = format!(
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
            store = toml_path(&store_dir),
            key = toml_path(&key_file),
        );
        std::fs::write(xv_dir.join("xv.conf"), config_content).expect("write config");

        Self {
            _tmp: tmp,
            home,
            config_dir,
            cache_dir,
        }
    }

    fn xv(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_xv"));
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_dir)
            .env("XV_NO_PARENT_CONFIG", "1")
            .env("XV_BACKEND", "local")
            .env("NO_COLOR", "1")
            .env("XV_CACHE_DIR", &self.cache_dir)
            .current_dir(&self.home);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.xv().args(args).output().expect("execute xv binary")
    }

    #[track_caller]
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "`xv {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// Assert stdout is exactly one JSON document and return it.
///
/// `serde_json::from_str` alone would be satisfied by the FIRST document of a
/// two-document stdout, which is precisely the bug this suite guards, so the
/// streaming deserializer is used to count values.
#[track_caller]
fn one_json_document(stdout: &str) -> serde_json::Value {
    let values: Vec<serde_json::Value> = serde_json::Deserializer::from_str(stdout)
        .into_iter::<serde_json::Value>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| panic!("stdout is not valid JSON ({e}):\n{stdout}"));
    assert_eq!(
        values.len(),
        1,
        "stdout must hold exactly one JSON document, found {}:\n{stdout}",
        values.len()
    );
    values.into_iter().next().expect("checked above")
}

/// Assert stdout is exactly one YAML document and return it.
#[track_caller]
fn one_yaml_document(stdout: &str) -> serde_json::Value {
    let values: Vec<serde_json::Value> = serde_yaml::Deserializer::from_str(stdout)
        .map(|doc| serde_json::Value::deserialize(doc).expect("yaml document"))
        .collect();
    assert_eq!(
        values.len(),
        1,
        "stdout must hold exactly one YAML document, found {}:\n{stdout}",
        values.len()
    );
    values.into_iter().next().expect("checked above")
}

/// The one plaintext value every fixture writes. No machine-mode run may leak
/// it on either stream, so a single `contains` check covers every secret the
/// suite creates.
const CANARY: &str = "machine-canary-2b6f";

/// The reserved attachment-key custody name. A bulk `set` refuses it outright,
/// which is the one deterministic per-item failure the local backend offers.
const RESERVED_KEY: &str = "xv-attachment-key";

const MIGRATE: [&str; 5] = ["migrate", "--from", "local:default", "--to", "local:other"];

// ---------------------------------------------------------------------------
// Fixtures. Each one builds the situation a scenario needs and is shared by the
// targeted test for that scenario and the table-driven contract test below.
// ---------------------------------------------------------------------------

/// A bare environment with no secrets.
fn empty_env() -> MachineEnv {
    MachineEnv::new()
}

/// Two secrets in `default`, an empty `other` vault to receive them.
fn seeded_two_secrets() -> MachineEnv {
    let env = MachineEnv::new();
    env.ok(&["set", "ALPHA", "--value", CANARY]);
    env.ok(&["set", "BETA", "--value", CANARY]);
    env.ok(&["vault", "create", "other"]);
    env
}

/// `ALPHA` present in both vaults, so `migrate --on-conflict fail` refuses
/// before any write.
fn conflicting_vaults() -> MachineEnv {
    let env = MachineEnv::new();
    env.ok(&["set", "ALPHA", "--value", CANARY]);
    env.ok(&["vault", "create", "other"]);
    env.ok(&["context", "use", "other", "--global"]);
    env.ok(&["set", "ALPHA", "--value", CANARY]);
    env.ok(&["context", "use", "default", "--global"]);
    env
}

/// Two secrets in one vault, so `mv ALPHA BETA` collides.
fn two_secrets_one_vault() -> MachineEnv {
    let env = MachineEnv::new();
    env.ok(&["set", "ALPHA", "--value", CANARY]);
    env.ok(&["set", "BETA", "--value", CANARY]);
    env
}

/// One secret whose rotation policy is long past due (`rotate --check` → 51).
fn due_secret() -> MachineEnv {
    let env = MachineEnv::new();
    env.ok(&["set", "STALE", "--value", CANARY]);
    env.ok(&[
        "update",
        "STALE",
        "--tag",
        "xv:rotate_every=30d",
        "--tag",
        "xv:rotated_at=2020-01-01T00:00:00Z",
    ]);
    env
}

/// A working directory holding a file that trips a built-in pattern, so
/// `xv scan` deterministically finds a leak and exits 50.
fn leaking_workdir() -> MachineEnv {
    let env = MachineEnv::new();
    // The canary rides alongside the AKIA token so the scanner still fires and
    // the no-leak assertion has something real to catch.
    write_home_file(
        &env,
        "leak.txt",
        format!("aws=AKIAIOSFODNN7EXAMPLE\npassword={CANARY}\n").as_bytes(),
    );
    env
}

/// A Keeper export with one unimportable record, one good one, and a shared
/// folder whose ACL has no xv equivalent.
fn keeper_import_env() -> MachineEnv {
    let env = MachineEnv::new();
    write_home_file(
        &env,
        "keeper.json",
        format!(
            r#"{{"shared_folders":[{{"path":"Team","can_edit":true,"permissions":[{{"name":"alice@example.com"}}]}}],
            "records":[{{"title":"Empty"}},{{"title":"Good","login":"u","password":"{CANARY}"}}]}}"#
        )
        .as_bytes(),
    );
    env
}

/// One readable upload path; the batch's second path never exists.
fn upload_batch_env() -> MachineEnv {
    let env = MachineEnv::new();
    write_home_file(&env, "good.txt", CANARY.as_bytes());
    env
}

/// A local directory for `file sync` to walk.
fn sync_env() -> MachineEnv {
    let env = MachineEnv::new();
    write_home_file(&env, "data/a.txt", CANARY.as_bytes());
    write_home_file(&env, "data/b.txt", CANARY.as_bytes());
    env
}

#[test]
fn migrate_clean_run_json_is_one_item_report() {
    let env = seeded_two_secrets();
    let out = env
        .xv()
        .args(MIGRATE)
        .args(["--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert_eq!(doc["summary"]["total"], 2, "{doc}");
    assert_eq!(doc["summary"]["succeeded"], 2, "{doc}");
    assert_eq!(doc["summary"]["skipped"], 0, "{doc}");
    assert_eq!(doc["summary"]["failed"], 0, "{doc}");
    let items = doc["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2, "{doc}");
    let mut names: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(names, ["ALPHA", "BETA"]);
    assert!(items.iter().all(|i| i["status"] == "ok"), "{doc}");

    // The banner never reaches stdout, and in machine mode the report
    // replaces it on stderr too.
    assert!(!stdout.contains("Source:"), "stdout:\n{stdout}");
    assert!(!stdout.contains("[ok]"), "stdout:\n{stdout}");
    assert!(!stderr.contains("Source:"), "stderr:\n{stderr}");
}

/// Human mode (no explicit `--format`): the banner and the per-item lines are
/// still printed word-for-word, on stderr, and stdout stays empty.
#[test]
fn migrate_human_run_keeps_the_banner_on_stderr() {
    let env = seeded_two_secrets();
    let out = env.xv().args(MIGRATE).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    for expected in [
        "Source:",
        "Target:",
        "to migrate:",
        "On conflict:",
        "Dry run?",
    ] {
        assert!(
            stderr.contains(expected),
            "missing {expected} in stderr:\n{stderr}"
        );
    }
    assert!(stderr.contains("[ok] ALPHA"), "stderr:\n{stderr}");
    assert!(stdout.trim().is_empty(), "stdout:\n{stdout}");
}

#[test]
fn migrate_clean_run_yaml_is_one_item_report() {
    let env = seeded_two_secrets();
    let out = env
        .xv()
        .args(MIGRATE)
        .args(["--format", "yaml"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_yaml_document(&stdout);
    assert_eq!(doc["summary"]["succeeded"], 2, "{doc}");
    assert_eq!(doc["items"].as_array().map(Vec::len), Some(2), "{doc}");
    assert!(!stderr.contains("Source:"), "stderr:\n{stderr}");
}

#[test]
fn migrate_clean_run_csv_is_item_rows() {
    let env = seeded_two_secrets();
    let out = env
        .xv()
        .args(MIGRATE)
        .args(["--format", "csv"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines[0], "name,status,detail,error", "stdout:\n{stdout}");
    assert_eq!(lines.len(), 3, "stdout:\n{stdout}");
    assert!(
        lines[1..].iter().all(|l| l.contains(",ok,")),
        "stdout:\n{stdout}"
    );
}

#[test]
fn migrate_conflict_fail_is_one_envelope_without_a_report() {
    let env = conflicting_vaults();

    let out = env
        .xv()
        .args(MIGRATE)
        .args(["--on-conflict", "fail", "--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_ne!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert!(doc["error"]["code"].is_string(), "{doc}");
    assert_eq!(
        doc["error"]["exit_code"].as_i64(),
        out.status.code().map(i64::from),
        "{doc}"
    );
    // The refusal precedes every write, so there is nothing to report.
    assert!(doc.get("report").is_none(), "{doc}");
    assert!(!stdout.contains("Source:"), "stdout:\n{stdout}");
}

#[test]
fn migrate_dry_run_json_is_one_plan_document() {
    let env = seeded_two_secrets();
    let out = env
        .xv()
        .args(MIGRATE)
        .args(["--dry-run", "--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert_eq!(doc["source"], "local:default", "{doc}");
    assert_eq!(doc["target"], "local:other", "{doc}");
    assert_eq!(doc["dry_run"], true, "{doc}");
    assert_eq!(doc["on_conflict"], "skip", "{doc}");
    let mut to_migrate: Vec<&str> = doc["to_migrate"]
        .as_array()
        .expect("to_migrate array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    to_migrate.sort_unstable();
    assert_eq!(to_migrate, ["ALPHA", "BETA"], "{doc}");
    assert_eq!(doc["to_skip"].as_array().map(Vec::len), Some(0), "{doc}");
    assert_eq!(doc["conflicts"].as_array().map(Vec::len), Some(0), "{doc}");
    assert_eq!(
        doc["attachment_previews"].as_array().map(Vec::len),
        Some(0),
        "{doc}"
    );

    // Dry run writes nothing.
    env.ok(&["context", "use", "other", "--global"]);
    let listed = env.ok(&["ls", "--format", "json"]);
    assert!(!listed.contains("ALPHA"), "{listed}");
}

// ---------------------------------------------------------------------------
// Task 4 — bulk and narrated commands
// ---------------------------------------------------------------------------

#[test]
fn bulk_set_json_is_one_item_report() {
    let env = MachineEnv::new();
    let out = env
        .xv()
        .args(["set", "ALPHA=a-value", "BETA=b-value", "--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert_eq!(doc["summary"]["total"], 2, "{doc}");
    assert_eq!(doc["summary"]["succeeded"], 2, "{doc}");
    assert_eq!(doc["summary"]["failed"], 0, "{doc}");
    let mut names: Vec<&str> = doc["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["ALPHA", "BETA"], "{doc}");
    assert!(!stdout.contains("Setting"), "stdout:\n{stdout}");
}

#[test]
fn bulk_set_partial_failure_is_one_envelope_with_a_report() {
    let env = MachineEnv::new();
    let out = env
        .xv()
        .args([
            "set",
            &format!("{RESERVED_KEY}={CANARY}"),
            &format!("GOOD={CANARY}"),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_ne!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert!(doc["error"]["code"].is_string(), "{doc}");
    assert_eq!(
        doc["error"]["exit_code"].as_i64(),
        out.status.code().map(i64::from),
        "{doc}"
    );
    assert_eq!(doc["report"]["summary"]["failed"], 1, "{doc}");
    assert_eq!(doc["report"]["summary"]["succeeded"], 1, "{doc}");
    let items = doc["report"]["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2, "{doc}");
    assert!(
        items
            .iter()
            .any(|i| i["status"] == "failed" && i["name"] == RESERVED_KEY),
        "{doc}"
    );
    // Never a value.
    assert!(!stdout.contains(CANARY), "stdout:\n{stdout}");
}

/// Human mode keeps every word of the bulk summary, on stderr, with nothing
/// on stdout.
#[test]
fn bulk_set_human_mode_keeps_the_summary_on_stderr() {
    let env = MachineEnv::new();
    let out = env
        .xv()
        .args(["set", "ALPHA=a-value", "BETA=b-value"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("Setting 2 secret(s)..."),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Bulk set complete: 2 succeeded, 0 failed"),
        "stderr:\n{stderr}"
    );
    assert!(stdout.trim().is_empty(), "stdout:\n{stdout}");
}

/// A single `set` no longer writes its Vault/Version detail to stdout.
#[test]
fn single_set_detail_lines_are_on_stderr() {
    let env = MachineEnv::new();
    let out = env
        .xv()
        .args(["set", "ALPHA", "--value", "a-value"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(0), "stderr:\n{stderr}");
    assert!(stderr.contains("Vault: default"), "stderr:\n{stderr}");
    assert!(stderr.contains("Version:"), "stderr:\n{stderr}");
    assert!(stdout.trim().is_empty(), "stdout:\n{stdout}");
}

#[test]
fn mv_dry_run_json_is_one_plan_document() {
    let env = MachineEnv::new();
    env.ok(&["set", "ALPHA", "--value", "a-value"]);
    let out = env
        .xv()
        .args(["mv", "ALPHA", "RENAMED", "--dry-run", "--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    let planned = doc["planned"].as_array().expect("planned array");
    assert_eq!(planned.len(), 1, "{doc}");
    assert_eq!(planned[0]["from"], "ALPHA", "{doc}");
    assert_eq!(planned[0]["to"], "RENAMED", "{doc}");
    assert!(!stdout.contains("->"), "stdout:\n{stdout}");
}

/// Human mode: the preview moves to stderr and stdout stays empty.
#[test]
fn mv_dry_run_human_preview_is_on_stderr() {
    let env = MachineEnv::new();
    env.ok(&["set", "ALPHA", "--value", "a-value"]);
    let out = env
        .xv()
        .args(["mv", "ALPHA", "RENAMED", "--dry-run"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(0), "stderr:\n{stderr}");
    assert!(stderr.contains("ALPHA -> RENAMED"), "stderr:\n{stderr}");
    assert!(stdout.trim().is_empty(), "stdout:\n{stdout}");
}

/// A destination collision is refused before any write, so the single document
/// is the bare envelope — the same rule `migrate --on-conflict fail` follows.
#[test]
fn mv_collision_is_one_envelope_and_no_preview_on_stdout() {
    let env = two_secrets_one_vault();
    let out = env
        .xv()
        .args(["mv", "ALPHA", "BETA", "--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_ne!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert!(doc["error"]["code"].is_string(), "{doc}");
    assert_eq!(
        doc["error"]["exit_code"].as_i64(),
        out.status.code().map(i64::from),
        "{doc}"
    );
    assert!(!stdout.contains("->"), "stdout:\n{stdout}");
}

#[test]
fn vault_import_dry_run_with_a_rejected_record_is_one_envelope_with_a_report() {
    // One unimportable record, one good one, and a shared folder whose ACL has
    // no xv equivalent — the last produces a fidelity-loss advisory that no
    // item can carry, so it must reach stderr even in machine mode.
    let env = keeper_import_env();
    let import_path = env.home.join("keeper.json");

    let out = env
        .xv()
        .args([
            "vault",
            "import",
            "default",
            "--fmt",
            "keeper",
            "--input",
            import_path.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_ne!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert!(doc["error"]["code"].is_string(), "{doc}");
    assert_eq!(doc["report"]["summary"]["failed"], 1, "{doc}");
    let items = doc["report"]["items"].as_array().expect("items array");
    assert!(
        items
            .iter()
            .any(|i| i["name"] == "Empty" && i["status"] == "failed"),
        "{doc}"
    );
    assert!(
        items
            .iter()
            .any(|i| i["name"] == "Good" && i["status"] == "skipped"),
        "{doc}"
    );
    // The per-record list never reaches stdout, and no password does either.
    assert!(!stdout.contains("  - "), "stdout:\n{stdout}");
    assert!(!stdout.contains("\"p\""), "stdout:\n{stdout}");

    // The fidelity-loss advisory belongs to the file, not to any item, so it
    // is still delivered — on stderr, where advisories live.
    assert!(
        stderr.contains("permissions were NOT applied"),
        "the shared-folder advisory must survive machine mode:\nstderr:\n{stderr}"
    );
    assert!(stderr.contains("alice@example.com"), "stderr:\n{stderr}");
    assert!(
        !stdout.contains("permissions were NOT applied"),
        "stdout:\n{stdout}"
    );
}

#[test]
fn copy_json_is_one_destination_metadata_document() {
    let env = seeded_two_secrets();
    let out = env
        .xv()
        .args([
            "copy", "ALPHA", "--from", "default", "--to", "other", "--format", "json",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert_eq!(doc["original_name"], "ALPHA", "{doc}");
    assert!(doc["version"].is_string(), "{doc}");
    assert!(!stdout.contains("Copying"), "stdout:\n{stdout}");
    assert!(!stdout.contains("Source:"), "stdout:\n{stdout}");
    assert!(!stdout.contains(CANARY), "stdout:\n{stdout}");
}

/// A dry run writes nothing, so the plan takes the destination metadata's
/// place — machine mode still owes stdout exactly one document.
#[test]
fn copy_dry_run_json_is_one_plan_document() {
    let env = seeded_two_secrets();
    let out = env
        .xv()
        .args([
            "copy",
            "ALPHA",
            "--from",
            "default",
            "--to",
            "other",
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert_eq!(doc["planned"]["from"]["vault"], "default", "{doc}");
    assert_eq!(doc["planned"]["from"]["name"], "ALPHA", "{doc}");
    assert_eq!(doc["planned"]["to"]["vault"], "other", "{doc}");
    assert_eq!(doc["planned"]["to"]["name"], "ALPHA", "{doc}");
    assert_eq!(doc["planned"]["move"], false, "{doc}");
    assert!(!stdout.contains("Copying"), "stdout:\n{stdout}");
    assert!(!stdout.contains(CANARY), "stdout:\n{stdout}");

    // A dry run writes nothing.
    env.ok(&["context", "use", "other", "--global"]);
    let listed = env.ok(&["ls", "--format", "json"]);
    assert!(!listed.contains("ALPHA"), "{listed}");
}

#[test]
fn copy_human_mode_keeps_the_narration_on_stderr() {
    let env = seeded_two_secrets();
    let out = env
        .xv()
        .args(["copy", "ALPHA", "--from", "default", "--to", "other"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("Copying secret 'ALPHA'"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Source: default/ALPHA"),
        "stderr:\n{stderr}"
    );
    assert!(stderr.contains("Target: other/ALPHA"), "stderr:\n{stderr}");
    assert!(stdout.trim().is_empty(), "stdout:\n{stdout}");
}

#[test]
fn move_json_is_one_destination_metadata_document() {
    let env = seeded_two_secrets();
    let out = env
        .xv()
        .args([
            "move", "ALPHA", "--from", "default", "--to", "other", "--force", "--format", "json",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert_eq!(doc["original_name"], "ALPHA", "{doc}");
    assert!(!stdout.contains("Moving"), "stdout:\n{stdout}");
    assert!(!stdout.contains("Deleting source"), "stdout:\n{stdout}");
    assert!(!stdout.contains(CANARY), "stdout:\n{stdout}");
}

#[test]
fn rotate_due_json_is_one_item_report() {
    let env = due_secret();

    let out = env
        .xv()
        .args(["rotate", "--due", "--force", "--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert_eq!(doc["summary"]["total"], 1, "{doc}");
    assert_eq!(doc["summary"]["succeeded"], 1, "{doc}");
    assert_eq!(doc["items"][0]["name"], "STALE", "{doc}");
    assert_eq!(doc["items"][0]["status"], "ok", "{doc}");
    assert!(!stdout.contains(CANARY), "stdout:\n{stdout}");
}

#[test]
fn version_json_is_one_object() {
    let env = MachineEnv::new();
    let out = env
        .xv()
        .args(["version", "--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(out.status.code(), Some(0), "stdout:\n{stdout}");
    let doc = one_json_document(&stdout);
    assert!(doc["version"].is_string(), "{doc}");
    assert!(doc["git_hash"].is_string(), "{doc}");
    assert!(doc["git_ref"].is_string(), "{doc}");
    assert!(doc["backends"].is_array(), "{doc}");
    assert!(!stdout.contains("crosstache Rust CLI"), "stdout:\n{stdout}");
}

/// Human mode keeps `version` exactly as it was: plain text on stdout.
#[test]
fn version_human_mode_is_unchanged_text_on_stdout() {
    let env = MachineEnv::new();
    let out = env.xv().args(["version"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(out.status.code(), Some(0), "stdout:\n{stdout}");
    assert!(stdout.contains("crosstache Rust CLI"), "stdout:\n{stdout}");
    assert!(stdout.contains("Version:"), "stdout:\n{stdout}");
    assert!(stdout.contains("Backends:"), "stdout:\n{stdout}");
}

// ---------------------------------------------------------------------------
// File batches, `file sync`, and `transfer`
// ---------------------------------------------------------------------------

/// Write `contents` at `relative` under the environment's HOME (the cwd every
/// `xv` invocation runs in) and return the path as the CLI sees it.
fn write_home_file(env: &MachineEnv, relative: &str, contents: &[u8]) -> PathBuf {
    let path = env.home.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent dir");
    std::fs::write(&path, contents).expect("write file");
    path
}

/// A multi-file `file upload` where the second path does not exist: stdout
/// holds exactly one document — the error envelope with the per-file
/// `ItemReport` attached under `report` — and the run exits non-zero.
#[test]
fn file_upload_batch_partial_failure_json_is_one_envelope_with_report() {
    let env = upload_batch_env();

    let out = env
        .xv()
        .args(["file", "upload", "good.txt", "missing.txt"])
        .args(["--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_ne!(
        out.status.code(),
        Some(0),
        "a missing upload path must fail\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert!(doc["error"]["message"].is_string(), "{doc}");
    // The envelope's declared exit code is the process's actual exit code.
    let declared = doc["error"]["exit_code"].as_i64().expect("exit_code");
    assert_eq!(out.status.code(), Some(declared as i32), "{doc}");
    let report = &doc["report"];
    assert_eq!(report["summary"]["total"], 2, "{doc}");
    assert_eq!(report["summary"]["succeeded"], 1, "{doc}");
    assert_eq!(report["summary"]["failed"], 1, "{doc}");
    let items = report["items"].as_array().expect("items array");
    assert_eq!(items[0]["name"], "good.txt", "{doc}");
    assert_eq!(items[0]["status"], "ok", "{doc}");
    assert_eq!(items[1]["name"], "missing.txt", "{doc}");
    assert_eq!(items[1]["status"], "failed", "{doc}");
    assert!(!stdout.contains("Uploading"), "stdout:\n{stdout}");
}

/// A clean multi-file `file upload`: one `ItemReport` document, exit 0.
#[test]
fn file_upload_batch_clean_json_is_one_item_report() {
    let env = MachineEnv::new();
    write_home_file(&env, "one.txt", b"one");
    write_home_file(&env, "two.txt", b"two");

    let out = env
        .xv()
        .args(["file", "upload", "one.txt", "two.txt"])
        .args(["--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert_eq!(doc["summary"]["total"], 2, "{doc}");
    assert_eq!(doc["summary"]["succeeded"], 2, "{doc}");
    assert_eq!(doc["summary"]["failed"], 0, "{doc}");
    assert!(doc["error"].is_null(), "{doc}");
    assert!(!stdout.contains("Upload completed"), "stdout:\n{stdout}");
}

/// `file sync --dry-run --format yaml`: the plan lines stay off stdout and the
/// sync summary is the run's single YAML document.
#[test]
fn file_sync_dry_run_yaml_is_one_document() {
    let env = sync_env();

    let out = env
        .xv()
        .args(["file", "sync", "data", "--direction", "up", "--dry-run"])
        .args(["--format", "yaml"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_yaml_document(&stdout);
    assert_eq!(doc["dry_run"], true, "{doc}");
    assert_eq!(doc["uploaded"], 2, "{doc}");
    assert!(!stdout.contains("upload (dry-run):"), "stdout:\n{stdout}");
    assert!(!stdout.contains("Sync summary"), "stdout:\n{stdout}");
}

/// `file sync --format csv`: stdout is CSV rows only, with no narration.
#[test]
fn file_sync_csv_is_rows_only() {
    let env = MachineEnv::new();
    write_home_file(&env, "data/a.txt", b"alpha");

    let out = env
        .xv()
        .args(["file", "sync", "data", "--direction", "up"])
        .args(["--format", "csv"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines[0], "report", "stdout:\n{stdout}");
    assert_eq!(lines.len(), 2, "stdout:\n{stdout}");
    assert!(!stdout.contains("upload:"), "stdout:\n{stdout}");
    assert!(!stdout.contains("Sync summary"), "stdout:\n{stdout}");
}

/// `transfer` honours the resolved format: a preview asked for as YAML is one
/// YAML document, not the pretty JSON the human path prints.
#[test]
fn transfer_preview_yaml_is_yaml_not_json() {
    let env = MachineEnv::new();
    env.ok(&["set", "cert", "--value", "cert-value"]);
    write_home_file(&env, "proof.txt", b"proof-content");
    env.ok(&["attach", "cert", "proof.txt"]);

    let out = env
        .xv()
        .args([
            "transfer",
            "cert",
            "--from",
            "default",
            "--to",
            "default",
            "--new-name",
            "certificate",
            "--move",
        ])
        .args(["--format", "yaml"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_err(),
        "transfer must not print JSON when --format yaml was asked for:\n{stdout}"
    );
    let doc = one_yaml_document(&stdout);
    assert_eq!(doc["attachment_count"], 1, "{doc}");
    assert_eq!(doc["intent"]["destination_name"], "certificate", "{doc}");
    assert!(!stdout.contains("proof-content"), "stdout:\n{stdout}");
}

/// A same-vault `mv` that carries attachments runs the transfer engine. Its
/// report used to be pretty-printed JSON unconditionally, so `--format yaml`
/// produced JSON on stdout regardless. It must now be the run's single
/// document, rendered in the resolved format.
#[test]
fn mv_with_attachments_yaml_is_one_yaml_document() {
    let env = MachineEnv::new();
    env.ok(&["set", "cert", "--value", CANARY]);
    write_home_file(&env, "proof.txt", b"proof-content");
    env.ok(&["attach", "cert", "proof.txt"]);

    let out = env
        .xv()
        .args([
            "mv",
            "cert",
            "certificate",
            "--with-attachments",
            "--offline",
        ])
        .args(["--format", "yaml"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_err(),
        "mv must not print JSON when --format yaml was asked for:\n{stdout}"
    );
    let doc = one_yaml_document(&stdout);
    assert_eq!(doc["complete"], true, "{doc}");
    assert!(doc["id"].is_string(), "{doc}");
    assert!(!stdout.contains(CANARY), "stdout:\n{stdout}");
    assert!(!stderr.contains(CANARY), "stderr:\n{stderr}");
}

/// `migrate` calls the transfer engine once per attachment-carrying secret
/// inside a loop. Each call used to print its own JSON report; the run must
/// instead park exactly one `ItemReport` whose item carries the transfer id.
#[test]
fn migrate_with_attachments_json_is_one_item_report_carrying_the_transfer() {
    let env = MachineEnv::new();
    env.ok(&["set", "cert", "--value", CANARY]);
    write_home_file(&env, "proof.txt", b"proof-content");
    env.ok(&["attach", "cert", "proof.txt"]);
    env.ok(&["vault", "create", "other"]);
    // Attaching in the destination materializes its attachment key ring, which
    // a cross-vault transfer requires be named explicitly via --to-key-id.
    env.ok(&["context", "use", "other", "--global"]);
    env.ok(&["set", "seed", "--value", CANARY]);
    env.ok(&["attach", "seed", "proof.txt"]);
    let status: serde_json::Value =
        serde_json::from_str(&env.ok(&["attachment-key", "status", "--format", "json"]))
            .expect("attachment-key status is one JSON document");
    let to_key_id = status["report"]["active_key_id"]
        .as_str()
        .expect("destination ring has an active key")
        .to_string();
    env.ok(&["context", "use", "default", "--global"]);

    let out = env
        .xv()
        .args(MIGRATE)
        .args(["--with-attachments", "--offline", "--to-key-id", &to_key_id])
        .args(["--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    let items = doc["items"].as_array().expect("items array");
    let cert = items
        .iter()
        .find(|item| item["name"] == "cert")
        .unwrap_or_else(|| panic!("no item for the attached secret:\n{doc}"));
    assert_eq!(cert["status"], "ok", "{doc}");
    let detail = cert["detail"].as_str().unwrap_or_default();
    assert!(
        detail.starts_with("attachment transfer ") && detail.len() > "attachment transfer ".len(),
        "the item must carry the engine's transfer id, got {detail:?}:\n{doc}"
    );
    assert!(!stdout.contains(CANARY), "stdout:\n{stdout}");
    assert!(!stderr.contains(CANARY), "stderr:\n{stderr}");
}

/// `xv attachments` used to print TSV rows plus a count line on stdout and
/// ignore `--format`. In machine mode it is one array and nothing else.
#[test]
fn attachments_json_is_one_array() {
    let env = MachineEnv::new();
    env.ok(&["set", "cert", "--value", CANARY]);
    write_home_file(&env, "proof.txt", b"proof-content");
    env.ok(&["attach", "cert", "proof.txt"]);

    let out = env
        .xv()
        .args(["attachments", "cert"])
        .args(["--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    let rows = doc
        .as_array()
        .unwrap_or_else(|| panic!("not an array:\n{doc}"));
    assert_eq!(rows.len(), 1, "{doc}");
    assert_eq!(rows[0]["name"], "proof.txt", "{doc}");
    assert!(
        !stdout.contains("attachment(s) on"),
        "the count line is status chrome and belongs on stderr:\n{stdout}"
    );
    assert!(!stdout.contains(CANARY), "stdout:\n{stdout}");
    assert!(!stderr.contains(CANARY), "stderr:\n{stderr}");
}

/// `--format auto` is not machine mode. Piped (as every test is), auto resolves
/// to JSON, and `file sync` keeps printing its summary on stdout exactly as it
/// did before the machine-output contract.
#[test]
fn file_sync_piped_auto_keeps_the_json_summary_on_stdout() {
    let env = sync_env();

    let out = env
        .xv()
        .args(["file", "sync", "data", "--direction", "up", "--dry-run"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let doc = one_json_document(&stdout);
    assert_eq!(doc["dry_run"], true, "{doc}");
    assert_eq!(doc["uploaded"], 2, "{doc}");
    assert_eq!(doc["downloaded"], 0, "{doc}");
    assert_eq!(doc["deleted"], 0, "{doc}");
    assert_eq!(doc["skipped"], 0, "{doc}");
}

// ---------------------------------------------------------------------------
// The contract itself: every machine-mode run writes exactly one document
// ---------------------------------------------------------------------------

/// One row of the contract matrix: a fixture that builds the situation and the
/// command to run in it, minus the `--format` pair the matrix supplies.
struct Scenario {
    name: &'static str,
    setup: fn() -> MachineEnv,
    args: &'static [&'static str],
    /// True only where the run refuses before producing a single row, so CSV
    /// stdout is legitimately empty. Every other cell must emit a header row —
    /// accepting empty stdout everywhere would let a silent regression pass.
    expect_empty_csv: bool,
}

/// The command list from the design doc's Verification section. Each one is
/// run three times, once per machine format.
const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "scan with findings",
        setup: leaking_workdir,
        args: &["scan"],
        expect_empty_csv: false,
    },
    Scenario {
        name: "rotate --check with a due secret",
        setup: due_secret,
        args: &["rotate", "--check"],
        expect_empty_csv: false,
    },
    Scenario {
        name: "vault import --dry-run with a rejected record",
        setup: keeper_import_env,
        args: &[
            "vault",
            "import",
            "default",
            "--fmt",
            "keeper",
            "--input",
            "keeper.json",
            "--dry-run",
        ],
        expect_empty_csv: false,
    },
    Scenario {
        name: "bulk set with an invalid name",
        setup: empty_env,
        args: &[
            "set",
            "xv-attachment-key=machine-canary-2b6f",
            "GOOD=machine-canary-2b6f",
        ],
        expect_empty_csv: false,
    },
    Scenario {
        name: "mv with a collision",
        setup: two_secrets_one_vault,
        args: &["mv", "ALPHA", "BETA"],
        // Refused on the collision before any row exists.
        expect_empty_csv: true,
    },
    Scenario {
        name: "migrate clean",
        setup: seeded_two_secrets,
        args: &["migrate", "--from", "local:default", "--to", "local:other"],
        expect_empty_csv: false,
    },
    Scenario {
        name: "migrate conflict",
        setup: conflicting_vaults,
        args: &[
            "migrate",
            "--from",
            "local:default",
            "--to",
            "local:other",
            "--on-conflict",
            "fail",
        ],
        // `--on-conflict fail` aborts before any item is recorded.
        expect_empty_csv: true,
    },
    Scenario {
        name: "file upload batch with a missing path",
        setup: upload_batch_env,
        args: &["file", "upload", "good.txt", "missing.txt"],
        expect_empty_csv: false,
    },
    Scenario {
        name: "copy",
        setup: seeded_two_secrets,
        args: &["copy", "ALPHA", "--from", "default", "--to", "other"],
        expect_empty_csv: false,
    },
    Scenario {
        name: "move",
        setup: seeded_two_secrets,
        args: &[
            "move", "ALPHA", "--from", "default", "--to", "other", "--force",
        ],
        expect_empty_csv: false,
    },
    Scenario {
        name: "file sync --dry-run",
        setup: sync_env,
        args: &["file", "sync", "data", "--direction", "up", "--dry-run"],
        expect_empty_csv: false,
    },
    Scenario {
        name: "version",
        setup: empty_env,
        args: &["version"],
        expect_empty_csv: false,
    },
];

/// Human status lines are `[ok] …`/`[warn] …`-shaped, so a bare `[` at the
/// start of a stderr line is only a violation when it is not one of those
/// prefixes — an unprefixed `[` (or any `{`) means a JSON document escaped
/// onto stderr.
fn stderr_line_is_a_document(line: &str) -> bool {
    if line.starts_with('{') {
        return true;
    }
    if !line.starts_with('[') {
        return false;
    }
    !["[ok] ", "[error] ", "[warn] ", "[info] ", "[hint] "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

/// Assert stdout parses as exactly one CSV document: a header row plus records
/// that all have the same field count. Empty stdout is accepted only for the
/// scenarios that refuse before a row exists (`expect_empty_csv`); everywhere
/// else an empty stdout is the regression this suite exists to catch.
#[track_caller]
fn one_csv_document(stdout: &str, label: &str, expect_empty: bool) {
    if stdout.trim().is_empty() {
        assert!(
            expect_empty,
            "{label}: CSV stdout is empty but this command must emit a header row"
        );
        return;
    }
    let mut reader = csv::ReaderBuilder::new()
        .flexible(false)
        .from_reader(stdout.as_bytes());
    let headers = reader
        .headers()
        .unwrap_or_else(|e| panic!("{label}: CSV stdout has no header row ({e}):\n{stdout}"))
        .clone();
    assert!(
        !headers.is_empty(),
        "{label}: CSV stdout has an empty header row:\n{stdout}"
    );
    for record in reader.records() {
        record.unwrap_or_else(|e| panic!("{label}: CSV stdout does not parse ({e}):\n{stdout}"));
    }
}

/// The whole contract, on every command the design doc names, in every machine
/// format:
///
/// * stdout holds exactly one document (or, for CSV, only rows);
/// * an envelope's `error.exit_code` is the process's real exit code;
/// * no document escapes onto stderr;
/// * no secret value appears on either stream.
#[test]
fn every_machine_mode_run_writes_exactly_one_document() {
    for scenario in SCENARIOS {
        for format in ["json", "yaml", "csv"] {
            let label = format!("{} [--format {format}]", scenario.name);
            let env = (scenario.setup)();
            let out = env
                .xv()
                .args(scenario.args)
                .args(["--format", format])
                .output()
                .unwrap_or_else(|e| panic!("{label}: could not run xv: {e}"));
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);

            // A signal-terminated run has no code to compare against.
            let code = out.status.code().unwrap_or_else(|| {
                panic!("{label}: no exit status\nstdout:\n{stdout}\nstderr:\n{stderr}")
            });

            match format {
                "json" => {
                    let doc = one_json_document(&stdout);
                    if let Some(error) = doc.get("error").filter(|e| !e.is_null()) {
                        assert_eq!(
                            error["exit_code"].as_i64(),
                            Some(i64::from(code)),
                            "{label}: the envelope's exit_code must be the process's:\n{doc}"
                        );
                    }
                }
                "yaml" => {
                    let doc = one_yaml_document(&stdout);
                    if let Some(error) = doc.get("error").filter(|e| !e.is_null()) {
                        assert_eq!(
                            error["exit_code"].as_i64(),
                            Some(i64::from(code)),
                            "{label}: the envelope's exit_code must be the process's:\n{doc}"
                        );
                    }
                }
                _ => one_csv_document(&stdout, &label, scenario.expect_empty_csv),
            }

            for line in stderr.lines() {
                assert!(
                    !stderr_line_is_a_document(line),
                    "{label}: a document escaped onto stderr: {line}\nstderr:\n{stderr}"
                );
            }

            assert!(
                !stdout.contains(CANARY),
                "{label}: the canary value reached stdout:\n{stdout}"
            );
            assert!(
                !stderr.contains(CANARY),
                "{label}: the canary value reached stderr:\n{stderr}"
            );
        }
    }
}
