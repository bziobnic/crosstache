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
//!
//! Hermetic: every test uses its own isolated config dir, local
//! (age-encrypted) backend store, and project directory containing a
//! `.xv.toml`. No Azure credentials or network access required.
//!
//! Run with:
//!   cargo test --test e2e_env_profile_defaults

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
    /// cwd.
    fn new(xv_toml: &str) -> Self {
        let tmp = TempDir::new().expect("create temp dir");
        let config_dir = tmp.path().join("config");
        let store_dir = tmp.path().join("store");
        let key_file = tmp.path().join("key.txt");
        let xv_dir = config_dir.join("xv");
        let project_dir = tmp.path().join("project");

        std::fs::create_dir_all(&xv_dir).expect("create config dir");
        std::fs::create_dir_all(&store_dir).expect("create store dir");
        std::fs::create_dir_all(&project_dir).expect("create project dir");

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
            store = store_dir.display(),
            key = key_file.display(),
        );
        std::fs::write(xv_dir.join("xv.conf"), config_content).expect("write config");
        std::fs::write(project_dir.join(".xv.toml"), xv_toml).expect("write .xv.toml");

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
    fn xv(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_xv"));
        cmd.env("XDG_CONFIG_HOME", &self.config_dir);
        cmd.env("XV_BACKEND", "local");
        cmd.env_remove("AZURE_SUBSCRIPTION_ID");
        cmd.env_remove("AZURE_TENANT_ID");
        cmd.env_remove("DEFAULT_VAULT");
        cmd.env_remove("XV_ENV");
        cmd.env("NO_COLOR", "1");
        cmd.current_dir(&self.project_dir);
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
