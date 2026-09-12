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

/// Two secrets in `default`, an empty `other` vault to receive them.
fn seeded_two_secrets() -> MachineEnv {
    let env = MachineEnv::new();
    env.ok(&["set", "ALPHA", "--value", "a-value"]);
    env.ok(&["set", "BETA", "--value", "b-value"]);
    env.ok(&["vault", "create", "other"]);
    env
}

const MIGRATE: [&str; 5] = ["migrate", "--from", "local:default", "--to", "local:other"];

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
    let env = MachineEnv::new();
    env.ok(&["set", "ALPHA", "--value", "a-value"]);
    env.ok(&["vault", "create", "other"]);
    env.ok(&["context", "use", "other", "--global"]);
    env.ok(&["set", "ALPHA", "--value", "other-value"]);
    env.ok(&["context", "use", "default", "--global"]);

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

/// The reserved attachment-key custody name. A bulk `set` refuses it outright,
/// which is the one deterministic per-item failure the local backend offers.
const RESERVED_KEY: &str = "xv-attachment-key";

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
            &format!("{RESERVED_KEY}=nope"),
            "GOOD=fine",
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
    assert!(!stdout.contains("fine"), "stdout:\n{stdout}");
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
    let env = MachineEnv::new();
    env.ok(&["set", "ALPHA", "--value", "a-value"]);
    env.ok(&["set", "BETA", "--value", "b-value"]);
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
    let env = MachineEnv::new();
    let import_path = env.home.join("keeper.json");
    // One unimportable record, one good one, and a shared folder whose ACL has
    // no xv equivalent — the last produces a fidelity-loss advisory that no
    // item can carry, so it must reach stderr even in machine mode.
    std::fs::write(
        &import_path,
        r#"{"shared_folders":[{"path":"Team","can_edit":true,"permissions":[{"name":"alice@example.com"}]}],
            "records":[{"title":"Empty"},{"title":"Good","login":"u","password":"p"}]}"#,
    )
    .expect("write keeper file");

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
    assert!(!stdout.contains("a-value"), "stdout:\n{stdout}");
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
    assert!(!stdout.contains("a-value"), "stdout:\n{stdout}");

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
    assert!(!stdout.contains("a-value"), "stdout:\n{stdout}");
}

#[test]
fn rotate_due_json_is_one_item_report() {
    let env = MachineEnv::new();
    env.ok(&["set", "STALE", "--value", "old-value"]);
    env.ok(&[
        "update",
        "STALE",
        "--tag",
        "xv:rotate_every=30d",
        "--tag",
        "xv:rotated_at=2020-01-01T00:00:00Z",
    ]);

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
    assert!(!stdout.contains("old-value"), "stdout:\n{stdout}");
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
