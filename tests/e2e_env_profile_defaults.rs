//! End-to-end regression tests for issue #308: `.xv.toml` env-profile
//! `group`/`folder` defaults were parsed into `EnvProfile` but never
//! consulted by any command — only display-only paths (`xv config show`,
//! `xv env show`) read them.
//!
//! Documented contract implemented here (see `docs/env-profiles.md` and
//! `README.md`'s "Project env profiles" section):
//!   - `group` default: applied by `xv run` as the injection filter, and by
//!     `xv set`/`xv gen --save` as the write-time group, whenever `--group`
//!     is omitted. NOT applied by `xv list`/`ls`.
//!   - `folder` default: applied only by `xv set`/`xv gen --save` when
//!     `--folder` is omitted. NOT applied by `xv list`/`ls` (matches the
//!     `ls`/`list` help text's existing documented contract).
//!   - An explicit `--group`/`--folder` (or, for `folder`, `xv update
//!     --clear-folder`, which doesn't consult the profile at all) always
//!     wins over the profile default.
//!   - `xv set` and `xv gen --save` share the exact same defaulting logic
//!     (`apply_profile_write_defaults` in `src/cli/secret_ops.rs`), so both
//!     construct identical requests from the same effective metadata.
//!   - A blank `group = ""` / `folder = ""` in the profile is treated as "no
//!     default", not a real (empty, unfilterable) value.
//!
//! Hermetic: every test runs through `common::xv_isolated_local_with_profile`
//! (`env_clear()` + explicit allowlist + `XV_NO_PARENT_CONFIG=1`, per the
//! #317 lesson) — no host env vars (`DEBUG`, `CACHE_TTL`,
//! `AZURE_CREDENTIAL_PRIORITY`, `BLOB_*`, ...) leak into the child, and
//! `.xv.toml` walk-up can't escape the isolated project dir. No Azure
//! credentials or network access required.
//!
//! Run with:
//!   cargo test --test e2e_env_profile_defaults

mod common;

use common::xv_isolated_local_with_profile;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

/// `.xv.toml` profile shared by most tests: single `dev` env with both a
/// `group` and `folder` default set.
const PROFILE_TOML: &str = r#"default_env = "dev"

[env.dev]
vault = "default"
group = "web"
folder = "app"
"#;

struct ProfileEnv {
    _tmp: TempDir,
    config_dir: PathBuf,
    project_dir: PathBuf,
}

impl ProfileEnv {
    /// Isolated env: local backend, global `xv.conf`, and a `.xv.toml`
    /// (`xv_toml`) written into the project dir that becomes the command's
    /// cwd. Config/store setup is delegated to
    /// `common::xv_isolated_local_with_profile` so it stays in lockstep with
    /// the rest of the hermetic test suite; `xv()` below rebuilds a fresh
    /// `Command` per call (a spawned `Command` can't be reused) using the
    /// exact same env-isolation recipe.
    fn new(xv_toml: &str) -> Self {
        let (_first_cmd, tmp) = xv_isolated_local_with_profile(xv_toml);
        let config_dir = tmp.path().join(".config");
        let project_dir = tmp.path().join("project");
        Self {
            _tmp: tmp,
            config_dir,
            project_dir,
        }
    }

    fn tmp_path(&self) -> &Path {
        self._tmp.path()
    }

    /// Return a `Command` pre-configured for this test environment, with
    /// cwd set to the project dir so `.xv.toml` walk-up finds it directly.
    /// Fully hermetic: `env_clear()` plus an explicit allowlist, matching
    /// `common::xv_isolated_local_with_profile` / `common::isolate`.
    fn xv(&self) -> Command {
        let mut cmd = common::xv();
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self._tmp.path())
            .env("XDG_CONFIG_HOME", &self.config_dir)
            .env("XV_NO_PARENT_CONFIG", "1")
            .env("XV_BACKEND", "local")
            .env("NO_COLOR", "1")
            .current_dir(&self.project_dir);
        cmd
    }

    fn xv_ok(&self, args: &[&str]) -> String {
        let output = self.xv().args(args).output().expect("execute xv binary");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            output.status.success(),
            "xv {:?} failed (exit {:?}):\nstdout: {}\nstderr: {}",
            args,
            output.status.code(),
            stdout,
            stderr,
        );
        stdout
    }

    fn xv_fail(&self, args: &[&str]) -> (String, String) {
        let output = self.xv().args(args).output().expect("execute xv binary");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            !output.status.success(),
            "xv {:?} should have failed but succeeded:\nstdout: {}\nstderr: {}",
            args,
            stdout,
            stderr,
        );
        (stdout, stderr)
    }

    /// Set a secret via stdin piping, with extra CLI args. Returns stdout on
    /// success.
    fn set_secret_with_args(&self, name: &str, value: &str, extra: &[&str]) -> String {
        let mut cmd = self.xv();
        cmd.args(["set", name, "--stdin"]);
        cmd.args(extra);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn xv set");
        child
            .stdin
            .take()
            .expect("child stdin")
            .write_all(value.as_bytes())
            .expect("write stdin");
        let output = child.wait_with_output().expect("wait xv set");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            output.status.success(),
            "xv set {name} failed (exit {:?}):\nstdout: {}\nstderr: {}",
            output.status.code(),
            stdout,
            stderr,
        );
        stdout
    }

    fn set_secret(&self, name: &str, value: &str) -> String {
        self.set_secret_with_args(name, value, &[])
    }

    /// `xv gen --save NAME [extra...]`. Returns stdout on success.
    fn gen_save_with_args(&self, name: &str, extra: &[&str]) -> String {
        let mut args = vec!["gen", "--raw", "--save", name];
        args.extend_from_slice(extra);
        self.xv_ok(&args)
    }
}

// ===========================================================================
// `xv set` — folder default (#308)
// ===========================================================================

#[test]
fn set_without_folder_flag_uses_profile_folder_default() {
    let env = ProfileEnv::new(PROFILE_TOML);
    env.set_secret("APP_SECRET", "v1");

    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(
        json.contains("\"app\""),
        "profile folder default ('app') not applied to secret with no --folder: {json}"
    );

    let scoped = env.xv_ok(&["ls", "app", "--names-only"]);
    assert!(
        scoped.contains("APP_SECRET"),
        "secret not listed under the profile's default folder: {scoped}"
    );
}

#[test]
fn set_with_explicit_folder_overrides_profile_default() {
    let env = ProfileEnv::new(PROFILE_TOML);
    env.set_secret_with_args("OTHER_SECRET", "v1", &["--folder", "other"]);

    let scoped_other = env.xv_ok(&["ls", "other", "--names-only"]);
    assert!(
        scoped_other.contains("OTHER_SECRET"),
        "explicit --folder should win: {scoped_other}"
    );

    let scoped_app = env.xv_ok(&["ls", "app", "--names-only"]);
    assert!(
        !scoped_app.contains("OTHER_SECRET"),
        "profile folder default should not apply when --folder was given: {scoped_app}"
    );
}

#[test]
fn clear_folder_on_update_not_resurrected_by_profile_default() {
    let env = ProfileEnv::new(PROFILE_TOML);
    env.set_secret_with_args("CLEAR_ME", "v1", &["--folder", "explicit"]);
    env.xv_ok(&["update", "CLEAR_ME", "--clear-folder"]);

    // The profile's folder default ('app') must not resurrect a folder the
    // user just explicitly cleared via `xv update --clear-folder`.
    let scoped_app = env.xv_ok(&["ls", "app", "--names-only"]);
    assert!(
        !scoped_app.contains("CLEAR_ME"),
        "cleared folder must not be resurrected by the profile default: {scoped_app}"
    );
    let scoped_explicit = env.xv_ok(&["ls", "explicit", "--names-only"]);
    assert!(
        !scoped_explicit.contains("CLEAR_ME"),
        "old explicit folder should no longer contain the secret: {scoped_explicit}"
    );

    // The secret should now be visible at the vault root (no folder).
    let root = env.xv_ok(&["ls", "--names-only"]);
    assert!(
        root.contains("CLEAR_ME"),
        "secret with cleared folder should appear at vault root: {root}"
    );
}

// ===========================================================================
// `xv gen --save` — shares `apply_profile_write_defaults` with `xv set`
// ===========================================================================

#[test]
fn gen_save_without_flags_uses_profile_group_and_folder_defaults() {
    let env = ProfileEnv::new(PROFILE_TOML); // group = "web", folder = "app"
    env.gen_save_with_args("GEN_SECRET", &[]);

    let json = env.xv_ok(&["ls", "app", "--format", "json"]);
    assert!(
        json.contains("GEN_SECRET"),
        "gen --save should land in the profile's default folder: {json}"
    );

    let out_file = env.tmp_path().join("gen_save_run_env.txt");
    let status = env
        .xv()
        .args([
            "run",
            "--",
            "sh",
            "-c",
            &format!("env | sort > '{}'", out_file.display()),
        ])
        .status()
        .expect("execute xv run");
    assert!(status.success(), "xv run should succeed");
    let contents = std::fs::read_to_string(&out_file).expect("read child env dump");
    assert!(
        contents.contains("GEN_SECRET"),
        "gen --save should pick up the profile's default group (web), matching `xv run`'s \
         default filter: {contents}"
    );
}

#[test]
fn gen_save_with_explicit_flags_overrides_profile_defaults() {
    let env = ProfileEnv::new(PROFILE_TOML); // group = "web", folder = "app"
    env.gen_save_with_args("GEN_OTHER", &["--group", "backend", "--folder", "other"]);

    let scoped_other = env.xv_ok(&["ls", "other", "--names-only"]);
    assert!(
        scoped_other.contains("GEN_OTHER"),
        "explicit --folder should win for gen --save: {scoped_other}"
    );
    let scoped_app = env.xv_ok(&["ls", "app", "--names-only"]);
    assert!(
        !scoped_app.contains("GEN_OTHER"),
        "profile folder default should not apply when --folder was given to gen --save: {scoped_app}"
    );

    let out_file = env.tmp_path().join("gen_save_explicit_run_env.txt");
    let status = env
        .xv()
        .args([
            "run",
            "--group",
            "backend",
            "--",
            "sh",
            "-c",
            &format!("env | sort > '{}'", out_file.display()),
        ])
        .status()
        .expect("execute xv run");
    assert!(status.success(), "xv run should succeed");
    let contents = std::fs::read_to_string(&out_file).expect("read child env dump");
    assert!(
        contents.contains("GEN_OTHER"),
        "explicit --group backend on gen --save should be injected by run --group backend: {contents}"
    );
}

// ===========================================================================
// `xv run` — group default (#308)
// ===========================================================================

#[test]
fn run_without_group_flag_uses_profile_group_default_as_filter() {
    let env = ProfileEnv::new(PROFILE_TOML); // group = "web"
    env.set_secret_with_args("WEB_SECRET", "v1", &["--group", "web"]);
    env.set_secret_with_args("OTHER_SECRET", "v2", &["--group", "backend"]);

    let out_file = env.tmp_path().join("run_env_default.txt");
    let status = env
        .xv()
        .args([
            "run",
            "--",
            "sh",
            "-c",
            &format!("env | sort > '{}'", out_file.display()),
        ])
        .status()
        .expect("execute xv run");
    assert!(status.success(), "xv run should succeed");

    let contents = std::fs::read_to_string(&out_file).expect("read child env dump");
    assert!(
        contents.contains("WEB_SECRET"),
        "profile group default should inject group=web secrets: {contents}"
    );
    assert!(
        !contents.contains("OTHER_SECRET"),
        "profile group default should filter out non-matching group secrets: {contents}"
    );
}

#[test]
fn run_with_explicit_group_flag_overrides_profile_default() {
    let env = ProfileEnv::new(PROFILE_TOML); // group = "web"
    env.set_secret_with_args("WEB_SECRET", "v1", &["--group", "web"]);
    env.set_secret_with_args("OTHER_SECRET", "v2", &["--group", "backend"]);

    let out_file = env.tmp_path().join("run_env_explicit.txt");
    let status = env
        .xv()
        .args([
            "run",
            "--group",
            "backend",
            "--",
            "sh",
            "-c",
            &format!("env | sort > '{}'", out_file.display()),
        ])
        .status()
        .expect("execute xv run");
    assert!(status.success(), "xv run should succeed");

    let contents = std::fs::read_to_string(&out_file).expect("read child env dump");
    assert!(
        contents.contains("OTHER_SECRET"),
        "explicit --group backend should win over the profile default: {contents}"
    );
    assert!(
        !contents.contains("WEB_SECRET"),
        "explicit --group backend should exclude group=web secrets: {contents}"
    );
}

// ===========================================================================
// Blank profile values (`group = ""` / `folder = ""`) are treated as absent
// ===========================================================================

const BLANK_PROFILE_TOML: &str = r#"default_env = "dev"

[env.dev]
vault = "default"
group = ""
folder = ""
"#;

#[test]
fn blank_profile_folder_is_not_written_as_an_empty_tag() {
    let env = ProfileEnv::new(BLANK_PROFILE_TOML);
    env.set_secret("ROOT_SECRET", "v1");

    // A blank `folder = ""` must resolve to `None`, not `Some("")`. Checked
    // against the raw JSON field (not just `ls` display grouping, which
    // treats an empty-string folder and a `null` folder identically at the
    // root level and would mask this regression either way).
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(
        json.contains("\"folder\": null"),
        "blank profile folder default should resolve to no folder at all, not an empty-string tag: {json}"
    );
    assert!(
        !json.contains("\"folder\": \"\""),
        "blank profile folder default must not be written as an empty-string folder tag: {json}"
    );
}

#[test]
fn blank_profile_group_is_not_written_as_an_empty_tag() {
    let env = ProfileEnv::new(BLANK_PROFILE_TOML);
    env.set_secret("ROOT_SECRET", "v1");

    // Same blank-is-absent check on the write side for `group`, checked
    // against the raw JSON field.
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(
        json.contains("\"groups\": null"),
        "blank profile group default should resolve to no group at all, not an empty-string tag: {json}"
    );
    assert!(
        !json.contains("\"groups\": \"\""),
        "blank profile group default must not be written as an empty-string group tag: {json}"
    );
}

#[test]
fn blank_profile_group_is_not_used_as_run_filter() {
    let env = ProfileEnv::new(BLANK_PROFILE_TOML);
    // Explicit --group bypasses the write-time default entirely, so this
    // secret's own tag can't accidentally self-cancel against a buggy
    // read-side resolution (an unpatched resolver would tag *unflagged*
    // writes with group="" too, which would coincidentally match an
    // equally-buggy groups=[""] run filter and mask the regression).
    env.set_secret_with_args("SOME_SECRET", "v1", &["--group", "unrelated-group"]);

    // A blank `group = ""` must not become an unfilterable `groups=[""]`
    // selector that trips `xv run`'s fail-loud empty-selection check with a
    // group the user never typed.
    let out_file = env.tmp_path().join("blank_group_run_env.txt");
    let status = env
        .xv()
        .args([
            "run",
            "--",
            "sh",
            "-c",
            &format!("env | sort > '{}'", out_file.display()),
        ])
        .status()
        .expect("execute xv run");
    assert!(
        status.success(),
        "xv run should succeed with no usable group filter (blank profile group treated as absent)"
    );
    let contents = std::fs::read_to_string(&out_file).expect("read child env dump");
    assert!(
        contents.contains("SOME_SECRET"),
        "with no effective group filter, all secrets should be injected regardless of their own group: {contents}"
    );
}

// ===========================================================================
// `xv ls` — NOT scoped by the profile's write-side folder default
// ===========================================================================

#[test]
fn ls_default_view_is_not_scoped_by_profile_folder_default() {
    let env = ProfileEnv::new(PROFILE_TOML); // folder = "app"
                                             // Lands in "app" via the profile's write-time folder default.
    env.set_secret("DEFAULT_FOLDER_SECRET", "v1");
    // Explicitly placed outside the profile's folder default.
    env.set_secret_with_args("OTHER_FOLDER_SECRET", "v2", &["--folder", "other"]);

    // Root `xv ls` (no FOLDER positional) must show both folders — the
    // write-side folder default must not implicitly narrow the listing to
    // just its own folder.
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("\"app\""), "'app' folder missing: {json}");
    assert!(
        json.contains("\"other\""),
        "profile folder default incorrectly scoped `ls`, hiding 'other': {json}"
    );
}

// ===========================================================================
// `xv run` — fail-loud error attributes the profile default (#308 review)
// ===========================================================================

#[test]
fn run_fail_loud_error_attributes_profile_group_default() {
    let env = ProfileEnv::new(PROFILE_TOML); // group = "web"
                                             // No secrets set at all — the group=web filter (from the profile) will
                                             // match nothing, tripping the fail-loud empty-selection error.
    let (_stdout, stderr) = env.xv_fail(&["run", "--", "true"]);
    assert!(
        stderr.contains("from env profile default"),
        "fail-loud error should attribute the unmatched group to the profile default: {stderr}"
    );
}

// ===========================================================================
// #331: a `.xv.toml` with only a `[types.*]` block — zero `[env.*]` tables,
// no `default_env` — must not break env-dependent resolution. `xv list`
// already worked pre-fix; `xv set`/`xv run`/`xv get` did not, because the
// #320 write-default resolvers (`resolve_group`/`resolve_folder` in
// `src/config/settings.rs`) propagated `resolve_env`'s error even for the
// "this file defines no envs at all" case. A file that DOES define envs
// must keep erroring exactly as before on an unknown/explicit named `--env`.
// ===========================================================================

/// Types-only project file: no `[env.*]`, no `default_env`, just a custom
/// record type. This is a legitimate shape since record types (#321).
const TYPES_ONLY_TOML: &str = r#"[types.smtp]
fields = [
  { name = "host" },
  { name = "username", required = true },
  { name = "password", kind = "secret", primary = true },
]
"#;

#[test]
fn types_only_xv_toml_allows_set_list_get_run() {
    let env = ProfileEnv::new(TYPES_ONLY_TOML);

    // `xv set` (untyped) — this is the exact repro from #331.
    env.set_secret("mailer_untyped", "pw1");

    // `xv set --type <custom>` — the project's own custom type must be
    // usable too (not just built-ins), proving type resolution and
    // env-default resolution both work together on a types-only file.
    env.xv_ok(&[
        "set",
        "mailer",
        "--type",
        "smtp",
        "--field",
        "username=m@x.com",
        "--value",
        "pw2",
    ]);

    // `xv list` (already worked pre-fix, kept as a control).
    let json = env.xv_ok(&["list", "--format", "json"]);
    assert!(json.contains("mailer_untyped"), "list output: {json}");
    assert!(json.contains("mailer"), "list output: {json}");

    // `xv get` for both the untyped and the typed secret.
    let out = env.xv_ok(&["get", "mailer_untyped", "--raw"]);
    assert_eq!(out.trim(), "pw1");
    let out = env.xv_ok(&["get", "mailer", "--raw"]);
    assert_eq!(out.trim(), "pw2");

    // `xv run` — exercises the `resolve_group` write-default path too.
    let out_file = env.tmp_path().join("types_only_run_env.txt");
    let status = env
        .xv()
        .args([
            "run",
            "--",
            "sh",
            "-c",
            &format!("env | sort > '{}'", out_file.display()),
        ])
        .status()
        .expect("execute xv run");
    assert!(
        status.success(),
        "xv run should succeed on a types-only project file"
    );
    let contents = std::fs::read_to_string(&out_file).expect("read child env dump");
    assert!(
        contents.contains("MAILER_UNTYPED") || contents.contains("mailer_untyped"),
        "types-only project file should not block secret injection: {contents}"
    );
}

#[test]
fn types_only_xv_toml_unknown_env_still_errors_when_file_has_envs() {
    // Guard: a `.xv.toml` that DOES define envs must keep erroring exactly
    // as before on an unknown/explicit `--env` — only the "zero envs at
    // all" case is allowed to pass silently.
    let env = ProfileEnv::new(PROFILE_TOML); // defines [env.dev] only
    let (_stdout, stderr) = env.xv_fail(&["--env", "staging", "set", "mailer", "--value", "pw"]);
    assert!(
        stderr.contains("staging"),
        "error should mention the requested env name: {stderr}"
    );
    assert!(
        stderr.contains("dev"),
        "error should list the available envs: {stderr}"
    );

    let output = env
        .xv()
        .args(["--env", "staging", "set", "mailer", "--value", "pw"])
        .output()
        .expect("execute xv binary");
    assert_eq!(output.status.code(), Some(3), "unknown --env must exit 3");
}

#[test]
fn types_only_xv_toml_explicit_env_flag_errors_with_no_environments_message() {
    // An explicit `--env` against a file that defines *zero* environments
    // must still error (the user asked for a specific env by name) — but
    // with a message that says the file defines no environments, not a
    // rough "not defined; available: " with an empty list.
    let env = ProfileEnv::new(TYPES_ONLY_TOML);
    let (_stdout, stderr) = env.xv_fail(&["--env", "staging", "set", "mailer", "--value", "pw"]);
    assert!(
        stderr.contains("no environments") || stderr.contains("no [env"),
        "error should explain the file defines no environments: {stderr}"
    );
    assert!(
        !stderr.contains("available: \""),
        "error should not print a rough empty-quoted available list: {stderr}"
    );

    let output = env
        .xv()
        .args(["--env", "staging", "set", "mailer", "--value", "pw"])
        .output()
        .expect("execute xv binary");
    assert_eq!(
        output.status.code(),
        Some(3),
        "explicit --env against a no-envs file must still exit 3"
    );
}
