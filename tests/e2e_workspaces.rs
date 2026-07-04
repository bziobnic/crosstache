//! Hermetic end-to-end tests for multi-vault workspaces (Phase A).
//!
//! Two independent **local** (age-encrypted file) backends —
//! `[named_backends.local-a]` / `[named_backends.local-b]` — give a real
//! cross-backend workspace with zero cloud dependencies (spec §Testing).
//! Every test uses `env_clear()` + an explicit allowlist (the #317 lesson:
//! selective `env_remove()` leaks host vars into the child), so nothing here
//! reads or writes the host's real config/state.
//!
//! NOTE (Task 4 history): the `get`/`set` tests below originally had to seed
//! workspace state by writing the context JSON file directly, since `xv cx
//! add` didn't exist yet. Task 5 landed `cx add`/`rm`/`default`/`ls`, so
//! every test here now goes through the real CLI surface end-to-end.

use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// A hermetic environment with two independent local-backend stores
/// registered as named backends `local-a` and `local-b`, plus a private
/// global context directory. Neither backend is the top-level active
/// backend by default (top-level stays `"local"`, matching the store-less
/// default single-vault path) — `local-a`/`local-b` exist purely as
/// `named_backends` entries for `cx add --backend local-a|local-b`.
struct WorkspaceEnv {
    _tmp: TempDir,
    home: PathBuf,
    config_dir: PathBuf,
    /// Dedicated, hermetic `XV_CACHE_DIR` for this environment — always set
    /// (even when the config disables caching) so no test can ever read or
    /// write the real OS cache directory.
    cache_dir: PathBuf,
}

impl WorkspaceEnv {
    fn new() -> Self {
        Self::with_cache(false, 0)
    }

    /// Same hermetic two-local-backend setup as [`Self::new`], but with the
    /// on-disk secrets-list cache ENABLED (`cache_enabled = true`) and a
    /// generous `ttl_secs` — for tests that must observe cache
    /// hit/invalidate behavior (Bugbot review MINOR: every other
    /// `WorkspaceEnv` test runs with caching disabled, so a cache-keying
    /// bug like the alias-vs-kind mismatch this fix addresses would never
    /// surface here otherwise).
    fn with_cache_enabled(ttl_secs: u64) -> Self {
        Self::with_cache(true, ttl_secs)
    }

    fn with_cache(cache_enabled: bool, ttl_secs: u64) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().join("home");
        let config_dir = home.join(".config");
        let xv_dir = config_dir.join("xv");
        let store_default = tmp.path().join("default").join("store");
        let store_a = tmp.path().join("a").join("store");
        let store_b = tmp.path().join("b").join("store");
        let cache_dir = tmp.path().join("cache");
        // Each backend's key file must live in its OWN directory: the local
        // backend derives `recipients_file` from `key_file.parent()`
        // (src/backend/local/config.rs), so key files sharing a parent
        // directory would collide on the same recipients.txt and silently
        // cross-contaminate encryption identities across stores.
        let key_default = tmp.path().join("default").join("key.txt");
        let key_a = tmp.path().join("a").join("key.txt");
        let key_b = tmp.path().join("b").join("key.txt");

        std::fs::create_dir_all(&xv_dir).expect("create config dir");
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");
        for d in [&store_default, &store_a, &store_b] {
            std::fs::create_dir_all(d).expect("create store dir");
        }

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
cache_enabled = {cache_enabled}
cache_ttl_secs = {ttl_secs}
clipboard_timeout = 0

[local]
store_path = "{store_default}"
key_file = "{key_default}"
default_vault = "default"

[named_backends.local-a]
type = "local"
store_path = "{store_a}"
key_file = "{key_a}"
default_vault = "default"

[named_backends.local-b]
type = "local"
store_path = "{store_b}"
key_file = "{key_b}"
default_vault = "default"

[named_backends.aws-mock]
type = "aws"
region = "us-east-1"
# No real AWS calls are ever made against this entry in tests that don't
# need one — endpoint_url only needs to be syntactically present so client
# construction doesn't reach for real credentials at build time (mirrors
# tests/aws_backend_tests.rs's workspace_touching_only_aws_never_builds_azure).
endpoint_url = "http://127.0.0.1:1"
default_vault = "default"
"#,
            store_default = store_default.display(),
            key_default = key_default.display(),
            store_a = store_a.display(),
            key_a = key_a.display(),
            store_b = store_b.display(),
            key_b = key_b.display(),
        );
        std::fs::write(xv_dir.join("xv.conf"), config_content).expect("write config");

        Self {
            _tmp: tmp,
            home,
            config_dir,
            cache_dir,
        }
    }

    /// A hermetic `xv` command: `env_clear()` + an explicit allowlist, cwd
    /// pinned to a fresh empty scratch dir so no ancestor `.xv.toml` is ever
    /// picked up unless the test explicitly writes one for that purpose.
    fn xv(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_xv"));
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_dir)
            .env("XV_NO_PARENT_CONFIG", "1")
            .env("XV_BACKEND", "local")
            .env("NO_COLOR", "1")
            // Always hermetic, even when the config disables caching: no
            // test may ever read/write the real OS cache directory.
            .env("XV_CACHE_DIR", &self.cache_dir)
            .current_dir(&self.home);
        cmd
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.xv().args(args).output().expect("execute xv binary")
    }

    /// Like `xv()`, but with the top-level active backend overridden to
    /// `backend_name` via `XV_BACKEND` (clap env-populates `config.backend`
    /// from it) — for tests that need a NAMED backend (e.g. `"local-a"`)
    /// active with NO workspace attached, reusing this environment's
    /// existing `[named_backends.local-a]`/`[named_backends.local-b]`
    /// entries (Bugbot review round 3: no-workspace cache keys must
    /// converge on `config.effective_backend_name()`, which is exactly what
    /// this override exercises).
    fn xv_with_backend(&self, backend_name: &str) -> Command {
        let mut cmd = self.xv();
        cmd.env("XV_BACKEND", backend_name);
        cmd
    }

    fn run_with_backend(&self, backend_name: &str, args: &[&str]) -> std::process::Output {
        self.xv_with_backend(backend_name)
            .args(args)
            .output()
            .expect("execute xv binary")
    }

    fn ok_with_backend(&self, backend_name: &str, args: &[&str]) -> String {
        let out = self.run_with_backend(backend_name, args);
        assert!(
            out.status.success(),
            "command {args:?} (backend {backend_name}) failed.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Like `xv()`, but overrides the working directory — for tests that
    /// need a project subdirectory (with its own `.xv.toml`) while keeping
    /// the SAME global context store (`XDG_CONFIG_HOME` is unchanged).
    fn xv_in(&self, cwd: &std::path::Path) -> Command {
        let mut cmd = self.xv();
        cmd.current_dir(cwd);
        cmd
    }

    fn run_in(&self, cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
        self.xv_in(cwd)
            .args(args)
            .output()
            .expect("execute xv binary")
    }

    /// Path to the global context store file this environment's `cx`
    /// commands read/write (`ContextManager::global_context_path()`).
    fn context_file_path(&self) -> PathBuf {
        self.config_dir.join("xv").join("context")
    }

    /// Raw bytes of the context store file, for byte-identity assertions
    /// (empty `Vec` if the file doesn't exist yet).
    fn context_file_bytes(&self) -> Vec<u8> {
        std::fs::read(self.context_file_path()).unwrap_or_default()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "command {args:?} failed.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn err(&self, args: &[&str]) -> std::process::Output {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "command {args:?} unexpectedly succeeded.\nstdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        out
    }

    /// Like `ok`, but returns stdout+stderr combined — `success`/`info`/
    /// `hint` messages are written to stderr (stdout is reserved for
    /// machine-consumable payloads), so assertions on human-readable
    /// confirmation text need both streams.
    #[allow(dead_code)]
    fn ok_combined(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "command {args:?} failed.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        combined(&out)
    }
}

fn combined(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// ---------------------------------------------------------------------------
// `xv cx` surface (Task 5)
// ---------------------------------------------------------------------------

#[test]
fn cx_add_ls_rm_roundtrip() {
    let env = WorkspaceEnv::new();

    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    let ls = env.ok(&["cx", "ls"]);
    assert!(ls.contains("work"), "{ls}");
    assert!(ls.contains("stage"), "{ls}");
    assert!(ls.contains("local-a"), "{ls}");
    assert!(ls.contains("local-b"), "{ls}");

    env.ok(&["cx", "rm", "stage"]);
    let ls2 = env.ok(&["cx", "ls"]);
    assert!(!ls2.contains("stage"), "{ls2}");
    assert!(ls2.contains("work"), "{ls2}");
}

#[test]
fn cx_first_add_becomes_default() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let ls = env.ok(&["cx", "ls"]);
    // Single entry in this workspace, so the default marker ("*") appearing
    // anywhere in the output must be "work"'s.
    assert!(ls.contains("work"), "{ls}");
    assert!(ls.contains('*'), "{ls}");
}

#[test]
fn cx_add_duplicate_alias_errors() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let out = env.err(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "work",
    ]);
    let msg = combined(&out);
    assert!(msg.contains("work"), "{msg}");
}

#[test]
fn cx_add_alias_colliding_with_backend_name_errors() {
    let env = WorkspaceEnv::new();
    let out = env.err(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "azure",
    ]);
    let msg = combined(&out);
    assert!(msg.contains("azure") || msg.contains("collid"), "{msg}");
}

#[test]
fn cx_rm_default_requires_replacement() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    // "work" is the default (first-add); removing it while "stage" also
    // exists must error rather than silently reassigning the default.
    let out = env.err(&["cx", "rm", "work"]);
    let msg = combined(&out);
    assert!(msg.contains("default"), "{msg}");

    // After making "stage" the default, removing "work" must succeed.
    env.ok(&["cx", "default", "stage"]);
    env.ok(&["cx", "rm", "work"]);
    let ls = env.ok(&["cx", "ls"]);
    assert!(!ls.contains("work"), "{ls}");
}

#[test]
fn cx_rm_last_entry_restores_single_vault_behavior() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&["cx", "rm", "work"]);
    let ls_out = env.run(&["cx", "ls"]);
    assert!(ls_out.status.success());
    let msg = combined(&ls_out);
    assert!(
        msg.contains("No workspace attached") || msg.contains("single-vault"),
        "{msg}"
    );
}

#[test]
fn context_use_with_workspace_errors_pointing_at_cx_default() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let out = env.err(&["context", "use", "some-vault"]);
    let msg = combined(&out);
    assert!(msg.contains("cx default"), "{msg}");
}

/// Bugbot MEDIUM fix: `cx add`/`rm`/`default` must ERROR (not silently
/// no-op) when the cwd's active `.xv.toml` env profile declares a `vaults`
/// overlay — that overlay REPLACES the context workspace entirely per
/// `resolve_workspace`'s precedence, so a context-store mutation here
/// would persist and report success while every secret command kept using
/// the project overlay instead.
fn write_vaults_overlay(project_dir: &std::path::Path) {
    std::fs::create_dir_all(project_dir).expect("create project dir");
    std::fs::write(
        project_dir.join(".xv.toml"),
        r#"
default_env = "dev"

[env.dev]
vault = "ignored"
vaults = [
  { vault = "proj-vault", backend = "local-b", alias = "proj", default = true },
]
"#,
    )
    .expect("write .xv.toml");
}

/// A `.xv.toml` that DEFINES `[env.*]` blocks but selects none (no
/// `default_env`, and the test never passes `--env`/`XV_ENV`) — the
/// fail-closed case `Config::resolve_vault_name` already handles via
/// `resolve_env`'s `Err`, which `resolve_workspace_from` must now
/// propagate too (Bugbot round-4 fix) instead of silently falling through
/// to the personal context workspace.
fn write_unselected_envs_toml(project_dir: &std::path::Path) {
    std::fs::create_dir_all(project_dir).expect("create project dir");
    std::fs::write(
        project_dir.join(".xv.toml"),
        r#"
[env.dev]
vault = "proj-vault"
resource_group = "rg"
"#,
    )
    .expect("write .xv.toml");
}

/// A `.xv.toml` that defines ZERO `[env.*]` blocks at all — the post-#331
/// "types-only project file" shape. `resolve_env` returns `Ok(None)` for
/// this (not an `Err`), so workspace resolution must still fall through to
/// the context workspace normally.
fn write_no_envs_toml(project_dir: &std::path::Path) {
    std::fs::create_dir_all(project_dir).expect("create project dir");
    std::fs::write(
        project_dir.join(".xv.toml"),
        "# types-only project file (post-#331 shape): zero [env.*] blocks\n",
    )
    .expect("write .xv.toml");
}

/// Bugbot round-4 MEDIUM fix: a project `.xv.toml` that defines `[env.*]`
/// blocks but has none selected must fail closed (`EnvNotDefined`, exit 3)
/// for EVERY secret command — not just `Config::resolve_vault_name`'s
/// existing consumers, but workspace resolution too. Before the fix,
/// `resolve_workspace_from` treated `resolve_env`'s error as "no project
/// overlay" and silently fell through to the personal context workspace.
#[test]
fn workspace_resolution_fails_closed_when_project_envs_unselected() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    // Prove the personal workspace is real and reachable outside the
    // project directory, so a later "did it silently use this?" check
    // means something.
    env.ok(&["set", "work:SHARED_NAME", "--value", "personal-value"]);

    let project_dir = env.home.join("project-unselected");
    write_unselected_envs_toml(&project_dir);

    // No default_env, no --env, no XV_ENV: `.xv.toml` defines "dev" but
    // selects nothing.
    let out = env.run_in(&project_dir, &["get", "SHARED_NAME"]);
    assert!(!out.status.success(), "{}", combined(&out));
    assert_eq!(out.status.code(), Some(3), "{}", combined(&out));
    let msg = combined(&out);
    assert!(
        msg.contains("dev"),
        "error should name the available env(s): {msg}"
    );
    // Must NOT be any workspace-shaped outcome (not-found=10, ambiguous=13,
    // or a silent success returning the personal vault's value) — it must
    // be the config-family EnvNotDefined failure, before workspace
    // resolution ever gets to search or target the personal context.
    assert_ne!(out.status.code(), Some(10));
    assert_ne!(out.status.code(), Some(13));

    // Explicit `--env missing` (a name that isn't defined) must fail the
    // same way.
    let out2 = env.run_in(&project_dir, &["--env", "missing", "get", "SHARED_NAME"]);
    assert!(!out2.status.success(), "{}", combined(&out2));
    assert_eq!(out2.status.code(), Some(3), "{}", combined(&out2));
}

/// Guard: a `.xv.toml` with zero `[env.*]` blocks (post-#331 types-only
/// shape) must NOT trip the new fail-closed guard — `resolve_env` returns
/// `Ok(None)` for this case, so workspace resolution correctly falls
/// through to the context workspace, same as with no `.xv.toml` at all.
#[test]
fn types_or_no_envs_toml_still_falls_through_to_context_workspace() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);

    let project_dir = env.home.join("project-no-envs");
    write_no_envs_toml(&project_dir);

    let set_out = env.run_in(&project_dir, &["set", "work:HELLO", "--value", "v"]);
    assert!(set_out.status.success(), "{}", combined(&set_out));
    let get_out = env.run_in(&project_dir, &["get", "work:HELLO", "--raw"]);
    assert!(get_out.status.success(), "{}", combined(&get_out));
    assert_eq!(stdout_str(&get_out), "v");
}

#[test]
fn cx_add_errors_under_project_vaults_overlay() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let context_before = env.context_file_bytes();

    let project_dir = env.home.join("project");
    write_vaults_overlay(&project_dir);

    let out = env.run_in(
        &project_dir,
        &[
            "cx",
            "add",
            "default",
            "--backend",
            "local-b",
            "--as",
            "extra",
        ],
    );
    assert!(!out.status.success(), "{}", combined(&out));
    assert_eq!(out.status.code(), Some(3), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains(".xv.toml"), "{msg}");
    assert!(msg.contains("dev"), "{msg}");

    // The context store must be entirely UNCHANGED — the attempted
    // mutation must never reach disk.
    assert_eq!(env.context_file_bytes(), context_before);
}

#[test]
fn cx_rm_errors_under_project_vaults_overlay() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let context_before = env.context_file_bytes();

    let project_dir = env.home.join("project");
    write_vaults_overlay(&project_dir);

    let out = env.run_in(&project_dir, &["cx", "rm", "work"]);
    assert!(!out.status.success(), "{}", combined(&out));
    assert_eq!(out.status.code(), Some(3), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains(".xv.toml"), "{msg}");

    assert_eq!(env.context_file_bytes(), context_before);
}

#[test]
fn cx_default_errors_under_project_vaults_overlay() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    let context_before = env.context_file_bytes();

    let project_dir = env.home.join("project");
    write_vaults_overlay(&project_dir);

    let out = env.run_in(&project_dir, &["cx", "default", "stage"]);
    assert!(!out.status.success(), "{}", combined(&out));
    assert_eq!(out.status.code(), Some(3), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains(".xv.toml"), "{msg}");

    assert_eq!(env.context_file_bytes(), context_before);
}

/// Guard test: the same context store, from a cwd WITHOUT the overlay in
/// effect, must be entirely unaffected by the new guard — `cx add` (and by
/// extension rm/default) keeps working exactly as before outside the
/// project directory.
#[test]
fn cx_add_outside_project_still_works() {
    let env = WorkspaceEnv::new();
    // A project dir with a real vaults overlay exists on disk, but cwd
    // (env.home, the default for `env.ok`) is NOT it and has no ancestor
    // .xv.toml (XV_NO_PARENT_CONFIG=1 blocks walk-up regardless).
    let project_dir = env.home.join("project");
    write_vaults_overlay(&project_dir);

    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let ls = env.ok(&["cx", "ls"]);
    assert!(ls.contains("work"), "{ls}");
}

#[test]
fn cx_add_probes_vault_exists_unless_force() {
    let env = WorkspaceEnv::new();
    // "local-a" and "local-b" are both valid, reachable named backends —
    // the probe (a list call) must succeed against a real vault name even
    // if the vault is empty (local backend vaults are directories created
    // on demand, so this exercises the "reachable" path, not "must
    // pre-exist"). This mainly asserts --force is accepted and doesn't
    // itself break a normal add.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
        "--force",
    ]);
    let ls = env.ok(&["cx", "ls"]);
    assert!(ls.contains("work"), "{ls}");
}

// ---------------------------------------------------------------------------
// `get`/`set` workspace semantics (Task 4)
// ---------------------------------------------------------------------------

#[test]
fn cx_workspace_get_unqualified_unique_match() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:ONLY_IN_STAGE", "--value", "v1"]);
    let value = env.ok(&["get", "ONLY_IN_STAGE", "--raw"]);
    assert_eq!(value, "v1");
}

#[test]
fn get_ambiguous_errors_exit_13_lists_qualified_forms() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "work:DUP_NAME", "--value", "work-value"]);
    env.ok(&["set", "stage:DUP_NAME", "--value", "stage-value"]);

    let out = env.err(&["get", "DUP_NAME"]);
    assert_eq!(out.status.code(), Some(13), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains("work:DUP_NAME"), "{msg}");
    assert!(msg.contains("stage:DUP_NAME"), "{msg}");
}

#[test]
fn get_qualified_reads_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:API_KEY", "--value", "stage-secret"]);
    let value = env.ok(&["get", "stage:API_KEY", "--raw"]);
    assert_eq!(value, "stage-secret");
}

#[test]
fn get_unknown_alias_lists_attached() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    let out = env.err(&["get", "nope:SOMETHING"]);
    let msg = combined(&out);
    assert!(msg.contains("work"), "{msg}");
    assert!(msg.contains("stage"), "{msg}");
}

#[test]
fn set_unqualified_writes_default_only() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    // "work" is the default (first add).

    env.ok(&["set", "SHARED_NAME", "--value", "unqualified-value"]);

    // Must be readable directly from "work" (the default)...
    let work_value = env.ok(&["get", "work:SHARED_NAME", "--raw"]);
    assert_eq!(work_value, "unqualified-value");

    // ...and absent from "stage" — an unqualified write never searches.
    let stage_lookup = env.err(&["get", "stage:SHARED_NAME"]);
    assert!(!stage_lookup.status.success());
}

#[test]
fn set_qualified_writes_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:QUALIFIED_WRITE", "--value", "stage-only"]);
    let value = env.ok(&["get", "stage:QUALIFIED_WRITE", "--raw"]);
    assert_eq!(value, "stage-only");

    // Must NOT have landed in "work".
    let work_lookup = env.err(&["get", "work:QUALIFIED_WRITE"]);
    assert!(!work_lookup.status.success());
}

#[test]
fn exact_name_with_colon_wins_over_alias() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // A secret literally named "work:x" (colon included) written via a
    // qualified write targets the "work" vault's stored name "work:x"
    // itself only when addressed unambiguously; here we seed it through
    // the resolver's own exact-name-first path by writing to work with the
    // literal name, then reading the same literal string back.
    env.ok(&["set", "work:work:x", "--value", "literal-value"]);
    let value = env.ok(&["get", "work:x", "--raw"]);
    assert_eq!(value, "literal-value");
}

/// Bugbot MEDIUM fix: write-mode verbs (`update`/`delete`) must also apply
/// exact-name-first, scoped to the default vault — not just `set`.
///
/// How a literal `work:x` ends up in the default vault at all (since a
/// *qualified write* always wins on the fresh-secret path — there is no way
/// to `set` a literal `work:x` into the default vault once the `work`
/// alias is attached, because `xv set work:x` would target the `work`
/// vault's `x`, not create a literal in the default): this test creates
/// the literal BEFORE any workspace is attached (single-vault mode, where
/// "work:x" is just an ordinary — if colon-containing — name with no alias
/// interpretation at all), then attaches a workspace afterward whose
/// DEFAULT vault is that same pre-existing store under a different alias
/// ("home") while a SEPARATE alias ("work") points elsewhere. This is the
/// realistic scenario: pre-existing secrets predate the workspace; new
/// ones go through qualified addressing from the start.
#[test]
fn update_and_delete_exact_name_with_colon_wins_over_alias_in_default() {
    let env = WorkspaceEnv::new();

    // Single-vault mode: "work:x" is an ordinary literal name in the
    // top-level default store (the local backend's charset allows ':').
    env.ok(&["set", "work:x", "--value", "original-value"]);

    // Attach a workspace where "work" points elsewhere (local-a) but the
    // DEFAULT ("home") is the top-level store that already holds "work:x".
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local",
        "--as",
        "home",
        "--default",
    ]);

    // "work:x" must resolve the literal secret in the DEFAULT vault
    // ("home"), not alias-interpret "work" (-> local-a, path "x", which
    // doesn't exist there — `update_secret` on a nonexistent local-a
    // target would fail, so this `update` succeeding at all is itself
    // part of the proof).
    env.ok(&["update", "work:x", "updated-value"]);
    // Read back via "home:work:x": a QUALIFIED read ("home" is a real
    // alias) always targets that vault directly with the remainder as the
    // path, bypassing exact-name-first entirely — the cleanest way to
    // confirm placement without depending on read's own exact-name-first
    // (which would otherwise intercept a bare "work:x" query too, since
    // read searches every vault for literal matches first).
    assert_eq!(env.ok(&["get", "home:work:x", "--raw"]), "updated-value");

    env.ok(&["delete", "work:x", "--force"]);
    let deleted_lookup = env.err(&["get", "home:work:x"]);
    assert!(!deleted_lookup.status.success());
}

// ---------------------------------------------------------------------------
// No-workspace degenerate case: byte-identical to pre-workspace behavior.
// ---------------------------------------------------------------------------

/// True byte-golden comparison: no `cx add` is ever called in this test (the
/// workspace code path is provably inert — `resolve_workspace` sees no
/// context workspace and no `.xv.toml` overlay, so it returns `None` and
/// every verb takes the exact pre-workspace branch), so `set`/`get`'s full
/// stdout AND stderr must match these fixed golden strings byte-for-byte —
/// not just "contains a success-looking substring". A fresh hermetic store
/// makes both the vault name ("default") and the version ("v1", first
/// write) deterministic, so the golden is stable across runs.
#[test]
fn no_workspace_byte_identical() {
    let env = WorkspaceEnv::new();

    let set_result = env.run(&["set", "PLAIN_SECRET", "--value", "plain-value"]);
    assert!(set_result.status.success(), "{}", combined(&set_result));
    // `output::success`/`hint` go to stderr; the "Vault:"/"Version:" lines
    // are plain `println!` and land on stdout — both streams are pinned so
    // a future change that moves a line between them would fail loud here.
    assert_eq!(
        stdout_str(&set_result),
        "   Vault: default\n   Version: v1\n",
        "set's Vault/Version lines must be byte-identical to the pre-workspace golden"
    );
    assert_eq!(
        stderr_str(&set_result),
        "[ok] Successfully set secret 'PLAIN_SECRET'\n\
         [hint] Verify with 'xv get PLAIN_SECRET'\n",
        "set's human-readable confirmation output must be byte-identical to the pre-workspace golden"
    );

    let get_result = env.run(&["get", "PLAIN_SECRET", "--raw"]);
    assert!(get_result.status.success(), "{}", combined(&get_result));
    assert_eq!(stdout_str(&get_result), "plain-value");
    assert_eq!(
        stderr_str(&get_result),
        "",
        "get --raw must write nothing to stderr on success"
    );

    // Colon-looking input with no workspace attached must be treated as a
    // plain (unparsed) name — resolve_workspace returns None, so the
    // colon-address parser is never even consulted for this command.
    let colon_set = env.run(&["set", "literal:with:colons", "--value", "colon-value"]);
    assert!(colon_set.status.success(), "{}", combined(&colon_set));
    assert_eq!(
        stdout_str(&colon_set),
        "   Vault: default\n   Version: v1\n",
    );
    assert_eq!(
        stderr_str(&colon_set),
        "[ok] Successfully set secret 'literal:with:colons'\n\
         [hint] Verify with 'xv get literal:with:colons'\n",
    );
    let colon_get = env.run(&["get", "literal:with:colons", "--raw"]);
    assert!(colon_get.status.success(), "{}", combined(&colon_get));
    assert_eq!(stdout_str(&colon_get), "colon-value");
}

fn stdout_str(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_str(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Independent proof that the `no_workspace_byte_identical` test above is
/// meaningful: on pre-workspace code (`resolve_workspace` hard-wired to
/// return `None`, simulated here by simply never calling `cx add`), a
/// colon-containing name must round-trip as a single literal name, not be
/// split into alias+path. This guards the "exact-name-first / no alias
/// interpretation without a workspace" contract from silent regressions.
#[test]
fn no_workspace_colon_name_is_never_split() {
    let env = WorkspaceEnv::new();
    env.ok(&["set", "foo:bar/baz", "--value", "v1"]);
    let value = env.ok(&["get", "foo:bar/baz", "--raw"]);
    assert_eq!(value, "v1");
}

// ---------------------------------------------------------------------------
// Code-review follow-up: bulk `set` per-key workspace resolution (BLOCKER).
// ---------------------------------------------------------------------------

#[test]
fn bulk_set_unqualified_lands_in_workspace_default() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    // "work" is the default (first add), which differs from the top-level
    // active backend/vault a no-workspace command would have used.

    env.ok(&["set", "BULK_B=b", "BULK_C=c"]);

    assert_eq!(env.ok(&["get", "work:BULK_B", "--raw"]), "b");
    assert_eq!(env.ok(&["get", "work:BULK_C", "--raw"]), "c");

    // Must be absent from "stage" — an unqualified bulk key never searches,
    // it always targets the default entry, same as the single-secret path.
    let stage_lookup = env.err(&["get", "stage:BULK_B"]);
    assert!(!stage_lookup.status.success());
}

#[test]
fn bulk_set_alias_qualified_key_lands_in_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Mixed bulk set: one key qualified to each vault in the same command.
    env.ok(&["set", "work:BULK_A=a", "stage:BULK_D=d"]);

    assert_eq!(env.ok(&["get", "work:BULK_A", "--raw"]), "a");
    assert_eq!(env.ok(&["get", "stage:BULK_D", "--raw"]), "d");

    // Cross-check: each landed ONLY in its qualified vault.
    let work_lookup_d = env.err(&["get", "work:BULK_D"]);
    assert!(!work_lookup_d.status.success());
    let stage_lookup_a = env.err(&["get", "stage:BULK_A"]);
    assert!(!stage_lookup_a.status.success());
}

#[test]
fn bulk_set_no_workspace_unchanged() {
    let env = WorkspaceEnv::new();
    // No `cx add` at all — bulk set must behave exactly as before workspaces.
    env.ok(&["set", "BULK_X=x", "BULK_Y=y"]);
    assert_eq!(env.ok(&["get", "BULK_X", "--raw"]), "x");
    assert_eq!(env.ok(&["get", "BULK_Y", "--raw"]), "y");
}

// ---------------------------------------------------------------------------
// Code-review follow-up: destructive/write verbs (delete/update/rotate/
// restore/purge) and read-resolution verbs (history/rollback) now route
// through the same workspace resolver as get/set (MAJOR).
// ---------------------------------------------------------------------------

#[test]
fn delete_unqualified_targets_default_never_searches() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Secret exists ONLY in the non-default vault ("stage"). An unqualified
    // delete must target the default ("work") only — erroring not-found —
    // rather than searching and deleting the stage copy.
    env.ok(&["set", "stage:DEL_ONLY_STAGE", "--value", "v"]);
    let delete_result = env.err(&["delete", "DEL_ONLY_STAGE", "--force"]);
    assert!(!delete_result.status.success());

    // The stage copy must be untouched.
    assert_eq!(env.ok(&["get", "stage:DEL_ONLY_STAGE", "--raw"]), "v");
}

#[test]
fn delete_qualified_targets_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:DEL_ME", "--value", "v"]);
    env.ok(&["delete", "stage:DEL_ME", "--force"]);
    let lookup = env.err(&["get", "stage:DEL_ME"]);
    assert!(!lookup.status.success());
}

#[test]
fn delete_group_unqualified_targets_default_only() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // `--group` has no single secret name to alias-qualify — it must still
    // target the default vault only, never the whole workspace.
    env.ok(&["set", "--group", "demo", "work:GRP_B", "--value", "b"]);
    env.ok(&["set", "--group", "demo", "stage:GRP_C", "--value", "c"]);

    env.ok(&["delete", "--group", "demo", "--force"]);

    let work_lookup = env.err(&["get", "work:GRP_B"]);
    assert!(!work_lookup.status.success());
    // "stage" must be untouched — group delete never spans the workspace.
    assert_eq!(env.ok(&["get", "stage:GRP_C", "--raw"]), "c");
}

#[test]
fn update_unqualified_targets_default_never_searches() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Secret exists ONLY in "stage"; an unqualified update must target
    // "work" only and fail not-found rather than updating the stage copy.
    env.ok(&["set", "stage:UPD_ONLY_STAGE", "--value", "orig"]);
    let update_result = env.err(&["update", "UPD_ONLY_STAGE", "--note", "changed"]);
    assert!(!update_result.status.success());
}

#[test]
fn update_qualified_targets_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:UPD_ME", "--value", "orig"]);
    env.ok(&["update", "stage:UPD_ME", "orig-updated"]);
    assert_eq!(env.ok(&["get", "stage:UPD_ME", "--raw"]), "orig-updated");
}

#[test]
fn rotate_unqualified_targets_default_never_searches() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Secret exists ONLY in "stage"; an unqualified rotate must target
    // "work" only and fail not-found rather than rotating the stage copy.
    env.ok(&["set", "stage:ROT_ONLY_STAGE", "--value", "orig"]);
    let rotate_result = env.err(&["rotate", "ROT_ONLY_STAGE", "--force"]);
    assert!(!rotate_result.status.success());
    // The stage copy's value must be unchanged.
    assert_eq!(env.ok(&["get", "stage:ROT_ONLY_STAGE", "--raw"]), "orig");
}

#[test]
fn rotate_qualified_targets_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:ROT_ME", "--value", "orig"]);
    env.ok(&["rotate", "stage:ROT_ME", "--force"]);
    let rotated = env.ok(&["get", "stage:ROT_ME", "--raw"]);
    assert_ne!(rotated, "orig", "rotate must have generated a new value");
}

/// Bugbot round-3 MEDIUM fix: capability gates must evaluate the RESOLVED
/// workspace target's backend, not the process's top-level active
/// backend. `local` and `aws` genuinely differ on `has_secret_rotation`
/// (local: false, aws: true — the only capability flag that differs
/// across crosstache's backend kinds), so `rotate --native` is the one
/// verb where a real, hermetic, e2e-drivable capability MISMATCH exists:
/// the workspace's default is the `aws-mock` named backend (rotation
/// CAPABLE), while a non-default entry is `local-a` (rotation
/// INCAPABLE). Only the capability check itself needs to be hermetic —
/// AWS SDK client construction never makes a network call, so asserting
/// on which error comes back (capability-rejection vs. anything else) is
/// safe without real credentials.
#[test]
#[cfg_attr(not(feature = "aws"), ignore = "requires the aws feature")]
fn rotate_native_capability_gate_reflects_resolved_target_not_workspace_default() {
    let env = WorkspaceEnv::new();
    // aws-mock (rotation-capable) becomes the workspace default (first
    // add). `--force` skips the vault-exists probe, which would otherwise
    // need a real network round-trip against the fake endpoint.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "aws-mock",
        "--as",
        "cloud",
        "--force",
    ]);

    // The process's top-level active backend (xv.conf's plain `backend =
    // "local"`, a THIRD store distinct from every named backend here) has
    // `has_secret_rotation: false` — the OLD, buggy gate (checking
    // `registry.active().capabilities()`) would have rejected this
    // unqualified rotate even though the workspace's actual default
    // (aws-mock) supports it. This is a bare (colon-free) name, so Write
    // mode's exact-name-first probe never fires (nothing to disambiguate
    // for a bare name) — resolution only MATERIALIZES aws-mock (no network
    // call; AWS SDK client construction is lazy about credentials) and
    // then evaluates its capabilities, both hermetic. The actual
    // `native_rotate` call after the gate DOES need real network and WILL
    // fail — that's fine; the only thing asserted is that the failure is
    // NOT the capability-rejection message, proving the gate itself
    // resolved and read the correct (aws-mock) capabilities.
    let out = env.run(&["rotate", "ANY_SECRET", "--native", "--force"]);
    let msg = combined(&out);
    assert!(
        !msg.contains("does not support native rotation"),
        "aws-mock (the resolved default) supports native rotation, so this must not be \
         capability-rejected — a rejection here would mean the gate is still reading some \
         other backend's (e.g. the top-level active local backend's) capabilities: {msg}"
    );
}

/// Guard: the no-workspace capability-rejection error keeps its EXACT
/// current message and exit code — the local backend has no native
/// rotation support, and `rotate --native` without any workspace attached
/// must reject with byte-identical text to the pre-refactor behavior
/// (`resolved backend == active backend` in the no-workspace case, so
/// moving the gate after resolution changes nothing here).
#[test]
fn rotate_native_capability_error_no_workspace_is_byte_stable() {
    let env = WorkspaceEnv::new();
    // No `cx add` at all.
    let out = env.err(&["rotate", "SOME_SECRET", "--native", "--force"]);
    assert_eq!(out.status.code(), Some(2), "{}", combined(&out));
    let msg = combined(&out);
    assert_eq!(
        msg.trim(),
        "error[xv-invalid-argument]: Invalid argument: The local backend does not support native rotation. Native rotation is currently available on the aws backend only; without --native, 'xv rotate' generates a new value client-side on any backend."
    );
}

#[test]
fn history_ambiguous_errors_exit_13() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "work:HIST_DUP", "--value", "v1"]);
    env.ok(&["set", "stage:HIST_DUP", "--value", "v1"]);

    let out = env.err(&["history", "HIST_DUP"]);
    assert_eq!(out.status.code(), Some(13), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains("work:HIST_DUP"), "{msg}");
    assert!(msg.contains("stage:HIST_DUP"), "{msg}");
}

#[test]
fn history_qualified_reads_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:HIST_ME", "--value", "v1"]);
    let out = env.ok(&["history", "stage:HIST_ME"]);
    assert!(out.contains("HIST_ME") || !out.is_empty(), "{out}");
}

#[test]
fn rollback_unqualified_unique_match_searches_and_resolves() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Rollback is grouped with `get`/`history` as a read-resolution verb
    // (searches attached vaults on an unqualified name), NOT a
    // default-vault-only write — unlike delete/update/rotate/restore/purge.
    // The secret exists ONLY in "stage" (not the default, "work"); an
    // unqualified rollback must still find and roll it back there.
    env.ok(&["set", "stage:ROLL_ONLY_STAGE", "--value", "v1val"]);
    env.ok(&["update", "stage:ROLL_ONLY_STAGE", "v2val"]);
    env.ok(&["rollback", "ROLL_ONLY_STAGE", "--version", "v1", "--force"]);
    assert_eq!(env.ok(&["get", "stage:ROLL_ONLY_STAGE", "--raw"]), "v1val");
}

#[test]
fn rollback_ambiguous_errors_exit_13() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Same name, with a version history, in both vaults — an unqualified
    // rollback must be ambiguous (exit 13), same as `get`.
    env.ok(&["set", "work:ROLL_DUP", "--value", "v1val"]);
    env.ok(&["update", "work:ROLL_DUP", "v2val"]);
    env.ok(&["set", "stage:ROLL_DUP", "--value", "v1val"]);
    env.ok(&["update", "stage:ROLL_DUP", "v2val"]);

    let out = env.err(&["rollback", "ROLL_DUP", "--version", "v1", "--force"]);
    assert_eq!(out.status.code(), Some(13), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains("work:ROLL_DUP"), "{msg}");
    assert!(msg.contains("stage:ROLL_DUP"), "{msg}");
}

#[test]
fn rollback_qualified_targets_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:ROLL_ME", "--value", "v1val"]);
    env.ok(&["update", "stage:ROLL_ME", "v2val"]);
    env.ok(&["rollback", "stage:ROLL_ME", "--version", "v1", "--force"]);
    assert_eq!(env.ok(&["get", "stage:ROLL_ME", "--raw"]), "v1val");
}

#[test]
fn purge_unqualified_targets_default_never_searches() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Secret exists (soft-deleted) ONLY in "stage"; an unqualified purge
    // must target "work" only, never "stage". The local backend's purge is
    // idempotent on a name with nothing to purge (no trash entries — a
    // no-op `Ok(())`, not a not-found error), so "unqualified purge
    // succeeds" alone doesn't prove anything; the proof is that the stage
    // copy is STILL RESTORABLE afterward (i.e. still in trash, not
    // actually purged) — if the unqualified purge had wrongly searched and
    // purged the stage copy, this restore would fail. This is the
    // trait-path proof for the Bugbot HIGH fix: purge's legacy-vs-trait
    // decision now runs on the RESOLVED target, and on this (local-backend)
    // trait path that means default-only, never-searched addressing — same
    // contract as delete/update/rotate.
    env.ok(&["set", "stage:PURGE_ONLY_STAGE", "--value", "v"]);
    env.ok(&["delete", "stage:PURGE_ONLY_STAGE", "--force"]);
    env.ok(&["purge", "PURGE_ONLY_STAGE", "--force"]); // no-op against "work"

    env.ok(&["restore", "stage:PURGE_ONLY_STAGE"]);
    assert_eq!(env.ok(&["get", "stage:PURGE_ONLY_STAGE", "--raw"]), "v");

    // The stage copy must still be purgeable directly (qualified), for real.
    env.ok(&["delete", "stage:PURGE_ONLY_STAGE", "--force"]);
    env.ok(&["purge", "stage:PURGE_ONLY_STAGE", "--force"]);
}

#[test]
fn restore_and_purge_unqualified_target_default() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Seed the SAME name in both vaults, soft-delete both, then restore the
    // default ("work") unqualified — the "stage" copy must stay deleted.
    env.ok(&["set", "work:RESTORE_ME", "--value", "v"]);
    env.ok(&["set", "stage:RESTORE_ME", "--value", "v"]);
    env.ok(&["delete", "work:RESTORE_ME", "--force"]);
    env.ok(&["delete", "stage:RESTORE_ME", "--force"]);

    env.ok(&["restore", "RESTORE_ME"]);
    assert_eq!(env.ok(&["get", "work:RESTORE_ME", "--raw"]), "v");
    let stage_still_deleted = env.err(&["get", "stage:RESTORE_ME"]);
    assert!(!stage_still_deleted.status.success());

    // Purge the (now-restored, then re-deleted) default copy; qualified
    // purge on "stage" must independently succeed on its own deleted copy.
    env.ok(&["delete", "work:RESTORE_ME", "--force"]);
    env.ok(&["purge", "RESTORE_ME", "--force"]);
    env.ok(&["purge", "stage:RESTORE_ME", "--force"]);
}

/// No-workspace guard, extended to every write/read verb touched by this
/// follow-up: with no `cx add` at all, delete/update/rotate/restore/purge/
/// history/rollback must all behave exactly as before workspaces existed.
#[test]
fn no_workspace_write_and_read_verbs_unchanged() {
    let env = WorkspaceEnv::new();

    env.ok(&["set", "GUARD_ME", "--value", "orig"]);
    let hist = env.ok(&["history", "GUARD_ME"]);
    assert!(!hist.is_empty());

    env.ok(&["update", "GUARD_ME", "orig-updated"]);
    assert_eq!(env.ok(&["get", "GUARD_ME", "--raw"]), "orig-updated");

    env.ok(&["rollback", "GUARD_ME", "--version", "v1", "--force"]);
    assert_eq!(env.ok(&["get", "GUARD_ME", "--raw"]), "orig");
    // Put it back to the updated value so the rest of the guard proceeds
    // as it did before rollback was added to this test.
    env.ok(&["update", "GUARD_ME", "orig-updated"]);

    env.ok(&["rotate", "GUARD_ME", "--force"]);
    let rotated = env.ok(&["get", "GUARD_ME", "--raw"]);
    assert_ne!(rotated, "orig-updated");

    env.ok(&["delete", "GUARD_ME", "--force"]);
    let deleted_lookup = env.err(&["get", "GUARD_ME"]);
    assert!(!deleted_lookup.status.success());

    env.ok(&["restore", "GUARD_ME"]);
    assert_eq!(env.ok(&["get", "GUARD_ME", "--raw"]), rotated);

    env.ok(&["delete", "GUARD_ME", "--force"]);
    env.ok(&["purge", "GUARD_ME", "--force"]);
}

// ---------------------------------------------------------------------------
// Union `ls` (Phase B, Task 7)
// ---------------------------------------------------------------------------

#[test]
fn ls_union_shows_vault_column_when_multi() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:WORK_SECRET", "--value", "v1"]);
    env.ok(&["set", "stage:STAGE_SECRET", "--value", "v2"]);

    let out = env.ok(&["ls", "--format", "table"]);
    assert!(out.contains("Vault"), "expected a Vault column: {out}");
    assert!(out.contains("WORK_SECRET") && out.contains("work"), "{out}");
    assert!(
        out.contains("STAGE_SECRET") && out.contains("stage"),
        "{out}"
    );
}

/// The alias is carried through the union `ls` render pipeline via a
/// synthetic, in-memory-only tag (`WORKSPACE_ALIAS_TAG` in
/// `src/cli/secret_ops.rs`) so the existing folder-scoping/sort/render
/// helpers can be reused unchanged. This pins the invariant that shortcut
/// depends on: `--format json` must expose the alias ONLY under the
/// intentional `"vault"` key — never under its raw internal tag name, and
/// never inside the `"fields"` map (which only lifts `f.*`-prefixed tags).
#[test]
fn ls_union_json_exposes_alias_only_under_vault_key_never_leaks_internal_tag() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:WORK_SECRET", "--value", "v1"]);
    env.ok(&["set", "stage:STAGE_SECRET", "--value", "v2"]);

    let out = env.ok(&["ls", "--format", "json"]);

    // The synthetic tag's internal name must never reach output in any form.
    assert!(
        !out.contains("__xv_workspace_alias") && !out.contains("workspace_alias"),
        "the synthetic alias tag must never leak under its raw internal name: {out}"
    );

    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&out).expect("ls --format json must produce valid JSON");
    assert_eq!(rows.len(), 2, "{out}");

    for row in &rows {
        let name = row["name"].as_str().expect("name field");
        let expected_vault = match name {
            "WORK_SECRET" => "work",
            "STAGE_SECRET" => "stage",
            other => panic!("unexpected secret in output: {other}"),
        };
        assert_eq!(
            row["vault"].as_str(),
            Some(expected_vault),
            "alias must appear under the 'vault' key: {row}"
        );

        // The alias must not leak into the fields map (which only lifts
        // f.*-prefixed tags for typed records — neither of these secrets is
        // typed, so `fields` must be empty).
        let fields = row["fields"].as_object().expect("fields must be an object");
        assert!(
            fields.is_empty(),
            "alias must not leak into the fields map: {row}"
        );
        assert!(
            row.get("tags").is_none(),
            "no raw 'tags' key should be present at all in ls JSON output: {row}"
        );
    }
}

/// Byte-golden pin (spec §Backward compatibility): a single-entry workspace
/// must render EXACTLY like the no-workspace path against the same vault —
/// no VAULT column, identical header/footer text. Reuses the SAME env: the
/// bare (pre-`cx add`) local backend and `local-a` (post-`cx add`, aliased
/// "work") both use vault name "default", so their `ls --format table`
/// output for an identical secret is directly comparable byte-for-byte.
#[test]
fn ls_single_vault_output_unchanged() {
    let env = WorkspaceEnv::new();

    env.ok(&["set", "PINNED_SECRET", "--value", "v1"]);
    let before = env.ok(&["ls", "--format", "table"]);
    assert!(
        !before.contains("Vault") || before.contains("Vault:"),
        "sanity: no VAULT *column* pre-workspace: {before}"
    );

    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&["set", "work:PINNED_SECRET", "--value", "v1"]);
    let after = env.ok(&["ls", "--format", "table"]);

    assert_eq!(
        before, after,
        "single-entry workspace ls output must be byte-identical to the no-workspace path"
    );
}

/// Same byte-golden pin as `ls_single_vault_output_unchanged`, extended to
/// `--names-only` (Bugbot review MEDIUM): that branch used to prefix
/// `alias/` whenever the synthetic alias tag was present at all, ignoring
/// the `show_vault` (>=2 entries) gate every other render path honors — a
/// single-entry workspace must be byte-identical to no-workspace in EVERY
/// output form, not just `--format table`.
#[test]
fn ls_single_vault_names_only_output_unchanged() {
    let env = WorkspaceEnv::new();

    env.ok(&["set", "PINNED_SECRET", "--value", "v1"]);
    let before = env.ok(&["ls", "--names-only"]);
    assert!(
        !before.contains('/'),
        "sanity: no alias prefix pre-workspace: {before}"
    );

    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&["set", "work:PINNED_SECRET", "--value", "v1"]);
    let after = env.ok(&["ls", "--names-only"]);

    assert_eq!(
        before, after,
        "single-entry workspace ls --names-only output must be byte-identical to the \
         no-workspace path — no alias/ prefix with only one attached vault"
    );
}

#[test]
fn ls_union_composes_with_filter_and_type() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:PROD_DB", "--value", "v1"]);
    env.ok(&["set", "work:DEV_DB", "--value", "v2"]);
    env.ok(&["set", "stage:PROD_API", "--value", "v3"]);
    env.ok(&["set", "stage:DEV_API", "--value", "v4"]);

    let out = env.ok(&["ls", "--format", "table", "--filter", "PROD_*"]);
    assert!(out.contains("PROD_DB"), "{out}");
    assert!(out.contains("PROD_API"), "{out}");
    assert!(!out.contains("DEV_DB"), "{out}");
    assert!(!out.contains("DEV_API"), "{out}");
}

#[test]
fn ls_union_pagination_over_merged_set() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:AAA", "--value", "v1"]);
    env.ok(&["set", "work:BBB", "--value", "v2"]);
    env.ok(&["set", "stage:CCC", "--value", "v3"]);
    env.ok(&["set", "stage:DDD", "--value", "v4"]);

    // Merged set (alias-then-name sorted, "stage" < "work" alphabetically):
    // stage/CCC, stage/DDD, work/AAA, work/BBB. Page 1 of size 2 must be
    // exactly the first two — pagination over the MERGED set, not per-vault
    // (which would instead yield the first page from EACH vault, 4 rows
    // total).
    let page1 = env.ok(&["ls", "--format", "table", "--page", "1", "--page-size", "2"]);
    assert!(page1.contains("CCC"), "{page1}");
    assert!(page1.contains("DDD"), "{page1}");
    assert!(!page1.contains("AAA"), "{page1}");
    assert!(!page1.contains("BBB"), "{page1}");

    let page2 = env.ok(&["ls", "--format", "table", "--page", "2", "--page-size", "2"]);
    assert!(!page2.contains("CCC"), "{page2}");
    assert!(!page2.contains("DDD"), "{page2}");
    assert!(page2.contains("AAA"), "{page2}");
    assert!(page2.contains("BBB"), "{page2}");
}

/// Any attached vault erroring during a union read must fail the WHOLE
/// command, naming the vault — no partial unions (spec §Read semantics).
/// `--force` skips `cx add`'s vault-exists probe, attaching an entry whose
/// backend name ("ghost-backend") is neither a built-in kind nor a
/// `named_backends` entry — `materialize` fails the first (and only) time
/// it's touched.
#[test]
fn ls_union_fails_loud_when_vault_unreachable() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "ghost-vault",
        "--backend",
        "ghost-backend",
        "--as",
        "ghost",
        "--force",
    ]);
    env.ok(&["set", "work:REAL_SECRET", "--value", "v1"]);

    let out = env.err(&["ls", "--format", "table"]);
    let msg = combined(&out);
    assert!(
        msg.contains("ghost"),
        "must name the unreachable vault/alias: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Cache-enabled workspace coverage (Bugbot review BLOCKER follow-up):
// union-ls keys its per-vault cache entries by the workspace entry's
// REGISTRY name (`entry.backend`, e.g. "local-a"), but every write-side
// invalidation used to pass `Backend::name()` — the hardcoded backend KIND
// ("local") — so for named backends the invalidation targeted the wrong
// `(backend, vault)` cache path and a write within the TTL left a stale
// listing behind. Every other `WorkspaceEnv` test in this file runs with
// caching disabled (`cache_enabled = false`), so this class of bug was
// invisible to the whole suite until traced by code review — these tests
// exist specifically to keep that gap closed.
// ---------------------------------------------------------------------------

#[test]
fn ls_then_qualified_set_then_ls_reflects_write_with_cache_enabled() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:EXISTING", "--value", "v0"]);

    // Populate the cache for both attached vaults.
    let before = env.ok(&["ls", "--format", "table"]);
    assert!(before.contains("EXISTING"), "{before}");
    assert!(!before.contains("NEW_SECRET"), "{before}");

    // A qualified write into "work" — if invalidation had keyed off
    // `Backend::name()` ("local", the kind) instead of the resolved entry's
    // registry name ("local-a"), this write would invalidate a cache path
    // union-ls never reads from, leaving the stale `before` listing behind.
    env.ok(&["set", "work:NEW_SECRET", "--value", "v1"]);

    let after = env.ok(&["ls", "--format", "table"]);
    assert!(
        after.contains("NEW_SECRET"),
        "union ls must reflect the write within the cache TTL, not serve a stale cached listing: {after}"
    );
    assert!(after.contains("EXISTING"), "{after}");
}

#[test]
fn ls_then_qualified_delete_then_ls_reflects_write_with_cache_enabled() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:TO_DELETE", "--value", "v0"]);
    env.ok(&["set", "stage:KEEP_ME", "--value", "v1"]);

    let before = env.ok(&["ls", "--format", "table"]);
    assert!(before.contains("TO_DELETE"), "{before}");

    env.ok(&["delete", "work:TO_DELETE", "--force"]);

    let after = env.ok(&["ls", "--format", "table"]);
    assert!(
        !after.contains("TO_DELETE"),
        "union ls must not still show a deleted secret from a stale cached listing: {after}"
    );
    assert!(after.contains("KEEP_ME"), "{after}");
}

/// Guard: the plain no-workspace path must ALSO pair correctly with caching
/// enabled — the read side (`ls`) keys its cache entry by
/// `config.effective_backend_name()`, and `resolve_workspace_or_default`'s
/// degenerate (no-workspace) branch returns that exact same string, so a
/// write must still invalidate what `ls` actually reads from.
#[test]
fn no_workspace_ls_then_set_then_ls_reflects_write_with_cache_enabled() {
    let env = WorkspaceEnv::with_cache_enabled(300);

    env.ok(&["set", "PLAIN_EXISTING", "--value", "v0"]);
    let before = env.ok(&["ls", "--format", "table"]);
    assert!(before.contains("PLAIN_EXISTING"), "{before}");
    assert!(!before.contains("PLAIN_NEW"), "{before}");

    env.ok(&["set", "PLAIN_NEW", "--value", "v1"]);
    let after = env.ok(&["ls", "--format", "table"]);
    assert!(
        after.contains("PLAIN_NEW"),
        "no-workspace ls must reflect the write within the cache TTL: {after}"
    );

    env.ok(&["delete", "PLAIN_EXISTING", "--force"]);
    let after_delete = env.ok(&["ls", "--format", "table"]);
    assert!(
        !after_delete.contains("PLAIN_EXISTING"),
        "no-workspace ls must not still show a deleted secret from a stale cached listing: {after_delete}"
    );
}

// ---------------------------------------------------------------------------
// Bugbot review round 2 (HIGH): no-workspace cache keys must converge on
// `config.effective_backend_name()` (the registry/config name), not
// `Backend::name()`/`registry.active().name()` (the backend kind) — the
// same class of bug as round 1's workspace-vs-kind mismatch, but for the
// NO-WORKSPACE path: a NAMED backend active with no workspace attached
// (`config.backend = "local-a"`) was sharing one cache file with the
// built-in `local` backend for the same vault name, and a workspace write
// (keyed by `entry.backend`, e.g. "local-a") could invalidate a path a
// LATER no-workspace `ls` (active = "local-a") never reads from (it read
// the kind-keyed "local" path instead). All three tests below use
// `xv_with_backend("local-a", ..)` to make a NAMED backend the top-level
// active one with NO workspace attached, reusing this environment's
// existing `[named_backends.local-a]` entry.
// ---------------------------------------------------------------------------

#[test]
fn named_backend_active_no_workspace_cache_pairs() {
    let env = WorkspaceEnv::with_cache_enabled(300);

    env.ok_with_backend("local-a", &["set", "NAMED_EXISTING", "--value", "v0"]);
    let before = env.ok_with_backend("local-a", &["ls", "--format", "table"]);
    assert!(before.contains("NAMED_EXISTING"), "{before}");
    assert!(!before.contains("NAMED_NEW"), "{before}");

    // If the write and read cache keys diverged (write keyed by the kind
    // "local", read keyed by the config name "local-a", or vice versa),
    // this second `ls` would still serve the stale `before` listing.
    env.ok_with_backend("local-a", &["set", "NAMED_NEW", "--value", "v1"]);
    let after = env.ok_with_backend("local-a", &["ls", "--format", "table"]);
    assert!(
        after.contains("NAMED_NEW"),
        "named-backend-active (no workspace) ls must reflect the write within the \
         cache TTL — read/write cache keys must not diverge: {after}"
    );
    assert!(after.contains("NAMED_EXISTING"), "{after}");
}

#[test]
fn named_backend_does_not_share_cache_with_builtin_kind() {
    let env = WorkspaceEnv::with_cache_enabled(300);

    // Populate the cache while the BUILT-IN "local" backend is active
    // (vault "default", store_default).
    env.ok(&["set", "BUILTIN_LOCAL_SECRET", "--value", "v0"]);
    let builtin_listing = env.ok(&["ls", "--format", "table"]);
    assert!(
        builtin_listing.contains("BUILTIN_LOCAL_SECRET"),
        "{builtin_listing}"
    );

    // Switch the active backend to the NAMED "local-a" entry — same vault
    // NAME ("default"), but a completely different on-disk store with
    // nothing set in it yet. If the two shared one cache file (both keyed
    // by the kind "local"), this `ls` would incorrectly show the builtin
    // backend's secret.
    let named_listing = env.ok_with_backend("local-a", &["ls", "--format", "table"]);
    assert!(
        !named_listing.contains("BUILTIN_LOCAL_SECRET"),
        "named backend 'local-a' must not see the built-in 'local' backend's cached \
         listing just because they share a vault name: {named_listing}"
    );
}

/// A read/write key MISMATCH manifests as a stale HIT, not a miss — a
/// mismatched write invalidates a path nothing reads from, leaving
/// whatever the read path's own (different) key already cached untouched.
/// So this test must first populate a cache entry at the NO-WORKSPACE read
/// key (a plain miss would silently "work" regardless of any mismatch),
/// THEN write through the workspace path and confirm that entry — not some
/// entry nothing ever reads — got invalidated.
#[test]
fn workspace_write_invalidates_entry_seen_by_no_workspace_ls() {
    let env = WorkspaceEnv::with_cache_enabled(300);

    // Step 1: populate the cache at the NO-WORKSPACE read key (active
    // backend = "local-a", no workspace attached yet).
    env.ok_with_backend("local-a", &["set", "OLD_SECRET", "--value", "v0"]);
    let seed_listing = env.ok_with_backend("local-a", &["ls", "--format", "table"]);
    assert!(seed_listing.contains("OLD_SECRET"), "{seed_listing}");

    // Step 2: attach a workspace over the SAME backend/vault and write a
    // NEW secret through it — the qualified write is keyed by the
    // workspace entry's registry name (`entry.backend` = "local-a").
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&["set", "work:NEW_SECRET", "--value", "v1"]);

    // Step 3: remove the workspace (its only entry — this restores
    // single-vault behavior) and read again via the NO-WORKSPACE path with
    // the SAME active backend. If the workspace write's invalidation key
    // ("local-a") diverged from what step 1's read populated
    // (`config.effective_backend_name()`, also "local-a" when they're the
    // same backend), this read would still serve the STALE `seed_listing`
    // from step 1, missing NEW_SECRET entirely.
    env.ok(&["cx", "rm", "work"]);
    let no_workspace_listing = env.ok_with_backend("local-a", &["ls", "--format", "table"]);
    assert!(
        no_workspace_listing.contains("NEW_SECRET"),
        "no-workspace ls (active=local-a) must see the write made through the \
         workspace, not serve the stale pre-workspace cached listing — read/write \
         cache key shape must match across modes: {no_workspace_listing}"
    );
    assert!(
        no_workspace_listing.contains("OLD_SECRET"),
        "{no_workspace_listing}"
    );
}

// ---------------------------------------------------------------------------
// Union `find` (Phase B, Task 8)
// ---------------------------------------------------------------------------

#[test]
fn find_unions_workspace() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:DATABASE_PASSWORD", "--value", "v1"]);
    env.ok(&["set", "stage:DATABASE_TOKEN", "--value", "v2"]);

    let out = env.ok(&["find", "database"]);
    assert!(out.contains("work/DATABASE_PASSWORD"), "{out}");
    assert!(out.contains("stage/DATABASE_TOKEN"), "{out}");
}

/// `--all-vaults` keeps its existing, documented meaning (every vault the
/// active backend can list) — a strict superset of the workspace's
/// attached vaults — and takes priority even with a workspace present.
/// Here the active backend is the top-level `local` store (a THIRD vault,
/// not attached to the workspace at all): `--all-vaults` must reach it,
/// which the union-workspace path alone never would.
#[test]
fn find_all_vaults_still_superset() {
    let env = WorkspaceEnv::new();
    // Written to the top-level active ("local") backend directly, BEFORE
    // any workspace is attached — once a workspace exists, an unqualified
    // write always targets the workspace's default entry instead (spec
    // §Write semantics: unqualified writes never search, never fall back to
    // "the top-level active backend"), so this ordering is required to land
    // the secret in the unattached top-level vault at all.
    env.ok(&["set", "TOPLEVEL_ONLY", "--value", "v2"]);

    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&["set", "work:WORKSPACE_ONLY", "--value", "v1"]);

    let workspace_only = env.ok(&["find", "only"]);
    assert!(
        workspace_only.contains("WORKSPACE_ONLY"),
        "{workspace_only}"
    );
    assert!(
        !workspace_only.contains("TOPLEVEL_ONLY"),
        "workspace union must not see the unattached top-level vault: {workspace_only}"
    );

    let all_vaults = env.ok(&["find", "only", "--all-vaults"]);
    assert!(
        all_vaults.contains("TOPLEVEL_ONLY"),
        "--all-vaults must reach the top-level vault even with a workspace attached: {all_vaults}"
    );
}

#[test]
fn find_filter_composes_in_union() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:PROD_KEY", "--value", "v1"]);
    env.ok(&["set", "work:DEV_KEY", "--value", "v2"]);
    env.ok(&["set", "stage:PROD_TOKEN", "--value", "v3"]);

    let out = env.ok(&["find", "", "--filter", "PROD_*"]);
    assert!(out.contains("work/PROD_KEY"), "{out}");
    assert!(out.contains("stage/PROD_TOKEN"), "{out}");
    assert!(!out.contains("DEV_KEY"), "{out}");
}

// ---------------------------------------------------------------------------
// `ls --deleted` capability gating in a union workspace (Phase B, Task 9
// remaining scope)
// ---------------------------------------------------------------------------

/// Every shipped backend (Azure/local/AWS) supports soft-delete today, so
/// the actual "skip an incapable vault" branch isn't e2e-drivable (the same
/// class of limitation documented on `src/workspace/resolve.rs`'s
/// capability tests; `deleted_list_capability_skip_note` in
/// `src/cli/secret_ops.rs` unit-tests the note's wording directly). This
/// instead pins the POSITIVE union path: `--deleted` across two capable
/// vaults merges both, `alias/`-prefixed.
#[test]
fn ls_deleted_unions_capable_vaults_with_alias_prefix() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:GONE_WORK", "--value", "v1"]);
    env.ok(&["set", "stage:GONE_STAGE", "--value", "v2"]);
    env.ok(&["delete", "work:GONE_WORK", "--force"]);
    env.ok(&["delete", "stage:GONE_STAGE", "--force"]);

    let out = env.ok(&["ls", "--deleted", "--format", "table"]);
    assert!(out.contains("work/GONE_WORK"), "{out}");
    assert!(out.contains("stage/GONE_STAGE"), "{out}");
}

/// Bugbot review MEDIUM: `ls --deleted` in a >=2-entry workspace used to
/// rewrite names to `alias/name` BEFORE `--filter` glob matching, while the
/// live union `ls` path filters bare names per vault first — so an
/// anchored glob like `PROD_*` matched live rows but missed deleted ones
/// (`"work/PROD_X"` no longer starts with `"PROD_"`). This pins that
/// `ls --deleted --filter 'PROD_*'` matches the same base names a live
/// `ls --filter 'PROD_*'` would (captured on equivalent, not-yet-deleted
/// secrets), while still showing the `alias/` prefix per
/// `ls_deleted_unions_capable_vaults_with_alias_prefix` above.
///
/// Note: this local-backend e2e harness can't independently DISCRIMINATE
/// the fix from the bug — local's `Unrestricted` charset means `name` ==
/// `original_name` always, so `glob_matches_either_name`'s OR fallback via
/// the untouched `name` field masks the ordering bug here regardless. The
/// unit test `deleted_union_filter_must_run_on_bare_names_before_alias_prefix`
/// (`src/cli/secret_ops.rs`) constructs the diverging-name case (as on a
/// restricted-charset backend) that actually proves the ordering; this
/// e2e test instead pins the end-to-end feature contract itself.
#[test]
fn ls_deleted_filter_matches_bare_names_before_alias_prefix() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "work:PROD_ALPHA", "--value", "v1"]);
    env.ok(&["set", "work:DEV_ALPHA", "--value", "v2"]);
    env.ok(&["set", "stage:PROD_BETA", "--value", "v3"]);
    env.ok(&["set", "stage:DEV_BETA", "--value", "v4"]);

    // Capture what a LIVE union `ls --filter 'PROD_*'` matches, before
    // deleting anything — the set this test's deleted-list filter must
    // mirror for the same base names.
    let live_filtered = env.ok(&["ls", "--format", "table", "--filter", "PROD_*"]);
    assert!(live_filtered.contains("PROD_ALPHA"), "{live_filtered}");
    assert!(live_filtered.contains("PROD_BETA"), "{live_filtered}");
    assert!(!live_filtered.contains("DEV_ALPHA"), "{live_filtered}");
    assert!(!live_filtered.contains("DEV_BETA"), "{live_filtered}");

    env.ok(&["delete", "work:PROD_ALPHA", "--force"]);
    env.ok(&["delete", "work:DEV_ALPHA", "--force"]);
    env.ok(&["delete", "stage:PROD_BETA", "--force"]);
    env.ok(&["delete", "stage:DEV_BETA", "--force"]);

    let deleted_filtered = env.ok(&["ls", "--deleted", "--format", "table", "--filter", "PROD_*"]);
    // Same base names the live filter matched — now alias-prefixed
    // (>=2 attached vaults).
    assert!(
        deleted_filtered.contains("work/PROD_ALPHA"),
        "{deleted_filtered}"
    );
    assert!(
        deleted_filtered.contains("stage/PROD_BETA"),
        "{deleted_filtered}"
    );
    // The DEV_* secrets must still be excluded by the filter, not merely
    // reformatted.
    assert!(
        !deleted_filtered.contains("DEV_ALPHA"),
        "{deleted_filtered}"
    );
    assert!(!deleted_filtered.contains("DEV_BETA"), "{deleted_filtered}");
}

// ===========================================================================
// Phase C, Task 11: workspace aliases in `xv://` URIs (inject/run)
// ===========================================================================

#[test]
fn inject_alias_uri_resolves() {
    let env = WorkspaceEnv::new();
    // Alias "default" deliberately shares its NAME with the top-level active
    // backend's own default vault ("default"/store_default) — proving alias
    // resolution and the raw-vault-name fallback are genuinely distinct
    // code paths, not accidentally the same one.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "default",
    ]);
    env.ok(&["set", "default:RAW_SECRET", "--value", "aliased-value"]);

    let template_path = env.home.join("tpl_alias.txt");
    std::fs::write(&template_path, "value: xv://default/RAW_SECRET\n").expect("write template");
    let out_path = env.home.join("out_alias.txt");
    env.ok(&[
        "inject",
        "--template",
        template_path.to_str().unwrap(),
        "--out",
        out_path.to_str().unwrap(),
    ]);
    let rendered = std::fs::read_to_string(&out_path).expect("read output");
    assert!(rendered.contains("aliased-value"), "{rendered}");
}

#[test]
fn inject_raw_vault_name_still_works_when_no_alias_matches() {
    let env = WorkspaceEnv::new();
    // Seed the top-level active backend's OWN "default" vault before any
    // workspace exists (unqualified `set` -> config's default local backend
    // and vault).
    env.ok(&["set", "RAW_ONLY", "--value", "raw-active-value"]);
    // Workspace attached, but neither alias is named "default" — the URI's
    // vault segment must fall through to raw-vault-name meaning unchanged.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    let template_path = env.home.join("tpl_raw.txt");
    std::fs::write(&template_path, "value: xv://default/RAW_ONLY\n").expect("write template");
    let out_path = env.home.join("out_raw.txt");
    env.ok(&[
        "inject",
        "--template",
        template_path.to_str().unwrap(),
        "--out",
        out_path.to_str().unwrap(),
    ]);
    let rendered = std::fs::read_to_string(&out_path).expect("read output");
    assert!(rendered.contains("raw-active-value"), "{rendered}");
}

#[test]
fn inject_backend_qualified_uri_bypasses_alias() {
    let env = WorkspaceEnv::new();
    // Top-level active backend's own "default" vault gets one value...
    env.ok(&["set", "RAW_SECRET", "--value", "raw-active-value"]);
    // ...while the ATTACHED ALIAS "default" (same string!) points somewhere
    // else entirely (local-a's "default" vault) with a DIFFERENT value.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "default",
    ]);
    env.ok(&["set", "default:RAW_SECRET", "--value", "aliased-value"]);

    // Explicit backend prefix ("local") must bypass alias resolution
    // entirely and resolve against the active backend's raw vault name, even
    // though "default" is a live attached alias.
    let template_path = env.home.join("tpl_bypass.txt");
    std::fs::write(&template_path, "value: xv://local:default/RAW_SECRET\n")
        .expect("write template");
    let out_path = env.home.join("out_bypass.txt");
    env.ok(&[
        "inject",
        "--template",
        template_path.to_str().unwrap(),
        "--out",
        out_path.to_str().unwrap(),
    ]);
    let rendered = std::fs::read_to_string(&out_path).expect("read output");
    assert!(rendered.contains("raw-active-value"), "{rendered}");
    assert!(!rendered.contains("aliased-value"), "{rendered}");
}

#[test]
fn run_env_alias_uri_resolves() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&["set", "work:TOKEN", "--value", "work-value"]);

    let out = env
        .xv()
        .env("MY_REF", "xv://work/TOKEN")
        .args([
            "run",
            "--inherit-env",
            "--no-masking",
            "--",
            "printenv",
            "MY_REF",
        ])
        .output()
        .expect("execute xv run");
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("work-value"), "{stdout}");
}

#[test]
fn inject_alias_with_field_fragment() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "set",
        "work:CREDS",
        "--type",
        "login",
        "--field",
        "username=alice",
        "--value",
        "hunter2",
    ]);

    let template_path = env.home.join("tpl_field.txt");
    std::fs::write(&template_path, "user: xv://work/CREDS#username\n").expect("write template");
    let out_path = env.home.join("out_field.txt");
    env.ok(&[
        "inject",
        "--template",
        template_path.to_str().unwrap(),
        "--out",
        out_path.to_str().unwrap(),
    ]);
    let rendered = std::fs::read_to_string(&out_path).expect("read output");
    assert!(rendered.contains("alice"), "{rendered}");
}

// ===========================================================================
// Phase C, Task 12: cross-vault mv/copy via workspace aliases
// ===========================================================================

#[test]
fn mv_alias_to_alias_moves_across_stores() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:MOVE_ME", "--value", "moved-value"]);

    env.ok(&["mv", "work:MOVE_ME", "stage:/"]);

    let dest_value = env.ok(&["get", "stage:MOVE_ME", "--raw"]);
    assert_eq!(dest_value.trim(), "moved-value");

    // Source must be gone: it was a MOVE, not a copy.
    let err = env.err(&["get", "work:MOVE_ME"]);
    assert_eq!(
        err.status.code(),
        Some(10),
        "expected xv-secret-not-found exit code"
    );
}

#[test]
fn mv_alias_preserves_record_envelope_and_metadata() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&[
        "set",
        "work:CREDS",
        "--type",
        "login",
        "--field",
        "username=alice",
        "--value",
        "hunter2",
        "--group",
        "team-a",
        "--note",
        "important",
    ]);

    env.ok(&["mv", "work:CREDS", "stage:/"]);

    // The typed record's metadata field survives the cross-vault move.
    let field = env.ok(&["get", "stage:CREDS", "--field", "username", "--raw"]);
    assert!(field.contains("alice"), "{field}");
    // The primary secret field survives too.
    let record = env.ok(&["get", "stage:CREDS", "--record", "--format", "json"]);
    assert!(record.contains("hunter2"), "{record}");
}

#[test]
fn copy_accepts_aliases_in_from_to() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:SHARED", "--value", "copy-me"]);

    env.ok(&["copy", "SHARED", "--from", "work", "--to", "stage"]);

    let dest_value = env.ok(&["get", "stage:SHARED", "--raw"]);
    assert_eq!(dest_value.trim(), "copy-me");
    // Source must still exist — this is a copy, not a move.
    let src_value = env.ok(&["get", "work:SHARED", "--raw"]);
    assert_eq!(src_value.trim(), "copy-me");
}

// ===========================================================================
// Code review follow-up (Phase C): every mv form must resolve through the
// workspace — dest-only alias, source-only alias, fully unqualified, and
// the degenerate same-vault case — not just the both-sides-aliased form.
// ===========================================================================

#[test]
fn mv_unqualified_targets_workspace_default_not_config_vault() {
    let env = WorkspaceEnv::new();
    // Seed the SAME secret name in BOTH the plain (pre-workspace) default
    // vault and, after attaching, the workspace default entry — with
    // DIFFERENT values, so a wrong-vault mv is observable either way.
    env.ok(&["set", "SAME_NAME", "--value", "config-vault-value"]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
        "--default",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&[
        "set",
        "work:SAME_NAME",
        "--value",
        "workspace-default-value",
    ]);

    // Fully unqualified mv, no alias on either side.
    env.ok(&["mv", "SAME_NAME", "RENAMED"]);

    // The workspace default (work/local-a) was renamed.
    let renamed = env.ok(&["get", "work:RENAMED", "--raw"]);
    assert_eq!(renamed.trim(), "workspace-default-value");
    let missing = env.err(&["get", "work:SAME_NAME"]);
    assert!(!missing.status.success());

    // The plain config-level default vault (store_default, pre-workspace)
    // must be UNTOUCHED — proving unqualified mv resolved against the
    // WORKSPACE default, not `resolve_vault_for_trait`'s config-level
    // vault. Probed via an explicit backend-qualified xv:// URI, which
    // bypasses the workspace entirely (Task 11), rather than `get`, which
    // would search the ATTACHED vaults only and miss this vault either way.
    let template_path = env.home.join("check_raw.txt");
    std::fs::write(&template_path, "v: xv://local:default/SAME_NAME\n").expect("write template");
    let out_path = env.home.join("check_raw_out.txt");
    env.ok(&[
        "inject",
        "--template",
        template_path.to_str().unwrap(),
        "--out",
        out_path.to_str().unwrap(),
    ]);
    let rendered = std::fs::read_to_string(&out_path).expect("read output");
    assert!(rendered.contains("config-vault-value"), "{rendered}");
}

#[test]
fn mv_source_only_alias_moves_from_named_vault_to_default() {
    let env = WorkspaceEnv::new();
    // "stage" is default; "work" is NOT.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
        "--default",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&["set", "work:SRC_ONLY", "--value", "src-only-value"]);

    // Dest is unqualified ("archive/") -> resolves to the DEFAULT ("stage").
    // Before the fix, this treated "work:SRC_ONLY" as a literal name and
    // failed not-found instead of moving it cross-vault.
    env.ok(&["mv", "work:SRC_ONLY", "archive/"]);

    let value = env.ok(&["get", "stage:SRC_ONLY", "--raw"]);
    assert_eq!(value.trim(), "src-only-value");
    let listing = env.ok(&["ls", "--format", "json"]);
    assert!(listing.contains("archive"), "{listing}");
    let gone = env.err(&["get", "work:SRC_ONLY"]);
    assert!(!gone.status.success());
}

#[test]
fn mv_dest_only_alias_moves_to_named_vault_with_new_name() {
    let env = WorkspaceEnv::new();
    // "stage" is default; "work" is NOT.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
        "--default",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&["set", "stage:DST_ONLY", "--value", "dst-only-value"]);

    // Source unqualified -> resolves to the DEFAULT ("stage"); dest
    // alias-qualified -> "work". Before the fix, this fell through to
    // `parse_mv`'s `/`-only grammar and silently renamed the secret to a
    // LITERAL name `work:renamed-dst` in the SAME (default) vault.
    env.ok(&["mv", "DST_ONLY", "work:renamed-dst"]);

    let value = env.ok(&["get", "work:renamed-dst", "--raw"]);
    assert_eq!(value.trim(), "dst-only-value");
    let gone = env.err(&["get", "stage:DST_ONLY"]);
    assert!(!gone.status.success());
    // Regression guard: no literal secret named `work:renamed-dst` exists
    // in the default ("stage") vault — the historical silent-wrong-target bug.
    let no_literal = env.err(&["get", "stage:work:renamed-dst"]);
    assert!(!no_literal.status.success());
}

#[test]
fn mv_alias_qualified_source_matching_default_degenerates_to_same_vault_rename() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
        "--default",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    env.ok(&["set", "work:DEGEN_SRC", "--value", "degen-value"]);

    // "work" IS the default here — source and dest both resolve to the
    // SAME (backend, vault), so this must degenerate to the ordinary
    // same-vault rename path (execute_secret_mv), not a cross-vault
    // copy+delete, and must still succeed correctly.
    env.ok(&["mv", "work:DEGEN_SRC", "DEGEN_DST"]);

    let value = env.ok(&["get", "work:DEGEN_DST", "--raw"]);
    assert_eq!(value.trim(), "degen-value");
    let gone = env.err(&["get", "work:DEGEN_SRC"]);
    assert!(!gone.status.success());
}

#[test]
fn mv_source_exact_name_with_colon_wins_over_alias_when_default_differs() {
    let env = WorkspaceEnv::new();
    // Default is "stage" (NOT "work") — so alias interpretation of the
    // "work:" prefix and the exact-name-first literal probe (scoped to the
    // default vault, "stage" per the Phase A write rule) diverge observably.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
        "--default",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);

    // A secret LITERALLY named "work:x" sitting in the DEFAULT vault (stage).
    env.ok(&["set", "stage:work:x", "--value", "literal-value"]);

    env.ok(&["mv", "work:x", "moved-literal"]);

    // Must have renamed the LITERAL "work:x" in "stage" (the default), not
    // treated "work" as an alias pointing at the "work" backend/vault.
    let value = env.ok(&["get", "stage:moved-literal", "--raw"]);
    assert_eq!(value.trim(), "literal-value");
    // The "work" alias's vault must be untouched (nothing was ever set there).
    let missing = env.err(&["get", "work:x"]);
    assert!(!missing.status.success());
}

// ===========================================================================
// Code review follow-up: xv:// URI alias-vs-raw-vault-name precedence, both
// directions in one test.
// ===========================================================================

#[test]
fn uri_alias_precedence_over_raw_vault_of_same_name() {
    let env = WorkspaceEnv::new();
    // Raw vault "default" on the ACTIVE (top-level "local") backend, BEFORE
    // any workspace exists.
    env.ok(&["set", "RAW_SECRET", "--value", "raw-active-value"]);
    // Alias "default" attached on a DIFFERENT backend (local-a), same NAME
    // as the raw vault above.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "default",
    ]);
    env.ok(&["set", "default:RAW_SECRET", "--value", "aliased-value"]);

    let template_path = env.home.join("tpl_precedence.txt");
    std::fs::write(
        &template_path,
        "unqualified: xv://default/RAW_SECRET\nqualified: xv://local:default/RAW_SECRET\n",
    )
    .expect("write template");
    let out_path = env.home.join("out_precedence.txt");
    env.ok(&[
        "inject",
        "--template",
        template_path.to_str().unwrap(),
        "--out",
        out_path.to_str().unwrap(),
    ]);
    let rendered = std::fs::read_to_string(&out_path).expect("read output");
    // Unqualified xv://default/... resolves via the ALIAS — wins over the
    // raw vault of the same name.
    assert!(
        rendered.contains("unqualified: aliased-value"),
        "{rendered}"
    );
    // Explicit backend-qualified xv://local:default/... bypasses the alias
    // and reaches the raw vault.
    assert!(
        rendered.contains("qualified: raw-active-value"),
        "{rendered}"
    );
}
