//! Disclosure canary suite — the NEGATIVE direction.
//!
//! Every test here seeds a hermetic local (age-encrypted) store with two
//! canary plaintexts and then drives a real `xv` process across a surface
//! that is **not** a documented disclosure boundary, asserting the canary
//! appears in neither stdout, stderr, the on-disk cache, nor the audit log.
//!
//! The positive direction — the boundaries that *must* print a value — lives
//! in `boundary_*` tests in this same file, one per boundary, so that
//! `cargo test --test e2e_disclosure boundary_` enumerates the sanctioned
//! list and the rest of the file proves the complement stays silent.
//!
//! Isolation follows the `WorkspaceEnv` pattern from `e2e_workspaces.rs`:
//! `env_clear()` plus an explicit allowlist (the #317 lesson — selective
//! `env_remove()` leaks host vars into the child), a private `HOME` /
//! `XDG_CONFIG_HOME`, and a dedicated `XV_CACHE_DIR` so no test can read or
//! write the real OS cache. Unlike `tests/common::xv_isolated_local`, the
//! generated `xv.conf` turns the secrets-list cache **on** — a cache that is
//! never written cannot prove that cache bytes stay value-free.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// Plaintext of the untyped secret `LEAKY`.
const CANARY: &str = "disclosure-canary-7f3e";
/// Primary (`password`) field plaintext of the typed `login` record `REC`.
const RECORD_CANARY: &str = "record-canary-9c1d";

/// Render a path for interpolation into a **double-quoted** TOML string.
///
/// TOML basic strings process backslash escapes, so an unescaped Windows
/// path does not merely look wrong — `C:\Users\...` makes the parser read
/// `\U` as a unicode escape and reject the config outright.
fn toml_path(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// A hermetic local-backend environment with the secrets-list cache enabled
/// and the local audit trail on.
struct DisclosureEnv {
    _tmp: TempDir,
    home: PathBuf,
    config_dir: PathBuf,
    cache_dir: PathBuf,
    store_dir: PathBuf,
}

impl DisclosureEnv {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().join("home");
        let config_dir = home.join(".config");
        let xv_dir = config_dir.join("xv");
        let store_dir = tmp.path().join("store");
        let cache_dir = tmp.path().join("cache");
        let key_file = tmp.path().join("key.txt");

        std::fs::create_dir_all(&xv_dir).expect("create config dir");
        std::fs::create_dir_all(&store_dir).expect("create store dir");
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");

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
cache_enabled = true
cache_ttl_secs = 300
clipboard_timeout = 0

[local]
store_path = "{store}"
key_file = "{key}"
default_vault = "default"
audit = true
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
            store_dir,
        }
    }

    /// [`Self::new`] plus the two canary secrets: untyped `LEAKY` and the
    /// typed `login` record `REC` whose primary `password` field carries
    /// [`RECORD_CANARY`] inside the encrypted envelope.
    fn seeded() -> Self {
        let env = Self::new();
        env.ok(&["set", "LEAKY", "--value", CANARY]);
        env.ok(&[
            "set",
            "REC",
            "--type",
            "login",
            "--value",
            RECORD_CANARY,
            "--field",
            "username=alice",
        ]);
        env
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

    /// Run and require success, returning stdout.
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

    /// Run with extra environment variables applied on top of the hermetic
    /// base set.
    fn run_with_env(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut cmd = self.xv();
        cmd.args(args);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("execute xv binary")
    }

    /// stdout and stderr of a run, concatenated — the full byte surface a
    /// human or a pipe sees.
    fn output_of(&self, args: &[&str]) -> String {
        combined(&self.run(args))
    }

    /// The local audit log for the default vault (`[local].audit = true`).
    fn audit_log(&self) -> PathBuf {
        self.store_dir
            .join("vaults")
            .join("default")
            .join(".audit")
            .join("log.jsonl")
    }
}

/// Every regular file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// Assert neither canary appears anywhere in `haystack`.
#[track_caller]
fn assert_no_canary(label: &str, haystack: &str) {
    for canary in [CANARY, RECORD_CANARY] {
        assert!(
            !haystack.contains(canary),
            "{label} disclosed the canary {canary}:\n{haystack}"
        );
    }
}

/// stdout and stderr of one run, concatenated.
fn combined(out: &Output) -> String {
    let mut all = String::from_utf8_lossy(&out.stdout).into_owned();
    all.push('\n');
    all.push_str(&String::from_utf8_lossy(&out.stderr));
    all
}

/// Assert the command **succeeded** and that its stdout+stderr carry no
/// canary.
///
/// The success check is what keeps this suite non-vacuous: a clap usage
/// error, a config-load failure, or a renamed flag all produce a
/// canary-free stderr, so an assertion that only looked at the text would
/// pass while testing nothing. Every negative surface below therefore has
/// to actually run.
#[track_caller]
fn assert_silent(env: &DisclosureEnv, args: &[&str]) {
    assert_silent_with_env(env, args, &[]);
}

/// [`assert_silent`] with extra environment variables (e.g. `RUST_LOG`).
#[track_caller]
fn assert_silent_with_env(env: &DisclosureEnv, args: &[&str], extra: &[(&str, &str)]) {
    let out = env.run_with_env(args, extra);
    let all = combined(&out);
    assert!(
        out.status.success(),
        "`xv {}` did not run (exit {:?}); a failed command proves nothing about disclosure:\n{all}",
        args.join(" "),
        out.status.code(),
    );
    assert_no_canary(&format!("`xv {}`", args.join(" ")), &all);
}

/// Assert the command **failed for the intended reason** — non-zero exit
/// plus a stderr substring naming the cause — and that neither stream
/// carries a canary. Without the cause check, any unrelated failure (a
/// typo'd flag) would satisfy the test.
#[track_caller]
fn assert_fails_silently(env: &DisclosureEnv, args: &[&str], cause: &str) {
    let out = env.run(args);
    let all = combined(&out);
    assert!(
        !out.status.success(),
        "`xv {}` unexpectedly succeeded; this test needs a real failure:\n{all}",
        args.join(" ")
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(cause),
        "`xv {}` failed for the wrong reason (expected {cause:?}):\n{all}",
        args.join(" ")
    );
    assert_no_canary(&format!("`xv {}`", args.join(" ")), &all);
}

// ─── listing / search / metadata surfaces ──────────────────────────────────

#[test]
fn listing_surfaces_never_print_a_value() {
    let env = DisclosureEnv::seeded();
    // Every rendering path of `ls`: the human table, and each machine format.
    assert_silent(&env, &["list"]);
    for format in ["table", "json", "yaml", "csv", "plain"] {
        assert_silent(&env, &["--format", format, "list"]);
    }
    assert_silent(&env, &["list", "--long"]);
    assert_silent(&env, &["list", "--recursive"]);
    assert_silent(&env, &["list", "--names-only"]);
    assert_silent(&env, &["--format", "json", "list", "--type", "login"]);
}

#[test]
fn deleted_listing_never_prints_a_value() {
    let env = DisclosureEnv::seeded();
    env.ok(&["delete", "LEAKY", "--force"]);
    assert_silent(&env, &["list", "--deleted"]);
    assert_silent(&env, &["--format", "json", "list", "--deleted"]);
}

#[test]
fn history_never_prints_a_value() {
    let env = DisclosureEnv::seeded();
    // A second write so the history has more than one revision to render.
    env.ok(&["set", "LEAKY", "--value", CANARY]);
    assert_silent(&env, &["--format", "json", "history", "LEAKY"]);
    assert_silent(&env, &["history", "LEAKY"]);
}

#[test]
fn fuzzy_search_never_prints_a_value() {
    let env = DisclosureEnv::seeded();
    assert_silent(&env, &["find", "leak"]);
    // `--in` opts the ranker into extra metadata fields; none of them may
    // become a path to the value.
    assert_silent(
        &env,
        &[
            "find", "leak", "--in", "name", "--in", "folder", "--in", "groups", "--in", "note",
            "--in", "tags",
        ],
    );
    assert_silent(&env, &["--format", "json", "find", "rec"]);
}

#[test]
fn info_and_group_views_never_print_a_value() {
    let env = DisclosureEnv::seeded();
    assert_silent(&env, &["info", "LEAKY"]);
    assert_silent(&env, &["--format", "json", "info", "REC"]);
    assert_silent(&env, &["group", "list"]);
}

// ─── the clipboard path: `get` with no `--raw` ─────────────────────────────

/// Restores whatever was on the clipboard when the test started, even if
/// the test panics.
struct ClipboardRestore {
    clipboard: arboard::Clipboard,
    previous: Option<String>,
}

impl Drop for ClipboardRestore {
    fn drop(&mut self) {
        let _ = match self.previous.take() {
            Some(text) => self.clipboard.set_text(text),
            // Nothing readable was there to begin with; leave it empty
            // rather than leaving the canary behind.
            None => self.clipboard.set_text(String::new()),
        };
    }
}

/// A clipboard handle whose round-trip actually works on this host, with the
/// prior contents captured for restore — or `None` on a headless CI box, a
/// sandbox, or any host where `get_text` cannot read back what was written.
///
/// The guard matters: without it the test still *passes* on such a host, but
/// for the wrong reason — `copy_to_clipboard` only warns when the clipboard
/// is unavailable, so stdout would be canary-free because nothing happened.
fn clipboard_restore_if_available() -> Option<ClipboardRestore> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let previous = clipboard.get_text().ok();
    let sentinel = format!("xv-disclosure-probe-{}", std::process::id());
    clipboard.set_text(sentinel.clone()).ok()?;
    // A few bounded retries: some hosts report `ContentNotAvailable` for a
    // moment after a write. This is a capability probe, not synchronization
    // with the code under test.
    for _ in 0..40 {
        match clipboard.get_text() {
            Ok(read) if read == sentinel => {
                return Some(ClipboardRestore {
                    clipboard,
                    previous,
                })
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
    // Put back what we found before giving up.
    let _ = clipboard.set_text(previous.unwrap_or_default());
    None
}

#[test]
fn get_without_raw_never_prints_the_value() {
    // The copy really has to happen for this test to mean anything, so the
    // clipboard round-trip is verified first and the host's clipboard is
    // restored on the way out (including on panic).
    let Some(_restore) = clipboard_restore_if_available() else {
        eprintln!(
            "skipping get_without_raw_never_prints_the_value: no clipboard round-trip on this host"
        );
        return;
    };
    let env = DisclosureEnv::seeded();
    // The clipboard is a sanctioned boundary; stdout is not. Only
    // stdout/stderr are asserted on here — the clipboard contents
    // themselves are `tests/clipboard_tests.rs`'s subject.
    assert_silent(&env, &["get", "LEAKY"]);
    assert_silent(&env, &["get", "REC", "--field", "password"]);
}

// ─── export without the opt-in flag ────────────────────────────────────────

#[test]
fn vault_export_without_include_values_never_prints_a_value() {
    let env = DisclosureEnv::seeded();
    for format in ["json", "env", "txt"] {
        assert_silent(&env, &["vault", "export", "default", "--fmt", format]);
    }
}

// ─── the scanner ───────────────────────────────────────────────────────────

#[test]
fn scan_over_a_clean_file_never_prints_the_vault_value() {
    let env = DisclosureEnv::seeded();
    // The scanner holds every vault value in memory to match against the
    // file; a clean file must not make it echo what it was matching with.
    let target = env.home.join("scanme");
    std::fs::create_dir_all(&target).expect("create scan dir");
    std::fs::write(target.join("clean.txt"), "nothing to see here\n").expect("write scan input");
    let target = target.display().to_string();
    assert_silent(&env, &["scan", &target]);
    assert_silent(&env, &["--format", "json", "scan", &target]);
}

// ─── cache ─────────────────────────────────────────────────────────────────

#[test]
fn cache_status_and_cache_bytes_never_carry_a_value() {
    let env = DisclosureEnv::seeded();
    // Populate the on-disk secrets-list cache first.
    env.ok(&["list"]);
    let cached = files_under(&env.cache_dir);
    assert!(
        cached
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == "secrets-list-v5.json")),
        "no secrets-list cache entry was written ({cached:?}); this test cannot \
         prove anything about cache bytes"
    );
    for path in &cached {
        let bytes = std::fs::read(path).expect("read cache file");
        assert_no_canary(
            &format!("cache file {}", path.display()),
            &String::from_utf8_lossy(&bytes),
        );
    }
    assert_silent(&env, &["cache", "status"]);
}

// ─── error paths ───────────────────────────────────────────────────────────

#[test]
fn failing_commands_never_leak_a_value_on_stderr() {
    let env = DisclosureEnv::seeded();

    // A vault that does not exist.
    assert_fails_silently(
        &env,
        &["info", "nosuchvault", "--type", "vault"],
        "xv-vault-not-found",
    );
    // A secret that does not exist, on a vault that does.
    assert_fails_silently(
        &env,
        &["get", "NOSUCHSECRET", "--raw"],
        "xv-secret-not-found",
    );
    // A field that the record does not declare — the error lists the known
    // field NAMES, which is exactly the boundary it must not cross into
    // listing their values.
    assert_fails_silently(
        &env,
        &["get", "REC", "--field", "nosuchfield", "--raw"],
        "has no field",
    );
    // Reads driven through a context pointing at a vault that was never
    // created. The local backend surfaces this as a store-open failure
    // rather than a typed vault-not-found, so the cause assertion pins the
    // error CODE prefix; the typed case is covered by `info` above.
    env.ok(&["context", "use", "nosuchvault", "--global"]);
    assert_fails_silently(&env, &["get", "LEAKY"], "error[xv-");
    assert_fails_silently(&env, &["get", "LEAKY", "--raw"], "error[xv-");
    assert_fails_silently(
        &env,
        &["get", "REC", "--field", "password", "--raw"],
        "error[xv-",
    );
    // These two *succeed* against a nonexistent vault (an empty listing and
    // an empty export), so they belong in the success-asserting helper —
    // they are still a disclosure surface worth sweeping.
    assert_silent(&env, &["list"]);
    assert_silent(
        &env,
        &["vault", "export", "nosuchvault", "--include-values"],
    );
}

// ─── tracing / debug output ────────────────────────────────────────────────

#[test]
fn debug_logging_never_prints_a_value() {
    let env = DisclosureEnv::seeded();
    // `--debug` alone only raises xv's own level; `RUST_LOG=trace` turns
    // every span and event in the process on, which is the state a user
    // debugging a failure actually runs in.
    for args in [
        ["--debug", "list"].as_slice(),
        ["--debug", "history", "LEAKY"].as_slice(),
        ["--debug", "find", "leak"].as_slice(),
        ["--debug", "info", "REC"].as_slice(),
        ["--debug", "vault", "export", "default"].as_slice(),
        // Reads and decrypts the record envelope, then prints only a
        // metadata field: the value passes through the process under full
        // tracing without ever being the thing that is printed.
        ["--debug", "get", "REC", "--field", "username", "--raw"].as_slice(),
    ] {
        assert_silent_with_env(&env, args, &[("RUST_LOG", "trace")]);
    }
}

// ─── the local audit trail ─────────────────────────────────────────────────

#[test]
fn local_audit_log_never_records_a_value() {
    let env = DisclosureEnv::seeded();
    // Exercise the operations the audit log records: read, list, update,
    // delete, plus a failure (a missing secret) so the failure path is
    // covered too.
    env.ok(&["get", "LEAKY", "--raw"]);
    env.ok(&["list"]);
    env.ok(&["update", "LEAKY", "--note", "touched"]);
    let _ = env.run(&["get", "NOSUCHSECRET", "--raw"]);
    env.ok(&["delete", "LEAKY", "--force"]);

    let log = env.audit_log();
    assert!(
        log.exists(),
        "audit log missing at {}; the audit flag is not taking effect",
        log.display()
    );
    let contents = std::fs::read_to_string(&log).expect("read audit log");
    assert!(
        contents.lines().count() >= 3,
        "audit log has too few entries to prove anything:\n{contents}"
    );
    assert_no_canary("local audit log", &contents);
    // And the rendered view of it.
    assert_silent(&env, &["audit", "--vault", "default"]);
}

// ─── the store itself is ciphertext ────────────────────────────────────────

#[test]
fn on_disk_store_never_contains_plaintext() {
    let env = DisclosureEnv::seeded();
    env.ok(&["list"]);
    for path in files_under(&env.store_dir) {
        let bytes = std::fs::read(&path).expect("read store file");
        assert_no_canary(
            &format!("store file {}", path.display()),
            &String::from_utf8_lossy(&bytes),
        );
    }
}

// ═══ POSITIVE DIRECTION ═════════════════════════════════════════════════════
//
// One `boundary_*` test per sanctioned disclosure boundary. Together they are
// the complete CLI list: `cargo test --test e2e_disclosure boundary_`
// enumerates it, and everything above proves the complement stays silent. The
// remaining boundaries live next to their code —
// `web::disclosure_tests::boundary_web_reveal_returns_value` for
// `POST /api/secrets/{name}/value`, and
// `tui_view_tests::boundary_tui_reveal_renders_value` for the reveal
// keystroke.

/// Boundary: `xv get <name> --raw`.
#[test]
fn boundary_get_raw_prints_value() {
    let env = DisclosureEnv::seeded();
    let stdout = env.ok(&["get", "LEAKY", "--raw"]);
    // Exactly the value: `--raw` is the scripting contract, so nothing —
    // not a trailing newline, not a label — may ride along.
    assert_eq!(stdout, CANARY);
}

/// Boundary: `xv get <name> --field <f> --raw` on a typed record.
#[test]
fn boundary_get_field_raw_prints_record_field() {
    let env = DisclosureEnv::seeded();
    let stdout = env.ok(&["get", "REC", "--field", "password", "--raw"]);
    assert_eq!(stdout, RECORD_CANARY);
}

/// Boundary: `xv get <name> --record` — the sanctioned field-level
/// exception, where the envelope's secret fields are serialized by name.
#[test]
fn boundary_get_record_prints_envelope_fields() {
    let env = DisclosureEnv::seeded();
    let stdout = env.ok(&["--format", "json", "get", "REC", "--record"]);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("record json");
    assert_eq!(json["fields"]["password"], RECORD_CANARY);
    assert_eq!(json["type"], "login");
}

/// Boundary: `xv vault export --include-values`, in each format that carries
/// values (`keeper` is covered by
/// `tests/e2e_local_backend.rs::keeper_export_requires_include_values` /
/// `::keeper_export_round_trips_an_imported_file`).
#[test]
fn boundary_vault_export_include_values_prints_value() {
    let env = DisclosureEnv::seeded();
    for format in ["json", "env", "txt"] {
        let stdout = env.ok(&[
            "vault",
            "export",
            "default",
            "--fmt",
            format,
            "--include-values",
        ]);
        assert!(
            stdout.contains(CANARY),
            "`vault export --fmt {format} --include-values` dropped the value:\n{stdout}"
        );
        assert!(
            stdout.contains(RECORD_CANARY),
            "`vault export --fmt {format} --include-values` dropped the record value:\n{stdout}"
        );
    }
}

/// Boundary: `xv env pull` — the whole-vault plaintext export.
#[test]
fn boundary_env_pull_prints_value() {
    let env = DisclosureEnv::seeded();
    let stdout = env.ok(&["env", "pull", "--fmt", "json"]);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("pull json");
    let leaky = json
        .as_array()
        .expect("pull emits an array")
        .iter()
        .find(|e| e["name"] == "LEAKY")
        .expect("LEAKY in the pull output");
    assert_eq!(leaky["value"], CANARY);
    // ...and the dotenv rendering the default `--fmt plain` produces.
    let dotenv = env.ok(&["env", "pull", "--fmt", "plain"]);
    assert!(
        dotenv.contains(CANARY),
        "dotenv pull dropped the value:\n{dotenv}"
    );
}

/// Boundary: `xv diff --show-values` for a secret whose value differs
/// between the two vaults.
#[test]
fn boundary_diff_show_values_prints_value() {
    let env = DisclosureEnv::seeded();
    env.ok(&["vault", "create", "other"]);
    env.ok(&["context", "use", "other", "--global"]);
    env.ok(&["set", "LEAKY", "--value", "a-different-value"]);
    env.ok(&["context", "use", "default", "--global"]);

    // Without the flag, a differing value is reported but never shown.
    let quiet = env.ok(&["diff", "default", "other"]);
    assert!(quiet.contains("LEAKY"), "diff lost the secret:\n{quiet}");
    assert_no_canary("`xv diff` without --show-values", &quiet);

    let shown = env.ok(&["diff", "default", "other", "--show-values"]);
    assert!(
        shown.contains(CANARY),
        "`diff --show-values` dropped the value:\n{shown}"
    );
    assert!(
        shown.contains("a-different-value"),
        "`diff --show-values` dropped the other side:\n{shown}"
    );
}

/// Boundary: the scan orchestrator reads plaintext to match against files.
/// It must really match (proving it holds the value) while never echoing
/// the value into a finding — only the secret's NAME.
#[test]
fn boundary_scan_matches_the_value_without_printing_it() {
    let env = DisclosureEnv::seeded();
    let target = env.home.join("leaky");
    std::fs::create_dir_all(&target).expect("create scan dir");
    std::fs::write(target.join("app.conf"), format!("password = {CANARY}\n"))
        .expect("write scan input");
    let target = target.display().to_string();
    let combined = env.output_of(&["--format", "json", "scan", &target]);
    // A scan that finds something fails, so in machine mode the findings ride
    // inside the error envelope under `report` (compactly encoded) rather than
    // as a second, pretty-printed document.
    let compact = combined.replace("\": \"", "\":\"");
    assert!(
        compact.contains("\"secret_name\":\"LEAKY\""),
        "scan failed to match the planted value:\n{combined}"
    );
    // The finding names the secret and the file/line; it never echoes what
    // it matched with.
    assert_no_canary("`xv scan` finding", &combined);
}
